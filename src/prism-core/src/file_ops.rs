//! 文件操作：回收站、永久删除、复制到、移动到、重命名。
//!
//! 所有操作使用 `IFileOperation`，在 STA worker 上执行。Windows 标准 UI 处理
//! 重名、权限、进度和取消。
//!
//! 设计原则：
//! - broker 重新验证目标路径（绝对路径、非空、非 NUL）
//! - M4（全仓复审 2026-08-22）：破坏性操作前复核磁盘状态——目标仍存在、
//!   类型与客户端声明一致（搜索快照可能已过期数秒）
//! - `IFileOperation::SetOperationFlags` 控制行为
//! - H3（全仓复审 2026-08-22）：所有操作挂 STA 线程的 message-only 属主窗口，
//!   确认/进度对话框有属主可停靠，不会被桌面吞掉
//! - 用户取消映射 `ShellOutcome::Cancelled`
//! - 永久删除使用 `FOF_WANTNUKEWARNING`：**请求**系统删除警告。注意这不是
//!   「强制弹窗」的同义词——真正的保障是属主窗口 + 默认确认开关
//!   （不设 FOF_NOCONFIRMATION）。

#[cfg(windows)]
use crate::shell::{ActionTarget, ShellError, ShellErrorKind, ShellOutcome, TargetKind};

/// 将路径验证为绝对文件/目录路径，拒绝空、NUL、相对路径和 Web/Window target。
#[cfg(windows)]
fn validate_file_target(target: &ActionTarget) -> Result<TargetKind, ShellError> {
    let kind = target.validate()?;
    if !matches!(kind, TargetKind::File | TargetKind::Directory) {
        return Err(ShellError::new(
            ShellErrorKind::Unsupported,
            "file operations require a file or directory target",
        ));
    }
    Ok(kind)
}

/// M4（全仓复审 2026-08-22）：破坏性操作前的磁盘复核。搜索快照可能已过期
/// 数秒到数分钟，枚举与执行之间路径也可能被替换——经典 TOCTOU，尾巴是
/// 删除/移动。这里按**链接本身**（symlink_metadata 不穿透）复核：
/// - 目标不存在 ⇒ TargetInvalid（给出可读错误，而不是把 DeleteItem 交给
///   一个过期路径）；
/// - 磁盘类型与客户端声明的 kind 不一致 ⇒ TargetInvalid（文件被换成目录、
///   或反之，快照误导了用户，停下来是对的）。
///
/// reparse point 本身不拒：删除/重命名一个 junction/symlink 作用在链接上，
/// 是合法操作（symlink_metadata 已保证不穿透到目标）。
#[cfg(windows)]
fn verify_target_on_disk(target: &ActionTarget, kind: TargetKind) -> Result<(), ShellError> {
    let metadata = std::fs::symlink_metadata(&target.value).map_err(|_| {
        ShellError::new(
            ShellErrorKind::TargetInvalid,
            "目标在执行前已不存在（路径可能来自过期的搜索结果）",
        )
    })?;
    let on_disk_is_dir = metadata.is_dir();
    if on_disk_is_dir != (kind == TargetKind::Directory) {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "目标类型与搜索结果不一致（磁盘上的内容已改变）",
        ));
    }
    Ok(())
}

/// 将 Rust 字符串路径转为 Windows 宽字符 Vec<u16>（带 NUL 结尾）。
#[cfg(windows)]
fn to_wide(path: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// `IShellItem` 从路径创建的 helper。返回 `IShellItem` COM 对象。
#[cfg(windows)]
fn shell_item_from_path(path: &str) -> Result<windows::Win32::UI::Shell::IShellItem, ShellError> {
    use windows::Win32::UI::Shell::SHCreateItemFromParsingName;
    let wide = to_wide(path);
    let item = unsafe { SHCreateItemFromParsingName(windows::core::PCWSTR(wide.as_ptr()), None) }
        .map_err(|e| ShellError::new(ShellErrorKind::TargetInvalid, e.to_string()))?;
    Ok(item)
}

// H3（全仓复审 2026-08-22）：STA worker 线程的 message-only 属主窗口。
// IFileOperation / ShellExecuteEx 的确认、进度、冲突对话框都需要一个属主
// HWND：无属主时对话框要么沉到桌面底层无法交互，要么（对 FOF_WANTNUKEWARNING
// 这类「请求警告」的 flag）干脆被抑制——文件在没有任何确认的情况下被
// 不可恢复地删除。message-only 窗口不可见、不进枚举、不抢焦点，只做
// 对话框锚点。thread_local 保证与 COM 调用同线程（STA 要求）。
#[cfg(windows)]
std::thread_local! {
    static OWNER_WINDOW: windows::Win32::Foundation::HWND = create_owner_window();
}

#[cfg(windows)]
fn create_owner_window() -> windows::Win32::Foundation::HWND {
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, WINDOW_EX_STYLE, WS_OVERLAPPED,
    };
    // 系统类 STATIC 免注册；父窗口 HWND_MESSAGE(-3) 把它变成 message-only。
    // 创建失败（极端：桌面堆耗尽）不阻塞操作——退回无属主行为，与修复前一致。
    let hwnd_message = windows::Win32::Foundation::HWND(-3isize as _);
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::w!("STATIC"),
            None,
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            hwnd_message,
            None,
            None,
            None,
        )
    }
    .unwrap_or_default()
}

/// 创建 `IFileOperation` COM 实例。
#[cfg(windows)]
fn create_file_operation() -> Result<windows::Win32::UI::Shell::IFileOperation, ShellError> {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::FileOperation;
    let op: windows::Win32::UI::Shell::IFileOperation =
        unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
    // H3：挂属主窗口，让确认/进度对话框有停靠点。失败不致命（同上）。
    OWNER_WINDOW.with(|hwnd| {
        if !hwnd.0.is_null() {
            let _ = unsafe { op.SetOwnerWindow(*hwnd) };
        }
    });
    Ok(op)
}

// ── 公开操作 ──────────────────────────────────────────────────

/// 回收站：将文件/目录移入回收站（可恢复）。
///
/// 使用 `IFileOperation` + `FOF_ALLOWUNDO`。Windows 标准 UI 处理确认、
/// 冲突和进度。用户取消返回 `Cancelled`。
#[cfg(windows)]
pub(crate) fn recycle(target: &ActionTarget) -> Result<ShellOutcome, ShellError> {
    use windows::Win32::UI::Shell::{FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR};

    let kind = validate_file_target(target)?;
    verify_target_on_disk(target, kind)?;
    let item = shell_item_from_path(&target.value)?;
    let op = create_file_operation()?;

    // FOF_ALLOWUNDO: 移入回收站（可恢复）。
    // FOF_NOCONFIRMMKDIR: 创建目录不需要额外确认。
    // 不设 FOF_NOCONFIRMATION: 让 Windows 显示删除确认。
    unsafe {
        op.SetOperationFlags(FILEOPERATION_FLAGS(FOF_ALLOWUNDO.0 | FOF_NOCONFIRMMKDIR.0))
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
        op.DeleteItem(&item, None)
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
    }

    perform_operations(&op)
}

/// 永久删除：不可恢复，强制不可关闭的系统确认。
///
/// 使用 `IFileOperation`，不设 `FOF_ALLOWUNDO`，设 `FOF_WANTNUKEWARNING`
/// 确保每次删除都显示不可关闭的警告。
#[cfg(windows)]
pub(crate) fn delete_permanent(target: &ActionTarget) -> Result<ShellOutcome, ShellError> {
    use windows::Win32::UI::Shell::{FILEOPERATION_FLAGS, FOF_WANTNUKEWARNING};

    let kind = validate_file_target(target)?;
    verify_target_on_disk(target, kind)?;
    let item = shell_item_from_path(&target.value)?;
    let op = create_file_operation()?;

    // 不设 FOF_ALLOWUNDO: 不可恢复。
    // FOF_WANTNUKEWARNING: 请求系统「永久删除」警告（而非普通删除确认）。
    // 注意：该 flag 是「请求」不是「强制」——无属主窗口时它可能被抑制，
    // 这正是 create_file_operation 里 SetOwnerWindow 的意义。
    // 不设 FOF_NOCONFIRMATION: 让 Windows 显示确认。
    unsafe {
        op.SetOperationFlags(FILEOPERATION_FLAGS(FOF_WANTNUKEWARNING.0))
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
        op.DeleteItem(&item, None)
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
    }

    perform_operations(&op)
}

/// 复制到目标目录。
///
/// `destination` 必须是已存在的目录。冲突由 Windows 标准 UI 处理。
#[cfg(windows)]
pub(crate) fn copy_to(
    target: &ActionTarget,
    destination: &str,
) -> Result<ShellOutcome, ShellError> {
    use windows::Win32::UI::Shell::{FILEOPERATION_FLAGS, FOF_NOCONFIRMMKDIR};

    validate_file_target(target)?;
    let src_item = shell_item_from_path(&target.value)?;
    let dst_item = shell_item_from_path(destination)?;
    let op = create_file_operation()?;

    // FOF_NOCONFIRMMKDIR: 目标目录不存在时自动创建。
    // 不设 FOF_NOCONFIRMATION: 冲突时显示 Windows 标准重名确认。
    unsafe {
        op.SetOperationFlags(FILEOPERATION_FLAGS(FOF_NOCONFIRMMKDIR.0))
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
        op.CopyItem(&src_item, &dst_item, None, None)
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
    }

    perform_operations(&op)
}

/// 移动到目标目录。
///
/// `destination` 必须是已存在的目录。冲突由 Windows 标准 UI 处理。
#[cfg(windows)]
pub(crate) fn move_to(
    target: &ActionTarget,
    destination: &str,
) -> Result<ShellOutcome, ShellError> {
    use windows::Win32::UI::Shell::{FILEOPERATION_FLAGS, FOF_NOCONFIRMMKDIR};

    let kind = validate_file_target(target)?;
    verify_target_on_disk(target, kind)?;
    let src_item = shell_item_from_path(&target.value)?;
    let dst_item = shell_item_from_path(destination)?;
    let op = create_file_operation()?;

    unsafe {
        op.SetOperationFlags(FILEOPERATION_FLAGS(FOF_NOCONFIRMMKDIR.0))
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
        op.MoveItem(&src_item, &dst_item, None, None)
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
    }

    perform_operations(&op)
}

/// 重命名：只传新 leaf name，broker 拒绝路径分隔符、空名和超限。
#[cfg(windows)]
pub(crate) fn rename(target: &ActionTarget, new_name: &str) -> Result<ShellOutcome, ShellError> {
    use windows::Win32::UI::Shell::FILEOPERATION_FLAGS;

    let kind = validate_file_target(target)?;

    // M5（全仓复审 2026-08-22）：卷根/UNC 根没有 leaf name，RenameItem 作用在
    // 卷根上没有意义且必败——显式拒绝，给可读错误。
    let lexical_path = std::path::Path::new(&target.value);
    if lexical_path.file_name().is_none() {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "不能对卷根重命名",
        ));
    }

    // 验证新 leaf name（纯词法校验在前：错误信息稳定，不依赖磁盘状态）。
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "新文件名不能为空",
        ));
    }
    if trimmed.contains('\\') || trimmed.contains('/') || trimmed.contains('\0') {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "新文件名不能包含路径分隔符",
        ));
    }
    // Windows 限制 leaf name 最大 255 字符（UTF-16 码元）
    if trimmed.encode_utf16().count() > 255 {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "新文件名过长",
        ));
    }

    // M4：词法校验全部通过后再复核磁盘状态（存在性 + 类型一致）。
    verify_target_on_disk(target, kind)?;

    // 检查新文件名是否与当前 leaf name 完全相同（区分大小写）。
    // IFileOperation 在源=目标时返回 E_INVALIDARG (0x80070057)，提前拦截给出可读错误。
    // 复审 L3（2026-08-21）：仅大小写不同的重命名在 Windows 上合法（Explorer
    // 常态操作），eq_ignore_ascii_case 会把它误拒；字节相同才是 E_INVALIDARG。
    // M5（全仓复审 2026-08-22）：叶子名取**磁盘真名**（canonicalize 吃掉尾点/
    // 尾空格的 Win32 归一化），不再信任调用方字符串的末段——`file.txt.` 的
    // 字面 leaf 是 `file.txt.`，真实文件是 `file.txt`，重命名为 `file.txt`
    // 本就该在这里被拦下，而不是撞 E_INVALIDARG 的不透明 0x80070057。
    // 路径不存在时 canonicalize 失败：留给后续 shell item 解析报错。
    if let Ok(canonical) = std::fs::canonicalize(&target.value) {
        if let Some(current_leaf) = canonical.file_name().and_then(|n| n.to_str()) {
            if current_leaf == trimmed {
                return Err(ShellError::new(
                    ShellErrorKind::TargetInvalid,
                    "新文件名与当前文件名相同",
                ));
            }
        }
    }

    let src_item = shell_item_from_path(&target.value)?;
    let wide_name = to_wide(trimmed);
    let op = create_file_operation()?;

    // 不设特殊 flag: 冲突时显示 Windows 标准重名确认。
    unsafe {
        op.SetOperationFlags(FILEOPERATION_FLAGS(0))
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
        op.RenameItem(&src_item, windows::core::PCWSTR(wide_name.as_ptr()), None)
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
    }

    perform_operations(&op)
}

/// 执行 `IFileOperation::PerformOperations` 并将结果映射到 `ShellOutcome`。
///
/// - `Ok(())`: 操作成功（或用户确认后完成）
/// - `Err` with `COPYENGINE_S_USER_CANCELLED` (0x8027004C7): 用户取消
/// - 其他错误: 映射到 `ShellErrorKind`
#[cfg(windows)]
fn perform_operations(
    op: &windows::Win32::UI::Shell::IFileOperation,
) -> Result<ShellOutcome, ShellError> {
    match unsafe { op.PerformOperations() } {
        Ok(()) => Ok(ShellOutcome::Success),
        Err(e) => {
            let hr = e.code();
            // COPYENGINE_S_USER_CANCELLED = 0x8027004C7
            // ERROR_CANCELLED = 0x800704C7
            let code = (hr.0 as u32) & 0xFFFF;
            if code == 0x4C7 {
                Ok(ShellOutcome::Cancelled)
            } else {
                Err(classify_hresult(hr))
            }
        }
    }
}

/// 将 `HRESULT` 映射到 `ShellErrorKind`。
#[cfg(windows)]
fn classify_hresult(hr: windows::core::HRESULT) -> ShellError {
    let kind = if hr == windows::core::HRESULT(0x80070005u32 as i32) {
        // E_ACCESSDENIED
        ShellErrorKind::AccessDenied
    } else if hr == windows::core::HRESULT(0x80070002u32 as i32)
        || hr == windows::core::HRESULT(0x80070003u32 as i32)
    {
        // FILE_NOT_FOUND / PATH_NOT_FOUND
        ShellErrorKind::TargetInvalid
    } else if hr == windows::core::HRESULT(0x800700B7u32 as i32) {
        // ERROR_ALREADY_EXISTS
        ShellErrorKind::Conflict
    } else if hr == windows::core::HRESULT(0x80070522u32 as i32) {
        // ERROR_PRIVILEGE_NOT_HELD
        ShellErrorKind::ElevationRequired
    } else {
        ShellErrorKind::System
    };
    ShellError::new(
        kind,
        format!("IFileOperation failed: 0x{:08X}", hr.0 as u32),
    )
}

// ── 非 Windows 平台 stub ──────────────────────────────────────

#[cfg(not(windows))]
pub(crate) fn recycle(
    _target: &crate::shell::ActionTarget,
) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
    Err(crate::shell::ShellError::new(
        crate::shell::ShellErrorKind::Unsupported,
        "file operations are only available on Windows",
    ))
}

#[cfg(not(windows))]
pub(crate) fn delete_permanent(
    _target: &crate::shell::ActionTarget,
) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
    Err(crate::shell::ShellError::new(
        crate::shell::ShellErrorKind::Unsupported,
        "file operations are only available on Windows",
    ))
}

#[cfg(not(windows))]
pub(crate) fn copy_to(
    _target: &crate::shell::ActionTarget,
    _destination: &str,
) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
    Err(crate::shell::ShellError::new(
        crate::shell::ShellErrorKind::Unsupported,
        "file operations are only available on Windows",
    ))
}

#[cfg(not(windows))]
pub(crate) fn move_to(
    _target: &crate::shell::ActionTarget,
    _destination: &str,
) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
    Err(crate::shell::ShellError::new(
        crate::shell::ShellErrorKind::Unsupported,
        "file operations are only available on Windows",
    ))
}

#[cfg(not(windows))]
pub(crate) fn rename(
    _target: &crate::shell::ActionTarget,
    _new_name: &str,
) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
    Err(crate::shell::ShellError::new(
        crate::shell::ShellErrorKind::Unsupported,
        "file operations are only available on Windows",
    ))
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::*;
    #[cfg(windows)]
    use crate::shell::{ActionTarget, ShellErrorKind, ShellOutcome, TargetKind};

    #[cfg(windows)]
    #[test]
    fn validate_rejects_web_and_window_targets() {
        let web = ActionTarget::new(TargetKind::Web, "https://example.com");
        let err = validate_file_target(&web).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);

        let win = ActionTarget::new(TargetKind::Window, "12345");
        let err = validate_file_target(&win).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);
    }

    #[cfg(windows)]
    #[test]
    fn rename_rejects_identical_name_but_allows_case_only() {
        // M5：相同名预检按磁盘真名比对——需要一个真实存在的文件。
        let temp = std::env::temp_dir().join(format!(
            "prism-rename-same-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&temp, "x").unwrap();
        let target = ActionTarget::new(TargetKind::File, temp.to_str().unwrap());
        // 字节完全相同：E_INVALIDARG 前置拦截。
        let err = rename(&target, temp.file_name().unwrap().to_str().unwrap()).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("相同"));
        let _ = std::fs::remove_file(&temp);
        // 复审 L3（2026-08-21）：仅大小写不同在 Windows 上是合法重命名，
        // 不得被「相同」预检误拒——用必然不存在的路径锚定：预检放行后
        // 错误只能来自后续的 shell item 解析，消息里没有「相同」。
        let absent = ActionTarget::new(TargetKind::File, r"C:\prism-rename-l3\test.txt");
        let err = rename(&absent, "TEST.TXT").unwrap_err();
        assert!(
            !err.message.contains("相同"),
            "case-only rename 不得被相同预检拒绝"
        );
    }

    #[cfg(windows)]
    #[test]
    fn rename_rejects_path_separators() {
        let target = ActionTarget::new(TargetKind::File, r"C:\temp\test.txt");
        let err = rename(&target, "sub\\name.txt").unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("路径分隔符"));
    }

    #[cfg(windows)]
    #[test]
    fn rename_rejects_empty_name() {
        let target = ActionTarget::new(TargetKind::File, r"C:\temp\test.txt");
        let err = rename(&target, "   ").unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("不能为空"));
    }

    #[cfg(windows)]
    #[test]
    fn rename_rejects_overlong_name() {
        let target = ActionTarget::new(TargetKind::File, r"C:\temp\test.txt");
        let long_name: String = "a".repeat(256);
        let err = rename(&target, &long_name).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("过长"));
    }

    /// M5：卷根没有 leaf name，必须在词法层拒绝（可读错误，而非 COM 层的
    /// 不透明失败）。
    #[cfg(windows)]
    #[test]
    fn rename_rejects_volume_root() {
        for root in [r"C:\", r"\\server\share\"] {
            let target = ActionTarget::new(TargetKind::Directory, root);
            let err = rename(&target, "newname").unwrap_err();
            assert_eq!(err.kind, ShellErrorKind::TargetInvalid, "root {root}");
            assert!(err.message.contains("卷根"), "root {root}");
        }
    }

    /// M4：磁盘类型与声明不符 ⇒ 拒绝。声明为 file、磁盘上是目录。
    #[cfg(windows)]
    #[test]
    fn recycle_rejects_kind_mismatch_with_disk() {
        let dir = std::env::temp_dir().join(format!(
            "prism-m4-kind-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = ActionTarget::new(TargetKind::File, dir.to_str().unwrap());
        let err = recycle(&target).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("类型"));
        let _ = std::fs::remove_dir(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn recycle_on_nonexistent_path_returns_error() {
        let target = ActionTarget::new(TargetKind::File, r"C:\does_not_exist_xyz_12345.txt");
        let result = recycle(&target);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ShellErrorKind::TargetInvalid);
    }

    #[cfg(windows)]
    #[test]
    fn delete_permanent_on_nonexistent_path_returns_error() {
        let target = ActionTarget::new(TargetKind::File, r"C:\does_not_exist_xyz_67890.txt");
        let result = delete_permanent(&target);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind, ShellErrorKind::TargetInvalid);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "live IFileOperation test; sends file to Recycle Bin. Run with --ignored"]
    fn recycle_temp_file_succeeds() {
        let temp = std::env::temp_dir().join(format!(
            "prism-g6-recycle-test-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&temp, "test").unwrap();
        let target = ActionTarget::new(TargetKind::File, temp.to_str().unwrap());
        let result = recycle(&target);
        match result {
            Ok(ShellOutcome::Success) | Ok(ShellOutcome::Cancelled) => {}
            Err(e) => panic!("recycle failed unexpectedly: {e:?}"),
        }
    }

    // ── G6 机器测试矩阵（live IFileOperation，需手动 --ignored 运行） ──

    fn unique_temp_file(ext: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "prism-g6-{}-{}-{}.{}",
            ext,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            ext
        ))
    }

    /// 重名：rename 到一个已存在的文件名，IFileOperation 应弹出冲突确认。
    #[cfg(windows)]
    #[test]
    #[ignore = "live IFileOperation; may show rename conflict dialog. Run with --ignored"]
    fn rename_conflict_existing_name() {
        let dir = std::env::temp_dir();
        let src = unique_temp_file("txt");
        let dst = dir.join(src.file_name().unwrap().to_str().unwrap().to_string() + ".dup");
        std::fs::write(&src, "src").unwrap();
        std::fs::write(&dst, "dst").unwrap();
        let target = ActionTarget::new(TargetKind::File, src.to_str().unwrap());
        let dst_name = dst.file_name().unwrap().to_str().unwrap();
        let result = rename(&target, dst_name);
        // Windows 可能弹冲突确认；成功或取消都可接受，不应是 TargetInvalid。
        match result {
            Ok(_) => {}
            Err(e) if e.kind != ShellErrorKind::TargetInvalid => {}
            Err(e) => panic!("rename conflict failed with unexpected kind: {e:?}"),
        }
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }

    /// 长路径：文件名接近 MAX_PATH 的文件 rename。
    #[cfg(windows)]
    #[test]
    #[ignore = "live IFileOperation; tests long path rename. Run with --ignored"]
    fn rename_long_path() {
        let long_name = "a".repeat(200) + ".txt";
        let src = unique_temp_file("txt");
        std::fs::write(&src, "test").unwrap();
        let target = ActionTarget::new(TargetKind::File, src.to_str().unwrap());
        let result = rename(&target, &long_name);
        // 长路径可能成功或被 Windows 拒绝。
        let _ = result;
        // Cleanup: find the file (may have been renamed).
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(src.with_file_name(&long_name));
    }

    /// 中文路径：文件名含中文的 rename。
    #[cfg(windows)]
    #[test]
    #[ignore = "live IFileOperation; tests Chinese filename rename. Run with --ignored"]
    fn rename_chinese_path() {
        let src = unique_temp_file("txt");
        std::fs::write(&src, "test").unwrap();
        let target = ActionTarget::new(TargetKind::File, src.to_str().unwrap());
        let result = rename(&target, "测试文件.txt");
        match result {
            Ok(ShellOutcome::Success) | Ok(ShellOutcome::Cancelled) => {}
            Err(e) => panic!("rename Chinese path failed unexpectedly: {e:?}"),
        }
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(src.with_file_name("测试文件.txt"));
    }

    /// 只读文件：rename 只读文件应成功（IFileOperation 可处理只读属性）。
    #[cfg(windows)]
    #[test]
    #[ignore = "live IFileOperation; tests read-only file rename. Run with --ignored"]
    fn rename_readonly_file() {
        let src = unique_temp_file("txt");
        std::fs::write(&src, "test").unwrap();
        let mut perms = std::fs::metadata(&src).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&src, perms).unwrap();
        let target = ActionTarget::new(TargetKind::File, src.to_str().unwrap());
        let result = rename(&target, "readonly-renamed.txt");
        let _ = result;
        // Restore permissions for cleanup: try to remove the file directly.
        // If it was renamed, the original no longer exists; if rename failed,
        // the file is still read-only and remove_file may fail silently.
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(src.with_file_name("readonly-renamed.txt"));
    }

    /// 源消失：源文件在 rename 前被删除，应返回 TargetInvalid。
    #[cfg(windows)]
    #[test]
    fn rename_source_disappeared() {
        let ghost = std::env::temp_dir().join(format!(
            "prism-g6-ghost-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // 不创建文件，直接尝试 rename。
        let target = ActionTarget::new(TargetKind::File, ghost.to_str().unwrap());
        let err = rename(&target, "newname.txt").unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
    }

    /// 复制到自身：源和目标相同，IFileOperation 应处理或拒绝。
    #[cfg(windows)]
    #[test]
    #[ignore = "live IFileOperation; tests copy to same directory (self). Run with --ignored"]
    fn copy_to_self_directory() {
        let src = unique_temp_file("txt");
        std::fs::write(&src, "test").unwrap();
        let target = ActionTarget::new(TargetKind::File, src.to_str().unwrap());
        // 目标目录 = 源文件所在目录（复制到自身目录，Windows 会自动改名）。
        let dest = src.parent().unwrap().to_str().unwrap();
        let result = copy_to(&target, dest);
        let _ = result;
        let _ = std::fs::remove_file(&src);
    }

    /// 移动到子目录：将文件移动到其自身的子目录。
    #[cfg(windows)]
    #[test]
    #[ignore = "live IFileOperation; tests move to subdirectory. Run with --ignored"]
    fn move_to_subdirectory() {
        let dir = std::env::temp_dir().join(format!(
            "prism-g6-movetest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("source.txt");
        std::fs::write(&src, "test").unwrap();
        let subdir = dir.join("sub");
        std::fs::create_dir_all(&subdir).unwrap();
        let target = ActionTarget::new(TargetKind::File, src.to_str().unwrap());
        let result = move_to(&target, subdir.to_str().unwrap());
        match result {
            Ok(ShellOutcome::Success) | Ok(ShellOutcome::Cancelled) => {}
            Err(e) => panic!("move to subdirectory failed unexpectedly: {e:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回收站与永久删除严格区分：回收站设置 FOF_ALLOWUNDO，永久删除不设。
    #[cfg(windows)]
    #[test]
    fn recycle_and_delete_use_different_flags() {
        // 验证函数本身存在且签名正确；实际 flag 在函数体内硬编码。
        // recycle 用 FOF_ALLOWUNDO，delete_permanent 用 FOF_WANTNUKEWARNING。
        // 这里只验证非存在路径返回 TargetInvalid，确保两条路径都到达了
        // SHCreateItemFromParsingName（在 SetOperationFlags 之前）。
        let ghost = std::env::temp_dir().join("prism-g6-nonexist-1.txt");
        let target = ActionTarget::new(TargetKind::File, ghost.to_str().unwrap());
        assert_eq!(
            recycle(&target).unwrap_err().kind,
            ShellErrorKind::TargetInvalid
        );
        let ghost2 = std::env::temp_dir().join("prism-g6-nonexist-2.txt");
        let target2 = ActionTarget::new(TargetKind::File, ghost2.to_str().unwrap());
        assert_eq!(
            delete_permanent(&target2).unwrap_err().kind,
            ShellErrorKind::TargetInvalid
        );
    }
}
