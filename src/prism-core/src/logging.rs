//! Fail-open structured JSONL logging with bounded size rotation.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
static LOGGER: OnceLock<Mutex<Option<RollingLogger>>> = OnceLock::new();

struct RollingLogger {
    path: PathBuf,
    file: File,
}

pub fn init(process: &str, directory: &Path) {
    let logger = RollingLogger::open(process, directory).ok();
    if let Some(slot) = LOGGER.get() {
        if let Ok(mut guard) = slot.lock() {
            *guard = logger;
        }
    } else {
        let _ = LOGGER.set(Mutex::new(logger));
    }
}

pub fn event(level: &str, event: &str, elapsed_ms: Option<u128>, generation: Option<u64>) {
    let Some(slot) = LOGGER.get() else {
        return;
    };
    let Ok(mut guard) = slot.lock() else {
        return;
    };
    let Some(logger) = guard.as_mut() else {
        return;
    };
    if logger
        .write(level, event, None, elapsed_ms, generation)
        .is_err()
    {
        *guard = None;
    }
}

/// Like [`event`] but also writes a human-readable `detail` field (after sanitization)
/// so that error messages and diagnostic context survive in the log instead of being
/// reduced to an opaque hash. The `event` field stays the same (hashed id or short name)
/// for backward compatibility with existing log readers.
pub fn event_detail(
    level: &str,
    event: &str,
    detail: &str,
    elapsed_ms: Option<u128>,
    generation: Option<u64>,
) {
    let sanitized = sanitize(detail);
    let Some(slot) = LOGGER.get() else {
        return;
    };
    let Ok(mut guard) = slot.lock() else {
        return;
    };
    let Some(logger) = guard.as_mut() else {
        return;
    };
    if logger
        .write(level, event, Some(&sanitized), elapsed_ms, generation)
        .is_err()
    {
        *guard = None;
    }
}

pub fn redacted_message(message: &str) {
    event("info", &redacted_id(message), None, None);
}

/// Records a panic before the process dies.
///
/// The release profile uses `panic = "abort"`, so without this hook a panic
/// terminates the process leaving nothing behind: the frontend silently relaunches
/// the broker and the incident is unreconstructable. That is exactly what happened
/// on 2026-08-07 - `prism-core.exe` vanished under load and `broker.jsonl` held no
/// entry for that day.
///
/// The panic *location* comes from source code, so it is logged verbatim. The panic
/// *message* may embed a path or query and is hashed, matching [`redacted_message`].
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|at| format!("{}:{}", at.file(), at.line()));
        // `panic!("{path} missing")` would otherwise leak the path into the log.
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned());

        event(
            "error",
            &panic_event(location.as_deref(), payload.as_deref()),
            None,
            None,
        );
        previous(info);
    }));
}

/// Event text for a panic. Split out from the hook so the redaction contract is
/// testable without installing a process-global hook.
fn panic_event(location: Option<&str>, payload: Option<&str>) -> String {
    let location = location.unwrap_or("unknown");
    let detail = payload.map_or_else(|| "no_payload".to_owned(), redacted_id);
    format!("panic at {location} {detail}")
}

pub fn redacted_id(message: &str) -> String {
    format!("message_{:016x}", stable_hash(message))
}

/// Sanitizes free-form log text so it is safe to write without hashing.
///
/// Replaces backslashes with forward slashes for readability, and redacts the
/// username segment under `C:/Users/<name>/` (or any drive letter) to `<user>`.
/// Paths without a user directory (e.g. `C:/ProgramData/Prism`) are preserved.
pub fn sanitize(text: &str) -> String {
    let normalized = text.replace('\\', "/");
    // FRESH-AUDIT-2 G1: 旧阈值 12 + 模式条件 i+10<len 连手漏掉短用户名：
    // "C:/Users/j"（10 字节，单字符用户名无尾斜杠）完全不脱敏。模式最小长度
    // 是 10（盘符+":/Users/"+至少 1 字节用户名），短于它才可安全跳过。
    if normalized.len() < 10 {
        return normalized;
    }
    // Match patterns like "C:/Users/XXX/" (any single drive letter).
    // 按 UTF-8 序列复制：非 ASCII（如中文路径/用户名）原样保留，只有 ASCII
    // 字节参与脱敏匹配——旧实现逐字节 `as char` 会把中文拆成乱码。
    let bytes = normalized.as_bytes();
    let mut out = String::with_capacity(normalized.len());
    let mut i = 0;
    while i < bytes.len() {
        // Look for "<letter>:/Users/" at position i.
        // 模式共 9 字节 + 至少 1 字节用户名：i+10 <= len 即可，不要求尾随字节。
        if i + 10 <= bytes.len()
            && bytes[i + 1] == b':'
            && bytes[i + 2] == b'/'
            && &bytes[i + 3..i + 9] == b"Users/"
        {
            // Drive letter + ":/Users/"
            out.push(bytes[i] as char);
            out.push_str(":/Users/");
            i += 9;
            // Skip until the next '/' (the username segment).
            let name_start = i;
            while i < bytes.len() && bytes[i] != b'/' {
                i += 1;
            }
            if i > name_start {
                out.push_str("<user>");
            }
            continue;
        }
        let end = (i + utf8_sequence_len(bytes[i])).min(bytes.len());
        // 输入是合法 &str 且 i 始终落在字符边界上，切片安全。
        out.push_str(&normalized[i..end]);
        i = end;
    }
    out
}

/// UTF-8 首字节 → 序列长度（ASCII 为 1）。
fn utf8_sequence_len(byte: u8) -> usize {
    match byte {
        b if b < 0x80 => 1,
        b if b & 0xE0 == 0xC0 => 2,
        b if b & 0xF0 == 0xE0 => 3,
        b if b & 0xF8 == 0xF0 => 4,
        _ => 1,
    }
}

impl RollingLogger {
    fn open(process: &str, directory: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join(format!("{process}.jsonl"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { path, file })
    }

    fn write(
        &mut self,
        level: &str,
        event: &str,
        detail: Option<&str>,
        elapsed_ms: Option<u128>,
        generation: Option<u64>,
    ) -> std::io::Result<()> {
        if self.file.metadata()?.len() >= MAX_LOG_BYTES {
            self.rotate()?;
        }
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or(0);
        let record = if let Some(detail) = detail {
            serde_json::json!({
                "timestamp_ms": timestamp_ms,
                "level": level,
                "event": event,
                "detail": detail,
                "elapsed_ms": elapsed_ms,
                "generation": generation,
            })
        } else {
            serde_json::json!({
                "timestamp_ms": timestamp_ms,
                "level": level,
                "event": event,
                "elapsed_ms": elapsed_ms,
                "generation": generation,
            })
        };
        serde_json::to_writer(&mut self.file, &record)?;
        self.file.write_all(b"\n")?;
        self.file.flush()
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        let rotated = self.path.with_extension("jsonl.1");
        if rotated.exists() {
            std::fs::remove_file(&rotated)?;
        }
        std::fs::rename(&self.path, rotated)?;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        Ok(())
    }
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_record_contains_no_sensitive_payload() {
        let dir = std::env::temp_dir().join(format!("prism-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut logger = RollingLogger::open("broker", &dir).unwrap();
        logger
            .write("info", "search_complete", None, Some(12), Some(3))
            .unwrap();
        let text = std::fs::read_to_string(dir.join("broker.jsonl")).unwrap();
        assert!(text.contains("search_complete"));
        assert!(!text.contains("query"));
        assert!(!text.contains("path"));
        assert!(!text.contains("title"));
        assert!(!text.contains("http"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unwritable_directory_disables_file_logging() {
        let file = std::env::temp_dir().join(format!("prism-log-file-{}", std::process::id()));
        std::fs::write(&file, b"not a directory").unwrap();
        assert!(RollingLogger::open("broker", &file).is_err());
        let _ = std::fs::remove_file(file);
    }

    /// A panic message can embed a path or query, so only the source location is
    /// logged verbatim. Regression guard for the silent-crash defect: `panic = "abort"`
    /// plus no hook left `broker.jsonl` with no entry at all for the crash day.
    #[test]
    fn panic_event_keeps_location_and_hashes_the_message() {
        let text = panic_event(
            Some("src/prism-core/src/ipc.rs:412"),
            Some(r"C:\Users\me\secret.txt is missing"),
        );
        assert!(text.contains("src/prism-core/src/ipc.rs:412"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
        assert!(!text.contains(r"C:\Users"), "{text}");
        assert!(text.contains("message_"), "{text}");
    }

    #[test]
    fn panic_event_survives_a_missing_location_or_payload() {
        let text = panic_event(None, None);
        assert_eq!(text, "panic at unknown no_payload");
    }

    /// FRESH-AUDIT-2 G1: 12 字节阈值漏掉的短用户名（恰 11 字节的 "C:/Users/j"）。
    #[test]
    fn sanitize_redacts_single_char_username_without_trailing_slash() {
        assert_eq!(sanitize(r"C:\Users\j"), "C:/Users/<user>");
        assert_eq!(sanitize("C:/Users/ab"), "C:/Users/<user>");
        assert_eq!(sanitize("short text"), "short text");
    }

    #[test]
    fn rotation_failure_is_reported_without_panicking() {
        let dir =
            std::env::temp_dir().join(format!("prism-log-rotation-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut logger = RollingLogger::open("broker", &dir).unwrap();
        std::fs::create_dir(dir.join("broker.jsonl.1")).unwrap();
        assert!(logger.rotate().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitize_redacts_username() {
        let result = sanitize(r"C:\Users\jia\Documents\file.txt");
        assert!(
            result.contains("<user>"),
            "username should be redacted: {result}"
        );
        assert!(
            !result.contains("jia"),
            "original username must not appear: {result}"
        );
        assert!(
            result.contains("Documents/file.txt"),
            "rest of path should be preserved: {result}"
        );
    }

    #[test]
    fn sanitize_preserves_programdata() {
        let result = sanitize(r"C:\ProgramData\Prism\index-v5.bin");
        assert!(
            result.contains("ProgramData/Prism"),
            "ProgramData path should not be redacted: {result}"
        );
        assert!(
            !result.contains("<user>"),
            "no <user> placeholder expected: {result}"
        );
    }

    #[test]
    fn sanitize_handles_multiple_drive_letters() {
        let result = sanitize(r"D:\Users\bob\file.txt E:\Users\alice\other.txt");
        assert!(result.contains("<user>"), "{result}");
        assert!(!result.contains("bob"), "{result}");
        assert!(!result.contains("alice"), "{result}");
    }

    #[test]
    fn sanitize_preserves_chinese_text_and_redacts_chinese_usernames() {
        // 中文路径必须原样保留（旧实现按 Latin-1 重编码成乱码）。
        let plain = sanitize(r"D:\资料\项目文档\会议纪要.txt");
        assert_eq!(plain, "D:/资料/项目文档/会议纪要.txt");

        // 中文用户名同样要脱敏，且脱敏后的其余部分不乱码。
        let redacted = sanitize(r"C:\Users\小明的电脑\Documents\文件.txt");
        assert!(redacted.contains("<user>"), "{redacted}");
        assert!(!redacted.contains("小明的电脑"), "{redacted}");
        assert!(redacted.contains("Documents/文件.txt"), "{redacted}");
    }

    #[test]
    fn event_detail_writes_readable_text() {
        let dir =
            std::env::temp_dir().join(format!("prism-log-detail-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        init("test_detail", &dir);
        event_detail(
            "error",
            "rebuild_save_failed",
            "replace cache: Access is denied. (0x80070005)",
            None,
            None,
        );
        let text = std::fs::read_to_string(dir.join("test_detail.jsonl")).unwrap();
        assert!(
            text.contains("rebuild_save_failed"),
            "event name should be present: {text}"
        );
        assert!(
            text.contains("Access is denied"),
            "detail text should be readable: {text}"
        );
        assert!(
            text.contains("\"detail\""),
            "detail field should be a JSON key: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn event_without_detail_has_no_detail_field() {
        let dir = std::env::temp_dir().join(format!("prism-log-no-detail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        init("test_no_detail", &dir);
        event("info", "search_complete", Some(12), Some(3));
        let text = std::fs::read_to_string(dir.join("test_no_detail.jsonl")).unwrap();
        assert!(
            !text.contains("\"detail\""),
            "no detail field when event() is used: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
