//! Broker-owned, versioned usage history.
//!
//! v2 keeps counts forever (capacity-bounded LRU only — no time-based pruning),
//! scores targets with a lazily-decayed frecency (14-day half-life, folded in on
//! each event), and remembers which normalized query strings selected a target
//! (`QueryStat`, for query-aware ranking). Reads apply lazy decay from
//! `last_used_utc`; no background timers.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::persistence::{HistoryData, HistoryEntry, QueryStat, VersionedEnvelope};
use crate::shell::ActionTarget;

const HISTORY_FILE: &str = "history-v2.json";
/// v1 读取兜底：仅在 v2 文件缺失时读取一次。迁移发生在内存里（serde 默认值
/// 补全新字段 + 旧计数换算 frecency 初值），首个动作落盘为 v2；v1 原文件留在
/// 磁盘上（版本回退时旧 broker 仍能读它），clear 时一并删除。
const LEGACY_HISTORY_FILE: &str = "history-v1.json";
const MAX_ENTRIES: usize = 5000;
const MAX_QUERY_KEYS_PER_ENTRY: usize = 8;
const MAX_QUERY_KEY_CHARS: usize = 32;
const FRECENCY_HALF_LIFE_SECS: f64 = 14.0 * 86_400.0;

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

enum HistoryFileError {
    NotFound,
    Corrupt,
    FutureSchema,
    Invalid,
    Io,
}

impl HistoryFileError {
    fn diagnostic(&self) -> HistoryDiagnostic {
        match self {
            // NotFound 由调用方短路，永远不会走到这里；映射成 Io 仅作防御。
            HistoryFileError::NotFound => HistoryDiagnostic::Io,
            HistoryFileError::Corrupt => HistoryDiagnostic::Corrupt,
            HistoryFileError::FutureSchema => HistoryDiagnostic::FutureSchema,
            HistoryFileError::Invalid => HistoryDiagnostic::Invalid,
            HistoryFileError::Io => HistoryDiagnostic::Io,
        }
    }
}

/// `entries` 始终按 `last_used_utc` 降序（prune 维护）；`index` 是
/// (kind, target) → 下标的查找表——5000 条容量下 score()/query_pick()
/// 不能承受逐候选的线性扫描。
struct HistoryState {
    entries: Vec<HistoryEntry>,
    index: HashMap<(String, String), usize>,
}

impl HistoryState {
    fn from_entries(mut entries: Vec<HistoryEntry>) -> Self {
        prune(&mut entries);
        let index = build_index(&entries);
        Self { entries, index }
    }

    fn rebuild_index(&mut self) {
        self.index = build_index(&self.entries);
    }

    fn lookup(&self, target: &ActionTarget) -> Option<&HistoryEntry> {
        let position = *self.index.get(&(target.kind.clone(), target.value.clone()))?;
        self.entries.get(position)
    }
}

fn build_index(entries: &[HistoryEntry]) -> HashMap<(String, String), usize> {
    entries
        .iter()
        .enumerate()
        .map(|(position, entry)| ((entry.kind.clone(), entry.target.clone()), position))
        .collect()
}

pub struct HistoryStore {
    path: PathBuf,
    legacy_path: PathBuf,
    enabled: AtomicBool,
    state: RwLock<HistoryState>,
    diagnostic: RwLock<Option<HistoryDiagnostic>>,
}

impl HistoryStore {
    pub fn load(data_dir: &Path, enabled: bool) -> Self {
        Self::load_at(data_dir, enabled, now_utc())
    }

    fn load_at(data_dir: &Path, enabled: bool, now: u64) -> Self {
        let path = data_dir.join(HISTORY_FILE);
        let legacy_path = data_dir.join(LEGACY_HISTORY_FILE);
        let (entries, diagnostic) = match read_history_file(&path, now) {
            Ok(entries) => (entries, None),
            Err(HistoryFileError::NotFound) => match read_history_file(&legacy_path, now) {
                Ok(mut entries) => {
                    seed_legacy_frecency(&mut entries);
                    (entries, None)
                }
                Err(HistoryFileError::NotFound) => (Vec::new(), None),
                Err(other) => (Vec::new(), Some(other.diagnostic())),
            },
            Err(other) => (Vec::new(), Some(other.diagnostic())),
        };
        Self {
            path,
            legacy_path,
            enabled: AtomicBool::new(enabled),
            state: RwLock::new(HistoryState::from_entries(entries)),
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
        let mut state = self
            .state
            .write()
            .map_err(|_| "history lock is poisoned".to_string())?;
        state.entries.clear();
        state.rebuild_index();
        for file in [&self.path, &self.legacy_path] {
            match std::fs::remove_file(file) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("clear history: {error}")),
            }
        }
        if let Ok(mut diagnostic) = self.diagnostic.write() {
            *diagnostic = None;
        }
        Ok(())
    }

    pub fn record(&self, target: &ActionTarget, usage: HistoryUse) -> Result<(), String> {
        self.record_at(target, usage, None, now_utc())
    }

    fn record_at(
        &self,
        target: &ActionTarget,
        usage: HistoryUse,
        query: Option<&str>,
        now: u64,
    ) -> Result<(), String> {
        if !self.is_enabled() || !is_recordable(target) {
            return Ok(());
        }
        let mut state = self
            .state
            .write()
            .map_err(|_| "history lock is poisoned".to_string())?;
        let key = (target.kind.clone(), target.value.clone());
        let position = match state.index.get(&key).copied() {
            Some(position) => position,
            None => {
                state.entries.push(HistoryEntry {
                    kind: key.0.clone(),
                    target: key.1.clone(),
                    ..HistoryEntry::default()
                });
                let position = state.entries.len() - 1;
                state.index.insert(key, position);
                position
            }
        };
        let entry = &mut state.entries[position];
        // 事件时折算存量：frecency = frecency·exp(-Δt/τ) + w，再叠加本次计数。
        let elapsed = now.saturating_sub(entry.last_used_utc);
        let decayed = entry.frecency_milli as f64 * 0.001 * decay_factor(elapsed);
        entry.frecency_milli = frecency_milli_from(decayed + usage_weight(usage));
        match usage {
            HistoryUse::Execute => entry.execute_count = entry.execute_count.saturating_add(1),
            HistoryUse::Reveal => entry.reveal_count = entry.reveal_count.saturating_add(1),
            HistoryUse::Destination => {
                entry.destination_count = entry.destination_count.saturating_add(1)
            }
        }
        entry.last_used_utc = now;
        if entry.first_used_utc == 0 {
            entry.first_used_utc = now;
        }
        if let Some(query) = query {
            record_query_stat(entry, query, now);
        }
        prune(&mut state.entries);
        state.rebuild_index();
        persist(&self.path, &state.entries)?;
        Ok(())
    }

    pub fn score(&self, target: &ActionTarget) -> u32 {
        self.score_with_now(target, now_utc())
    }

    fn score_with_now(&self, target: &ActionTarget, now: u64) -> u32 {
        if !self.is_enabled() {
            return 0;
        }
        self.state
            .read()
            .ok()
            .and_then(|state| state.lookup(target).map(|entry| effective_frecency(entry, now)))
            .unwrap_or(0)
    }

    /// 该 target 是否被这个（已规范化的）查询串选中过。broker 排序接线前允许
    /// dead_code（查询记忆置顶在后续改动接入）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn query_pick(&self, target: &ActionTarget, query: &str) -> bool {
        if !self.is_enabled() || query.is_empty() {
            return false;
        }
        self.state
            .read()
            .ok()
            .and_then(|state| {
                state
                    .lookup(target)
                    .map(|entry| entry.queries.iter().any(|stat| stat.query == query))
            })
            .unwrap_or(false)
    }

    pub fn weights(&self) -> Vec<HistoryWeight> {
        if !self.is_enabled() {
            return Vec::new();
        }
        let now = now_utc();
        self.state
            .read()
            .map(|state| {
                state
                    .entries
                    .iter()
                    .map(|entry| HistoryWeight {
                        target: ActionTarget {
                            kind: entry.kind.clone(),
                            value: entry.target.clone(),
                        },
                        score: effective_frecency(entry, now),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn entries(&self) -> Vec<HistoryEntry> {
        self.state.read().unwrap().entries.clone()
    }

    #[cfg(test)]
    fn score_at(&self, target: &ActionTarget, now: u64) -> u32 {
        self.score_with_now(target, now)
    }
}

/// exp(-elapsed/τ)，τ 由 14 天半衰期换算。惰性衰减：存量只在事件时折算，
/// 读取时按 last_used 折算，全程不需要定时器。
fn decay_factor(elapsed_secs: u64) -> f64 {
    if elapsed_secs == 0 {
        return 1.0;
    }
    (-(elapsed_secs as f64) * std::f64::consts::LN_2 / FRECENCY_HALF_LIFE_SECS).exp()
}

/// 读取时的有效分：四舍五入到整数（Δt=0 时与事件权重 w 数值一致，
/// 旧的 score 断言语义因此保持）。
fn effective_frecency(entry: &HistoryEntry, now: u64) -> u32 {
    let value =
        entry.frecency_milli as f64 * 0.001 * decay_factor(now.saturating_sub(entry.last_used_utc));
    (value + 0.5).min(u32::MAX as f64) as u32
}

fn frecency_milli_from(value: f64) -> u32 {
    let milli = (value * 1000.0).round();
    if milli >= u32::MAX as f64 {
        u32::MAX
    } else {
        milli as u32
    }
}

fn usage_weight(usage: HistoryUse) -> f64 {
    match usage {
        HistoryUse::Execute => 4.0,
        HistoryUse::Reveal => 2.0,
        HistoryUse::Destination => 1.0,
    }
}

fn record_query_stat(entry: &mut HistoryEntry, raw_query: &str, now: u64) {
    let Some(key) = normalized_query_key(raw_query) else {
        return;
    };
    if let Some(stat) = entry.queries.iter_mut().find(|stat| stat.query == key) {
        stat.count = stat.count.saturating_add(1);
        stat.last_used_utc = now;
    } else {
        entry.queries.push(QueryStat {
            query: key,
            count: 1,
            last_used_utc: now,
        });
    }
    trim_query_stats(entry);
}

/// 键归一化：trim → 小写 → 截 32 字符；空串/NUL 不记录。调用方（broker
/// 记录链路）先做查询语法级归一化（剥 ext:/path: 等），这里是存储侧兜底。
fn normalized_query_key(raw: &str) -> Option<String> {
    let key: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .take(MAX_QUERY_KEY_CHARS)
        .collect();
    (!key.is_empty() && !key.contains('\0')).then_some(key)
}

fn trim_query_stats(entry: &mut HistoryEntry) {
    if entry.queries.len() <= MAX_QUERY_KEYS_PER_ENTRY {
        return;
    }
    entry.queries.sort_by(|left, right| {
        right
            .last_used_utc
            .cmp(&left.last_used_utc)
            .then_with(|| left.query.cmp(&right.query))
    });
    entry.queries.truncate(MAX_QUERY_KEYS_PER_ENTRY);
}

fn legacy_history_score(entry: &HistoryEntry) -> u32 {
    entry
        .execute_count
        .saturating_mul(4)
        .saturating_add(entry.reveal_count.saturating_mul(2))
        .saturating_add(entry.destination_count)
}

/// v1 只有裸计数：把旧加权分（execute×4 + reveal×2 + destination×1）当作
/// frecency 初值，升级不清零已积累的使用权重。事件写入的 frecency 至少是
/// w×1000（w≥1），因此「计数>0 而 milli==0」只可能来自 v1 迁移。
fn seed_legacy_frecency(entries: &mut [HistoryEntry]) {
    for entry in entries.iter_mut() {
        if entry.frecency_milli == 0 {
            let legacy = legacy_history_score(entry);
            if legacy > 0 {
                entry.frecency_milli = legacy.saturating_mul(1000);
            }
        }
    }
}

fn is_recordable(target: &ActionTarget) -> bool {
    matches!(
        target.kind.as_str(),
        "file" | "directory" | "application" | "window"
    ) && !target.value.is_empty()
        && !target.value.contains('\0')
        && target.value.len() <= 32 * 1024
}

/// 容量 LRU：按 last_used 降序截断到 MAX_ENTRIES，并裁剪每条的查询键。
/// 没有时间维度剪枝——使用数据只被容量挤出，不被时间删除。
fn prune(entries: &mut Vec<HistoryEntry>) {
    entries.sort_by(|left, right| {
        right
            .last_used_utc
            .cmp(&left.last_used_utc)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.target.cmp(&right.target))
    });
    entries.truncate(MAX_ENTRIES);
    for entry in entries.iter_mut() {
        trim_query_stats(entry);
    }
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

fn read_history_file(path: &Path, now: u64) -> Result<Vec<HistoryEntry>, HistoryFileError> {
    let bytes = std::fs::read(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => HistoryFileError::NotFound,
        _ => HistoryFileError::Io,
    })?;
    let envelope = serde_json::from_slice::<VersionedEnvelope<HistoryData>>(&bytes).map_err(|_| {
        isolate(path, now);
        HistoryFileError::Corrupt
    })?;
    match envelope.into_compatible() {
        Ok(data) => Ok(data.entries),
        Err(error) if error.starts_with("schema version") => {
            isolate(path, now);
            Err(HistoryFileError::FutureSchema)
        }
        Err(_) => {
            isolate(path, now);
            Err(HistoryFileError::Invalid)
        }
    }
}

fn isolate(path: &Path, now: u64) {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    let stem = file_name.strip_suffix(".json").unwrap_or(file_name);
    let destination = parent.join(format!("{stem}.corrupt-{now}.json"));
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
            .record_at(&target("C:\\x"), HistoryUse::Execute, None, 1_000_000)
            .unwrap();
        store.set_enabled(true);
        store
            .record_at(
                &ActionTarget {
                    kind: "web".into(),
                    value: "https://example.invalid".into(),
                },
                HistoryUse::Execute,
                None,
                1_000_000,
            )
            .unwrap();
        assert!(store.entries().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_time_based_retention_and_clear_removes_both_files() {
        let dir = test_dir("capacity");
        let store = HistoryStore::load_at(&dir, true, 20_000_000);
        store
            .record_at(&target("C:\\old"), HistoryUse::Execute, None, 1)
            .unwrap();
        store
            .record_at(&target("C:\\recent"), HistoryUse::Reveal, None, 20_000_000)
            .unwrap();
        // v2 不再按时间剪枝：闲置再久的条目也保留，只能被容量 LRU 挤出。
        assert!(store.entries().iter().any(|entry| entry.target == "C:\\old"));
        assert_eq!(store.score_at(&target("C:\\recent"), 20_000_000), 2);
        std::fs::write(dir.join(LEGACY_HISTORY_FILE), b"{}").unwrap();
        store.clear().unwrap();
        assert!(!dir.join(HISTORY_FILE).exists());
        assert!(!dir.join(LEGACY_HISTORY_FILE).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_evicts_oldest_beyond_capacity_and_trims_queries() {
        let mut entries: Vec<HistoryEntry> = (0..=MAX_ENTRIES as u64)
            .map(|stamp| HistoryEntry {
                kind: "file".into(),
                target: format!("C:\\f{stamp}"),
                last_used_utc: stamp,
                ..HistoryEntry::default()
            })
            .collect();
        entries.get_mut(1).unwrap().queries = (0..9)
            .map(|stamp| QueryStat {
                query: format!("q{stamp}"),
                count: 1,
                last_used_utc: stamp,
            })
            .collect();
        prune(&mut entries);
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert!(
            entries.iter().all(|entry| entry.target != r"C:\f0"),
            "最旧的条目被挤出"
        );
        assert_eq!(
            entries.last().map(|entry| entry.target.as_str()),
            Some(r"C:\f1")
        );
        let trimmed = entries
            .iter()
            .find(|entry| entry.target == r"C:\f1")
            .unwrap();
        assert_eq!(trimmed.queries.len(), MAX_QUERY_KEYS_PER_ENTRY);
        assert!(
            !trimmed.queries.iter().any(|stat| stat.query == "q0"),
            "最旧的查询键被 LRU 挤出"
        );
    }

    #[test]
    fn corrupt_and_future_files_are_isolated_without_blocking() {
        let dir = test_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(HISTORY_FILE), b"{secret-path-not-json").unwrap();
        let store = HistoryStore::load_at(&dir, true, 42);
        assert_eq!(store.diagnostic(), Some(HistoryDiagnostic::Corrupt));
        assert!(store.entries().is_empty());
        assert!(dir.join("history-v2.corrupt-42.json").exists());

        std::fs::write(
            dir.join(HISTORY_FILE),
            br#"{"schema_version":999,"data":{"entries":[]}}"#,
        )
        .unwrap();
        let future = HistoryStore::load_at(&dir, true, 43);
        assert_eq!(future.diagnostic(), Some(HistoryDiagnostic::FutureSchema));
        assert!(future.entries().is_empty());

        // v2 已被上一例隔离改名，此时 v1 损坏同样隔离，不阻塞启动。
        std::fs::write(dir.join(LEGACY_HISTORY_FILE), b"not-json").unwrap();
        let legacy_corrupt = HistoryStore::load_at(&dir, true, 44);
        assert_eq!(legacy_corrupt.diagnostic(), Some(HistoryDiagnostic::Corrupt));
        assert!(legacy_corrupt.entries().is_empty());
        assert!(dir.join("history-v1.corrupt-44.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn valid_v2_shadows_a_corrupt_legacy_file() {
        let dir = test_dir("v2-wins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(HISTORY_FILE),
            br#"{"schema_version":2,"data":{"entries":[{"kind":"file","target":"C:\\ok.txt","execute_count":1,"last_used_utc":10}]}}"#,
        )
        .unwrap();
        std::fs::write(dir.join(LEGACY_HISTORY_FILE), b"garbage").unwrap();
        let store = HistoryStore::load_at(&dir, true, 50);
        assert_eq!(store.diagnostic(), None);
        assert_eq!(store.entries().len(), 1);
        assert!(
            dir.join(LEGACY_HISTORY_FILE).exists(),
            "v2 可用时 v1 不被读取，也不被隔离"
        );
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

    #[test]
    fn legacy_v1_seeds_frecency_and_persists_v2_on_first_record() {
        let dir = test_dir("migrate");
        std::fs::create_dir_all(&dir).unwrap();
        let now = 1_800_000_000u64;
        std::fs::write(
            dir.join(LEGACY_HISTORY_FILE),
            format!(r#"{{"schema_version":1,"data":{{"entries":[{{"kind":"file","target":"C:\\proj\\config.json","execute_count":3,"reveal_count":1,"last_used_utc":{now}}}]}}}}"#),
        )
        .unwrap();
        let store = HistoryStore::load_at(&dir, true, now);
        // 旧计数 3×4+1×2=14 换算成 frecency 初值；last_used=now → 衰减为零。
        assert_eq!(store.score_at(&target("C:\\proj\\config.json"), now), 14);
        let half = FRECENCY_HALF_LIFE_SECS as u64;
        assert_eq!(store.score_at(&target("C:\\proj\\config.json"), now + half), 7);
        // 迁移先发生在内存：首个动作才落 v2 文件，v1 原文件保留。
        assert!(!dir.join(HISTORY_FILE).exists());
        store
            .record_at(&target("C:\\new.txt"), HistoryUse::Execute, None, now)
            .unwrap();
        assert!(dir.join(HISTORY_FILE).exists());
        assert!(dir.join(LEGACY_HISTORY_FILE).exists());
        let upgraded: VersionedEnvelope<HistoryData> =
            serde_json::from_slice(&std::fs::read(dir.join(HISTORY_FILE)).unwrap()).unwrap();
        assert_eq!(upgraded.schema_version, 2);
        assert!(upgraded
            .data
            .entries
            .iter()
            .any(|entry| entry.target == r"C:\proj\config.json"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn frecency_decays_between_events_and_lazily_on_read() {
        let dir = test_dir("frecency");
        let store = HistoryStore::load_at(&dir, true, 1_000_000);
        let doc = target("C:\\doc.md");
        let half = FRECENCY_HALF_LIFE_SECS as u64;
        store
            .record_at(&doc, HistoryUse::Execute, None, 1_000_000)
            .unwrap();
        assert_eq!(store.score_at(&doc, 1_000_000), 4);
        assert_eq!(store.score_at(&doc, 1_000_000 + half), 2);
        // 事件间衰减：半衰期后再用一次 → 2 + 4 = 6。
        store
            .record_at(&doc, HistoryUse::Execute, None, 1_000_000 + half)
            .unwrap();
        assert_eq!(store.score_at(&doc, 1_000_000 + half), 6);
        // 长期不用 → 衰减归零。
        assert_eq!(store.score_at(&doc, 1_000_000 + half * 20), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn query_stats_dedupe_cap_and_query_pick() {
        let dir = test_dir("queries");
        let store = HistoryStore::load_at(&dir, true, 1_000_000);
        let doc = target("C:\\conf.txt");
        store
            .record_at(&doc, HistoryUse::Execute, Some("conf"), 1_000_000)
            .unwrap();
        store
            .record_at(&doc, HistoryUse::Execute, Some("Conf "), 1_000_100)
            .unwrap();
        store
            .record_at(&doc, HistoryUse::Execute, Some("other"), 1_000_200)
            .unwrap();
        let entries = store.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.target == r"C:\conf.txt")
            .unwrap();
        assert_eq!(entry.queries.len(), 2);
        let conf = entry
            .queries
            .iter()
            .find(|stat| stat.query == "conf")
            .unwrap();
        assert_eq!(conf.count, 2);

        // 超过 8 个键：最旧的（conf/other）被 LRU 挤出。
        for index in 0..8 {
            store
                .record_at(
                    &doc,
                    HistoryUse::Execute,
                    Some(&format!("q{index}")),
                    1_001_000 + index,
                )
                .unwrap();
        }
        let entries = store.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.target == r"C:\conf.txt")
            .unwrap();
        assert_eq!(entry.queries.len(), MAX_QUERY_KEYS_PER_ENTRY);
        assert!(!entry.queries.iter().any(|stat| stat.query == "conf"));

        assert!(store.query_pick(&doc, "q7"));
        assert!(!store.query_pick(&doc, "conf"));
        assert!(!store.query_pick(&doc, "never"));
        store.set_enabled(false);
        assert!(!store.query_pick(&doc, "q7"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
