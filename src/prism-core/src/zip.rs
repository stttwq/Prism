//! ZIP 压缩 adapter：三级回退策略。
//!
//! 1. 设置自定义路径：`settings.json` 的 `ZipProgram` 字段指定压缩程序可执行文件。
//! 2. 自动探测：注册表查找已安装的 7-Zip (`7z.exe`)。
//! 3. Windows 内置回退：Shell COM `CopyHere` 到 `.zip` folder 对象。
//!
//! 无论哪条路径，输出固定 `.zip`，冲突交给系统确认。

use crate::shell::{ActionTarget, ShellError, ShellErrorKind, ShellOutcome, TargetKind};
use std::sync::Mutex;

/// 缓存 (探测输入, 结果)：custom_path 与缓存输入一致时复用，变化即重新探测——
/// 否则用户在设置里改压缩程序后要重启 broker 才生效。
static ZIP_PROGRAM: Mutex<(Option<String>, Option<ZipProgram>)> = Mutex::new((None, None));

/// 探测到的压缩程序。
#[derive(Debug, Clone)]
enum ZipProgram {
    /// 外部可执行文件（7-Zip 或用户自定义），路径已知。
    External {
        exe_path: String,
        is_seven_zip: bool,
    },
    /// 使用 Windows 内置 Shell COM。
    WindowsShell,
    /// 探测失败，无可用压缩程序。
    #[allow(dead_code)]
    None,
}

/// 获取缓存的压缩程序探测结果；探测输入（custom_path）变化时重新探测。
fn get_zip_program(custom_path: Option<&str>) -> ZipProgram {
    let normalized = custom_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string);
    let mut guard = ZIP_PROGRAM
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let (Some(cached_input), Some(cached)) = &*guard {
        if Some(cached_input.as_str()) == normalized.as_deref() {
            return cached.clone();
        }
    }
    let detected = detect_zip_program(normalized.as_deref());
    *guard = (normalized, Some(detected.clone()));
    detected
}

/// 探测可用的压缩程序。按优先级：
/// 1. 用户自定义路径（如果设置且文件存在）
/// 2. 注册表查找 7-Zip
/// 3. Windows 内置 Shell COM
fn detect_zip_program(custom_path: Option<&str>) -> ZipProgram {
    // 1. 用户自定义路径
    if let Some(path) = custom_path {
        let trimmed = path.trim();
        if !trimmed.is_empty() && std::path::Path::new(trimmed).is_file() {
            return ZipProgram::External {
                exe_path: trimmed.to_string(),
                is_seven_zip: false, // 自定义程序可能不是 7-Zip，用通用调用方式
            };
        }
    }

    // 2. 自动探测 7-Zip
    if let Some(exe) = detect_seven_zip() {
        return ZipProgram::External {
            exe_path: exe,
            is_seven_zip: true,
        };
    }

    // 3. Windows 内置 Shell COM
    ZipProgram::WindowsShell
}

/// 通过注册表探测已安装的 7-Zip，返回 `7z.exe` 完整路径。
#[cfg(windows)]
fn detect_seven_zip() -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
        REG_VALUE_TYPE,
    };

    // 7-Zip 在注册表中的安装路径：HKEY_LOCAL_MACHINE\SOFTWARE\7-Zip\Path
    let subkey: Vec<u16> = OsStrExt::encode_wide(std::ffi::OsStr::new("SOFTWARE\\7-Zip"))
        .chain(std::iter::once(0))
        .collect();
    let value_name: Vec<u16> = OsStrExt::encode_wide(std::ffi::OsStr::new("Path"))
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return None;
        }
        let _guard = scopeguard(|| {
            let _ = RegCloseKey(hkey);
        });

        // 先查长度
        let mut len: u32 = 0;
        let mut reg_type = REG_VALUE_TYPE::default();
        let status = RegQueryValueExW(
            hkey,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut reg_type),
            None,
            Some(&mut len),
        );
        if status.is_err() || len == 0 {
            return None;
        }

        // 读值
        let mut buf = vec![0u8; len as usize];
        let status = RegQueryValueExW(
            hkey,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut reg_type),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        );
        if status.is_err() {
            return None;
        }

        // 注册表值是 REG_SZ（宽字符），转换为 String
        let wide: Vec<u16> = buf
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        let install_dir = String::from_utf16_lossy(
            &wide[..wide
                .len()
                .saturating_sub(wide.iter().rev().take_while(|&&c| c == 0).count())],
        );
        let install_dir = install_dir.trim_end_matches('\\');

        let exe_path = format!("{install_dir}\\7z.exe");
        if std::path::Path::new(&exe_path).is_file() {
            return Some(exe_path);
        }
    }

    None
}

#[cfg(not(windows))]
fn detect_seven_zip() -> Option<String> {
    None
}

/// RAII guard for registry handle cleanup.
#[cfg(windows)]
fn scopeguard<F: FnOnce()>(f: F) -> impl Drop {
    struct Guard<F: FnOnce()>(Option<F>);
    impl<F: FnOnce()> Drop for Guard<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }
    Guard(Some(f))
}

/// 校验 zip 目标与输出路径，`zip` 与 `zip_external` 共用同一规则。
fn validate_zip_request(target: &ActionTarget, output_path: &str) -> Result<(), ShellError> {
    let kind = target.validate()?;
    if !matches!(kind, TargetKind::File | TargetKind::Directory) {
        return Err(ShellError::new(
            ShellErrorKind::Unsupported,
            "zip requires a file or directory target",
        ));
    }
    if !output_path.to_lowercase().ends_with(".zip") {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "output path must end with .zip",
        ));
    }
    Ok(())
}

/// 将文件/目录压缩为 ZIP。
///
/// `target` 是要压缩的文件或目录。输出 ZIP 文件路径由调用方决定。
/// `output_path` 是目标 `.zip` 文件路径。
/// `custom_zip_program` 是 `settings.json` 的 `ZipProgram` 字段值。
pub(crate) fn zip(
    target: &ActionTarget,
    output_path: &str,
    custom_zip_program: Option<&str>,
) -> Result<ShellOutcome, ShellError> {
    validate_zip_request(target, output_path)?;

    let program = get_zip_program(custom_zip_program);
    match program {
        ZipProgram::External {
            exe_path,
            is_seven_zip,
        } => zip_with_external(&target.value, output_path, &exe_path, is_seven_zip),
        ZipProgram::WindowsShell => zip_with_shell_com(&target.value, output_path),
        ZipProgram::None => Err(ShellError::new(
            ShellErrorKind::System,
            "no ZIP program available",
        )),
    }
}

/// S2a：仅当压缩走外部进程（7-Zip/自定义程序）时执行并返回 `Some(result)`；
/// 探测为 Windows Shell COM 时返回 `None`，交回调用方走原 STA 队列——
/// `CopyHere` 需要 COM apartment，必须留在 STA worker 上。
///
/// 外部进程路径是纯 `std::process` 调用（分钟级等待），放进单线程 STA 队列会把
/// 唯一的 Shell worker 占死：期间所有 Shell 动作（含 scan_apps）无限排队。
/// 目标/输出校验与 [`zip`] 完全一致。
pub(crate) fn zip_external(
    target: &ActionTarget,
    output_path: &str,
    custom_zip_program: Option<&str>,
) -> Option<Result<ShellOutcome, ShellError>> {
    if let Err(error) = validate_zip_request(target, output_path) {
        return Some(Err(error));
    }
    match get_zip_program(custom_zip_program) {
        ZipProgram::External {
            exe_path,
            is_seven_zip,
        } => Some(zip_with_external(
            &target.value,
            output_path,
            &exe_path,
            is_seven_zip,
        )),
        ZipProgram::WindowsShell | ZipProgram::None => None,
    }
}

/// 用外部程序（7-Zip 或自定义）压缩。
#[cfg(windows)]
fn zip_with_external(
    source: &str,
    output: &str,
    exe_path: &str,
    is_seven_zip: bool,
) -> Result<ShellOutcome, ShellError> {
    let mut cmd = std::process::Command::new(exe_path);
    if is_seven_zip {
        // 7z a <output.zip> <source> -aoa：a=添加，-aoa=覆盖所有（不静默，系统处理冲突确认）
        cmd.arg("a").arg(output).arg(source).arg("-aoa");
    } else {
        // 自定义程序：传 source 和 output 作为参数，让用户自己处理格式
        cmd.arg(source).arg(output);
    }

    let status = cmd
        .status()
        .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;

    if status.success() {
        Ok(ShellOutcome::Success)
    } else {
        // 退出码 1 在 7-Zip 里是警告（如部分文件被跳过），不算失败
        match status.code() {
            Some(1) if is_seven_zip => Ok(ShellOutcome::Success),
            Some(code) => Err(ShellError::new(
                ShellErrorKind::System,
                format!("ZIP program exited with code {code}"),
            )),
            None => Err(ShellError::new(
                ShellErrorKind::System,
                "ZIP program was terminated by signal",
            )),
        }
    }
}

#[cfg(not(windows))]
fn zip_with_external(
    _source: &str,
    _output: &str,
    _exe_path: &str,
    _is_seven_zip: bool,
) -> Result<ShellOutcome, ShellError> {
    Err(ShellError::new(
        ShellErrorKind::Unsupported,
        "ZIP is only available on Windows",
    ))
}

/// 用 Windows 内置 Shell COM 压缩（`Shell.Application` + `CopyHere`）。
///
/// 这是 Windows 10 1803+ 的内置能力，无需第三方依赖。
/// 创建空 ZIP 文件，然后用 `Shell.NameSpace(zip_path).CopyHere(source)`。
#[cfg(windows)]
fn zip_with_shell_com(source: &str, output: &str) -> Result<ShellOutcome, ShellError> {
    use windows::core::{BSTR, VARIANT};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

    // 创建空 ZIP 文件（22 字节的 ZIP 端序签名 + 0 数据）
    let empty_zip: [u8; 22] = [
        0x50, 0x4B, 0x05, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    std::fs::write(output, empty_zip)
        .map_err(|e| ShellError::new(classify_io_error(&e), e.to_string()))?;

    // 使用 Shell.Application COM 对象
    let shell: windows::Win32::UI::Shell::IShellDispatch = unsafe {
        CoCreateInstance(
            &windows::Win32::UI::Shell::Shell,
            None,
            CLSCTX_INPROC_SERVER,
        )
    }
    .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;

    unsafe {
        // NameSpace 返回 Folder 对象，参数是 VARIANT(BSTR)
        let output_var = VARIANT::from(BSTR::from(output));
        let folder = shell
            .NameSpace(&output_var)
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;

        // CopyHere(source, flags)
        // source 也需要是 VARIANT(BSTR)
        let source_var = VARIANT::from(BSTR::from(source));
        folder
            .CopyHere(&source_var, &VARIANT::default())
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;
    }

    crate::logging::event("info", "zip_shell_com_complete", None, None);
    Ok(ShellOutcome::Success)
}

#[cfg(not(windows))]
fn zip_with_shell_com(_source: &str, _output: &str) -> Result<ShellOutcome, ShellError> {
    Err(ShellError::new(
        ShellErrorKind::Unsupported,
        "ZIP is only available on Windows",
    ))
}

/// 分类 IO 错误到 ShellErrorKind。
#[cfg(windows)]
fn classify_io_error(error: &std::io::Error) -> ShellErrorKind {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => ShellErrorKind::AccessDenied,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput => {
            ShellErrorKind::TargetInvalid
        }
        std::io::ErrorKind::AlreadyExists => ShellErrorKind::Conflict,
        _ => ShellErrorKind::System,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn detect_seven_zip_returns_some_or_none() {
        // 探测结果取决于机器是否安装了 7-Zip，只验证不 panic。
        let result = detect_seven_zip();
        if let Some(ref path) = result {
            assert!(
                std::path::Path::new(path).is_file(),
                "detected 7z.exe path should exist: {path}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn detect_zip_program_with_invalid_custom_path_falls_through() {
        let prog = detect_zip_program(Some(r"C:\nonexistent\zip.exe"));
        // 自定义路径不存在 → 回退到探测或 WindowsShell
        match prog {
            ZipProgram::External { .. } | ZipProgram::WindowsShell => {}
            ZipProgram::None => panic!("should fall through to WindowsShell or 7-Zip"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn detect_zip_program_with_valid_custom_path_uses_it() {
        // 用 explorer.exe 作为假的自定义程序
        let prog = detect_zip_program(Some(r"C:\Windows\explorer.exe"));
        match prog {
            ZipProgram::External {
                exe_path,
                is_seven_zip,
            } => {
                assert_eq!(exe_path, r"C:\Windows\explorer.exe");
                assert!(!is_seven_zip);
            }
            _ => panic!("should detect external program"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn zip_rejects_non_zip_output() {
        let target = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        let err = zip(&target, r"C:\out.rar", None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains(".zip"));
    }

    #[cfg(windows)]
    #[test]
    fn zip_rejects_web_target() {
        let target = ActionTarget::new(TargetKind::Web, "https://example.com");
        let err = zip(&target, r"C:\out.zip", None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);
    }

    /// S2a：zip_external 的目标/输出校验与 zip 完全一致（错误在程序探测之前
    /// 返回，不依赖机器上装没装 7-Zip）。
    #[cfg(windows)]
    #[test]
    fn zip_external_validates_like_zip() {
        let web = ActionTarget::new(TargetKind::Web, "https://example.com");
        let err = zip_external(&web, r"C:\out.zip", None)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);
        assert_eq!(
            err.message,
            zip(&web, r"C:\out.zip", None).unwrap_err().message
        );

        let file = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        let bad_output = zip_external(&file, r"C:\out.rar", None)
            .unwrap()
            .unwrap_err();
        assert_eq!(bad_output.kind, ShellErrorKind::TargetInvalid);
        assert_eq!(
            bad_output.message,
            zip(&file, r"C:\out.rar", None).unwrap_err().message
        );
    }
}
