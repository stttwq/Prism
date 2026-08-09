//! Fail-open structured JSONL logging with bounded size rotation.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
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
    if logger.write(level, event, elapsed_ms, generation).is_err() {
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
        let record = serde_json::json!({
            "timestamp_ms": timestamp_ms,
            "level": level,
            "event": event,
            "elapsed_ms": elapsed_ms,
            "generation": generation,
        });
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
            .write("info", "search_complete", Some(12), Some(3))
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
}
