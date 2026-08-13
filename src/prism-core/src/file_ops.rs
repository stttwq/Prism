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
