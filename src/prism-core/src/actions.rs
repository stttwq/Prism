//! 动作面板：基础动作（打开所在文件夹 / 复制 / 剪切 / 复制路径）。
//! 系统右键菜单（IContextMenu / shell:n）第一版不做，留给后续。
//!
//! 消息合同见 frontend-spec.md §6 流程 D、§7。

use crate::ipc::ActionItem;
use crate::log;

/// 返回某路径的基础动作列表（file/folder 通用）。
pub fn list_actions(path: &str) -> Result<Vec<ActionItem>, String> {
    validate_path(path)?;
    Ok(vec![
        ActionItem {
            id: "open_folder".into(),
            label: "打开所在文件夹".into(),
            icon_glyph: "\u{E8DA}".into(), // OpenFolderHorizontal
            has_submenu: false,
            is_section_header: false,
        },
        ActionItem {
            id: "copy".into(),
            label: "复制".into(),
            icon_glyph: "\u{E8C8}".into(), // Copy
            has_submenu: false,
            is_section_header: false,
        },
        ActionItem {
            id: "cut".into(),
            label: "剪切".into(),
            icon_glyph: "\u{E8C6}".into(), // Cut
            has_submenu: false,
            is_section_header: false,
        },
        ActionItem {
            id: "copy_path".into(),
            label: "复制路径至剪贴板".into(),
            icon_glyph: "\u{E8C8}".into(),
            has_submenu: false,
            is_section_header: false,
        },
        // 系统右键菜单（IContextMenu / shell:n）后续版本再挂"快捷菜单"节。
    ])
}

/// 执行动作。`action` 为 list_actions 返回的 id。
pub fn run_action(path: &str, action: &str) -> Result<(), String> {
    validate_path(path)?;
    match action {
        "open_folder" => reveal_in_explorer(path),
        "copy" => clipboard_set_files(path, preferred_drop_effect_copy()),
        "cut" => clipboard_set_files(path, preferred_drop_effect_move()),
        "copy_path" => clipboard_set_text(path),
        other if other.starts_with("shell:") => Err("系统右键菜单尚未实现".into()),
        other => Err(format!("未知动作：{other}")),
    }
}

fn validate_path(path: &str) -> Result<(), String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("路径为空".into());
    }
    if path.contains('\0') {
        return Err("路径含非法字符".into());
    }
    if !std::path::Path::new(path).is_absolute() {
        return Err("拒绝相对路径".into());
    }
    Ok(())
}

/// explorer /select,"path"
#[cfg(windows)]
fn reveal_in_explorer(path: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    let normalized = path.replace('/', "\\");
    let arg = format!("/select,\"{normalized}\"");
    std::process::Command::new("explorer")
        .raw_arg(arg)
        .spawn()
        .map(|_| {
            log(format!("动作 open_folder：{path}"));
        })
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn reveal_in_explorer(path: &str) -> Result<(), String> {
    Err(format!("非 Windows 平台无法定位：{path}"))
}

// ── 剪贴板：文本 ──────────────────────────────────────────────

#[cfg(windows)]
fn clipboard_set_text(text: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
    use windows::Win32::System::DataExchange::{
        EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let wide: Vec<u16> = std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let bytes = wide.len() * 2;

    unsafe {
        if OpenClipboard(HWND::default()).is_err() {
            return Err("无法打开剪贴板".into());
        }
        let _close = scopeguard_close();
        EmptyClipboard().map_err(|e| format!("清空剪贴板失败：{e}"))?;

        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| format!("GlobalAlloc：{e}"))?;
        if hmem.is_invalid() {
            return Err("GlobalAlloc 返回空".into());
        }
        let ptr = GlobalLock(hmem);
        if ptr.is_null() {
            let _ = GlobalFree(hmem);
            return Err("GlobalLock 失败".into());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
        let _ = GlobalUnlock(hmem);

        if SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(hmem.0)).is_err() {
            let _ = GlobalFree(hmem);
            return Err("SetClipboardData 文本失败".into());
        }
        // 成功后系统接管 hmem，不要 GlobalFree。
        log(format!("动作 copy_path：{text}"));
        Ok(())
    }
}

#[cfg(not(windows))]
fn clipboard_set_text(text: &str) -> Result<(), String> {
    let _ = text;
    Err("非 Windows 平台无剪贴板".into())
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
fn clipboard_set_files(path: &str, drop_effect: u32) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
    use windows::Win32::System::DataExchange::{
        EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
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
            return Err("无法打开剪贴板".into());
        }
        let _close = scopeguard_close();
        EmptyClipboard().map_err(|e| format!("清空剪贴板失败：{e}"))?;

        // CF_HDROP
        let hmem = GlobalAlloc(GMEM_MOVEABLE, total).map_err(|e| format!("GlobalAlloc：{e}"))?;
        if hmem.is_invalid() {
            return Err("GlobalAlloc 返回空".into());
        }
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(hmem);
            return Err("GlobalLock 失败".into());
        }
        // 清零并写 DROPFILES 头。
        std::ptr::write_bytes(ptr, 0, total);
        let df = ptr as *mut DROPFILES;
        (*df).pFiles = header_size as u32;
        (*df).fWide = windows::Win32::Foundation::BOOL(1);
        std::ptr::copy_nonoverlapping(
            wide.as_ptr() as *const u8,
            ptr.add(header_size),
            list_bytes,
        );
        let _ = GlobalUnlock(hmem);

        if SetClipboardData(CF_HDROP.0 as u32, HANDLE(hmem.0)).is_err() {
            let _ = GlobalFree(hmem);
            return Err("SetClipboardData HDROP 失败".into());
        }

        // Preferred DropEffect（复制 vs 剪切）；失败不回滚 HDROP（资源管理器仍可粘贴为复制）。
        let fmt_name: Vec<u16> = OsStrExt::encode_wide(std::ffi::OsStr::new("Preferred DropEffect"))
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
        log(format!("动作 {verb}：{path}"));
        Ok(())
    }
}

#[cfg(not(windows))]
fn clipboard_set_files(path: &str, drop_effect: u32) -> Result<(), String> {
    let _ = (path, drop_effect);
    Err("非 Windows 平台无剪贴板".into())
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
        assert!(list_actions("relative\\x.txt").is_err());
    }

    #[test]
    fn list_actions_has_basics() {
        let items = list_actions(r"C:\Windows\explorer.exe").expect("ok");
        let ids: Vec<_> = items.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"open_folder"));
        assert!(ids.contains(&"copy"));
        assert!(ids.contains(&"cut"));
        assert!(ids.contains(&"copy_path"));
        assert_eq!(items.len(), 4, "第一版仅四条基础动作，无快捷菜单占位");
        assert!(items.iter().all(|a| !a.is_section_header));
    }

    #[test]
    fn run_unknown_action_errors() {
        let err = run_action(r"C:\Windows\explorer.exe", "nope").unwrap_err();
        assert!(err.contains("未知动作"));
    }
}
