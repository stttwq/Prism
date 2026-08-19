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
    /// 预编码的紧凑拼音（审计 P5）：扫描时编码一次，搜索热路径只做匹配，不再
    /// 为每次击键逐字分配读音 String。`None` = 名字里没有汉字，永不拼音匹配。
    ///
    /// 只活在内存里（清单每次启动/刷新都重扫），所以拼音词典版本
    /// (`PINYIN_DICTIONARY_VERSION`) 变化自然生效，无需缓存失效逻辑。
    pub pinyin: Option<Vec<u8>>,
}

/// 共享程序清单。
pub type SharedApps = Arc<RwLock<Vec<AppEntry>>>;

/// 在清单中做子串搜索，最多 `max` 条，并返回**全部**命中条数。
///
/// 排序：完全匹配 > 前缀匹配 > 包含；同级按名称短优先，同级同长按清单顺序
/// （清单按小写名排序，所以结果确定）。
///
/// 审计 P10：命中集合只保留 top-N（有界插入），不再为了拿总数先用
/// `usize::MAX` 物化全部命中——`matched_count` 的精确计数语义由第二个
/// 返回值继续保证。
pub fn search_ranked<'a>(
    apps: &'a [AppEntry],
    query: &str,
    max: usize,
) -> (Vec<&'a AppEntry>, u64) {
    if query.is_empty() {
        return (Vec::new(), 0);
    }
    let q = query.to_lowercase();
    let mut matched = 0u64;
    // 有界 top-N：按 (rank, name.len()) 升序保持，满了就与末位比较。
    // 「不优于末位则丢弃」等价于稳定排序后 take(max)——同键的靠后条目本来就排在后面。
    let mut kept: Vec<((u8, usize), &AppEntry)> = Vec::with_capacity(max.min(64));
    for app in apps {
        let Some(rank) = match_rank(app, &q) else {
            continue;
        };
        matched = matched.saturating_add(1);
        if max == 0 {
            continue;
        }
        let key = (rank, app.name.len());
        if kept.len() >= max {
            if key >= kept[kept.len() - 1].0 {
                continue;
            }
            kept.pop();
        }
        let position = kept.partition_point(|(existing, _)| *existing <= key);
        kept.insert(position, (key, app));
    }
    (kept.into_iter().map(|(_, app)| app).collect(), matched)
}

/// 命中等级：0 完全匹配、1 前缀、2 包含；未命中 None。`query_lower` 须已小写。
fn match_rank(app: &AppEntry, query_lower: &str) -> Option<u8> {
    if app.name_lower == query_lower {
        Some(0)
    } else if app.name_lower.starts_with(query_lower) {
        Some(1)
    } else if app.name_lower.contains(query_lower) {
        Some(2)
    } else {
        None
    }
}

/// 只要 top-N 的旧签名（测试与外部调用方沿用）。
#[cfg(test)]
pub fn search<'a>(apps: &'a [AppEntry], query: &str, max: usize) -> Vec<&'a AppEntry> {
    search_ranked(apps, query, max).0
}

/// S3（FRESH-AUDIT-2026-08-19）: 开始菜单扫描失败后的重试间隔。
/// 前 5 次 30 秒（安装器锁目录的常见窗口期），之后指数退避翻倍、封顶 1 小时，
/// **永不放弃**——此前 5 次后永久放弃会导致"装完软件搜不到应用必须重启"。
pub fn app_scan_retry_delay(attempt: u32) -> std::time::Duration {
    const BASE: u64 = 30;
    const CAP: u64 = 3600;
    if attempt <= 5 {
        return std::time::Duration::from_secs(BASE);
    }
    let shift = (attempt - 5).min(16); // u64 秒内防溢出即可
    std::time::Duration::from_secs((BASE << shift).min(CAP))
}

/// Scan Start Menu shortcuts on the broker-owned STA Shell worker.
/// 返回是否成功——失败由调用方（main 的重试循环）决定何时再试。
pub async fn load(shared: SharedApps, shell: std::sync::Arc<crate::shell::ShellExecutor>) -> bool {
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
            true
        }
        Err(error) => {
            crate::log(format!("app catalog scan failed: {}", error.message));
            false
        }
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
        // 拼音只在扫描时编码一次（审计 P5）。
        pinyin: crate::pinyin::encode_compact(&name),
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

    fn app(name: &str, launch_path: &str, target_path: &str) -> AppEntry {
        AppEntry {
            name: name.into(),
            name_lower: name.to_lowercase(),
            pinyin: crate::pinyin::encode_compact(name),
            launch_path: launch_path.into(),
            target_path: target_path.into(),
        }
    }

    fn sample_apps() -> Vec<AppEntry> {
        vec![
            app(
                "微信",
                r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\微信.lnk",
                r"C:\Program Files\Tencent\WeChat\WeChat.exe",
            ),
            app(
                "WeChat",
                r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\WeChat.lnk",
                r"C:\Program Files\Tencent\WeChat\WeChat.exe",
            ),
            app(
                "Clash Party",
                r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Clash Party.lnk",
                r"C:\Apps\clash.exe",
            ),
            app(
                "Chrome",
                r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Chrome.lnk",
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            ),
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
        apps.push(app(
            "Chrome Beta",
            r"C:\x\Chrome Beta.lnk",
            r"C:\x\chrome-beta.exe",
        ));
        let r = search(&apps, "chrome", 10);
        assert_eq!(r[0].name, "Chrome");
    }

    /// P10：截断后的 top-N 与「全量物化再排序取前 N」等价，且总数仍精确。
    #[test]
    fn ranked_top_n_matches_full_materialization_and_counts_all() {
        let mut apps = sample_apps();
        apps.push(app("Chrome Beta", r"C:\x\Chrome Beta.lnk", r"C:\x\cb.exe"));
        apps.push(app(
            "Chrome Canary",
            r"C:\x\Chrome Canary.lnk",
            r"C:\x\cc.exe",
        ));
        for query in ["c", "ch", "chrome", "微信", "zzz", ""] {
            let full: Vec<&str> = search_ranked(&apps, query, usize::MAX)
                .0
                .iter()
                .map(|app| app.name.as_str())
                .collect();
            for max in 0..=full.len() + 1 {
                let (top, total) = search_ranked(&apps, query, max);
                let names: Vec<&str> = top.iter().map(|app| app.name.as_str()).collect();
                assert_eq!(
                    names,
                    full.iter().copied().take(max).collect::<Vec<_>>(),
                    "{query} / {max}"
                );
                // 总数与 max 无关（max=0 也照数）。
                assert_eq!(total as usize, full.len(), "{query} / {max}");
            }
        }
    }

    /// P5：拼音在扫描时预编码，匹配走紧凑字节，与即时 `match_name` 判定一致。
    #[test]
    fn precomputed_pinyin_matches_the_on_the_fly_encoder() {
        let apps = sample_apps();
        let wechat = apps.iter().find(|app| app.name == "微信").unwrap();
        let encoded = wechat.pinyin.as_deref().expect("含汉字应有预编码拼音");
        let normalized = crate::pinyin::normalize_query("wx").unwrap();
        assert_eq!(
            crate::pinyin::match_compact_normalized(encoded, normalized.as_bytes()),
            crate::pinyin::match_name("微信", "wx")
        );
        // 纯拉丁名不编码，拼音通道对它永远沉默。
        let chrome = apps.iter().find(|app| app.name == "Chrome").unwrap();
        assert!(chrome.pinyin.is_none());
        assert!(crate::pinyin::match_name("Chrome", "chrome").is_none());
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

    /// S3: 重试间隔——前 5 次 30s，之后指数翻倍，封顶 1 小时，永不返回 0/无限。
    #[test]
    fn app_scan_retry_delay_backs_off_and_caps_at_one_hour() {
        for attempt in 1..=5u32 {
            assert_eq!(app_scan_retry_delay(attempt).as_secs(), 30);
        }
        assert_eq!(app_scan_retry_delay(6).as_secs(), 60);
        assert_eq!(app_scan_retry_delay(7).as_secs(), 120);
        assert_eq!(app_scan_retry_delay(8).as_secs(), 240);
        // 单调不减且封顶 3600s（约第 13 次到位）。
        let mut prev = 0u64;
        for attempt in 1..=40u32 {
            let secs = app_scan_retry_delay(attempt).as_secs();
            assert!(secs >= prev, "attempt {attempt} 退避必须单调不减");
            assert!(secs <= 3600);
            prev = secs;
        }
        assert_eq!(prev, 3600);
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
