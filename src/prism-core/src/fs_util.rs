//! Atomic file replacement shared by all persistence layers (cache, sidecar, history).
//!
//! On Windows, uses `ReplaceFileW` for true atomic replace (no window where the
//! destination is absent). On non-Windows, `std::fs::rename` is already atomic on
//! POSIX. The initial-install case (destination does not exist yet) falls back to
//! a plain rename in both cases.

use std::path::Path;

/// Atomically replaces `destination` with `temporary`.
///
/// `label` is included in error messages so callers can distinguish which
/// persistence layer failed (e.g. "cache", "pinyin sidecar", "history").
pub fn atomic_replace(temporary: &Path, destination: &Path, label: &str) -> Result<(), String> {
    if !destination.exists() {
        return std::fs::rename(temporary, destination)
            .map_err(|error| format!("install initial {label}: {error}"));
    }
    replace_existing(temporary, destination, label)
}

#[cfg(windows)]
fn replace_existing(temporary: &Path, destination: &Path, label: &str) -> Result<(), String> {
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
    .map_err(|error| format!("replace {label}: {error}"))
}

#[cfg(not(windows))]
fn replace_existing(temporary: &Path, destination: &Path, _label: &str) -> Result<(), String> {
    // POSIX rename is atomic and replaces the destination if it exists.
    std::fs::rename(temporary, destination).map_err(|error| error.to_string())
}
