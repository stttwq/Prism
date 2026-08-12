//! 文件操作：回收站、永久删除、复制到、移动到、重命名。
//!
//! 所有操作使用 `IFileOperation`，在 STA worker 上执行。Windows 标准 UI 处理
//! 重名、权限、进度和取消。永久删除强制不可关闭的系统确认。
//!
//! 设计原则：
//! - broker 重新验证目标路径（绝对路径、非空、非 NUL）
//! - `IFileOperation::SetOperationFlags` 控制行为
//! - 用户取消映射 `ShellOutcome::Cancelled`
//! - 永久删除使用 `FOF_WANTNUKEWARNING`，不可关闭确认

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

/// 创建 `IFileOperation` COM 实例。
#[cfg(windows)]
fn create_file_operation() -> Result<windows::Win32::UI::Shell::IFileOperation, ShellError> {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::FileOperation;
    unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))
}

// ── 公开操作 ──────────────────────────────────────────────────

/// 回收站：将文件/目录移入回收站（可恢复）。
///
/// 使用 `IFileOperation` + `FOF_ALLOWUNDO`。Windows 标准 UI 处理确认、
/// 冲突和进度。用户取消返回 `Cancelled`。
#[cfg(windows)]
pub(crate) fn recycle(target: &ActionTarget) -> Result<ShellOutcome, ShellError> {
    use windows::Win32::UI::Shell::{FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR};

    validate_file_target(target)?;
    let item = shell_item_from_path(&target.value)?;
    let op = create_file_operation()?;

    // FOF_ALLOWUNDO: 移入回收站（可恢复）。
    // FOF_NOCONFIRMMKDIR: 创建目录不需要额外确认。
    // 不设 FOF_NOCONFIRMATION: 让 Windows 显示删除确认。
    unsafe {
        op.SetOperationFlags(FILEOPERATION_FLAGS(
            FOF_ALLOWUNDO.0 | FOF_NOCONFIRMMKDIR.0,
        ))
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

    validate_file_target(target)?;
    let item = shell_item_from_path(&target.value)?;
    let op = create_file_operation()?;

    // 不设 FOF_ALLOWUNDO: 不可恢复。
    // FOF_WANTNUKEWARNING: 强制显示永久删除警告，不可关闭。
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
pub(crate) fn copy_to(target: &ActionTarget, destination: &str) -> Result<ShellOutcome, ShellError> {
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
pub(crate) fn move_to(target: &ActionTarget, destination: &str) -> Result<ShellOutcome, ShellError> {
    use windows::Win32::UI::Shell::{FILEOPERATION_FLAGS, FOF_NOCONFIRMMKDIR};

    validate_file_target(target)?;
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

    validate_file_target(target)?;

    // 验证新 leaf name
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

    // 检查新文件名是否与当前 leaf name 相同（不区分大小写）。
    // IFileOperation 在源=目标时返回 E_INVALIDARG (0x80070057)，提前拦截给出可读错误。
    if let Some(current_leaf) = std::path::Path::new(&target.value)
        .file_name()
        .and_then(|n| n.to_str())
    {
        if current_leaf.eq_ignore_ascii_case(trimmed) {
            return Err(ShellError::new(
                ShellErrorKind::TargetInvalid,
                "新文件名与当前文件名相同",
            ));
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
fn perform_operations(op: &windows::Win32::UI::Shell::IFileOperation) -> Result<ShellOutcome, ShellError> {
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
    } else if hr == windows::core::HRESULT(0x80070002u32 as i32) || hr == windows::core::HRESULT(0x80070003u32 as i32) {
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
    ShellError::new(kind, format!("IFileOperation failed: 0x{:08X}", hr.0 as u32))
}

// ── 非 Windows 平台 stub ──────────────────────────────────────

#[cfg(not(windows))]
pub(crate) fn recycle(_target: &crate::shell::ActionTarget) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
    Err(crate::shell::ShellError::new(
        crate::shell::ShellErrorKind::Unsupported,
        "file operations are only available on Windows",
    ))
}

#[cfg(not(windows))]
pub(crate) fn delete_permanent(_target: &crate::shell::ActionTarget) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
    Err(crate::shell::ShellError::new(
        crate::shell::ShellErrorKind::Unsupported,
        "file operations are only available on Windows",
    ))
}

#[cfg(not(windows))]
pub(crate) fn copy_to(_target: &crate::shell::ActionTarget, _destination: &str) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
    Err(crate::shell::ShellError::new(
        crate::shell::ShellErrorKind::Unsupported,
        "file operations are only available on Windows",
    ))
}

#[cfg(not(windows))]
pub(crate) fn move_to(_target: &crate::shell::ActionTarget, _destination: &str) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
    Err(crate::shell::ShellError::new(
        crate::shell::ShellErrorKind::Unsupported,
        "file operations are only available on Windows",
    ))
}

#[cfg(not(windows))]
pub(crate) fn rename(_target: &crate::shell::ActionTarget, _new_name: &str) -> Result<crate::shell::ShellOutcome, crate::shell::ShellError> {
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
    fn rename_rejects_same_name_case_insensitive() {
        let target = ActionTarget::new(TargetKind::File, r"C:\temp\test.txt");
        // 完全相同
        let err = rename(&target, "test.txt").unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("相同"));
        // 大小写不同但名字相同
        let err = rename(&target, "TEST.TXT").unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("相同"));
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
}
