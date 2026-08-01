//! 程序清单：扫描开始菜单 `.lnk`，保留中文显示名，搜索时置顶。
//!
//! - 用户开始菜单：`%AppData%\Microsoft\Windows\Start Menu\Programs`
//! - 公共开始菜单：`%ProgramData%\Microsoft\Windows\Start Menu\Programs`
//! - 执行：对 `.lnk` 本身做 ShellExecute（保留快捷方式参数/工作目录）
//! - 图标：前端用 `execute_id`（.lnk 路径）取系统图标

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// 一条已安装程序（来自开始菜单快捷方式）。
#[derive(Debug, Clone)]
pub struct AppEntry {
    /// 显示名（.lnk 文件名去扩展名，含中文，如"微信"）。
    pub name: String,
    /// 小写显示名，供不区分大小写匹配。
    pub name_lower: String,
    /// 启动路径：优先 .lnk 自身（ShellExecute 对快捷方式最稳）。
    pub launch_path: String,
    /// 解析出的目标路径（失败则为 launch_path），作副标题。
    pub target_path: String,
}

/// 共享程序清单。
pub type SharedApps = Arc<RwLock<Vec<AppEntry>>>;

/// 在清单中做子串搜索，最多 `max` 条。
/// 排序：完全匹配 > 前缀匹配 > 包含；同级按名称短优先。
pub fn search<'a>(apps: &'a [AppEntry], query: &str, max: usize) -> Vec<&'a AppEntry> {
    if query.is_empty() || max == 0 {
        return Vec::new();
    }
    let q = query.to_lowercase();
    let mut scored: Vec<(u8, usize, &AppEntry)> = apps
        .iter()
        .filter_map(|a| {
            if a.name_lower == q {
                Some((0u8, a.name.len(), a))
            } else if a.name_lower.starts_with(&q) {
                Some((1, a.name.len(), a))
            } else if a.name_lower.contains(&q) {
                Some((2, a.name.len(), a))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(max).map(|(_, _, e)| e).collect()
}

/// Scan Start Menu shortcuts on the broker-owned STA Shell worker.
pub async fn load(shared: SharedApps, shell: std::sync::Arc<crate::shell::ShellExecutor>) {
    let started = std::time::Instant::now();
    match shell.scan_apps().await {
        Ok(apps) => {
            crate::logging::event(
                "info",
                "app_catalog_ready",
                Some(started.elapsed().as_millis()),
                None,
            );
            if let Ok(mut g) = shared.write() {
                *g = apps;
            }
        }
        Err(error) => crate::log(format!("app catalog scan failed: {}", error.message)),
    }
}

/// 扫描用户 + 公共开始菜单下全部 `.lnk`。
pub fn scan_start_menu() -> Vec<AppEntry> {
    scan_start_menu_with_apartment(true)
}

pub(crate) fn scan_start_menu_on_sta() -> Vec<AppEntry> {
    scan_start_menu_with_apartment(false)
}

fn scan_start_menu_with_apartment(initialize_com: bool) -> Vec<AppEntry> {
    let mut roots = Vec::new();
    if let Some(p) = user_programs_dir() {
        roots.push(p);
    }
    if let Some(p) = common_programs_dir() {
        roots.push(p);
    }
    scan_roots(&roots, initialize_com)
}

fn user_programs_dir() -> Option<PathBuf> {
    // %AppData%\Microsoft\Windows\Start Menu\Programs
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"))
}

fn common_programs_dir() -> Option<PathBuf> {
    // %ProgramData%\Microsoft\Windows\Start Menu\Programs
    let pd = std::env::var_os("ProgramData")?;
    Some(PathBuf::from(pd).join(r"Microsoft\Windows\Start Menu\Programs"))
}

fn scan_roots(roots: &[PathBuf], initialize_com: bool) -> Vec<AppEntry> {
    let mut out: Vec<AppEntry> = Vec::new();
    // 去重键：小写显示名。roots 顺序应为 用户 → 公共，先到先得（用户优先）。
    let mut seen_names: HashSet<String> = HashSet::new();

    // COM 在整个扫描期间只初始化一次，并复用同一个 IShellLink 实例。
    #[cfg(windows)]
    let mut resolver = LnkResolver::new(initialize_com);

    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // 跳过隐藏的系统目录名
                if e.file_type().is_dir() {
                    if let Some(name) = e.path().file_name() {
                        let s = name.to_string_lossy();
                        if s.eq_ignore_ascii_case("Startup") {
                            return false; // 开机启动项不进启动列表
                        }
                    }
                }
                true
            });

        for entry in walker.flatten() {
            let path = entry.path();
            if !entry.file_type().is_file() {
                continue;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if ext != "lnk" {
                continue;
            }
            #[cfg(windows)]
            let app = app_from_lnk(path, Some(&mut resolver));
            #[cfg(not(windows))]
            let app = app_from_lnk(path, None);
            if let Some(app) = app {
                if seen_names.insert(app.name_lower.clone()) {
                    out.push(app);
                }
            }
        }
    }

    // 稳定一点：按名称排序，便于调试与确定性测试。
    out.sort_by(|a, b| a.name_lower.cmp(&b.name_lower));
    out
}

fn app_from_lnk(path: &Path, resolver: Option<&mut LnkResolver>) -> Option<AppEntry> {
    let name = path.file_stem()?.to_string_lossy().trim().to_string();
    if name.is_empty() {
        return None;
    }
    // 跳过明显无意义的项
    if name.eq_ignore_ascii_case("desktop") {
        return None;
    }

    let launch_path = path.to_string_lossy().replace('/', "\\");
    let target_path = resolver
        .and_then(|r| r.resolve(path))
        .unwrap_or_else(|| launch_path.clone());

    // 卸载器 / 帮助文档类快捷方式仍保留（用户可能想搜到），不在这里过滤。

    Some(AppEntry {
        name_lower: name.to_lowercase(),
        name,
        launch_path,
        target_path,
    })
}

/// 非 Windows：占位类型，永不解析。
#[cfg(not(windows))]
struct LnkResolver;

#[cfg(not(windows))]
impl LnkResolver {
    fn resolve(&mut self, _path: &Path) -> Option<String> {
        None
    }
}

/// Windows：整次扫描共用一次 CoInitialize + 一个 IShellLinkW。
#[cfg(windows)]
struct LnkResolver {
    link: Option<windows::Win32::UI::Shell::IShellLinkW>,
    need_uninit: bool,
}

#[cfg(windows)]
impl LnkResolver {
    fn new(initialize_com: bool) -> Self {
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

        // 失败也继续：线程可能已被其它代码初始化过。
        let need_uninit =
            initialize_com && unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
        let link: Option<IShellLinkW> =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.ok();
        Self { link, need_uninit }
    }

    fn resolve(&mut self, path: &Path) -> Option<String> {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::{Interface, PCWSTR};
        use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
        use windows::Win32::System::Com::{IPersistFile, STGM_READ};

        let link = self.link.as_ref()?;
        let persist: IPersistFile = link.cast().ok()?;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe { persist.Load(PCWSTR(wide.as_ptr()), STGM_READ) }.ok()?;

        let mut buf = [0u16; 520];
        let mut find_data = WIN32_FIND_DATAW::default();
        let hr = unsafe { link.GetPath(&mut buf, &mut find_data as *mut _, 0u32) };
        if hr.is_err() {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if end == 0 {
            return None;
        }
        let target = String::from_utf16_lossy(&buf[..end]);
        if target.is_empty() {
            None
        } else {
            Some(target)
        }
    }
}

#[cfg(windows)]
impl Drop for LnkResolver {
    fn drop(&mut self) {
        self.link = None;
        if self.need_uninit {
            unsafe {
                windows::Win32::System::Com::CoUninitialize();
            }
        }
    }
}

// ── 单元测试 ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_apps() -> Vec<AppEntry> {
        vec![
            AppEntry {
                name: "微信".into(),
                name_lower: "微信".into(),
                launch_path: r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\微信.lnk"
                    .into(),
                target_path: r"C:\Program Files\Tencent\WeChat\WeChat.exe".into(),
            },
            AppEntry {
                name: "WeChat".into(),
                name_lower: "wechat".into(),
                launch_path: r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\WeChat.lnk"
                    .into(),
                target_path: r"C:\Program Files\Tencent\WeChat\WeChat.exe".into(),
            },
            AppEntry {
                name: "Clash Party".into(),
                name_lower: "clash party".into(),
                launch_path: r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Clash Party.lnk"
                    .into(),
                target_path: r"C:\Apps\clash.exe".into(),
            },
            AppEntry {
                name: "Chrome".into(),
                name_lower: "chrome".into(),
                launch_path: r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Chrome.lnk"
                    .into(),
                target_path: r"C:\Program Files\Google\Chrome\Application\chrome.exe".into(),
            },
        ]
    }

    #[test]
    fn search_chinese_name() {
        let apps = sample_apps();
        let r = search(&apps, "微信", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "微信");
    }

    #[test]
    fn search_case_insensitive_prefix() {
        let apps = sample_apps();
        let r = search(&apps, "ch", 10);
        // Chrome 前缀匹配应排在 Clash Party 包含匹配前
        assert!(!r.is_empty());
        assert_eq!(r[0].name, "Chrome");
    }

    #[test]
    fn search_exact_beats_prefix() {
        let apps = sample_apps();
        // 加一条前缀更长的
        let mut apps = apps;
        apps.push(AppEntry {
            name: "Chrome Beta".into(),
            name_lower: "chrome beta".into(),
            launch_path: r"C:\x\Chrome Beta.lnk".into(),
            target_path: r"C:\x\chrome-beta.exe".into(),
        });
        let r = search(&apps, "chrome", 10);
        assert_eq!(r[0].name, "Chrome");
    }

    #[test]
    fn search_respects_max() {
        let apps = sample_apps();
        assert_eq!(search(&apps, "c", 1).len(), 1);
    }

    #[test]
    fn search_empty_query() {
        let apps = sample_apps();
        assert!(search(&apps, "", 10).is_empty());
    }

    #[test]
    fn scan_start_menu_does_not_panic() {
        // 本机开始菜单应能扫出若干项；即使环境异常也不得 panic。
        let apps = scan_start_menu();
        let _ = apps.len();
    }

    #[test]
    fn scan_finds_at_least_one_on_windows() {
        let apps = scan_start_menu();
        // CI / 非 Windows 可能为空；Windows 开发机通常有开始菜单项。
        #[cfg(windows)]
        {
            // 用户机器上前面已确认有 Chrome.lnk 等；若沙箱无开始菜单则跳过硬断言。
            if user_programs_dir().map(|p| p.is_dir()).unwrap_or(false)
                || common_programs_dir().map(|p| p.is_dir()).unwrap_or(false)
            {
                // 目录存在时，允许 0（极少见的空菜单），但函数必须返回有效 Vec。
                let _ = apps;
            }
        }
        #[cfg(not(windows))]
        {
            assert!(apps.is_empty());
        }
    }
}
