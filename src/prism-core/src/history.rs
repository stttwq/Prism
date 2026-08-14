//! Broker-owned, versioned usage history with bounded retention.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::persistence::{HistoryData, HistoryEntry, VersionedEnvelope};
use crate::shell::ActionTarget;

const HISTORY_FILE: &str = "history-v1.json";
const MAX_ENTRIES: usize = 500;
const RETENTION_SECONDS: u64 = 90 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryUse {
    Execute,
    Reveal,
    Destination,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryDiagnostic {
    Corrupt,
    FutureSchema,
    Invalid,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryWeight {
    pub target: ActionTarget,
    pub score: u32,
}

pub struct HistoryStore {
    path: PathBuf,
    enabled: AtomicBool,
    entries: RwLock<Vec<HistoryEntry>>,
    diagnostic: RwLock<Option<HistoryDiagnostic>>,
}

impl HistoryStore {
    pub fn load(data_dir: &Path, enabled: bool) -> Self {
        Self::load_at(data_dir, enabled, now_utc())
    }

    fn load_at(data_dir: &Path, enabled: bool, now: u64) -> Self {
        let path = data_dir.join(HISTORY_FILE);
        let (mut entries, diagnostic) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<VersionedEnvelope<HistoryData>>(&bytes) {
                Ok(envelope) => match envelope.into_compatible() {
                    Ok(data) => (data.entries, None),
                    Err(error) if error.starts_with("schema version") => {
                        isolate(&path, now);
                        (Vec::new(), Some(HistoryDiagnostic::FutureSchema))
                    }
                    Err(_) => {
                        isolate(&path, now);
                        (Vec::new(), Some(HistoryDiagnostic::Invalid))
                    }
                },
                Err(_) => {
                    isolate(&path, now);
                    (Vec::new(), Some(HistoryDiagnostic::Corrupt))
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(_) => (Vec::new(), Some(HistoryDiagnostic::Io)),
        };
        prune(&mut entries, now);
        Self {
            path,
            enabled: AtomicBool::new(enabled),
            entries: RwLock::new(entries),
            diagnostic: RwLock::new(diagnostic),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    pub fn diagnostic(&self) -> Option<HistoryDiagnostic> {
        self.diagnostic.read().ok().and_then(|value| value.clone())
    }

    pub fn take_diagnostic(&self) -> Option<HistoryDiagnostic> {
        self.diagnostic
            .write()
            .ok()
            .and_then(|mut value| value.take())
    }

    pub fn clear(&self) -> Result<(), String> {
        let mut entries = self
            .entries
            .write()
            .map_err(|_| "history lock is poisoned".to_string())?;
        entries.clear();
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("clear history: {error}")),
        }
        if let Ok(mut diagnostic) = self.diagnostic.write() {
            *diagnostic = None;
        }
        Ok(())
    }

    pub fn record(&self, target: &ActionTarget, usage: HistoryUse) -> Result<(), String> {
        self.record_at(target, usage, now_utc())
    }

    fn record_at(&self, target: &ActionTarget, usage: HistoryUse, now: u64) -> Result<(), String> {
        if !self.is_enabled() || !is_recordable(target) {
            return Ok(());
        }
        let mut entries = self
            .entries
            .write()
            .map_err(|_| "history lock is poisoned".to_string())?;
        prune(&mut entries, now);
        let entry = if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.kind == target.kind && entry.target == target.value)
        {
            entry
        } else {
            entries.push(HistoryEntry {
                kind: target.kind.clone(),
                target: target.value.clone(),
                ..HistoryEntry::default()
            });
            entries
                .last_mut()
                .ok_or_else(|| "history insertion failed".to_string())?
        };
        match usage {
            HistoryUse::Execute => entry.execute_count = entry.execute_count.saturating_add(1),
            HistoryUse::Reveal => entry.reveal_count = entry.reveal_count.saturating_add(1),
            HistoryUse::Destination => {
                entry.destination_count = entry.destination_count.saturating_add(1)
            }
        }
        entry.last_used_utc = now;
        prune(&mut entries, now);
        persist(&self.path, &entries)?;
        Ok(())
    }

    pub fn score(&self, target: &ActionTarget) -> u32 {
        if !self.is_enabled() {
            return 0;
        }
        self.entries
            .read()
            .ok()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.kind == target.kind && entry.target == target.value)
                    .map(history_score)
            })
            .unwrap_or(0)
    }

    pub fn weights(&self) -> Vec<HistoryWeight> {
        if !self.is_enabled() {
            return Vec::new();
        }
        self.entries
            .read()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| HistoryWeight {
                        target: ActionTarget {
                            kind: entry.kind.clone(),
                            value: entry.target.clone(),
                        },
                        score: history_score(entry),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn entries(&self) -> Vec<HistoryEntry> {
        self.entries.read().unwrap().clone()
    }
}

fn history_score(entry: &HistoryEntry) -> u32 {
    entry
        .execute_count
        .saturating_mul(4)
        .saturating_add(entry.reveal_count.saturating_mul(2))
        .saturating_add(entry.destination_count)
}

fn is_recordable(target: &ActionTarget) -> bool {
    matches!(
        target.kind.as_str(),
        "file" | "directory" | "application" | "window"
    ) && !target.value.is_empty()
        && !target.value.contains('\0')
        && target.value.len() <= 32 * 1024
}

fn prune(entries: &mut Vec<HistoryEntry>, now: u64) {
    let cutoff = now.saturating_sub(RETENTION_SECONDS);
    entries.retain(|entry| entry.last_used_utc >= cutoff);
    entries.sort_by(|left, right| {
        right
            .last_used_utc
            .cmp(&left.last_used_utc)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.target.cmp(&right.target))
    });
    entries.truncate(MAX_ENTRIES);
}

fn persist(path: &Path, entries: &[HistoryEntry]) -> Result<(), String> {
    let parent = path.parent().ok_or("history path has no parent")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create history directory: {error}"))?;
    let envelope = VersionedEnvelope::new(HistoryData {
        entries: entries.to_vec(),
    })?;
    let bytes =
        serde_json::to_vec(&envelope).map_err(|error| format!("encode history: {error}"))?;
    let temporary = parent.join(format!("{HISTORY_FILE}.tmp"));
    let mut file = std::fs::File::create(&temporary)
        .map_err(|error| format!("create history temporary file: {error}"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("write history temporary file: {error}"))?;
    drop(file);
    crate::fs_util::atomic_replace(&temporary, path, "history")
}

fn isolate(path: &Path, now: u64) {
    let Some(parent) = path.parent() else {
        return;
    };
    let destination = parent.join(format!("history-v1.corrupt-{now}.json"));
    let _ = std::fs::rename(path, destination);
}

fn now_utc() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn test_dir(name: &str) -> PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("prism-history-{name}-{}-{id}", std::process::id()))
    }

    fn target(value: impl Into<String>) -> ActionTarget {
        ActionTarget {
            kind: "file".into(),
            value: value.into(),
        }
    }

    #[test]
    fn disabled_and_web_paths_do_not_record() {
        let dir = test_dir("disabled");
        let store = HistoryStore::load_at(&dir, false, 1_000_000);
        store
            .record_at(&target("C:\\x"), HistoryUse::Execute, 1_000_000)
            .unwrap();
        store.set_enabled(true);
        store
            .record_at(
                &ActionTarget {
                    kind: "web".into(),
                    value: "https://example.invalid".into(),
                },
                HistoryUse::Execute,
                1_000_000,
            )
            .unwrap();
        assert!(store.entries().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retention_counting_and_clear_are_deterministic() {
        let dir = test_dir("retention");
        let store = HistoryStore::load_at(&dir, true, 20_000_000);
        store
            .record_at(&target("C:\\old"), HistoryUse::Execute, 1)
            .unwrap();
        for index in 0..=MAX_ENTRIES {
            store
                .record_at(
                    &target(format!("C:\\item-{index}")),
                    HistoryUse::Execute,
                    20_000_000 + index as u64,
                )
                .unwrap();
        }
        store
            .record_at(&target("C:\\item-500"), HistoryUse::Reveal, 20_001_000)
            .unwrap();
        assert_eq!(store.entries().len(), MAX_ENTRIES);
        assert!(!store
            .entries()
            .iter()
            .any(|entry| entry.target == "C:\\old"));
        assert_eq!(store.score(&target("C:\\item-500")), 6);
        store.set_enabled(false);
        assert_eq!(store.score(&target("C:\\item-500")), 0);
        store.clear().unwrap();
        assert!(!dir.join(HISTORY_FILE).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_and_future_files_are_isolated_without_blocking() {
        let dir = test_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(HISTORY_FILE), b"{secret-path-not-json").unwrap();
        let store = HistoryStore::load_at(&dir, true, 42);
        assert_eq!(store.diagnostic(), Some(HistoryDiagnostic::Corrupt));
        assert!(store.entries().is_empty());
        assert!(dir.join("history-v1.corrupt-42.json").exists());

        std::fs::write(
            dir.join(HISTORY_FILE),
            br#"{"schema_version":999,"data":{"entries":[]}}"#,
        )
        .unwrap();
        let future = HistoryStore::load_at(&dir, true, 43);
        assert_eq!(future.diagnostic(), Some(HistoryDiagnostic::FutureSchema));
        assert!(future.entries().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn diagnostic_is_reported_once() {
        let dir = test_dir("diagnostic-once");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(HISTORY_FILE), b"not-json").unwrap();

        let store = HistoryStore::load_at(&dir, true, 42);
        assert_eq!(store.take_diagnostic(), Some(HistoryDiagnostic::Corrupt));
        assert_eq!(store.take_diagnostic(), None);

        let _ = std::fs::remove_dir_all(dir);
    }
}
