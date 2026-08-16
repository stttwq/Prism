//! Atomic file replacement shared by all persistence layers (cache, sidecar, history).
//!
//! On Windows, `ReplaceFileW` is attempted **first**: one call covers the common
//! "destination exists" case and eliminates the TOCTOU window of the old
//! `exists()` pre-check (the destination being deleted or created between probe
//! and replace would send the replacement down the wrong branch). Only
//! `ERROR_FILE_NOT_FOUND` falls back to a plain rename (the initial-install
//! case). A destination with hidden/system attributes is rejected by
//! `ReplaceFileW` with ACCESS_DENIED — the likelier real-world failure source —
//! and is logged for visibility instead of being silently retried elsewhere.
//! On non-Windows, `std::fs::rename` is already atomic on POSIX.

use std::path::Path;

/// Atomically replaces `destination` with `temporary`.
///
/// `label` is included in error messages so callers can distinguish which
/// persistence layer failed (e.g. "cache", "pinyin sidecar", "history").
pub fn atomic_replace(temporary: &Path, destination: &Path, label: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        if let Err((code, message)) = replace_file_windows(temporary, destination) {
            if code == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.0 {
                // 初始安装：目标尚不存在，rename 即安装。
                return std::fs::rename(temporary, destination)
                    .map_err(|error| format!("install initial {label}: {error}"));
            }
            if code == windows::Win32::Foundation::ERROR_ACCESS_DENIED.0 {
                // hidden/system 属性的目标会被 ReplaceFileW 直接拒绝（真实环境比
                // 双实例竞争更可能的失败源）。只记日志提示人工处置——剥掉属性再
                // 替换会改变文件语义，不在这里做。
                crate::log(format!(
                    "atomic_replace: {label} 被拒绝（ACCESS_DENIED），目标 {destination:?} 可能带 hidden/system 属性"
                ));
            }
            return Err(format!("replace {label}: {message}"));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(temporary, destination).map_err(|error| format!("replace {label}: {error}"))
    }
}

/// 从 `windows::core::Error` 提取经典 Win32 错误码（HRESULT 形如 0x8007xxxx
/// 时取低 16 位），非 Win32 映射的 HRESULT 返回 0。
#[cfg(windows)]
fn win32_code(error: &windows::core::Error) -> u32 {
    let hresult = error.code().0 as u32;
    if (hresult & 0xFFFF_0000) == 0x8007_0000 {
        hresult & 0xFFFF
    } else {
        0
    }
}

/// `ReplaceFileW` 单次尝试，失败时携带 Win32 错误码供调用方分支决策。
#[cfg(windows)]
fn replace_file_windows(temporary: &Path, destination: &Path) -> Result<(), (u32, String)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};

    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let temporary: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        ReplaceFileW(
            PCWSTR(destination.as_ptr()),
            PCWSTR(temporary.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    }
    .map_err(|error| (win32_code(&error), error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("prism-fsutil-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 初始安装：目标不存在 → ReplaceFileW 返回 FILE_NOT_FOUND → 回退 rename 完成，
    /// 与旧 exists() 预判路径行为一致。
    #[test]
    fn initial_install_falls_back_to_rename() {
        let dir = temp_dir("install");
        let tmp = dir.join("data.tmp");
        let dst = dir.join("data.bin");
        std::fs::write(&tmp, b"payload").unwrap();

        atomic_replace(&tmp, &dst, "cache").unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
        assert!(!tmp.exists(), "临时文件在安装后应被移走");
    }

    /// 常态替换：目标已存在 → ReplaceFileW 原地替换为新内容。
    #[test]
    fn existing_destination_is_replaced_in_place() {
        let dir = temp_dir("replace");
        let dst = dir.join("data.bin");
        std::fs::write(&dst, b"old").unwrap();
        let tmp = dir.join("data.tmp");
        std::fs::write(&tmp, b"new").unwrap();

        atomic_replace(&tmp, &dst, "cache").unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), b"new");
        assert!(!tmp.exists(), "临时文件在替换后应被吞掉");
    }

    /// hidden 目标（ReplaceFileW 常见拒绝源）：无论本机 ReplaceFileW 对 hidden
    /// 目标的实际行为如何，都不允许出现"目标丢失/内容撕裂"的中间态——
    /// 成功则内容为新文件，失败则原目标原样保留。
    #[test]
    #[cfg(windows)]
    fn hidden_destination_never_loses_the_file() {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
        };

        let dir = temp_dir("hidden");
        let dst = dir.join("data.bin");
        std::fs::write(&dst, b"old").unwrap();
        let wide: Vec<u16> = dst
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe { SetFileAttributesW(PCWSTR(wide.as_ptr()), FILE_ATTRIBUTE_HIDDEN) }.unwrap();

        let tmp = dir.join("data.tmp");
        std::fs::write(&tmp, b"new").unwrap();
        match atomic_replace(&tmp, &dst, "cache") {
            Ok(()) => assert_eq!(std::fs::read(&dst).unwrap(), b"new"),
            Err(message) => assert_eq!(
                std::fs::read(&dst).unwrap(),
                b"old",
                "替换失败时目标必须原样保留：{message}"
            ),
        }

        // 还原属性，保证 temp 目录清理不受 hidden 影响。
        unsafe { SetFileAttributesW(PCWSTR(wide.as_ptr()), FILE_ATTRIBUTE_NORMAL) }.unwrap();
    }

    /// 临时文件本身缺失是调用方 bug：必须报错而不是静默"成功"。
    #[test]
    fn missing_temporary_is_an_error() {
        let dir = temp_dir("missing-tmp");
        let dst = dir.join("data.bin");
        std::fs::write(&dst, b"old").unwrap();

        let error = atomic_replace(&dir.join("absent.tmp"), &dst, "cache").unwrap_err();
        assert!(error.contains("cache"), "错误信息应带 label：{error}");
        assert_eq!(std::fs::read(&dst).unwrap(), b"old");
    }
}
