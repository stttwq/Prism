//! 动作面板：文件/文件夹/应用目标的结构化动作 allowlist。
//!
//! 动作 id 是稳定封闭枚举，由 `ActionId` 定义。broker 根据 target kind 重新验证
//! action 与参数，不信任 WPF 传来的路径/命令。所有目标在执行前重新解析存在性
//! 和类型。`ActionId` 的变体即协议上限——前端不能发明新 id。
//!
//! 消息合同见 frontend-spec.md §6 流程 D、§7。

use crate::ipc::ActionItem;
use crate::shell::{ActionTarget, ShellError, ShellErrorKind, TargetKind};
use std::str::FromStr;

/// 稳定封闭的动作 id 枚举。序列化/反序列化走小写 `snake_case`。
///
/// 协议上限：前端只能传这些 id。未知 id 在 `ActionId::from_str` 里被拒，
/// 不进入 `run_action_direct`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    // ── 文件/文件夹通用 ──────────────────────────────────────────
    /// 打开所在文件夹并选中（explorer /select）。
    OpenFolder,
    /// 复制到剪贴板（CF_HDROP + DROPEFFECT_COPY）。
    Copy,
    /// 剪切到剪贴板（CF_HDROP + DROPEFFECT_MOVE）。
    Cut,
    /// 复制路径文本到剪贴板（CF_UNICODETEXT）。
    CopyPath,
    /// 系统属性页（ShellExecute "properties"）。
    Properties,
    /// 系统打开方式对话框（ShellExecute "openas"）。仅文件。
    OpenWith,
    /// 重命名（WPF 内联编辑 → leaf 验证 → Shell worker 执行）。
    Rename,
    /// 复制到…（显式子流程：DestinationPicker → IFileOperation）。
    CopyTo,
    /// 移动到…（显式子流程：DestinationPicker → IFileOperation）。
    MoveTo,
    /// 移入回收站（IFileOperation，可恢复）。
    Recycle,
    /// 永久删除（IFileOperation，强制不可关闭的系统确认）。
    DeletePermanent,
    /// 压缩为 ZIP（7-Zip 优先，Windows 11 内置回退）。
    Zip,

    // ── 应用专属 ────────────────────────────────────────────────
    /// 定位真实可执行程序所在文件夹。
    LocateApp,
    /// 复制真实可执行程序路径到剪贴板。
    CopyAppPath,
    /// 查看真实可执行程序属性。
    AppProperties,
    /// 以管理员身份运行真实可执行程序（ShellExecute "runas"）。
    RunAsAdmin,
}

/// 解析动作 id 字符串失败时的错误。未知 id 返回此错误，
/// 由调用方映射为 `ShellErrorKind::Unsupported`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownActionError;

impl std::fmt::Display for UnknownActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown action id")
    }
}

impl std::error::Error for UnknownActionError {}

impl FromStr for ActionId {
    type Err = UnknownActionError;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        match id {
            "open_folder" => Ok(Self::OpenFolder),
            "copy" => Ok(Self::Copy),
            "cut" => Ok(Self::Cut),
            "copy_path" => Ok(Self::CopyPath),
            "properties" => Ok(Self::Properties),
            "open_with" => Ok(Self::OpenWith),
            "rename" => Ok(Self::Rename),
            "copy_to" => Ok(Self::CopyTo),
            "move_to" => Ok(Self::MoveTo),
            "recycle" => Ok(Self::Recycle),
            "delete_permanent" => Ok(Self::DeletePermanent),
            "zip" => Ok(Self::Zip),
            "locate_app" => Ok(Self::LocateApp),
            "copy_app_path" => Ok(Self::CopyAppPath),
            "app_properties" => Ok(Self::AppProperties),
            "run_as_admin" => Ok(Self::RunAsAdmin),
            _ => Err(UnknownActionError),
        }
    }
}

impl ActionId {
    /// 序列化用的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenFolder => "open_folder",
            Self::Copy => "copy",
            Self::Cut => "cut",
            Self::CopyPath => "copy_path",
            Self::Properties => "properties",
            Self::OpenWith => "open_with",
            Self::Rename => "rename",
            Self::CopyTo => "copy_to",
            Self::MoveTo => "move_to",
            Self::Recycle => "recycle",
            Self::DeletePermanent => "delete_permanent",
            Self::Zip => "zip",
            Self::LocateApp => "locate_app",
            Self::CopyAppPath => "copy_app_path",
            Self::AppProperties => "app_properties",
            Self::RunAsAdmin => "run_as_admin",
        }
    }

    /// Segoe Fluent Icons 字形码。无图标返回空字符串。
    fn icon_glyph(self) -> &'static str {
        match self {
            Self::OpenFolder => "\u{E8DA}", // OpenFolderHorizontal
            Self::Copy | Self::CopyPath | Self::CopyAppPath | Self::CopyTo => "\u{E8C8}", // Copy
            Self::Cut | Self::MoveTo => "\u{E8C6}", // Cut
            Self::Properties | Self::AppProperties => "\u{E946}", // Page / Properties
            Self::OpenWith => "\u{E7B7}",   // OpenWith
            Self::Rename => "\u{E8AC}",     // Rename
            Self::Recycle => "\u{E74D}",    // Delete (recycle)
            Self::DeletePermanent => "\u{E74D}", // Delete (permanent)
            Self::Zip => "\u{E7F8}",        // ZipFolder
            Self::LocateApp => "\u{E8DA}",  // OpenFolderHorizontal
            Self::RunAsAdmin => "\u{E7EF}", // Shield / Admin
        }
    }

    /// 动作的人类可读标签。
    fn label(self) -> &'static str {
        match self {
            Self::OpenFolder => "打开所在文件夹",
            Self::Copy => "复制",
            Self::Cut => "剪切",
            Self::CopyPath => "复制路径至剪贴板",
            Self::Properties => "属性",
            Self::OpenWith => "打开方式",
            Self::Rename => "重命名",
            Self::CopyTo => "复制到…",
            Self::MoveTo => "移动到…",
            Self::Recycle => "移入回收站",
            Self::DeletePermanent => "永久删除",
            Self::Zip => "压缩为 ZIP",
            Self::LocateApp => "打开所在文件夹",
            Self::CopyAppPath => "复制路径至剪贴板",
            Self::AppProperties => "属性",
            Self::RunAsAdmin => "以管理员身份运行",
        }
    }

    /// 该动作是否需要 mutation 子流程（DestinationPicker / RenameEditor / 进度等）。
    /// mutation 动作在第一批尚未接入 Shell worker，由 `run_action_direct` 显式拒绝。
    #[allow(dead_code)]
    fn is_mutation(self) -> bool {
        matches!(
            self,
            Self::Rename
                | Self::CopyTo
                | Self::MoveTo
                | Self::Recycle
                | Self::DeletePermanent
                | Self::Zip
        )
    }
}

/// 返回某 target kind 允许的动作列表。顺序即面板显示顺序。
///
/// broker 在此处重新验证 target kind，不信任 WPF 传来的路径/命令。
/// Unknown/Window/Web target 不执行文件动作副作用。
pub fn list_actions(target: &ActionTarget) -> Result<Vec<ActionItem>, ShellError> {
    let kind = target.validate()?;
    let allowed = allowed_actions(kind);
    if allowed.is_empty() {
        return Err(ShellError::new(
            ShellErrorKind::Unsupported,
            "该目标不支持文件动作",
        ));
    }
    Ok(allowed.into_iter().map(action_item).collect())
}

/// target kind → 允许的 action id 列表。这是协议上限的 allowlist。
fn allowed_actions(kind: TargetKind) -> Vec<ActionId> {
    match kind {
        TargetKind::File => vec![
            ActionId::OpenFolder,
            ActionId::Copy,
            ActionId::Cut,
            ActionId::CopyPath,
            ActionId::Properties,
            ActionId::OpenWith,
            ActionId::Rename,
            ActionId::CopyTo,
            ActionId::MoveTo,
            ActionId::Recycle,
            ActionId::DeletePermanent,
            ActionId::Zip,
        ],
        TargetKind::Directory => vec![
            ActionId::OpenFolder,
            ActionId::Copy,
            ActionId::Cut,
            ActionId::CopyPath,
            ActionId::Properties,
            ActionId::Rename,
            ActionId::CopyTo,
            ActionId::MoveTo,
            ActionId::Recycle,
            ActionId::DeletePermanent,
            ActionId::Zip,
        ],
        TargetKind::Application => vec![
            ActionId::OpenFolder,
            ActionId::CopyAppPath,
            ActionId::AppProperties,
            ActionId::RunAsAdmin,
        ],
        // Window / Web 不进入文件动作面板
        TargetKind::Window | TargetKind::Web => Vec::new(),
        // K0：命令身份不能进文件动作面板——list_actions 因 allowed.is_empty() 返 Unsupported
        TargetKind::Command => Vec::new(),
    }
}

fn action_item(id: ActionId) -> ActionItem {
    ActionItem {
        id: id.as_str().into(),
        label: id.label().into(),
        icon_glyph: id.icon_glyph().into(),
        has_submenu: false,
        is_section_header: false,
        // K2 §4.2：内置动作默认值——invocation_kind=builtin_action、is_enabled=true，
        // 其余 None。skip_serializing_if 保证不出现在 JSON 里。
        invocation_kind: "builtin_action".into(),
        command_id: None,
        is_enabled: true,
        disabled_reason: None,
    }
}

/// 执行动作。`action` 为 `list_actions` 返回的 id 字符串。
///
/// broker 在此处重新解析 `ActionId`，未知 id 被拒。mutation 动作在第一批
/// 尚未接入 Shell worker，显式返回 `Unsupported`。
/// L4（全仓复审 2026-08-22）：返回类型化 ShellError，错误类别在构造点显式
/// 给出——此前 shell.rs 用中文子串从消息文本反推类别，改一句文案就会改掉
/// 前端分支的错误分类。
pub(crate) fn run_action_direct(path: &str, action: &str) -> Result<(), ShellError> {
    let id = action
        .parse::<ActionId>()
        .map_err(|_| ShellError::new(ShellErrorKind::Unsupported, format!("未知动作：{action}")))?;
    validate_path(path)?;
    match id {
        ActionId::OpenFolder => reveal_in_explorer(path),
        ActionId::Copy => clipboard_set_files(path, preferred_drop_effect_copy()),
        ActionId::Cut => clipboard_set_files(path, preferred_drop_effect_move()),
        ActionId::CopyPath => clipboard_set_text(path),
        ActionId::CopyAppPath => clipboard_set_text(path),
        // 第一批：mutation 动作和无 mutation 的 Properties/OpenWith/LocateApp/AppProperties/RunAsAdmin
        // 由 ShellOperation 直接路由（Properties/OpenWith）或留待第二批接入。
        // 这里只处理直接可执行的无 mutation 动作。
        ActionId::Properties
        | ActionId::OpenWith
        | ActionId::LocateApp
        | ActionId::AppProperties
        | ActionId::RunAsAdmin => Err(ShellError::new(
            ShellErrorKind::Unsupported,
            format!("{action} 由 Shell 路由直接处理，不应进入 run_action_direct"),
        )),
        ActionId::Rename
        | ActionId::CopyTo
        | ActionId::MoveTo
        | ActionId::Recycle
        | ActionId::DeletePermanent
        | ActionId::Zip => Err(ShellError::new(
            ShellErrorKind::Unsupported,
            format!("{action} 尚未实现（mutation 动作第二批接入）"),
        )),
    }
}

fn validate_path(path: &str) -> Result<(), ShellError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(ShellError::new(ShellErrorKind::TargetInvalid, "路径为空"));
    }
    if path.contains('\0') {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "路径含非法字符",
        ));
    }
    if !std::path::Path::new(path).is_absolute() {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "拒绝相对路径",
        ));
    }
    Ok(())
}

/// explorer /select,"path"
#[cfg(windows)]
fn reveal_in_explorer(path: &str) -> Result<(), ShellError> {
    use std::os::windows::process::CommandExt;

    let normalized = path.replace('/', "\\");
    let arg = format!("/select,\"{normalized}\"");
    std::process::Command::new("explorer")
        .raw_arg(arg)
        .spawn()
        .map(|_| {
            crate::logging::event("info", "shell_reveal_complete", None, None);
        })
        .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))
}

#[cfg(not(windows))]
fn reveal_in_explorer(path: &str) -> Result<(), ShellError> {
    Err(ShellError::new(
        ShellErrorKind::Unsupported,
        format!("非 Windows 平台无法定位：{path}"),
    ))
}

// ── 剪贴板：文本 ──────────────────────────────────────────────

#[cfg(windows)]
fn clipboard_set_text(text: &str) -> Result<(), ShellError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
    use windows::Win32::System::DataExchange::{EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let wide: Vec<u16> = std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let bytes = wide.len() * 2;

    unsafe {
        if OpenClipboard(HWND::default()).is_err() {
            return Err(ShellError::new(ShellErrorKind::System, "无法打开剪贴板"));
        }
        let _close = scopeguard_close();
        // L6（全仓复审 2026-08-22）：先分配并锁定，再清空——EmptyClipboard 之后
        // 任何失败路径都会让用户白丢原有剪贴板内容。
        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes)
            .map_err(|e| ShellError::new(ShellErrorKind::System, format!("GlobalAlloc：{e}")))?;
        if hmem.is_invalid() {
            return Err(ShellError::new(
                ShellErrorKind::System,
                "GlobalAlloc 返回空",
            ));
        }
        let ptr = GlobalLock(hmem);
        if ptr.is_null() {
            let _ = GlobalFree(hmem);
            return Err(ShellError::new(ShellErrorKind::System, "GlobalLock 失败"));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
        let _ = GlobalUnlock(hmem);

        EmptyClipboard()
            .map_err(|e| ShellError::new(ShellErrorKind::System, format!("清空剪贴板失败：{e}")))?;

        if SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(hmem.0)).is_err() {
            let _ = GlobalFree(hmem);
            return Err(ShellError::new(
                ShellErrorKind::System,
                "SetClipboardData 文本失败",
            ));
        }
        // 成功后系统接管 hmem，不要 GlobalFree。
        crate::logging::event("info", "clipboard_copy_path_complete", None, None);
        Ok(())
    }
}

#[cfg(not(windows))]
fn clipboard_set_text(text: &str) -> Result<(), ShellError> {
    let _ = text;
    Err(ShellError::new(
        ShellErrorKind::Unsupported,
        "非 Windows 平台无剪贴板",
    ))
}

// ── 剪贴板：文件（CF_HDROP + Preferred DropEffect） ───────────

/// DROPEFFECT_COPY = 1
fn preferred_drop_effect_copy() -> u32 {
    1
}
/// DROPEFFECT_MOVE = 2
fn preferred_drop_effect_move() -> u32 {
    2
}

#[cfg(windows)]
fn clipboard_set_files(path: &str, drop_effect: u32) -> Result<(), ShellError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
    use windows::Win32::System::DataExchange::{
        EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::DROPFILES;

    let normalized = path.replace('/', "\\");
    let wide: Vec<u16> = std::ffi::OsStr::new(&normalized)
        .encode_wide()
        .chain(std::iter::once(0)) // 路径结尾
        .chain(std::iter::once(0)) // 双 NUL 结束列表
        .collect();

    let header_size = std::mem::size_of::<DROPFILES>();
    let list_bytes = wide.len() * 2;
    let total = header_size + list_bytes;

    unsafe {
        if OpenClipboard(HWND::default()).is_err() {
            return Err(ShellError::new(ShellErrorKind::System, "无法打开剪贴板"));
        }
        let _close = scopeguard_close();

        // L6（全仓复审 2026-08-22）：先分配并填充，再清空——EmptyClipboard 之后
        // 的失败路径会白丢用户原有剪贴板内容。
        let hmem = GlobalAlloc(GMEM_MOVEABLE, total)
            .map_err(|e| ShellError::new(ShellErrorKind::System, format!("GlobalAlloc：{e}")))?;
        if hmem.is_invalid() {
            return Err(ShellError::new(
                ShellErrorKind::System,
                "GlobalAlloc 返回空",
            ));
        }
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(hmem);
            return Err(ShellError::new(ShellErrorKind::System, "GlobalLock 失败"));
        }
        // 清零并写 DROPFILES 头。
        std::ptr::write_bytes(ptr, 0, total);
        let df = ptr as *mut DROPFILES;
        (*df).pFiles = header_size as u32;
        (*df).fWide = windows::Win32::Foundation::BOOL(1);
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr.add(header_size), list_bytes);
        let _ = GlobalUnlock(hmem);

        EmptyClipboard()
            .map_err(|e| ShellError::new(ShellErrorKind::System, format!("清空剪贴板失败：{e}")))?;

        // CF_HDROP
        if SetClipboardData(CF_HDROP.0 as u32, HANDLE(hmem.0)).is_err() {
            let _ = GlobalFree(hmem);
            return Err(ShellError::new(
                ShellErrorKind::System,
                "SetClipboardData HDROP 失败",
            ));
        }

        // Preferred DropEffect（复制 vs 剪切）；失败不回滚 HDROP（资源管理器仍可粘贴为复制）。
        // L6：失败记事件日志——剪切静默降级为复制时，至少留有诊断痕迹。
        let fmt_name: Vec<u16> =
            OsStrExt::encode_wide(std::ffi::OsStr::new("Preferred DropEffect"))
                .chain(std::iter::once(0))
                .collect();
        let fmt = RegisterClipboardFormatW(PCWSTR(fmt_name.as_ptr()));
        if fmt != 0 {
            if let Ok(heffect) = GlobalAlloc(GMEM_MOVEABLE, 4) {
                if !heffect.is_invalid() {
                    let ep = GlobalLock(heffect) as *mut u32;
                    if ep.is_null() {
                        let _ = GlobalFree(heffect);
                    } else {
                        *ep = drop_effect;
                        let _ = GlobalUnlock(heffect);
                        if SetClipboardData(fmt, HANDLE(heffect.0)).is_err() {
                            let _ = GlobalFree(heffect);
                            crate::logging::event(
                                "warn",
                                "clipboard_drop_effect_failed",
                                None,
                                None,
                            );
                        }
                    }
                }
            }
        }

        let verb = if drop_effect == preferred_drop_effect_move() {
            "cut"
        } else {
            "copy"
        };
        crate::logging::event(
            "info",
            if verb == "cut" {
                "clipboard_cut_complete"
            } else {
                "clipboard_copy_complete"
            },
            None,
            None,
        );
        Ok(())
    }
}

#[cfg(not(windows))]
fn clipboard_set_files(path: &str, drop_effect: u32) -> Result<(), ShellError> {
    let _ = (path, drop_effect);
    Err(ShellError::new(
        ShellErrorKind::Unsupported,
        "非 Windows 平台无剪贴板",
    ))
}

/// RAII：离开作用域时 CloseClipboard。
#[cfg(windows)]
struct ClipboardGuard;
#[cfg(windows)]
fn scopeguard_close() -> ClipboardGuard {
    ClipboardGuard
}
#[cfg(windows)]
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::DataExchange::CloseClipboard();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_actions_rejects_relative() {
        let target = ActionTarget::new(TargetKind::File, "relative\\x.txt");
        assert!(list_actions(&target).is_err());
    }

    #[test]
    fn list_actions_has_basics() {
        let target = ActionTarget::new(TargetKind::File, r"C:\Windows\explorer.exe");
        let items = list_actions(&target).expect("ok");
        let ids: Vec<_> = items.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"open_folder"));
        assert!(ids.contains(&"copy"));
        assert!(ids.contains(&"cut"));
        assert!(ids.contains(&"copy_path"));
        assert!(items.iter().all(|a| !a.is_section_header));
    }

    #[test]
    fn file_allowlist_includes_all_file_actions() {
        let target = ActionTarget::new(TargetKind::File, r"C:\Windows\explorer.exe");
        let items = list_actions(&target).expect("ok");
        let ids: Vec<_> = items.iter().map(|a| a.id.as_str()).collect();
        // 文件比文件夹多 OpenWith
        assert!(ids.contains(&"open_with"));
        assert!(ids.contains(&"rename"));
        assert!(ids.contains(&"copy_to"));
        assert!(ids.contains(&"move_to"));
        assert!(ids.contains(&"recycle"));
        assert!(ids.contains(&"delete_permanent"));
        assert!(ids.contains(&"zip"));
        assert!(ids.contains(&"properties"));
    }

    #[test]
    fn directory_allowlist_excludes_open_with() {
        let target = ActionTarget::new(TargetKind::Directory, r"C:\Windows");
        let items = list_actions(&target).expect("ok");
        let ids: Vec<_> = items.iter().map(|a| a.id.as_str()).collect();
        assert!(!ids.contains(&"open_with"), "目录不应有打开方式");
        assert!(ids.contains(&"rename"));
        assert!(ids.contains(&"zip"));
    }

    #[test]
    fn application_allowlist_is_app_only() {
        let target = ActionTarget::new(TargetKind::Application, r"C:\Windows\explorer.exe");
        let items = list_actions(&target).expect("ok");
        let ids: Vec<_> = items.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "open_folder",
                "copy_app_path",
                "app_properties",
                "run_as_admin",
            ]
        );
    }

    #[test]
    fn window_and_web_have_no_actions() {
        let window = ActionTarget::new(TargetKind::Window, "12345");
        assert!(list_actions(&window).is_err());
        let web = ActionTarget::new(TargetKind::Web, "https://example.com");
        assert!(list_actions(&web).is_err());
    }

    #[test]
    fn run_unknown_action_errors() {
        let err = run_action_direct(r"C:\Windows\explorer.exe", "nope").unwrap_err();
        assert!(err.message.contains("未知动作"));
    }

    #[test]
    fn action_id_roundtrip() {
        for id in [
            ActionId::OpenFolder,
            ActionId::Copy,
            ActionId::Cut,
            ActionId::CopyPath,
            ActionId::Properties,
            ActionId::OpenWith,
            ActionId::Rename,
            ActionId::CopyTo,
            ActionId::MoveTo,
            ActionId::Recycle,
            ActionId::DeletePermanent,
            ActionId::Zip,
            ActionId::LocateApp,
            ActionId::CopyAppPath,
            ActionId::AppProperties,
            ActionId::RunAsAdmin,
        ] {
            let s = id.as_str();
            assert_eq!(s.parse::<ActionId>(), Ok(id), "roundtrip failed for {s}");
        }
    }

    #[test]
    fn unknown_action_id_rejected() {
        assert!("not_a_real_action".parse::<ActionId>().is_err());
        assert!("".parse::<ActionId>().is_err());
    }

    #[test]
    fn mutation_actions_are_not_yet_implemented() {
        let path = r"C:\Windows\explorer.exe";
        for action in [
            "rename",
            "copy_to",
            "move_to",
            "recycle",
            "delete_permanent",
            "zip",
        ] {
            let err = run_action_direct(path, action).unwrap_err();
            assert!(
                err.message.contains("尚未实现"),
                "mutation action {action} should be rejected in batch 1: {:?}",
                err
            );
        }
    }

    #[test]
    fn properties_and_open_with_are_routed_not_direct() {
        let path = r"C:\Windows\explorer.exe";
        for action in [
            "properties",
            "open_with",
            "locate_app",
            "app_properties",
            "run_as_admin",
        ] {
            let err = run_action_direct(path, action).unwrap_err();
            assert!(
                err.message.contains("Shell 路由直接处理"),
                "action {action} should be routed via ShellOperation: {:?}",
                err
            );
        }
    }
}
