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
/// L3（全仓复审 2026-08-22）：缓存的外部 exe 不存在时（会话中卸载/移动了
/// 7-Zip 或自定义程序）也重新探测——否则继续 spawn 一个已消失的路径，
/// 每次压缩都失败。
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
            let still_valid = match cached {
                ZipProgram::External { exe_path, .. } => std::path::Path::new(exe_path).is_file(),
                _ => true,
            };
            if still_valid {
                return cached.clone();
            }
        }
    }
    let detected = detect_zip_program(normalized.as_deref());
    *guard = (normalized, Some(detected.clone()));
    detected
}

/// 探测可用的压缩程序。按优先级：
/// 1. 用户自定义路径（如果设置、文件存在且是 .exe）
/// 2. 注册表查找 7-Zip
/// 3. Windows 内置 Shell COM
fn detect_zip_program(custom_path: Option<&str>) -> ZipProgram {
    // 1. 用户自定义路径
    // L2（全仓复审 2026-08-22）：要求 .exe 扩展名——settings.json 是用户可改
    // 的配置，至少把「必须是可执行映像」钉住；spawn 语义 <exe> <source> <output>
    // 决定了任何非可执行文件被选中都只会浪费一次进程创建。
    if let Some(path) = custom_path {
        let trimmed = path.trim();
        let is_exe = std::path::Path::new(trimmed)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
        if is_exe && std::path::Path::new(trimmed).is_file() {
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
    // B4（AUDIT-4 批次C）：输出已存在时明确报 Conflict，不再静默覆写——
    // 此前三级回退都会破坏已存在的 zip（外部路径 -aoa / Shell COM 先写空 zip）。
    if std::path::Path::new(output_path).exists() {
        return Err(ShellError::new(
            ShellErrorKind::Conflict,
            "output zip already exists",
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
        // 7z a <output.zip> <source> -aoa：a=添加，-aoa=已存在的输出 zip 直接覆盖。
        // 输出路径恒为 <source>.zip（zip_output_path），覆盖的是本动作上次产物；
        // 不能去掉 -aoa——broker 是无控制台的后台进程，7z 的覆盖交互提示会永久挂死。
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
///
/// B4（AUDIT-4 批次C，2026-08-21）：`CopyHere` 是异步提交——立即返回 Success
/// 意味着 zip 还在后台写、甚至失败也无人知晓。修法：flags 带
/// FOF_SILENT|FOF_NOCONFIRMATION|FOF_NOERRORUI（broker 无控制台，任何 UI
/// 挂起都会拖死 STA worker），调用后轮询「条目数 + 文件尺寸」双稳定再上报
///（见 H6 注释）；有界等待（60s）后按完成上报——轮询是启发式，宁可乐观
/// 上报也不无限占住唯一 STA 队列。输出已存在的冲突在
/// `validate_zip_request` 拦截。
#[cfg(windows)]
fn zip_with_shell_com(source: &str, output: &str) -> Result<ShellOutcome, ShellError> {
    // M1（复审 2026-08-21）：失败清理外壳。validate_zip_request 只拒「已存在」
    // 的输出——空 zip 写下之后任何一步失败（CoCreateInstance/NameSpace/
    // CopyHere）都会把 22 字节残骸留在磁盘，后续同路径重试永远 Conflict，
    // 直到用户手删。validate 保证走到这里时 output 是本函数自己创建的，
    // 删除只清自己的产物。
    let result = zip_with_shell_com_inner(source, output);
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

#[cfg(windows)]
fn zip_with_shell_com_inner(source: &str, output: &str) -> Result<ShellOutcome, ShellError> {
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

        // CopyHere(source, flags)：4=FOF_SILENT、16=FOF_NOCONFIRMATION、
        // 1024=FOF_NOERRORUI——静默 + 免确认 + 免错误 UI，任何交互都会挂死
        // 无控制台的 broker STA worker。
        let source_var = VARIANT::from(BSTR::from(source));
        folder
            .CopyHere(&source_var, &VARIANT::from(4i16 | 16 | 1024))
            .map_err(|e| ShellError::new(ShellErrorKind::System, e.to_string()))?;

        // H6（全仓复审 2026-08-22）：完成判定改为「条目数 + zip 文件尺寸」
        // 双稳定。原先只看 Items().Count()——压目录时目录节点一出现 count 即 1，
        // 首个 500ms 窗口就可能与预采样相等而提前判完成（内容还在流式写入）；
        // 而 Items() 失败时 count=-1 永不满足 >0，空转 60s 后仍 fall through
        // 到 Ok——22 字节残骸被报成压缩成功。新口径：
        // - count>0 且 size>空 zip 骨架（22B）且连续 2 个窗口不变 ⇒ 完成；
        // - 到 deadline 从未见过有效采样（count≤0 或 size≤22）⇒ 判失败，
        //   外层清理逻辑会移除残骸；
        // - 见过有效采样但迟迟不稳定 ⇒ 60s 截断按完成上报（轮询是启发式，
        //   宁可乐观也不无限占住唯一 STA 队列，原行为保留）。
        const EMPTY_ZIP_BYTES: u64 = 22;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let zip_stats = || -> (i32, u64) {
            let count = folder
                .Items()
                .ok()
                .and_then(|items| items.Count().ok())
                .unwrap_or(-1);
            let size = std::fs::metadata(output)
                .map(|meta| meta.len())
                .unwrap_or(0);
            (count, size)
        };
        let mut last = zip_stats();
        let mut stable_rounds = 0u32;
        let mut ever_progressed = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let current = zip_stats();
            let progressed = current.0 > 0 && current.1 > EMPTY_ZIP_BYTES;
            if progressed {
                ever_progressed = true;
            }
            if current == last && progressed {
                stable_rounds += 1;
                if stable_rounds >= 2 {
                    break;
                }
            } else {
                stable_rounds = 0;
            }
            last = current;
            if std::time::Instant::now() >= deadline {
                crate::logging::event("warn", "zip_shell_com_wait_timeout", None, None);
                if !ever_progressed {
                    return Err(ShellError::new(
                        ShellErrorKind::System,
                        "zip via Shell COM made no progress within 60s",
                    ));
                }
                break;
            }
        }
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

    /// B4（AUDIT-4 批次C）：输出 zip 已存在时报 Conflict，不再静默覆写。
    #[cfg(windows)]
    #[test]
    fn zip_rejects_existing_output_as_conflict() {
        let dir = std::env::temp_dir().join(format!("prism-zip-b4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.zip");
        std::fs::write(&output, b"existing").unwrap();

        let target = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        let err = zip(&target, output.to_str().unwrap(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Conflict);

        // zip_external 的校验与 zip 一致。
        let err2 = zip_external(&target, output.to_str().unwrap(), None)
            .unwrap()
            .unwrap_err();
        assert_eq!(err2.kind, ShellErrorKind::Conflict);
        assert_eq!(
            std::fs::read(&output).unwrap(),
            b"existing",
            "已存在的 zip 不得被覆写"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
