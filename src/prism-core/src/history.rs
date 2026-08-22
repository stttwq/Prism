//! Broker-owned, versioned usage history.
//!
//! v2 keeps counts forever (capacity-bounded LRU only — no time-based pruning),
//! scores targets with a lazily-decayed frecency (14-day half-life, folded in on
//! each event), and remembers which normalized query strings selected a target
//! (`QueryStat`, for query-aware ranking). Reads apply lazy decay from
//! `last_used_utc`; no background timers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::RwLock;
use std::sync::{Arc, Mutex};
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
    /// MRU 列表（空查询等）按时间而不是分数排序需要它。`weights()` 按
    /// last_used 降序返回，与内部条目顺序一致。
    pub last_used_utc: u64,
}

#[derive(Debug)]
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
///
/// 审计 P2：键从 `(String, String)` 改成 interned 单串 `kind + '\0' + value`。
/// 查找侧配合线程局部缓冲（[`with_composed_key`]）借出 `&str`，走
/// `HashMap<String, _>` 的 `Borrow<str>` 通道——每击键 600-1000 次查找不再
/// 各分配两个 String。
struct HistoryState {
    entries: Vec<HistoryEntry>,
    index: HashMap<String, usize>,
}

/// 复合键分隔符。`is_recordable` 拒绝含 NUL 的 value，kind 取自固定白名单
/// （file/directory/application/window），因此 NUL 不可能出现在两侧内容里，
/// 键无歧义。
const KEY_SEPARATOR: char = '\0';

fn write_composed_key(kind: &str, value: &str, buffer: &mut String) {
    buffer.clear();
    buffer.reserve(kind.len() + 1 + value.len());
    buffer.push_str(kind);
    buffer.push(KEY_SEPARATOR);
    buffer.push_str(value);
}

fn composed_key(kind: &str, value: &str) -> String {
    let mut key = String::new();
    write_composed_key(kind, value, &mut key);
    key
}

thread_local! {
    /// 查找专用缓冲：只在 [`with_composed_key`] 内借出，闭包里不再嵌套调用，
    /// 所以 borrow_mut 不会重入。
    static LOOKUP_KEY: RefCell<String> = RefCell::new(String::with_capacity(160));
}

fn with_composed_key<R>(kind: &str, value: &str, action: impl FnOnce(&str) -> R) -> R {
    LOOKUP_KEY.with(|cell| {
        let mut buffer = cell.borrow_mut();
        write_composed_key(kind, value, &mut buffer);
        action(buffer.as_str())
    })
}

/// 同一套 interned 复合键对 broker 侧的 dedup 集合开放（审计 P6）：注入历史
/// 候选与索引结果的去重集合用它做键，不再逐条构造 `(String, String)` 元组。
pub(crate) fn target_key(kind: &str, value: &str) -> String {
    composed_key(kind, value)
}

/// [`target_key`] 的零分配查找侧：借出线程局部缓冲里的键。闭包内不得再调用
/// 本函数或 [`HistoryStore`] 的查找方法（同一缓冲会被重入借用）。
pub(crate) fn with_target_key<R>(kind: &str, value: &str, action: impl FnOnce(&str) -> R) -> R {
    with_composed_key(kind, value, action)
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
        let position = with_composed_key(&target.kind, &target.value, |key| {
            self.index.get(key).copied()
        })?;
        self.entries.get(position)
    }
}

fn build_index(entries: &[HistoryEntry]) -> HashMap<String, usize> {
    entries
        .iter()
        .enumerate()
        .map(|(position, entry)| (composed_key(&entry.kind, &entry.target), position))
        .collect()
}

pub struct HistoryStore {
    path: PathBuf,
    legacy_path: PathBuf,
    enabled: AtomicBool,
    state: RwLock<HistoryState>,
    diagnostic: RwLock<Option<HistoryDiagnostic>>,
    /// 审计 P1：weights 快照缓存。record_at 写入后用 `invalidate_weights()`
    /// 置空；weights() 在缓存命中时直接 Arc clone（refcount++），消除每击键
    /// 全量 clone 5000 条的开销。用独立 Mutex 不依赖 RwLock 写锁——
    /// 多读者可并发检查缓存，仅在 miss 时持锁重建。
    weights_cache: Mutex<Option<Arc<[HistoryWeight]>>>,
    /// G4（FRESH-AUDIT-2）：写盘节流。动作历史每条记录都全量 JSON+fsync 太密——
    /// 连续动作只落一次盘，脏数据由下一次到期写入或 Drop 兜底（丢失窗口 ≤ 节流间隔）。
    persist_gate: Mutex<HistoryPersistGate>,
    /// H1（复审 2026-08-21）：persist 写盘段（临时文件 create/write/fsync/replace）
    /// 的串行化锁。G4 门只串行化**决策**：record 走 spawn_blocking、定时冲刷在
    /// 独立线程，两次 ≥250ms 间隔的判定仍可能在 I/O 上重叠（首个 fsync 被慢盘/
    /// AV 拖住时），共享同一个 `history-v2.json.tmp` 交错写会产出撕裂 JSON 并被
    /// atomic_replace 装上——下次启动解析失败整表隔离丢弃。持锁后 250ms 节流下
    /// 实际不存在竞争等待。
    persist_lock: Mutex<()>,
    /// M2（复审 2026-08-21）：clear() 的代际号。record/冲刷线程在决策落盘时
    /// 快照代际，persist 前复核——clear 已发生则放弃，防止在飞的旧快照把
    /// 刚清空的历史文件复活。
    epoch: AtomicU64,
}

/// G4: 节流间隔。动作通常成串发生（连续打开/定位），首条立即落盘，
/// 后续 250ms 窗口内的合并到下一次到期写入。
const MIN_PERSIST_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Default)]
struct HistoryPersistGate {
    last: Option<std::time::Instant>,
    dirty: bool,
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
            weights_cache: Mutex::new(None),
            persist_gate: Mutex::new(HistoryPersistGate::default()),
            persist_lock: Mutex::new(()),
            epoch: AtomicU64::new(0),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// L 批次（FRESH-AUDIT-3-2026-08-20）：定时冲刷节流窗口内的脏数据。
    /// G4 的节流依赖「下一次动作或 Drop 到来」才落盘——动作串结束后若无后续
    /// 动作，窗口内的脏数据要等到进程退出才写；进程被强杀（崩溃/任务管理器）
    /// 时丢失。本线程把丢失窗口压回 ≤ 2× 节流间隔。持 Weak：最后一个强引用
    /// Drop 时既有兜底冲刷照常运行，线程随后自行退出；线程创建失败只是回到
    /// G4 的既有语义（下次动作或 Drop 落盘），不致命。
    pub fn start_periodic_flush(self: &Arc<Self>) {
        let weak = std::sync::Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("history-flush".into())
            .spawn(move || loop {
                std::thread::sleep(MIN_PERSIST_INTERVAL);
                let Some(store) = weak.upgrade() else {
                    return;
                };
                // 只读判定：到期且（有真实变更留下的）脏标记才 clone+persist。
                // M2：代际在判定前快照，persist 侧复核——clear 夹在中间时放弃。
                let captured_epoch = store.epoch.load(Ordering::Acquire);
                if store.persist_if_due_and_dirty() {
                    let _ = store.persist_snapshot(captured_epoch);
                }
            });
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
        // M2：与 persist_snapshot 同一把锁——clear 与在飞落盘互斥。否则 clear
        // 可以插进「代际检查通过 → 快照读取完成」之间：旧快照随后写回磁盘，
        // 已清空的历史复活。锁序恒为 persist_lock → state，与 persist 一致。
        let _persist = self
            .persist_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .write()
            .map_err(|_| "history lock is poisoned".to_string())?;
        state.entries.clear();
        state.rebuild_index();
        if let Ok(mut cache) = self.weights_cache.lock() {
            *cache = None;
        }
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
        // G4: 清空后重置节流门，避免先前的脏标记在下次 record 前意外重建文件。
        if let Ok(mut gate) = self.persist_gate.lock() {
            *gate = HistoryPersistGate::default();
        }
        // M2：代际 +1——所有在飞（已快照代际、尚未落盘）的 persist 就此作废，
        // 不把 clear 前的旧快照写回磁盘复活已清空的历史。
        self.epoch.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    pub fn record(&self, target: &ActionTarget, usage: HistoryUse) -> Result<(), String> {
        self.record_at(target, usage, None, now_utc())
    }

    /// 带查询记忆的动作记录：`query` 为已归一化的查询键；None = 调用方没有
    /// 查询上下文（旧前端），只记 frecency 不记 query 子表。
    pub fn record_with_query(
        &self,
        target: &ActionTarget,
        usage: HistoryUse,
        query: Option<&str>,
    ) -> Result<(), String> {
        self.record_at(target, usage, query, now_utc())
    }

    pub(crate) fn record_at(
        &self,
        target: &ActionTarget,
        usage: HistoryUse,
        query: Option<&str>,
        now: u64,
    ) -> Result<(), String> {
        if !self.is_enabled() || !is_recordable(target) {
            return Ok(());
        }
        // M2：代际在变更前快照——落盘前复核，clear 夹在中间则放弃。
        let captured_epoch = self.epoch.load(Ordering::Acquire);
        // 锁内：内存更新 + 仅超容量时 prune。JSON 编码 + fsync 出锁后执行，
        // 避免写锁持有期间阻塞所有搜索的 score()/weights() 读操作。
        // （审计 P3+M3：参考 Everything "数据库全驻内存、退出才写盘" 的思路——
        // 内存操作与持久化解耦。G4 节流后快照只在真正落盘时才 clone。）
        {
            let mut state = self
                .state
                .write()
                .map_err(|_| "history lock is poisoned".to_string())?;
            let key = composed_key(&target.kind, &target.value);
            let position = match state.index.get(&key).copied() {
                Some(position) => position,
                None => {
                    state.entries.push(HistoryEntry {
                        kind: target.kind.clone(),
                        target: target.value.clone(),
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
            // 仅在新增条目导致超容量时 prune（O(n log n)），日常已有条目更新不触发。
            // 不变量复刻：entries 按 last_used 降序 + (kind,target) tie-break
            // （prune 内部 sort_by 保证），index 由 rebuild_index 重建。
            if state.entries.len() > MAX_ENTRIES {
                prune(&mut state.entries);
                state.rebuild_index();
            }
        };
        // 条目已变更：weights 快照缓存失效，下次 weights() 重建。
        if let Ok(mut cache) = self.weights_cache.lock() {
            *cache = None;
        }
        // G4 写盘节流：锁外判定 + 到期才落盘（首条立即落盘）。
        if !self.should_persist_now() {
            return Ok(());
        }
        self.persist_snapshot(captured_epoch)
    }

    /// H1+M2（复审 2026-08-21）：唯一的落盘入口。persist_lock 串行化写盘段
    /// （防共享 tmp 名交错撕裂）；落盘前复核代际（clear 已发生则放弃，防复活）。
    /// clone 在读锁内（纯 memcpy），锁释放后再 persist——编码+写盘+fsync+
    /// ReplaceFileW 绝不持 state 锁（record 写者与 async 线程的 score() 不被
    /// 一次慢盘 fsync 拖住）。
    fn persist_snapshot(&self, captured_epoch: u64) -> Result<(), String> {
        let _guard = self
            .persist_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.epoch.load(Ordering::Acquire) != captured_epoch {
            // 二轮复核 LOW：决策侧已把门上的脏标记消费掉，这里放弃落盘会让
            // 「clear 后立刻到达的新 record」（内存新于磁盘）既不落盘也不置脏，
            // Drop/定时冲刷都不再收敛。重新置脏交回下一拍。
            if let Ok(mut gate) = self.persist_gate.lock() {
                gate.dirty = true;
            }
            return Ok(());
        }
        let snapshot = self.state.read().ok().map(|state| state.entries.clone());
        match snapshot {
            Some(entries) => persist(&self.path, entries),
            None => Ok(()),
        }
    }

    /// G4: 到期判定并记账（record 路径：先改内存再调这里，置脏合理）。
    /// 返回 true = 本次应落盘（调用方随后 clone+persist）。
    fn should_persist_now(&self) -> bool {
        let Ok(mut gate) = self.persist_gate.lock() else {
            return false;
        };
        gate.dirty = true;
        let due = gate
            .last
            .is_none_or(|at| at.elapsed() >= MIN_PERSIST_INTERVAL);
        if due {
            gate.last = Some(std::time::Instant::now());
            gate.dirty = false;
        }
        due
    }

    /// AUDIT-4-2026-08-20 修 1：定时线程专用的**只读**到期判定——到期且脏才
    /// 落盘并清脏，绝不置脏。此前的定时线程复用 `should_persist_now`
    /// （无条件 `dirty = true`），导致空闲时也每 250ms 全量 clone+JSON+fsync
    /// 一次，进程全生命期持续（磁盘写入风暴）。
    fn persist_if_due_and_dirty(&self) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let Ok(mut gate) = self.persist_gate.lock() else {
            return false;
        };
        let due = gate
            .last
            .is_none_or(|at| at.elapsed() >= MIN_PERSIST_INTERVAL);
        if gate.dirty && due {
            gate.last = Some(std::time::Instant::now());
            gate.dirty = false;
            return true;
        }
        false
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
            .and_then(|state| {
                state
                    .lookup(target)
                    .map(|entry| effective_frecency(entry, now))
            })
            .unwrap_or(0)
    }

    /// 该 target 是否被这个查询串选中过。入参先做与记录侧相同的键归一化
    /// （trim→小写→截断），调用方传剥掉过滤词的 name query 即可。
    pub fn query_pick(&self, target: &ActionTarget, query: &str) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let Some(key) = normalized_query_key(query) else {
            return false;
        };
        self.query_pick_by_key(target, &key)
    }

    /// 直传已归一化键的版本（审计 P8）：一次搜索里整批候选共用同一个键，
    /// 逐条重复归一化只是重复分配。归一化幂等，所以与 [`Self::query_pick`]
    /// 对同一查询串的判定完全一致；调用方须传 [`normalized_query_key`] 的输出。
    pub fn query_pick_by_key(&self, target: &ActionTarget, key: &str) -> bool {
        if !self.is_enabled() || key.is_empty() {
            return false;
        }
        self.state
            .read()
            .ok()
            .and_then(|state| {
                state
                    .lookup(target)
                    .map(|entry| entry.queries.iter().any(|stat| stat.query == key))
            })
            .unwrap_or(false)
    }

    /// 返回权重快照。缓存命中时仅 refcount++（零 clone）；miss 时在读锁内
    /// 重建并缓存。record_at / clear 写入后缓存失效。
    /// 审计 P1：从每击键全量 clone 5000 条改为 Arc 快照发布。
    pub fn weights(&self) -> Arc<[HistoryWeight]> {
        if !self.is_enabled() {
            return Arc::from([]);
        }
        // 快路径：缓存命中直接 clone Arc（refcount++）。
        if let Ok(cache) = self.weights_cache.lock() {
            if let Some(snapshot) = cache.as_ref() {
                return Arc::clone(snapshot);
            }
        }
        // 慢路径：读锁内重建。
        // 复审 H2（2026-08-21 全仓重审）：record_at 对已有条目原地更新、新条目
        // 尾插，entries 的 last_used 降序只在 prune（超容量）与 load 时恢复——
        // 两次 prune 之间的 entries 无序。weights() 的消费方（空查询 MRU 注入）
        // 按返回顺序截断，这里必须显式排序，否则刚用过的文件沉底进不了列表。
        let now = now_utc();
        let snapshot: Arc<[HistoryWeight]> = self
            .state
            .read()
            .map(|state| {
                let mut list = state
                    .entries
                    .iter()
                    .map(|entry| HistoryWeight {
                        target: ActionTarget {
                            kind: entry.kind.clone(),
                            value: entry.target.clone(),
                        },
                        score: effective_frecency(entry, now),
                        last_used_utc: entry.last_used_utc,
                    })
                    .collect::<Vec<_>>();
                list.sort_by(|left, right| {
                    right
                        .last_used_utc
                        .cmp(&left.last_used_utc)
                        .then_with(|| left.target.kind.cmp(&right.target.kind))
                        .then_with(|| left.target.value.cmp(&right.target.value))
                });
                list.into()
            })
            .unwrap_or_else(|_| Arc::from([]));
        // 缓存：try_lock 避免在锁竞争时阻塞——miss 时多一次重建是无害的（幂等）。
        if let Ok(mut cache) = self.weights_cache.lock() {
            if cache.is_none() {
                *cache = Some(Arc::clone(&snapshot));
            }
        }
        snapshot
    }

    /// 读取时的 (有效分, last_used)。窗口空查询等 MRU 列表需要时间而不是分数。
    pub fn usage(&self, target: &ActionTarget) -> Option<(u32, u64)> {
        self.usage_at(target, now_utc())
    }

    pub(crate) fn usage_at(&self, target: &ActionTarget, now: u64) -> Option<(u32, u64)> {
        if !self.is_enabled() {
            return None;
        }
        self.state.read().ok().and_then(|state| {
            state
                .lookup(target)
                .map(|entry| (effective_frecency(entry, now), entry.last_used_utc))
        })
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
/// 记录链路）先做查询语法级归一化（剥 > 模式前缀与 ext:/path: 等），
/// 这里是存储侧兜底，幂等。
pub(crate) fn normalized_query_key(raw: &str) -> Option<String> {
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

/// G4（FRESH-AUDIT-2）：Drop 兜底冲刷节流窗口内的脏数据——broker 退出时
/// 最近 250ms 内的动作记录不丢。
/// M8（全仓复审 2026-08-22）：Drop 也走 persist_lock——当前安全只靠
/// 「HistoryStore 恒在 Arc 里、定时线程 upgrade 后强引用>0」这一约定；
/// 未来任何非 Arc 用法/不持强引用的写盘路径出现时，绕锁直写会让两个
/// 写者交错撕裂同一个 .tmp 文件。锁在 Drop 里独占可得，零成本。
impl Drop for HistoryStore {
    fn drop(&mut self) {
        let gate = self
            .persist_gate
            .get_mut()
            .unwrap_or_else(|p| p.into_inner());
        if !gate.dirty || !self.is_enabled() {
            return;
        }
        let _guard = self
            .persist_lock
            .get_mut()
            .unwrap_or_else(|p| p.into_inner());
        let Ok(state) = self.state.read() else {
            return;
        };
        let _ = persist(&self.path, state.entries.clone());
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

fn persist(path: &Path, entries: Vec<HistoryEntry>) -> Result<(), String> {
    let parent = path.parent().ok_or("history path has no parent")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create history directory: {error}"))?;
    // G4: entries 按值移动进信封，免去此前 record 侧 clone 之后这里的第二次全量 to_vec。
    let envelope = VersionedEnvelope::new(HistoryData { entries })?;
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
    let envelope =
        serde_json::from_slice::<VersionedEnvelope<HistoryData>>(&bytes).map_err(|_| {
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

pub(crate) fn now_utc() -> u64 {
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

    /// M2（复审 2026-08-21）：clear 之后，在飞的 persist（clear 前快照的代际）
    /// 必须放弃落盘——旧快照写回会复活已清空的历史文件。
    #[test]
    fn clear_aborts_in_flight_persist() {
        let dir = test_dir("clear-abort");
        let store = HistoryStore::load_at(&dir, true, 1_000_000);
        store
            .record_at(
                &target("C:\\gone.txt"),
                HistoryUse::Execute,
                None,
                1_000_000,
            )
            .unwrap();
        assert!(store.path.exists(), "首条 record 立即落盘");
        // 模拟在飞路径：快照代际 → clear → persist。
        let captured = store.epoch.load(Ordering::Acquire);
        store.clear().unwrap();
        assert!(!store.path.exists());
        store.persist_snapshot(captured).unwrap();
        assert!(
            !store.path.exists(),
            "clear 后在飞 persist 必须放弃，不得复活历史文件"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 复审 H2（2026-08-21 全仓重审）：record_at 尾插/原地更新使 entries 无序
    /// （降序只在 prune/load 时恢复），weights() 必须按 last_used 显式降序——
    /// 空查询 MRU 注入按 weights 顺序截断，刚用过的文件必须排在最前。
    /// 序列刻意选尾插序与 MRU 序分歧的纯粹形（second 后用）：去掉 sort_by
    /// 该断言必挂。
    #[test]
    fn weights_returns_true_mru_order_between_prunes() {
        let dir = test_dir("weights-mru");
        let store = HistoryStore::load_at(&dir, true, 1_000_000);
        store
            .record_at(&target("C:\\first.txt"), HistoryUse::Execute, None, 1_000)
            .unwrap();
        store
            .record_at(&target("C:\\second.txt"), HistoryUse::Execute, None, 2_000)
            .unwrap();
        // entries 插入序 = [first, second]；MRU 序 = [second, first]。
        let weights = store.weights();
        let values: Vec<&str> = weights.iter().map(|w| w.target.value.as_str()).collect();
        assert_eq!(
            values,
            vec!["C:\\second.txt", "C:\\first.txt"],
            "最近使用的条目必须排最前（真 MRU），不能按插入序"
        );
        // 复用已有条目上浮：first 更新为最新后必须回到首位（原地更新不动位置）。
        store
            .record_at(&target("C:\\first.txt"), HistoryUse::Execute, None, 3_000)
            .unwrap();
        let after = store.weights();
        let values_after: Vec<&str> = after.iter().map(|w| w.target.value.as_str()).collect();
        assert_eq!(values_after, vec!["C:\\first.txt", "C:\\second.txt"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// H1（复审 2026-08-21）：并发 persist_snapshot 全部经 persist_lock 串行化——
    /// 收敛后文件可解析、无 .tmp 残留（共享 tmp 名的交错写曾可产出撕裂 JSON
    /// 并被 atomic_replace 装上，下次启动整表隔离丢弃）。
    #[test]
    fn concurrent_persists_are_serialized_and_leave_no_torn_file() {
        let dir = test_dir("persist-race");
        let store = Arc::new(HistoryStore::load_at(&dir, true, 1_000_000));
        for index in 0..16u64 {
            store
                .record_at(
                    &target(format!("C:\\f{index}.txt")),
                    HistoryUse::Execute,
                    None,
                    1_000_000 + index,
                )
                .unwrap();
        }
        let captured = store.epoch.load(Ordering::Acquire);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                store.persist_snapshot(captured).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let entries = read_history_file(&store.path, 1_000_016).expect("历史文件必须保持可解析");
        assert_eq!(entries.len(), 16);
        assert!(
            !dir.join(format!("{HISTORY_FILE}.tmp")).exists(),
            "临时文件必须被 replace 消费干净"
        );
        let _ = std::fs::remove_dir_all(dir);
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

    /// AUDIT-4-2026-08-20 修 1 的回归锚：定时线程的只读判定在**无变更**时
    /// 不得置脏——空闲等待多个节拍后历史文件必须不存在（修复前每 250ms
    /// 全量落盘一次，文件立刻出现且持续被重写）。
    #[test]
    fn periodic_flush_never_marks_dirty_on_its_own() {
        let dir = test_dir("idle-flush");
        let store = HistoryStore::load_at(&dir, true, 1_000_000);
        // 无任何 record：连续两拍只读判定都为假。
        assert!(!store.persist_if_due_and_dirty());
        assert!(!store.persist_if_due_and_dirty());
        assert!(!store.path.exists(), "空闲判定不得触发落盘");

        // 第一条 record 立即落盘（G4 首条语义），第二条在 250ms 窗口内置脏
        // 不落盘，随后定时判定接管冲刷。
        store
            .record_at(&target("C:\\audit4"), HistoryUse::Execute, None, 1_000_000)
            .unwrap();
        assert!(store.path.exists(), "首条 record 立即落盘");
        store
            .record_at(&target("C:\\audit4b"), HistoryUse::Execute, None, 1_000_001)
            .unwrap();
        // 窗口未到：不冲刷（脏标记留在门上）。
        assert!(!store.persist_if_due_and_dirty());
        std::thread::sleep(MIN_PERSIST_INTERVAL + MIN_PERSIST_INTERVAL / 2);
        assert!(store.persist_if_due_and_dirty(), "到期且脏必须冲刷");
        // 冲刷清脏后，下一拍回到空闲静默。
        std::thread::sleep(MIN_PERSIST_INTERVAL + MIN_PERSIST_INTERVAL / 2);
        assert!(!store.persist_if_due_and_dirty());
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
        assert!(store
            .entries()
            .iter()
            .any(|entry| entry.target == "C:\\old"));
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
        assert_eq!(
            legacy_corrupt.diagnostic(),
            Some(HistoryDiagnostic::Corrupt)
        );
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
        assert_eq!(
            store.score_at(&target("C:\\proj\\config.json"), now + half),
            7
        );
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
        // P8：直传归一化键与走归一化外壳的判定必须一致（键归一化幂等）。
        assert!(store.query_pick_by_key(&doc, "q7"));
        assert!(store.query_pick_by_key(&doc, &normalized_query_key(" Q7 ").unwrap()));
        assert!(!store.query_pick_by_key(&doc, "conf"));
        assert!(!store.query_pick_by_key(&doc, ""));
        store.set_enabled(false);
        assert!(!store.query_pick(&doc, "q7"));
        assert!(!store.query_pick_by_key(&doc, "q7"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// P2：interned 单串键不能让「kind 尾巴 + value 头」拼成同一个键。
    /// `'\0'` 分隔符 + `is_recordable` 拒 NUL 是这条不变量的依据。
    #[test]
    fn interned_index_key_does_not_collide_across_kinds() {
        assert_ne!(
            composed_key("file", "directory"),
            composed_key("filedirectory", "")
        );
        assert_ne!(composed_key("file", "\\a"), composed_key("file\\", "a"));

        let dir = test_dir("interned-key");
        let store = HistoryStore::load_at(&dir, true, 1_000_000);
        let file = ActionTarget {
            kind: "file".into(),
            value: "C:\\same".into(),
        };
        let directory = ActionTarget {
            kind: "directory".into(),
            value: "C:\\same".into(),
        };
        store
            .record_at(&file, HistoryUse::Execute, None, 1_000_000)
            .unwrap();
        // 同 value 不同 kind 是两条独立记录，查找不得串台。
        assert_eq!(store.score_at(&file, 1_000_000), 4);
        assert_eq!(store.score_at(&directory, 1_000_000), 0);
        store
            .record_at(&directory, HistoryUse::Reveal, None, 1_000_000)
            .unwrap();
        assert_eq!(store.score_at(&file, 1_000_000), 4);
        assert_eq!(store.score_at(&directory, 1_000_000), 2);
        assert_eq!(store.entries().len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
