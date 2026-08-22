//! Compact FRN-indexed hierarchy shared by MFT construction and USN replay.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use serde::{Deserialize, Serialize};

/// F9（FRESH-AUDIT-2）：类型化的卷变更错误。此前 USN 重放/首建靠
/// "broken parent chain" 文案前缀做字符串匹配决定"延迟重试"还是"整卷重建"
/// ——改一处文案重放就静默变重建。Display 输出与旧字符串逐字一致，
/// 日志与上层错误文本不变。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeError {
    /// 父记录缺失/越界：重放侧据此延迟重试（子先于父到达是正常乱序）。
    BrokenParentChain { record: u32 },
    /// MFT 记录号超出 u32。
    MftRecordExceedsU32(u64),
    /// 记录号超出紧凑索引上限。
    RecordLimit(u32),
    /// 槽表病态稀疏。
    SparseSlots { required: usize, present: usize },
    /// 文件名含 NUL。
    NameNul,
    /// 名字池超出 u32。
    NamePoolOverflow,
}

impl std::fmt::Display for VolumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrokenParentChain { record } => {
                write!(f, "broken parent chain at record {record}")
            }
            Self::MftRecordExceedsU32(frn) => write!(f, "MFT record {frn} exceeds u32"),
            Self::RecordLimit(record) => {
                write!(f, "MFT record {record} exceeds compact-index limit")
            }
            Self::SparseSlots { required, present } => {
                write!(
                    f,
                    "MFT slot table is pathologically sparse: {required}/{present}"
                )
            }
            Self::NameNul => write!(f, "file name contains NUL"),
            Self::NamePoolOverflow => write!(f, "name pool exceeds u32"),
        }
    }
}

impl std::error::Error for VolumeError {}

pub const FLAG_PRESENT: u16 = 0x0001;
pub const FLAG_DIRECTORY: u16 = 0x0002;
pub const FLAG_EXCLUDED: u16 = 0x0004;
/// 槽表按 MFT 记录号预分配（12 字节/槽，resize 全量落实）。
/// 上限 2M 会拒绝现代系统盘（2~8M 记录很常见），且 build 失败会让整个索引
/// 服务退出。16M ≈ 192MB 槽表上限，实际按卷真实记录数落实；粗坏的 MFT
/// 解析（天文数字 FRN 配极小活跃数）仍由 prepare_initial_capacity 的
/// 4096× 密度检查拦截。
const MAX_RECORD_NUMBER: usize = 16_777_216;
const MAX_PATH_DEPTH: usize = 64;
const NO_NAME: u32 = u32::MAX;

/// Deepest record [`VolumeIndex::path_for`] can still render, counted in hops above the
/// volume root: the walk spends one iteration per component plus one for the root itself.
pub const MAX_RECORD_DEPTH: usize = MAX_PATH_DEPTH - 1;

/// Upper bound for one ancestor walk: how far below the requested root a candidate may
/// sit. Equal to [`MAX_RECORD_DEPTH`], so a root placed at the volume root accepts exactly
/// the records whose path can be constructed. A root nested `r` hops below the volume root
/// still only guarantees `r + depth <= MAX_RECORD_DEPTH` records get a path; deeper hits
/// are dropped by path construction, exactly as in an unscoped search.
pub const MAX_ANCESTOR_DEPTH: usize = MAX_RECORD_DEPTH;

/// Per-request memo ceiling for [`RootFilter`]. Bounded so a root search cannot grow
/// resident memory; the memo lives and dies with a single search request.
const MAX_ANCESTOR_MEMO: usize = 4_096;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeSlot {
    pub parent_record: u32,
    pub name_off: u32,
    pub sequence: u16,
    pub flags: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeId {
    pub guid: String,
    pub serial: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeIndex {
    pub volume_id: VolumeId,
    pub mount_path: String,
    pub journal_id: u64,
    pub next_usn: i64,
    pub root_record: u32,
    pub nodes: Vec<NodeSlot>,
    pub names: Vec<u8>,
    #[serde(skip)]
    initial_name_bytes: usize,
    #[serde(skip)]
    dead_name_bytes: usize,
    /// F3: names 指纹（serde skip 不入盘）。append 滚入、compact 整算、
    /// 缓存载入后由 [`VolumeIndex::recompute_derived_counters`] 重算。
    #[serde(skip)]
    pub names_fingerprint: u64,
    /// G4（FRESH-AUDIT-2）：在位节点计数（serde skip）。ensure_slot 的稀疏度
    /// 防线此前每次增长都全表扫描计数——首建期多次增长累计 O(n²)。
    /// upsert/delete 增减、载入后整算。
    #[serde(skip)]
    present_slots: usize,
}

/// F3: names 指纹的 FNV-1a 常量（与 pinyin_sidecar 的 FNV 族一致但独立维护，
/// 两者不是同一个值域，不共享常量以免耦合）。
const NAMES_FNV_OFFSET: u64 = 0xcbf29ce484222325;
const NAMES_FNV_PRIME: u64 = 0x100000001b3;

fn fold_names_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(NAMES_FNV_PRIME);
    }
    hash
}

fn names_pool_fingerprint(pool: &[u8]) -> u64 {
    fold_names_bytes(NAMES_FNV_OFFSET, pool)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexHit {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub match_metadata: MatchMetadata,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    #[default]
    Literal,
    FullPinyin,
    Initials,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatchMetadata {
    #[serde(default, skip_serializing_if = "is_literal")]
    pub kind: MatchKind,
    pub class: u8,
    pub position: u32,
    pub score: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub history_score: u32,
}

fn is_literal(value: &MatchKind) -> bool {
    *value == MatchKind::Literal
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// frecency 使用桶：≥64→3 / ≥16→2 / ≥4→1 / 否则 0（约一次近期使用的门槛）。
/// 分桶而非连续值：防止高频老条目永久霸榜，也保证桶内仍由匹配质量决胜。
/// 索引器侧 history_score 恒 0，桶比较恒等——此函数只影响 broker 最终排序。
fn usage_tier(score: u32) -> u8 {
    if score >= 64 {
        3
    } else if score >= 16 {
        2
    } else if score >= 4 {
        1
    } else {
        0
    }
}

impl Ord for MatchMetadata {
    fn cmp(&self, other: &Self) -> Ordering {
        // S1（PRISM-IMPL-PLAN-4-2026-08-20）：class 提到 kind 之前——「整名精确/
        // 前缀」这个更强的匹配信号不再被「碰巧含子串」压制。kind 优先时 `dy`
        // 的字面噪声（Kennedy.docx，class 2）无条件压过「抖音」（Initials，
        // class 0），拼音命中再被 Top-8 截断。同 class 内 kind 仍锁死：
        // 字面 > 全拼 > 首字母。
        self.class
            .cmp(&other.class)
            .then(self.kind.cmp(&other.kind))
            .then(usage_tier(other.history_score).cmp(&usage_tier(self.history_score)))
            .then(self.position.cmp(&other.position))
            .then(other.history_score.cmp(&self.history_score))
            .then(self.score.cmp(&other.score))
    }
}

impl PartialOrd for MatchMetadata {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOutcome {
    pub items: Vec<IndexHit>,
    pub is_truncated: bool,
    pub scanned_nodes: u64,
    pub name_candidates: u64,
    pub matched_count: u64,
    pub entered_top_k: u64,
    pub path_constructions: u64,
}

#[derive(Debug, Eq)]
struct RankedCandidate<'a> {
    volume_index: usize,
    mount_path: &'a str,
    record: u32,
    name: &'a str,
    is_directory: bool,
    metadata: MatchMetadata,
}

impl PartialEq for RankedCandidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Ord for RankedCandidate<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.metadata
            .class
            .cmp(&other.metadata.class)
            .then(self.metadata.position.cmp(&other.metadata.position))
            .then(self.metadata.score.cmp(&other.metadata.score))
            .then_with(|| self.name.cmp(other.name))
            .then_with(|| self.mount_path.cmp(other.mount_path))
            .then(self.record.cmp(&other.record))
            .then(self.is_directory.cmp(&other.is_directory))
    }
}

impl PartialOrd for RankedCandidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    RebuildRequired,
}

/// A resolved current-directory scope: which volume, and which directory record every
/// candidate must descend from. Deliberately a search-time value, not node state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootBound {
    pub volume_index: usize,
    pub root_record: u32,
}

/// Bounded ancestor verification for one search request.
///
/// Candidates are accepted only when the existing `parent_record` chain reaches
/// `root_record` within [`MAX_ANCESTOR_DEPTH`] hops. The *distance* of every record walked
/// on the way up is memoized, which makes repeated hits inside the same directory cheap
/// without storing anything in [`NodeSlot`]. The memo is dropped with the request.
#[derive(Debug)]
pub struct RootFilter {
    bound: RootBound,
    /// Hops from a record up to `bound.root_record`, or `None` when the record provably
    /// does not descend from it. Distances are absolute, so a verdict never depends on
    /// which candidate happened to be scanned first.
    memo: HashMap<u32, Option<u32>>,
    trail: Vec<u32>,
}

impl RootFilter {
    pub fn new(bound: RootBound) -> Self {
        Self {
            bound,
            memo: HashMap::new(),
            trail: Vec::new(),
        }
    }

    pub fn bound(&self) -> RootBound {
        self.bound
    }

    /// Returns true when `record` on `volume_index` is the root itself or below it.
    /// Cross-volume candidates, tombstones, missing parents and cycles are all rejected.
    pub fn accepts(&mut self, volume_index: usize, volume: &VolumeIndex, record: u32) -> bool {
        if volume_index != self.bound.volume_index {
            return false;
        }
        if let Some(known) = self.memo.get(&record) {
            return depth_is_in_scope(*known);
        }
        let Some(depth) = ancestor_depth(
            volume,
            record,
            self.bound.root_record,
            &self.memo,
            &mut self.trail,
        ) else {
            // The ceiling was reached without a verdict: the records walked past may well
            // be inside the root, so nothing is memoized and nothing gets poisoned.
            self.trail.clear();
            return false;
        };
        // `trail[0]` is `record` itself and each further entry sits one hop closer to the
        // root, so distances are exact for the whole walk.
        for (hop, visited) in self.trail.drain(..).enumerate() {
            if self.memo.len() >= MAX_ANCESTOR_MEMO {
                break;
            }
            self.memo
                .insert(visited, depth.map(|depth| depth.saturating_sub(hop as u32)));
        }
        if self.memo.len() < MAX_ANCESTOR_MEMO {
            self.memo.entry(record).or_insert(depth);
        }
        depth_is_in_scope(depth)
    }
}

/// A record is in scope when it descends from the root within the depth ceiling.
fn depth_is_in_scope(depth: Option<u32>) -> bool {
    depth.is_some_and(|depth| depth as usize <= MAX_ANCESTOR_DEPTH)
}

/// Walks up the parent chain and reports how far `record` sits below `root_record`.
///
/// * `Some(Some(depth))` — `record` descends from the root after `depth` hops.
/// * `Some(None)` — provably outside: dead root, tombstone, missing slot or cycle.
/// * `None` — undetermined because [`MAX_ANCESTOR_DEPTH`] hops were spent first. The
///   caller must treat this as a rejection *and* memoize nothing.
///
/// On return, `trail` holds the records walked, starting at `record`.
fn ancestor_depth(
    volume: &VolumeIndex,
    record: u32,
    root_record: u32,
    memo: &HashMap<u32, Option<u32>>,
    trail: &mut Vec<u32>,
) -> Option<Option<u32>> {
    trail.clear();
    let root_is_live = volume.nodes.get(root_record as usize).is_some_and(|slot| {
        slot.flags & (FLAG_PRESENT | FLAG_DIRECTORY) == FLAG_PRESENT | FLAG_DIRECTORY
    });
    if !root_is_live {
        // The root directory itself went away; the whole scope is stale.
        return Some(None);
    }
    let mut current = record;
    loop {
        if current == root_record {
            return Some(Some(trail.len() as u32));
        }
        if let Some(known) = memo.get(&current) {
            return Some(known.map(|depth| depth.saturating_add(trail.len() as u32)));
        }
        if trail.len() >= MAX_ANCESTOR_DEPTH {
            return None;
        }
        let Some(slot) = volume.nodes.get(current as usize) else {
            return Some(None);
        };
        if slot.flags & FLAG_PRESENT == 0 {
            return Some(None);
        }
        if slot.parent_record == current || trail.contains(&current) {
            return Some(None);
        }
        trail.push(current);
        current = slot.parent_record;
    }
}

pub(crate) struct MutationSnapshot {
    nodes_len: usize,
    names_len: usize,
    slots: Vec<(u32, NodeSlot)>,
    /// L 批次（FRESH-AUDIT-3-2026-08-20）：回滚时一并还原的派生计数——
    /// 否则回滚后计数与池/槽表状态失配（fingerprint 漂移致 sidecar 误判、
    /// dead_name_bytes 虚高提前压缩、present_slots 偏差影响稀疏度防线）。
    /// 回滚路径随后必被重建覆盖，影响有限，但修正只要多存三个数。
    names_fingerprint: u64,
    dead_name_bytes: usize,
    present_slots: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexState {
    pub volumes: Vec<VolumeIndex>,
    pub generation: u64,
    pub events_since_checkpoint: u64,
}

impl VolumeIndex {
    pub fn new(
        volume_id: VolumeId,
        mount_path: String,
        journal_id: u64,
        next_usn: i64,
        root_record: u32,
    ) -> Result<Self, String> {
        let root = root_record as usize;
        if root >= MAX_RECORD_NUMBER {
            return Err("root MFT record exceeds compact-index limit".into());
        }
        let mut nodes = vec![NodeSlot::default(); root + 1];
        nodes[root] = NodeSlot {
            parent_record: root_record,
            name_off: NO_NAME,
            sequence: 0,
            flags: FLAG_PRESENT | FLAG_DIRECTORY,
        };
        Ok(Self {
            volume_id,
            mount_path,
            journal_id,
            next_usn,
            root_record,
            nodes,
            names: Vec::new(),
            initial_name_bytes: 0,
            dead_name_bytes: 0,
            names_fingerprint: NAMES_FNV_OFFSET,
            present_slots: 1, // 根节点在 new 内即置 PRESENT
        })
    }

    pub fn finish_initial_build(&mut self) {
        self.nodes.shrink_to_fit();
        self.names.shrink_to_fit();
        self.initial_name_bytes = self.names.len();
        self.dead_name_bytes = 0;
    }

    pub(crate) fn prepare_initial_capacity(
        &mut self,
        max_record: u32,
        live_records: usize,
    ) -> Result<(), String> {
        let required = max_record as usize + 1;
        if required > MAX_RECORD_NUMBER {
            return Err(format!(
                "MFT record {max_record} exceeds compact-index limit"
            ));
        }
        // 2026-08-22 审查：NTFS 大批删除后槽表不缩、活跃数缩——曾冲上 2M 文件
        // 再清理到 60k 的合法数据卷（33× 密度）会被旧 32× 阈值判成病态稀疏，
        // build 确定性失败、延迟重试永久空转。4096× 仍拦得住粗坏解析
        //（解析 bug 产出的近 16M FRN 配极小活跃数，密度数万倍起）。
        if required > 65_536 && required > live_records.max(1).saturating_mul(4096) {
            return Err(format!(
                "MFT slot table is pathologically sparse: {required}/{live_records}"
            ));
        }
        self.nodes.resize(required, NodeSlot::default());
        Ok(())
    }

    /// M1（FRESH-AUDIT-2026-08-19）: 首建前按实测名字总字节预留 names 容量。
    /// 此前 names 靠 upsert 里的 extend_from_slice 摊销倍增增长——60MB 的池在
    /// 扩容瞬间会再要一份 120MB（峰值尖峰）。枚举完成时记录数与池大小都已知，
    /// min(池大小, 记录数×64) 封顶防止病态长名池过度预留。
    pub(crate) fn reserve_names_capacity(&mut self, estimate_bytes: usize) {
        self.names
            .reserve(estimate_bytes.saturating_sub(self.names.len()));
    }

    pub fn split_frn(frn: u64) -> Result<(u32, u16), String> {
        let record = frn & 0x0000_ffff_ffff_ffff;
        if record > u32::MAX as u64 {
            return Err(format!("MFT record {record} exceeds u32"));
        }
        Ok((record as u32, (frn >> 48) as u16))
    }

    pub fn upsert(
        &mut self,
        frn: u64,
        parent_frn: u64,
        name: &str,
        is_directory: bool,
    ) -> Result<ApplyOutcome, VolumeError> {
        let (record, sequence) =
            Self::split_frn(frn).map_err(|_| VolumeError::MftRecordExceedsU32(frn))?;
        let (parent_record, _) = Self::split_frn(parent_frn)
            .map_err(|_| VolumeError::MftRecordExceedsU32(parent_frn))?;
        if parent_record as usize >= self.nodes.len()
            || self.nodes[parent_record as usize].flags & FLAG_PRESENT == 0
        {
            return Err(VolumeError::BrokenParentChain { record });
        }
        self.ensure_slot(record)?;

        let parent_excluded = self.nodes[parent_record as usize].flags & FLAG_EXCLUDED != 0;
        let excluded = parent_excluded
            || is_excluded_name(name)
            || (name.eq_ignore_ascii_case("Installer")
                && self.node_name_is(parent_record, "Windows"));
        let old = self.nodes[record as usize];
        // G4: 首次在位的槽位计数 +1（槽原本就在表内但非在位的复活不算增长）。
        if old.flags & FLAG_PRESENT == 0 {
            self.present_slots += 1;
        }
        let crossed_boundary =
            old.flags & FLAG_PRESENT != 0 && (old.flags & FLAG_EXCLUDED != 0) != excluded;
        if crossed_boundary && is_directory {
            return Ok(ApplyOutcome::RebuildRequired);
        }
        let keep_name = is_directory || !excluded;
        // Track dead bytes when overwriting an existing present name.
        if old.flags & FLAG_PRESENT != 0 && old.name_off != NO_NAME {
            let old_name_len = self.names.get(old.name_off as usize..).and_then(|tail| {
                tail.iter()
                    .position(|byte| *byte == 0)
                    .map(|offset| offset + 1)
            });
            if let Some(len) = old_name_len {
                self.dead_name_bytes = self.dead_name_bytes.saturating_add(len);
            }
        }
        let name_off = if keep_name {
            self.append_name(name)?
        } else {
            NO_NAME
        };
        self.nodes[record as usize] = NodeSlot {
            parent_record,
            name_off,
            sequence,
            flags: FLAG_PRESENT
                | if is_directory { FLAG_DIRECTORY } else { 0 }
                | if excluded { FLAG_EXCLUDED } else { 0 },
        };
        Ok(ApplyOutcome::Applied)
    }

    pub fn delete(&mut self, frn: u64) -> Result<(), VolumeError> {
        let (record, sequence) =
            Self::split_frn(frn).map_err(|_| VolumeError::MftRecordExceedsU32(frn))?;
        let Some(slot) = self.nodes.get_mut(record as usize) else {
            return Ok(());
        };
        if slot.flags & FLAG_PRESENT != 0 && slot.sequence == sequence {
            if slot.name_off != NO_NAME {
                let name_len = self.names.get(slot.name_off as usize..).and_then(|tail| {
                    tail.iter()
                        .position(|byte| *byte == 0)
                        .map(|offset| offset + 1)
                });
                if let Some(len) = name_len {
                    self.dead_name_bytes = self.dead_name_bytes.saturating_add(len);
                }
            }
            slot.flags &= !FLAG_PRESENT;
            slot.name_off = NO_NAME;
            self.present_slots = self.present_slots.saturating_sub(1);
        }
        Ok(())
    }

    pub fn path_for(&self, record: u32) -> Result<String, String> {
        let mut current = record;
        let mut parts: Vec<&str> = Vec::new();
        for _ in 0..MAX_PATH_DEPTH {
            let slot = self
                .nodes
                .get(current as usize)
                .ok_or_else(|| format!("broken parent record {current}"))?;
            if slot.flags & FLAG_PRESENT == 0 {
                return Err(format!("missing parent record {current}"));
            }
            if current == self.root_record {
                let mut path = self.mount_path.trim_end_matches(['\\', '/']).to_owned();
                path.push('\\');
                for part in parts.iter().rev() {
                    if !path.ends_with('\\') {
                        path.push('\\');
                    }
                    path.push_str(part);
                }
                return Ok(path);
            }
            let name = self.name_at(slot.name_off)?;
            parts.push(name);
            if slot.parent_record == current {
                return Err(format!("parent cycle at record {current}"));
            }
            current = slot.parent_record;
        }
        Err("parent chain exceeds depth 64".into())
    }

    pub fn search(&self, query: &str, max: usize) -> Vec<IndexHit> {
        search_volumes(
            std::slice::from_ref(self),
            query,
            max,
            &[],
            None,
            &QueryFilters::none(),
        )
        .items
    }

    /// Bounded, memo-free descendant check. Use [`RootFilter`] when many records are
    /// verified against the same root inside one request.
    pub fn is_descendant_or_self(&self, record: u32, root_record: u32) -> bool {
        let memo = HashMap::new();
        let mut trail = Vec::new();
        depth_is_in_scope(ancestor_depth(self, record, root_record, &memo, &mut trail).flatten())
    }

    pub fn memory_bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<NodeSlot>() + self.names.capacity()
    }

    /// AUDIT-2026-08-18 R-C2: 轻量结构不变量检查——开销 O(1)，不做 path_for 遍历。
    /// 全量 validate（含每节点 path_for）保留给 load 侧；save 侧走 validate_structure +
    /// 抽样 path_for。
    pub(crate) fn validate_structure(&self) -> Result<(), String> {
        if self.nodes.len() > MAX_RECORD_NUMBER {
            return Err("node table exceeds compact-index limit".into());
        }
        let root = self
            .nodes
            .get(self.root_record as usize)
            .ok_or("cache root record is outside node table")?;
        if root.flags & (FLAG_PRESENT | FLAG_DIRECTORY) != FLAG_PRESENT | FLAG_DIRECTORY {
            return Err("cache root record is not a present directory".into());
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.nodes.len() > MAX_RECORD_NUMBER {
            return Err("node table exceeds compact-index limit".into());
        }
        let root = self
            .nodes
            .get(self.root_record as usize)
            .ok_or("cache root record is outside node table")?;
        if root.flags & (FLAG_PRESENT | FLAG_DIRECTORY) != FLAG_PRESENT | FLAG_DIRECTORY {
            return Err("cache root record is not a present directory".into());
        }
        for (record, slot) in self.nodes.iter().enumerate() {
            if slot.flags & FLAG_PRESENT == 0 || record == self.root_record as usize {
                continue;
            }
            let parent = self
                .nodes
                .get(slot.parent_record as usize)
                .ok_or_else(|| format!("cache parent record is outside node table: {record}"))?;
            if parent.flags & FLAG_PRESENT == 0 {
                // 父目录已被删除但子节点仍在——USN replay 的合法中间态。
                // 搜索路径在 path_for() 中会跳过这些节点，不影响功能。
                continue;
            }
            if parent.flags & FLAG_DIRECTORY == 0 {
                return Err(format!("cache parent is not a directory: {record}"));
            }
            if slot.name_off == NO_NAME {
                if slot.flags & FLAG_EXCLUDED == 0 || slot.flags & FLAG_DIRECTORY != 0 {
                    return Err(format!("cache node has no name: {record}"));
                }
                continue;
            }
            self.name_at(slot.name_off)?;
            // M1（复审 2026-08-21）：祖先链上**任意**墓碑都是 USN 重放的合法
            // 中间态（目录删除记录先于后代删除记录到达/落盘）。直接父墓碑
            // 上方已容忍；隔代墓碑曾走 path_for 报错——save 侧只抽 1% 节点，
            // 该状态可被写进缓存，load 侧全量校验却拒绝它，缓存从此永久
            // 不可装载（每次重启全量重建）。path_for 的其余错误（记录越界/
            // 父环/超深）仍按损坏硬拒。
            match self.path_for(record as u32) {
                Ok(_) => {}
                Err(reason) if reason.starts_with("missing parent record") => continue,
                Err(reason) => return Err(reason),
            }
        }
        Ok(())
    }

    /// S1（FRESH-AUDIT-2026-08-19）: 是否到达压缩阈值——只读谓词，
    /// 供锁外维护路径在 clone 前判定（压缩本体在 compact_names_if_needed）。
    pub fn needs_name_compact(&self) -> bool {
        let threshold = (8 * 1024 * 1024usize).max(self.initial_name_bytes / 4);
        // Fast path: use the dead-name counter accumulated by delete/upsert
        // instead of scanning every node on every USN batch.  The counter is
        // serde-skipped (starts at 0 after a cache load), so add a fallback:
        // if the pool has grown far past its initial size the counter may be
        // underreporting, and a full scan is the safe thing to do.
        self.dead_name_bytes > threshold
            || self.names.len()
                > self
                    .initial_name_bytes
                    .saturating_add(threshold.saturating_mul(2))
    }

    /// S1: 测试探针——强制到达压缩阈值 / 读回死字节计数。
    /// cfg(test)：仅测试构建存在，避免非测试构建的 dead_code 告警。
    #[cfg(test)]
    pub(crate) fn force_name_compact_threshold_for_test(&mut self) {
        self.dead_name_bytes = usize::MAX;
    }

    /// B3（AUDIT-4 批次C，2026-08-21）：names/nodes 内容哈希，供 v5 envelope
    /// 校验和。names 池内容经 `names_fingerprint`（F3，载入侧整算）纳入；
    /// 节点表逐槽哈希 (record, parent, name_off, sequence, flags)。NodeSlot
    /// 字段增删时同步 review 本函数——漏字段=校验盲区。
    pub fn content_hash(&self) -> u64 {
        fn fnv(hash: &mut u64, value: u64) {
            for byte in value.to_le_bytes() {
                *hash ^= u64::from(byte);
                *hash = hash.wrapping_mul(0x100000001b3);
            }
        }
        let mut hash: u64 = 0xcbf29ce484222325;
        fnv(&mut hash, self.names_fingerprint);
        fnv(&mut hash, self.nodes.len() as u64);
        for (record, slot) in self.nodes.iter().enumerate() {
            fnv(&mut hash, record as u64);
            fnv(&mut hash, u64::from(slot.parent_record));
            fnv(&mut hash, u64::from(slot.name_off));
            fnv(&mut hash, u64::from(slot.sequence));
            fnv(&mut hash, u64::from(slot.flags));
        }
        hash
    }

    #[cfg(test)]
    pub(crate) fn dead_name_bytes_for_test(&self) -> usize {
        self.dead_name_bytes
    }

    pub fn compact_names_if_needed(&mut self) -> Result<bool, String> {
        if !self.needs_name_compact() {
            return Ok(false);
        }
        let mut live_bytes: usize = 0;
        let mut replacement = Vec::with_capacity(self.names.len());
        for slot in &mut self.nodes {
            if slot.flags & FLAG_PRESENT == 0 || slot.name_off == NO_NAME {
                continue;
            }
            let start = slot.name_off as usize;
            // 与上方预检同样的防御：越界的 name_off 返回 Err 走压缩失败 →
            // watcher 报错 → 重建的安全路径，而不是直接切片 panic
            // （release 是 panic=abort，整进程静默消失）。
            let name_bytes = self
                .names
                .get(start..)
                .ok_or_else(|| format!("name offset {start} out of range"))?;
            let end = name_bytes
                .iter()
                .position(|byte| *byte == 0)
                .map(|offset| start + offset)
                .ok_or_else(|| "unterminated name pool entry".to_string())?;
            let offset = u32::try_from(replacement.len()).map_err(|_| "name pool exceeds u32")?;
            replacement.extend_from_slice(&self.names[start..end]);
            replacement.push(0);
            live_bytes = live_bytes.saturating_add(end - start + 1);
            slot.name_off = offset;
        }
        replacement.shrink_to_fit();
        self.names = replacement;
        self.dead_name_bytes = 0;
        // F3: 池被重写，指纹随之整算（identity 会变化——与旧的全池哈希行为一致，
        // 压缩本就改变池内容，sidecar 失配走既有重建路径）。
        self.recompute_derived_counters();
        // AUDIT-2026-08-18 R-C3: 槽表此前只增不减（上限 16M × 12B = 192MB/卷）。
        // 记录号是外部键、不能重映射（P4），只能在压缩时回收尾部连续的
        // tombstone/从未使用槽。尾部界必须覆盖所有在位节点自身的记录号与其
        // parent_record——缓存校验器把"父记录越界"当硬错误（见 validate），
        // 被引用的父槽即使是 tombstone 也得留在表内。
        let mut live_bound = 0usize;
        for (index, slot) in self.nodes.iter().enumerate() {
            if slot.flags & FLAG_PRESENT != 0 {
                live_bound = live_bound
                    .max(index + 1)
                    .max(slot.parent_record as usize + 1);
            }
        }
        if live_bound < self.nodes.len() {
            self.nodes.truncate(live_bound);
            self.nodes.shrink_to_fit();
        }
        // initial_name_bytes 基线随池重写由 recompute_derived_counters 恢复
        // （复审 M 2026-08-21：names.len() 即压缩后基线）。
        Ok(true)
    }

    pub(crate) fn snapshot_mutations(
        &self,
        frns: impl IntoIterator<Item = u64>,
    ) -> Result<MutationSnapshot, String> {
        let mut slots = Vec::new();
        // 去重用 HashSet：一批 USN 记录数千条，原先的线性扫描是 O(n²)
        // （且发生在 index 写锁内）。
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for frn in frns {
            let (record, _) = Self::split_frn(frn)?;
            if record as usize >= self.nodes.len() || !seen.insert(record) {
                continue;
            }
            slots.push((record, self.nodes[record as usize]));
        }
        Ok(MutationSnapshot {
            nodes_len: self.nodes.len(),
            names_len: self.names.len(),
            slots,
            names_fingerprint: self.names_fingerprint,
            dead_name_bytes: self.dead_name_bytes,
            present_slots: self.present_slots,
        })
    }

    pub(crate) fn rollback_mutations(&mut self, snapshot: MutationSnapshot) {
        self.nodes.truncate(snapshot.nodes_len);
        for (record, slot) in snapshot.slots {
            self.nodes[record as usize] = slot;
        }
        self.names.truncate(snapshot.names_len);
        self.names_fingerprint = snapshot.names_fingerprint;
        self.dead_name_bytes = snapshot.dead_name_bytes;
        self.present_slots = snapshot.present_slots;
    }

    fn ensure_slot(&mut self, record: u32) -> Result<(), VolumeError> {
        let required = record as usize + 1;
        if required > MAX_RECORD_NUMBER {
            return Err(VolumeError::RecordLimit(record));
        }
        if required > self.nodes.len() {
            // G4: 稀疏度防线改读维护计数（此前每次增长全表扫描计数）。
            // 阈值与 prepare_initial_capacity 同为 4096×（理由见彼处注释）。
            let present = self.present_slots.max(1);
            if required > 65_536 && required > present.saturating_mul(4096) {
                return Err(VolumeError::SparseSlots { required, present });
            }
            self.nodes.resize(required, NodeSlot::default());
        }
        Ok(())
    }

    fn append_name(&mut self, name: &str) -> Result<u32, VolumeError> {
        if name.contains('\0') {
            return Err(VolumeError::NameNul);
        }
        let offset = u32::try_from(self.names.len()).map_err(|_| VolumeError::NamePoolOverflow)?;
        self.names.extend_from_slice(name.as_bytes());
        self.names.push(0);
        // F3: 滚入追加字节（含终止符）——与全池整算在相同追加顺序下结果一致。
        self.names_fingerprint = fold_names_bytes(self.names_fingerprint, name.as_bytes());
        self.names_fingerprint = fold_names_bytes(self.names_fingerprint, &[0]);
        Ok(offset)
    }

    /// F3+G4: 重算派生计数（names 指纹 + 在位槽位数 + 名字池基线）。v5 缓存
    /// 载入后调用（三者都是 serde skip 字段），名字池压缩后调用（池与槽表都被
    /// 重写）。整算与增量滚入在相同字节序列下等值。
    /// 复审 M（2026-08-21 全仓重审）：initial_name_bytes 也在此恢复——它是
    /// serde skip，载入后若保持 0，needs_name_compact 的 fallback 阈值退化为
    /// 常量 16MB，names 池超 16MB 的卷每次缓存命中启动后的第一个维护 tick 都
    /// 触发一次无谓的全卷压缩（clone + 全池重写，纯浪费）。载入/压缩后的
    /// 当前池长就是新基线。
    pub fn recompute_derived_counters(&mut self) {
        self.names_fingerprint = names_pool_fingerprint(&self.names);
        self.initial_name_bytes = self.names.len();
        self.present_slots = self
            .nodes
            .iter()
            .filter(|slot| slot.flags & FLAG_PRESENT != 0)
            .count();
    }

    pub(crate) fn name_at(&self, offset: u32) -> Result<&str, String> {
        if offset == NO_NAME {
            return Ok("");
        }
        let start = offset as usize;
        let tail = self
            .names
            .get(start..)
            .ok_or_else(|| format!("name offset {offset} is outside pool"))?;
        let length = tail
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| format!("name at {offset} is unterminated"))?;
        std::str::from_utf8(&tail[..length]).map_err(|error| error.to_string())
    }

    fn node_name_is(&self, record: u32, expected: &str) -> bool {
        self.nodes
            .get(record as usize)
            .filter(|slot| slot.flags & FLAG_PRESENT != 0 && slot.name_off != NO_NAME)
            .and_then(|slot| self.name_at(slot.name_off).ok())
            .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    }
}

impl IndexState {
    pub fn search(&self, query: &str, max: usize) -> SearchOutcome {
        self.search_with_exclusions(query, max, &[])
    }

    pub fn search_with_exclusions(
        &self,
        query: &str,
        max: usize,
        exclusion_paths: &[String],
    ) -> SearchOutcome {
        search_volumes(
            &self.volumes,
            query,
            max,
            exclusion_paths,
            None,
            &QueryFilters::none(),
        )
    }

    /// Same ranking as [`Self::search_with_exclusions`], but candidates outside `root`
    /// are dropped *before* the global Top-K heap, so truncation and ordering describe
    /// the root-scoped result set only. `None` means an unrestricted global search.
    pub fn search_in_root(
        &self,
        query: &str,
        max: usize,
        exclusion_paths: &[String],
        root: Option<RootBound>,
    ) -> SearchOutcome {
        self.search_in_root_filtered(query, max, exclusion_paths, root, &QueryFilters::none())
    }

    /// G7: ext/path filter-aware variant. Filters are applied **before** the Top-K heap
    /// so truncation and ordering describe the filtered result set only.
    pub fn search_in_root_filtered(
        &self,
        query: &str,
        max: usize,
        exclusion_paths: &[String],
        root: Option<RootBound>,
        filters: &QueryFilters,
    ) -> SearchOutcome {
        search_volumes(&self.volumes, query, max, exclusion_paths, root, filters)
    }

    pub fn memory_bytes(&self) -> usize {
        self.volumes.iter().map(VolumeIndex::memory_bytes).sum()
    }
}

fn is_excluded_name(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "$Recycle.Bin",
        "System Volume Information",
        "WinSxS",
        "node_modules",
        ".git",
        ".svn",
        "__pycache__",
    ];
    NAMES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn match_metadata(name: &str, terms: &NameTerms) -> Option<MatchMetadata> {
    // S4（PRISM-IMPL-PLAN-4-2026-08-20）：单 term（含空查询）与旧的单一子串
    // 匹配逐字节等价——这是主要安全网，绝大多数字面命中走这条快路径。
    if let Some(single) = terms.single() {
        return match_metadata_single(name, single);
    }
    // 多 term AND：每个 term 都是名字的子串才命中。多 term 不产生 class 0
    //（整名精确对多 term 无定义）；「任一 term 在位置 0」→ class 1，否则 2。
    // position = 各 term 命中位置（UTF-16 单元）的最小值。
    let mut position = u32::MAX;
    let mut at_start = false;
    for term in terms.iter() {
        let byte_position = find_case_insensitive(name, term)?;
        at_start |= byte_position == 0;
        let utf16_position = name[..byte_position].encode_utf16().count() as u32;
        position = position.min(utf16_position);
    }
    Some(MatchMetadata {
        kind: MatchKind::Literal,
        class: if at_start { 1 } else { 2 },
        position,
        score: name.encode_utf16().count() as u32,
        history_score: 0,
    })
}

/// S4 之前的旧实现，单 term 语义原样保留（见 match_metadata 注释）。
fn match_metadata_single(name: &str, query_lower: &str) -> Option<MatchMetadata> {
    let byte_position = find_case_insensitive(name, query_lower)?;
    // N1（FRESH-AUDIT-2026-08-19）: 此前非 ASCII 名字每次比较都 to_lowercase()
    // 分配一个完整副本——中文库每次击键百万次堆分配。现在：
    // - class-0（全名精确）只在字节数相同时才做忽略大小写比较（name_eq_ignore_case
    //   内含零分配快路径），扫描成本 O(1) 比较而非 O(n) 分配；
    // - position 直接在原串上数 UTF-16 单元——UI 高亮按原名字符偏移对齐，长度会
    //   变化的大小写折叠（İ→i̇ 等）下比旧的 lowered 串偏移更准。
    let class = if name.len() == query_lower.len() && name_eq_ignore_case(name, query_lower) {
        0
    } else if byte_position == 0 {
        1
    } else {
        2
    };
    let position = name[..byte_position].encode_utf16().count() as u32;
    Some(MatchMetadata {
        kind: MatchKind::Literal,
        class,
        position,
        score: name.encode_utf16().count() as u32,
        history_score: 0,
    })
}

/// S4（PRISM-IMPL-PLAN-4-2026-08-20）：名字查询的空白 AND 分词。
/// 查询按空白切分、丢弃空串、逐个降幂；命中 = 每个 term 都是名字的子串
///（AND，顺序无关）。空查询映射为单个空 term（与旧的空串行为逐字节一致，
/// G7 的「仅 ext:/path: 过滤」路径依赖它匹配一切）。所有名字匹配点
///（索引器字面扫描 / broker 字面回退 / 应用清单 / 拼音去重口径）共用本类型，
/// 口径不一致会导致同一条结果以字面与拼音双出行或漏行。
#[derive(Debug, Clone)]
pub struct NameTerms {
    terms: Vec<String>,
}

impl NameTerms {
    pub fn parse(query: &str) -> Self {
        // H1（复审 2026-08-21）：多 term AND 的匹配成本 = 每 term 一次子串扫描，
        // 索引器管道对 Authenticated Users 开放——不去重不封顶时，
        //「a a a …」×1MB 查询可达 10^12 级字节比较（CPU 耗尽 LocalSystem 服务）。
        // 去重不改变 AND 语义（a AND a = a）；封顶 16 term 是诚实降级：
        // 超长分词列表的搜索意图本就模糊，匹配前 16 个已覆盖真实输入。
        const MAX_TERMS: usize = 16;
        let mut terms: Vec<String> = Vec::new();
        for term in query.split_whitespace() {
            let lowered = term.to_lowercase();
            if !terms.contains(&lowered) {
                terms.push(lowered);
                if terms.len() >= MAX_TERMS {
                    break;
                }
            }
        }
        if terms.is_empty() {
            terms.push(String::new());
        }
        Self { terms }
    }

    /// 单 term（含空 term）时返回它——调用方走与旧行为逐字节等价的快路径。
    pub fn single(&self) -> Option<&str> {
        match self.terms.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.terms.iter()
    }
}

/// 大小写不敏感子串查找，返回**原名字节偏移**（字符边界安全）。
/// N1（FRESH-AUDIT-2026-08-19）三条路径全部避免对名字做 to_lowercase() 分配：
/// 1. 纯 ASCII 查询：字节级 windows 扫描。对任意（含非 ASCII）名字都安全——
///    UTF-8 自同步：多字节序列的每个字节都 >= 0x80，ASCII 查询窗口（比较前
///    to_ascii_lowercase 只影响 a-z）既不会落进序列中间也不会跨序列匹配，
///    命中位置必为 ASCII 字符边界。
/// 2. 查询含非 ASCII 且名字无大写字符（典型中文名）：小写化是恒等变换，
///    直接 find 零分配。
/// 3. 名字含大写字符且查询非 ASCII：回落 to_lowercase()（与旧行为一致），
///    并把 lowered 偏移映射回原串（长度变化的折叠如 İ→i̇ 才会走映射分支）。
pub(crate) fn find_case_insensitive(name: &str, query_lower: &str) -> Option<usize> {
    if query_lower.is_empty() {
        // G7: an empty name query matches every candidate (used when ext:/path:
        // filters are the only criteria). Position 0 means "match at start".
        return Some(0);
    }
    if query_lower.is_ascii() {
        name.as_bytes()
            .windows(query_lower.len())
            .position(|window| {
                window
                    .iter()
                    .zip(query_lower.as_bytes())
                    .all(|(left, right)| left.to_ascii_lowercase() == *right)
            })
    } else if !name.chars().any(char::is_uppercase) {
        name.find(query_lower)
    } else {
        let lowered = name.to_lowercase();
        let hit = lowered.find(query_lower)?;
        if lowered.len() == name.len() {
            Some(hit)
        } else {
            Some(map_lowered_offset(name, hit))
        }
    }
}

/// 把 to_lowercase() 结果串里的字节偏移映射回原串字节偏移。
/// 只有长度会变化的大小写折叠（İ、ẞ 等罕见字符）才会走到这里——常规大写字母
/// 长度不变，调用方已用 `lowered.len() == name.len()` 短路掉恒等情形。
fn map_lowered_offset(name: &str, lowered_offset: usize) -> usize {
    let mut lowered_pos = 0usize;
    for (idx, ch) in name.char_indices() {
        if lowered_pos >= lowered_offset {
            return idx;
        }
        lowered_pos += ch.to_lowercase().map(|c| c.len_utf8()).sum::<usize>();
    }
    name.len()
}

/// Case-insensitive whole-name comparison that also works for non-ASCII names.
pub(crate) fn name_eq_ignore_case(left: &str, right: &str) -> bool {
    if left.is_ascii() && right.is_ascii() {
        left.eq_ignore_ascii_case(right)
    } else if !left.chars().any(char::is_uppercase) && !right.chars().any(char::is_uppercase) {
        // N1: 双方都无大写字符（典型中文/纯数字名）——小写化是恒等变换，直接比较。
        left == right
    } else {
        left.to_lowercase() == right.to_lowercase()
    }
}

/// G7: per-request ext:/path: filter set applied before the Top-K heap.
///
/// `exts` are normalized (leading dot stripped, lowercased); a candidate passes when its
/// file extension matches any entry (OR). `paths` are case-insensitive substrings matched
/// against the candidate's full path; all must match (AND). Both checks happen after name
/// matching but before the bounded heap, so `is_truncated` describes the filtered set only.
#[derive(Debug, Default, Clone)]
pub struct QueryFilters {
    exts: Vec<String>,
    paths: Vec<String>,
}

impl QueryFilters {
    pub fn new(exts: Vec<String>, paths: Vec<String>) -> Self {
        // N1: path needle 在构造时降幂一次（每请求一次），热路径比较不再
        // 对每个候选路径反复 to_lowercase()。
        let paths = paths
            .into_iter()
            .map(|needle| needle.to_lowercase())
            .collect();
        Self { exts, paths }
    }

    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.exts.is_empty() && self.paths.is_empty()
    }

    /// True when at least one `path:` filter is present (requires path construction).
    pub(crate) fn has_path_filter(&self) -> bool {
        !self.paths.is_empty()
    }

    /// Low-cost extension check (no path construction needed).
    /// Directories never have an extension in this model.
    pub(crate) fn ext_matches(&self, name: &str, is_directory: bool) -> bool {
        if self.exts.is_empty() {
            return true;
        }
        if is_directory {
            return false;
        }
        let ext = file_extension(name);
        ext.is_some_and(|e| {
            self.exts
                .iter()
                .any(|target| target.eq_ignore_ascii_case(e))
        })
    }

    /// High-cost path check: requires a constructed full path. All path substrings must
    /// match (AND), case-insensitive. N1: needle 已在构造时降幂；路径比较走
    /// find_case_insensitive 的零分配路径，只有"非 ASCII needle + 路径含大写"的
    /// 罕见组合才对路径整体降幂。
    pub(crate) fn path_matches(&self, path: &str) -> bool {
        if self.paths.is_empty() {
            return true;
        }
        self.paths.iter().all(|needle| {
            if needle.is_ascii() || !path.chars().any(char::is_uppercase) {
                find_case_insensitive(path, needle).is_some()
            } else {
                path.to_lowercase().contains(needle.as_str())
            }
        })
    }
}

/// Extracts the extension from a file name: the portion after the last `.`.
/// `file.txt` → `txt`; `archive.tar.gz` → `gz`; `noext` → None; `.gitignore` → `gitignore`.
fn file_extension(name: &str) -> Option<&str> {
    let last_dot = name.rfind('.')?;
    // A leading dot with nothing after it (e.g. ".") has no extension.
    if last_dot + 1 >= name.len() {
        return None;
    }
    Some(&name[last_dot + 1..])
}

/// N2（FRESH-AUDIT-2026-08-19）: 触发并行扫描的最小总槽位数。
/// 低于此值起线程的开销超过收益（本机 1.2M 记录远超此阈值）。
const PARALLEL_SCAN_MIN_SLOTS: usize = 512 * 1024;

/// N2: 每个扫描块的槽位上限。256K 足够摊薄任务粒度，又不至于让单块过长。
const SCAN_CHUNK_SLOTS: usize = 256 * 1024;

/// N2: 单次搜索的并行度封顶。tokio blocking 池默认 512 线程 = 64 连接上限 × 8，
/// 恰好贴边——这里封 8 并给文档注明组合约束（broker 实际只有 1 条长连接）。
const SCAN_MAX_THREADS: usize = 8;

fn search_volumes(
    volumes: &[VolumeIndex],
    query: &str,
    max: usize,
    exclusion_paths: &[String],
    root: Option<RootBound>,
    filters: &QueryFilters,
) -> SearchOutcome {
    search_volumes_impl(
        volumes,
        query,
        max,
        exclusion_paths,
        root,
        filters,
        PARALLEL_SCAN_MIN_SLOTS,
    )
}

/// 单块扫描的累积器（N2：串行/并行路径共用一套判定逻辑）。
struct ScanAccumulator<'a> {
    heap: BinaryHeap<RankedCandidate<'a>>,
    scanned_nodes: u64,
    name_candidates: u64,
    matched_count: u64,
    /// 诊断计数：串行与并行路径的计数语义不同（并行按块局部堆计数），
    /// 只用于粗粒度观测，不参与等价性断言。
    entered_top_k: u64,
    path_constructions: u64,
}

impl<'a> ScanAccumulator<'a> {
    fn new(max: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(max),
            scanned_nodes: 0,
            name_candidates: 0,
            matched_count: 0,
            entered_top_k: 0,
            path_constructions: 0,
        }
    }

    /// 归并另一个累积器：候选按与串行路径完全相同的堆规则进入全局堆，
    /// 因此全局 Top-K 与串行结果逐元素一致（某元素在全局 Top-K 内 ⟹ 它在其
    /// 所在块自己的 Top-K 内，归并不丢候选）。
    fn merge_from(&mut self, other: ScanAccumulator<'a>, max: usize) {
        for candidate in other.heap {
            if self.heap.len() < max {
                self.heap.push(candidate);
                self.entered_top_k = self.entered_top_k.saturating_add(1);
            } else if self.heap.peek().is_some_and(|worst| candidate < *worst) {
                self.heap.pop();
                self.heap.push(candidate);
                self.entered_top_k = self.entered_top_k.saturating_add(1);
            }
        }
        self.scanned_nodes = self.scanned_nodes.saturating_add(other.scanned_nodes);
        self.name_candidates = self.name_candidates.saturating_add(other.name_candidates);
        self.matched_count = self.matched_count.saturating_add(other.matched_count);
        self.entered_top_k = self.entered_top_k.saturating_add(other.entered_top_k);
        self.path_constructions = self
            .path_constructions
            .saturating_add(other.path_constructions);
    }
}

/// 扫描单个卷的一段连续槽位（N2 抽出，串行/并行共用，判定逻辑与抽出前逐行一致）。
/// 参数多是刻意的：全部是热路径的直接输入，包一层上下文结构体只增加间接层。
#[allow(clippy::too_many_arguments)]
fn scan_slot_range<'a>(
    volumes: &'a [VolumeIndex],
    volume_index: usize,
    range: std::ops::Range<usize>,
    terms: &NameTerms,
    exclusions: &[NormalizedExclusion],
    root_filter: &mut Option<RootFilter>,
    filters: &QueryFilters,
    has_path_filter: bool,
    max: usize,
    acc: &mut ScanAccumulator<'a>,
) {
    let volume = &volumes[volume_index];
    for record in range {
        let Some(slot) = volume.nodes.get(record) else {
            continue;
        };
        acc.scanned_nodes = acc.scanned_nodes.saturating_add(1);
        if slot.flags & (FLAG_PRESENT | FLAG_EXCLUDED) != FLAG_PRESENT || slot.name_off == NO_NAME {
            continue;
        }
        acc.name_candidates = acc.name_candidates.saturating_add(1);
        let Ok(name) = volume.name_at(slot.name_off) else {
            continue;
        };
        let Some(metadata) = match_metadata(name, terms) else {
            continue;
        };
        if root_filter
            .as_mut()
            .is_some_and(|filter| !filter.accepts(volume_index, volume, record as u32))
        {
            continue;
        }
        if exclusions
            .iter()
            .any(|exclusion| exclusion.matches(volume, record as u32))
        {
            continue;
        }
        // G7: ext filter — low cost, checks the name only. Applied before path
        // construction and before the Top-K heap.
        let is_directory = slot.flags & FLAG_DIRECTORY != 0;
        if !filters.ext_matches(name, is_directory) {
            continue;
        }
        // G7: path filter — high cost, needs a constructed full path. Only run
        // when a path filter is present; still before the Top-K heap so
        // `is_truncated` describes the filtered set only.
        if has_path_filter {
            acc.path_constructions = acc.path_constructions.saturating_add(1);
            let path = match volume.path_for(record as u32) {
                Ok(path) => path,
                Err(_) => continue,
            };
            if !filters.path_matches(&path) {
                continue;
            }
        }
        acc.matched_count = acc.matched_count.saturating_add(1);
        let candidate = RankedCandidate {
            volume_index,
            mount_path: &volume.mount_path,
            record: record as u32,
            name,
            is_directory,
            metadata,
        };
        if acc.heap.len() < max {
            acc.heap.push(candidate);
            acc.entered_top_k = acc.entered_top_k.saturating_add(1);
        } else if acc.heap.peek().is_some_and(|worst| candidate < *worst) {
            acc.heap.pop();
            acc.heap.push(candidate);
            acc.entered_top_k = acc.entered_top_k.saturating_add(1);
        }
    }
}

/// `search_volumes` 的可注入阈值版本（N2 等价性测试用 threshold=0 强制并行 /
/// usize::MAX 强制串行做逐字节比对）。
fn search_volumes_impl(
    volumes: &[VolumeIndex],
    query: &str,
    max: usize,
    exclusion_paths: &[String],
    root: Option<RootBound>,
    filters: &QueryFilters,
    parallel_threshold: usize,
) -> SearchOutcome {
    // G7: an empty name query normally means "no search", but when ext:/path:
    // filters are present the empty name matches every candidate (match_metadata
    // returns Some for empty query), so we must not short-circuit.
    // P1（第一轮 bug 修复）：空查询 + root 同理 = 浏览该目录（路径查询分支）。
    if (query.is_empty() && filters.is_empty() && root.is_none()) || max == 0 {
        return SearchOutcome {
            items: Vec::new(),
            is_truncated: false,
            scanned_nodes: 0,
            name_candidates: 0,
            matched_count: 0,
            entered_top_k: 0,
            path_constructions: 0,
        };
    }

    // S4：空白 AND 分词一次，串行/并行共用；单 term 与旧的单一降幂子串等价。
    let terms = NameTerms::parse(query);
    let exclusions: Vec<_> = exclusion_paths
        .iter()
        .filter_map(|path| NormalizedExclusion::parse(path))
        .collect();
    let has_path_filter = filters.has_path_filter();

    let total_slots: usize = volumes.iter().map(|volume| volume.nodes.len()).sum();
    let mut acc = if total_slots >= parallel_threshold {
        parallel_scan(
            volumes,
            &terms,
            &exclusions,
            root,
            filters,
            has_path_filter,
            max,
        )
    } else {
        let mut acc = ScanAccumulator::new(max);
        let mut root_filter = root.map(RootFilter::new);
        for volume_index in 0..volumes.len() {
            let len = volumes[volume_index].nodes.len();
            scan_slot_range(
                volumes,
                volume_index,
                0..len,
                &terms,
                &exclusions,
                &mut root_filter,
                filters,
                has_path_filter,
                max,
                &mut acc,
            );
        }
        acc
    };

    let ranked = acc.heap.into_sorted_vec();
    // Path construction for surviving candidates: counted separately from the path-filter
    // constructions above (which only ran when a path filter was present).
    acc.path_constructions = acc.path_constructions.saturating_add(ranked.len() as u64);
    let items = ranked
        .into_iter()
        .filter_map(|candidate| {
            let volume = &volumes[candidate.volume_index];
            volume.path_for(candidate.record).ok().map(|path| IndexHit {
                name: candidate.name.to_owned(),
                path,
                is_directory: candidate.is_directory,
                match_metadata: candidate.metadata,
            })
        })
        .collect();
    SearchOutcome {
        items,
        is_truncated: acc.matched_count > max as u64,
        scanned_nodes: acc.scanned_nodes,
        name_candidates: acc.name_candidates,
        matched_count: acc.matched_count,
        entered_top_k: acc.entered_top_k,
        path_constructions: acc.path_constructions,
    }
}

/// N2: 分块并行扫描。块 = 卷内连续 256K 槽（不跨卷——卷的挂载路径/父链独立）；
/// worker 从原子游标取块；每 worker 一份 RootFilter（memo 判定是绝对值，不依赖
/// 扫描顺序，分 worker 与单实例语义一致）；块内只读共享 `&[VolumeIndex]`，
/// 不新增锁。归并保持与串行完全相同的堆规则，全局 Top-K 与串行结果一致
/// （RankedCandidate::Ord 是全序，无并列歧义）。
fn parallel_scan<'a>(
    volumes: &'a [VolumeIndex],
    terms: &NameTerms,
    exclusions: &[NormalizedExclusion],
    root: Option<RootBound>,
    filters: &QueryFilters,
    has_path_filter: bool,
    max: usize,
) -> ScanAccumulator<'a> {
    let mut chunks: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
    for (volume_index, volume) in volumes.iter().enumerate() {
        let len = volume.nodes.len();
        let mut start = 0;
        while start < len {
            let end = (start + SCAN_CHUNK_SLOTS).min(len);
            chunks.push((volume_index, start..end));
            start = end;
        }
    }

    let worker_count = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .clamp(1, SCAN_MAX_THREADS)
        .min(chunks.len());
    if worker_count <= 1 {
        // 块太少或单核：就地串行，不起线程。
        let mut acc = ScanAccumulator::new(max);
        let mut root_filter = root.map(RootFilter::new);
        for (volume_index, range) in chunks {
            scan_slot_range(
                volumes,
                volume_index,
                range,
                terms,
                exclusions,
                &mut root_filter,
                filters,
                has_path_filter,
                max,
                &mut acc,
            );
        }
        return acc;
    }

    let next_chunk = std::sync::atomic::AtomicUsize::new(0);
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..worker_count)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = ScanAccumulator::new(max);
                    let mut root_filter = root.map(RootFilter::new);
                    loop {
                        let index = next_chunk.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if index >= chunks.len() {
                            break;
                        }
                        let (volume_index, range) = &chunks[index];
                        scan_slot_range(
                            volumes,
                            *volume_index,
                            range.clone(),
                            terms,
                            exclusions,
                            &mut root_filter,
                            filters,
                            has_path_filter,
                            max,
                            &mut local,
                        );
                    }
                    local
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(acc) => acc,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect::<Vec<_>>()
    });

    let mut global = ScanAccumulator::new(max);
    for local in results {
        global.merge_from(local, max);
    }
    global
}

struct NormalizedExclusion {
    root: String,
    components: Vec<String>,
}

pub(crate) struct ExclusionMatcher(Vec<NormalizedExclusion>);

impl ExclusionMatcher {
    pub(crate) fn new(paths: &[String]) -> Self {
        Self(
            paths
                .iter()
                .filter_map(|path| NormalizedExclusion::parse(path))
                .collect(),
        )
    }

    pub(crate) fn matches(&self, volume: &VolumeIndex, record: u32) -> bool {
        self.0
            .iter()
            .any(|exclusion| exclusion.matches(volume, record))
    }
}

impl NormalizedExclusion {
    fn parse(path: &str) -> Option<Self> {
        let normalized = path.trim().replace('/', "\\");
        let mut parts = normalized.split('\\').filter(|part| !part.is_empty());
        let root = parts.next()?.to_owned();
        Some(Self {
            root,
            components: parts.map(ToOwned::to_owned).collect(),
        })
    }

    fn matches(&self, volume: &VolumeIndex, record: u32) -> bool {
        let mount = volume.mount_path.trim_end_matches(['\\', '/']);
        if !mount.eq_ignore_ascii_case(&self.root) {
            return false;
        }
        let mut current = record;
        let mut names = Vec::new();
        for _ in 0..MAX_PATH_DEPTH {
            let Some(slot) = volume.nodes.get(current as usize) else {
                return false;
            };
            if current == volume.root_record {
                names.reverse();
                return self.components.len() <= names.len()
                    && self
                        .components
                        .iter()
                        .zip(names)
                        .all(|(expected, actual)| expected.eq_ignore_ascii_case(actual));
            }
            let Ok(name) = volume.name_at(slot.name_off) else {
                return false;
            };
            names.push(name);
            if slot.parent_record == current {
                return false;
            }
            current = slot.parent_record;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- N1（FRESH-AUDIT-2026-08-19）: 大小写折叠零分配路径的语义锚定 ----------

    /// 纯 ASCII 查询必须能命中任意（含中文）名字，位置是原名字节偏移。
    /// 注意契约：query 参数必须已降幂（"ABC" 是调用方错误，此处只验降幂后的 "abc"）。
    #[test]
    fn n1_ascii_query_matches_non_ascii_name_at_original_offset() {
        let name = "微信ABC文档.txt";
        assert_eq!(find_case_insensitive(name, "abc"), Some("微信".len()));
        assert_eq!(
            find_case_insensitive(name, "txt"),
            Some("微信ABC文档.".len())
        );
        assert_eq!(find_case_insensitive(name, "zzz"), None);
    }

    /// 非 ASCII 查询 + 无大写字符的名字（典型中文）：直接命中，零分配路径。
    #[test]
    fn n1_non_ascii_query_matches_plain_name() {
        assert_eq!(
            find_case_insensitive("微信文档", "文档"),
            Some("微信".len())
        );
        assert_eq!(find_case_insensitive("微信文档", "工作"), None);
    }

    /// 非 ASCII 查询 + 含大写拉丁的名字：回落降幂路径，位置映射回原串。
    #[test]
    fn n1_non_ascii_query_with_uppercase_name_maps_offset_back() {
        let name = "下载X目录"; // 大写拉丁 X + 中文混合
        assert_eq!(find_case_insensitive(name, "目录"), Some("下载X".len()));
    }

    /// 长度会变化的大小写折叠（İ → i̇，2 字节变 3 字节）：lowered 偏移必须
    /// 正确映射回原串偏移，且切片不 panic（字符边界安全）。
    #[test]
    fn n1_length_changing_fold_maps_offset_back_to_original() {
        let name = "aİb文字";
        // lowered = "ai\u{307}b文字"（İ 展开为 i + 组合上点）
        let hit = find_case_insensitive(name, "文字").expect("中文子串必须命中");
        let sliced = &name[hit..];
        assert_eq!(sliced, "文字");
    }

    /// class-0/精确匹配与位置语义在混合大小写下保持：match_metadata 的
    /// position 是原名的 UTF-16 单元偏移（UI 高亮对齐）。
    #[test]
    fn n1_match_metadata_positions_are_original_name_offsets() {
        let meta = match_metadata("微信ABC文档", &NameTerms::parse("abc")).expect("必须命中");
        assert_eq!(meta.class, 2); // 非前缀也非全名
        assert_eq!(meta.position, 2); // "微信" = 2 个 UTF-16 单元
        let exact = match_metadata("微信", &NameTerms::parse("微信")).expect("全名必须命中");
        assert_eq!(exact.class, 0);
        let prefix = match_metadata("微信ABC", &NameTerms::parse("wx"))
            .or(match_metadata("微信ABC", &NameTerms::parse("微")));
        assert!(prefix.is_some());
        assert_eq!(prefix.unwrap().class, 1);
    }

    /// S4（PRISM-IMPL-PLAN-4-2026-08-20）：多 term AND 语义——「抖音 视频」
    /// 命中「抖音-短视频.mp4」（顺序无关），不命中只含其一的名字；多 term
    /// 不产生 class 0；position 取各命中 UTF-16 偏移最小值；尾随空格的查询
    /// 与去空格后同结果。
    /// 复审 H1（2026-08-21）：多 term 去重 + 封顶 16——索引器管道对 AU 开放，
    /// 「a a a …」×1MB 的查询曾是每名字 50 万次子串扫描的 CPU 耗尽面。
    #[test]
    fn h1_name_terms_dedup_and_cap() {
        // 去重：AND 语义不变（a AND a = a）。
        let terms = NameTerms::parse("a A b  a");
        assert_eq!(terms.iter().count(), 2);
        // 封顶 16：超出的 term 诚实降级（不参与 AND）。
        let long = (0..24)
            .map(|i| format!("t{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let capped = NameTerms::parse(&long);
        assert_eq!(capped.iter().count(), 16);
        assert!(capped.iter().all(|term| term.starts_with('t')));
        // 空白查询仍是单空 term（G7 依赖）。
        assert_eq!(NameTerms::parse("   ").single(), Some(""));
    }

    #[test]
    fn s4_multi_term_and_semantics() {
        let terms = NameTerms::parse("抖音 视频");
        assert_eq!(terms.single(), None);
        let hit = match_metadata("抖音-短视频.mp4", &terms).expect("两段都在名字里必须命中");
        assert_eq!(hit.class, 1, "「抖音」在位置 0 → class 1");
        assert_eq!(hit.position, 0);
        // term 顺序无关：命中位置取最小值。
        let reversed =
            match_metadata("视频-抖音.mp4", &NameTerms::parse("视频 抖音")).expect("顺序无关");
        assert_eq!(reversed.position, 0);
        // AND 不退化成 OR：只含一个 term 的名字不命中。
        assert!(match_metadata("抖音-别的.mp4", &terms).is_none());
        assert!(match_metadata("别的-视频.mp4", &terms).is_none());
        // 多 term 无 class 0（整名精确对多 term 无定义）。
        let mid = match_metadata("a抖音x视频y", &terms).expect("两段都在");
        assert_eq!(mid.class, 2);
        assert_eq!(mid.position, 1);
        // 尾随空格：与去空格后逐字节同结果。
        let trailing = NameTerms::parse("prism ");
        assert_eq!(trailing.single(), Some("prism"));
        assert!(match_metadata("prism.exe", &trailing).is_some());
        // 空查询 → 单个空 term → 匹配一切（G7 仅过滤路径依赖）。
        let empty = NameTerms::parse("");
        assert_eq!(empty.single(), Some(""));
        assert!(match_metadata("任意名字", &empty).is_some());
    }

    /// path 过滤：needle 预降幂后对含大写/纯中文路径的匹配语义不变。
    #[test]
    fn n1_path_filter_matches_with_pre_lowered_needles() {
        let program = QueryFilters::new(vec![], vec!["Program".into()]);
        assert!(program.path_matches("C:\\Program Files\\App"));
        assert!(program.path_matches("c:\\program files\\app"));
        assert!(!program.path_matches("D:\\Docs\\App"));

        // 非 ASCII needle：纯小写路径走零分配 find，含大写路径回落降幂。
        let data = QueryFilters::new(vec![], vec!["数据".into()]);
        assert!(data.path_matches("D:\\数据集\\app"));
        assert!(data.path_matches("D:\\数据集\\App"));
        assert!(!data.path_matches("D:\\docs"));

        // 多 needle 是 AND 语义。
        let both = QueryFilters::new(vec![], vec!["program".into(), "数据".into()]);
        assert!(both.path_matches("C:\\Program Files\\数据"));
        assert!(!both.path_matches("C:\\Program Files\\docs"));
    }

    // --- N2（FRESH-AUDIT-2026-08-19）: 并行扫描等价性与确定性 ------------------

    /// 构造大规模合成卷：一个目录 + 海量文件（含中文名/大小写/目录混合）。
    fn dense_volume(mount: &str, count: usize) -> VolumeIndex {
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: format!("vol-{mount}"),
                serial: 7,
            },
            format!("{mount}\\"),
            9,
            10,
            5,
        )
        .unwrap();
        volume
            .prepare_initial_capacity((count + 16) as u32, count)
            .unwrap();
        volume.upsert(frn(10, 1), frn(5, 0), "dir", true).unwrap();
        for i in 0..count {
            let record = 11 + i as u32;
            let name = match i % 4 {
                0 => format!("file-{i:06}.txt"),
                1 => format!("FILE-{i:06}.PDF"),
                2 => format!("文档-{i:06}"),
                _ => format!("Mixed{i:06}Dir"),
            };
            volume
                .upsert(frn(record, 1), frn(10, 1), &name, i % 16 == 15)
                .unwrap();
        }
        volume
    }

    /// 并行与串行结果逐元素一致（Top-K 归并的正确性锚定）。
    #[test]
    fn n2_parallel_scan_matches_serial_results() {
        let volumes = vec![dense_volume("C:", 30_000)];
        let filters = QueryFilters::none();
        for query in ["file", "文档", "mixed", "file-0000"] {
            let serial = search_volumes_impl(&volumes, query, 8, &[], None, &filters, usize::MAX);
            let parallel = search_volumes_impl(&volumes, query, 8, &[], None, &filters, 1);
            assert_eq!(parallel.items.len(), serial.items.len(), "query={query}");
            for (p, s) in parallel.items.iter().zip(serial.items.iter()) {
                assert_eq!(p.name, s.name, "query={query}");
                assert_eq!(p.path, s.path, "query={query}");
                assert_eq!(p.is_directory, s.is_directory, "query={query}");
                assert_eq!(p.match_metadata, s.match_metadata, "query={query}");
            }
            assert_eq!(parallel.is_truncated, serial.is_truncated);
            assert_eq!(parallel.scanned_nodes, serial.scanned_nodes);
            assert_eq!(parallel.name_candidates, serial.name_candidates);
            assert_eq!(parallel.matched_count, serial.matched_count);
            assert_eq!(parallel.path_constructions, serial.path_constructions);
        }
    }

    /// 多卷 + 块边界：小卷不足一块、大卷跨多块，结果仍与串行一致。
    #[test]
    fn n2_parallel_scan_chunk_boundaries_across_volumes() {
        let volumes = vec![
            dense_volume("C:", 40_000),
            dense_volume("D:", 100), // 不足一块
        ];
        let filters = QueryFilters::none();
        let serial = search_volumes_impl(&volumes, "file", 16, &[], None, &filters, usize::MAX);
        let parallel = search_volumes_impl(&volumes, "file", 16, &[], None, &filters, 1);
        assert_eq!(parallel.items.len(), serial.items.len());
        for (p, s) in parallel.items.iter().zip(serial.items.iter()) {
            assert_eq!(
                (p.name.clone(), p.path.clone()),
                (s.name.clone(), s.path.clone())
            );
        }
        assert_eq!(parallel.matched_count, serial.matched_count);
        assert_eq!(parallel.scanned_nodes, serial.scanned_nodes);
    }

    /// 并行路径确定性：同一输入两次运行结果完全相同（RankedCandidate 全序）。
    #[test]
    fn n2_parallel_scan_is_deterministic() {
        let volumes = vec![dense_volume("C:", 12_000)];
        let filters = QueryFilters::none();
        let first = search_volumes_impl(&volumes, "file", 8, &[], None, &filters, 1);
        let second = search_volumes_impl(&volumes, "file", 8, &[], None, &filters, 1);
        assert_eq!(first.items.len(), second.items.len());
        for (a, b) in first.items.iter().zip(second.items.iter()) {
            assert_eq!(
                (a.name.as_str(), a.path.as_str()),
                (b.name.as_str(), b.path.as_str())
            );
        }
    }

    /// 根范围过滤在并行路径下语义不变（每 worker 一份 RootFilter，memo 判定绝对）。
    #[test]
    fn n2_parallel_scan_preserves_root_filter_semantics() {
        let mut volumes = vec![dense_volume("C:", 8_000)];
        // 把 100..200 号记录搬到 record 2000 的子目录下，构造第二棵子树。
        volumes[0]
            .upsert(frn(2000, 1), frn(10, 1), "sub", true)
            .unwrap();
        for i in 100..200u32 {
            volumes[0]
                .upsert(
                    frn(3000 + i, 1),
                    frn(2000, 1),
                    &format!("file-{i:06}.txt"),
                    false,
                )
                .unwrap();
        }
        let root = Some(RootBound {
            volume_index: 0,
            root_record: frn(2000, 1) as u32,
        });
        let filters = QueryFilters::none();
        let serial = search_volumes_impl(&volumes, "file", 32, &[], root, &filters, usize::MAX);
        let parallel = search_volumes_impl(&volumes, "file", 32, &[], root, &filters, 1);
        assert!(!serial.items.is_empty());
        assert_eq!(serial.matched_count, parallel.matched_count);
        assert_eq!(serial.items.len(), parallel.items.len());
        for (s, p) in serial.items.iter().zip(parallel.items.iter()) {
            assert_eq!(s.path, p.path);
            assert!(
                p.path.ends_with("sub\\") || p.path.contains("sub\\"),
                "{}",
                p.path
            );
        }
    }

    /// M1: 预留名字池容量后，写入不超过预留量的名字不得触发再扩容
    /// （capacity 在 upsert 前后保持不变——倍增尖峰被消除的直接锚定）。
    #[test]
    fn m1_reserved_names_capacity_absorbs_upserts_without_regrowth() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "dir", true).unwrap();
        let estimate = 64 * 1024;
        volume.reserve_names_capacity(estimate);
        let capacity_after_reserve = volume.names.capacity();
        assert!(capacity_after_reserve >= estimate);

        let mut written = 0usize;
        let mut record = 11u32;
        while written + 32 < estimate {
            let name = format!("f-{record:07}.dat");
            volume
                .upsert(frn(record, 1), frn(10, 1), &name, false)
                .unwrap();
            written += name.len() + 1;
            record += 1;
        }

        assert_eq!(
            volume.names.capacity(),
            capacity_after_reserve,
            "预留量内的写入不得触发再扩容"
        );
        assert!(volume.names.len() > estimate / 2, "布景应确实写入大量名字");
    }

    /// N2 实测基准（手动跑）：单卷 1.2M 记录的串行 vs 并行全扫延迟。
    /// 运行：cargo test --lib n2_scan_benchmark -- --ignored --nocapture
    #[test]
    #[ignore = "基准测试：约需十几秒构建合成卷，仅在需要实测数据时手动运行"]
    fn n2_scan_benchmark() {
        let volumes = vec![dense_volume("C:", 1_200_000)];
        let filters = QueryFilters::none();
        for query in ["file", "文档"] {
            let serial_start = std::time::Instant::now();
            let serial = search_volumes_impl(&volumes, query, 8, &[], None, &filters, usize::MAX);
            let serial_ms = serial_start.elapsed().as_millis();
            let parallel_start = std::time::Instant::now();
            let parallel = search_volumes_impl(&volumes, query, 8, &[], None, &filters, 1);
            let parallel_ms = parallel_start.elapsed().as_millis();
            assert_eq!(parallel.items.len(), serial.items.len());
            println!(
                "n2 benchmark query={query} serial={serial_ms}ms parallel={parallel_ms}ms matched={}",
                serial.matched_count
            );
        }
    }

    fn volume() -> VolumeIndex {
        VolumeIndex::new(
            VolumeId {
                guid: "volume".into(),
                serial: 7,
            },
            "C:\\".into(),
            9,
            10,
            5,
        )
        .unwrap()
    }

    fn frn(record: u32, sequence: u16) -> u64 {
        (u64::from(sequence) << 48) | u64::from(record)
    }

    #[test]
    fn node_slot_is_twelve_bytes() {
        assert_eq!(std::mem::size_of::<NodeSlot>(), 12);
    }

    #[test]
    fn initial_capacity_uses_complete_mft_density() {
        let mut volume = volume();
        volume.prepare_initial_capacity(70_000, 3_000).unwrap();
        assert_eq!(volume.nodes.len(), 70_001);
        assert!(volume.prepare_initial_capacity(100_000, 1).is_err());
    }

    #[test]
    fn initial_capacity_supports_large_modern_volumes_up_to_the_cap() {
        // 现代系统盘常见 2~8M 条 MFT 记录：3M 记录 + 足量活跃文件必须可索引。
        let mut volume = volume();
        volume.prepare_initial_capacity(3_000_000, 150_000).unwrap();
        assert_eq!(volume.nodes.len(), 3_000_001);
        // 超过 16,777,216 上限仍拒绝（内存护栏）。
        assert!(volume
            .prepare_initial_capacity(16_777_216, 16_777_216)
            .is_err());
    }

    /// 全量审查（2026-08-22）：NTFS 大批删除后槽表不缩、活跃数缩——
    /// 冲上 2M 文件再清理到 60k 的合法数据卷（33× 密度）必须可索引；
    /// 旧的 32× 阈值会把它判成病态稀疏，build 确定性失败、重试永久空转。
    /// 粗坏解析（16M 级 FRN 配 1k 活跃，16000×）仍要拒绝。
    #[test]
    fn initial_capacity_accepts_legitimately_shrunken_volumes() {
        let mut volume = volume();
        volume.prepare_initial_capacity(2_000_000, 60_000).unwrap();
        assert!(volume.prepare_initial_capacity(16_000_000, 1_000).is_err());
    }

    /// AUDIT-2026-08-18 R-C3: 高记录号节点删除后，names 压缩必须同步回收
    /// 尾部 tombstone 槽（nodes.len 回落、容量收缩）；被在位节点引用为父的
    /// 槽即使在高位也必须保留。
    #[test]
    fn compact_names_reclaims_trailing_node_slots() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "dir", true).unwrap();
        // 高位父目录 + 更高位的子文件。
        volume
            .upsert(frn(900, 1), frn(10, 1), "high", true)
            .unwrap();
        volume
            .upsert(frn(1000, 1), frn(900, 1), "tail.txt", false)
            .unwrap();
        assert!(volume.nodes.len() >= 1001);

        // 删掉最高位子文件 → 尾部界应回落到 901（高位父目录被在位子…父在位自身即界）。
        volume.delete(frn(1000, 1)).unwrap();
        volume.dead_name_bytes = usize::MAX; // 强制触发压缩
        assert!(volume.compact_names_if_needed().unwrap());
        assert_eq!(volume.nodes.len(), 901, "尾部 tombstone 槽必须被回收");

        // 再删高位父目录 → 回落到低位的 dir。
        volume.delete(frn(900, 1)).unwrap();
        volume.dead_name_bytes = usize::MAX;
        assert!(volume.compact_names_if_needed().unwrap());
        assert_eq!(volume.nodes.len(), 11);

        // 回收后路径构造仍正常。
        assert_eq!(volume.path_for(10).unwrap(), r"C:\dir");
    }

    #[test]
    fn create_rename_move_delete_and_sequence_reuse() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "dir", true).unwrap();
        volume
            .upsert(frn(11, 1), frn(10, 1), "old.txt", false)
            .unwrap();
        assert_eq!(volume.path_for(11).unwrap(), r"C:\dir\old.txt");
        volume.upsert(frn(12, 1), frn(5, 0), "other", true).unwrap();
        volume
            .upsert(frn(11, 1), frn(12, 1), "new.txt", false)
            .unwrap();
        assert_eq!(volume.path_for(11).unwrap(), r"C:\other\new.txt");
        volume.delete(frn(11, 1)).unwrap();
        assert!(volume.search("new", 10).is_empty());
        volume
            .upsert(frn(11, 2), frn(5, 0), "reused.txt", false)
            .unwrap();
        volume.delete(frn(11, 1)).unwrap();
        assert_eq!(volume.search("reused", 10).len(), 1);
    }

    #[test]
    fn rename_makes_old_name_unsearchable() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "dir", true).unwrap();
        volume
            .upsert(frn(11, 1), frn(10, 1), "old_name.txt", false)
            .unwrap();
        // Rename: same FRN, new name.
        volume
            .upsert(frn(11, 1), frn(10, 1), "new_name.txt", false)
            .unwrap();
        // New name should be searchable.
        assert_eq!(volume.search("new_name", 10).len(), 1);
        // Old name should NOT be searchable.
        assert_eq!(
            volume.search("old_name", 10).len(),
            0,
            "old name should not be found after rename"
        );
    }

    #[test]
    fn directory_rename_updates_descendants_without_traversal() {
        let mut volume = volume();
        volume
            .upsert(frn(10, 1), frn(5, 0), "before", true)
            .unwrap();
        for record in 11..111 {
            volume
                .upsert(frn(record, 1), frn(10, 1), &format!("file-{record}"), false)
                .unwrap();
        }
        let nodes_before = volume.nodes.len();
        volume.upsert(frn(10, 1), frn(5, 0), "after", true).unwrap();
        assert_eq!(nodes_before, volume.nodes.len());
        assert_eq!(volume.path_for(110).unwrap(), r"C:\after\file-110");
    }

    #[test]
    fn unicode_and_exclusion_boundary_are_explicit() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "资料", true).unwrap();
        volume
            .upsert(frn(11, 1), frn(10, 1), "报告.txt", false)
            .unwrap();
        assert_eq!(volume.search("报告", 10)[0].path, r"C:\资料\报告.txt");
        assert_eq!(
            volume
                .upsert(frn(10, 1), frn(5, 0), "node_modules", true)
                .unwrap(),
            ApplyOutcome::RebuildRequired
        );
    }

    #[test]
    fn search_is_case_insensitive_name_only_empty_safe_and_bounded() {
        let mut volume = volume();
        volume
            .upsert(frn(10, 1), frn(5, 0), "NeedleParent", true)
            .unwrap();
        volume
            .upsert(frn(11, 1), frn(10, 1), "unrelated.txt", false)
            .unwrap();
        volume
            .upsert(frn(12, 1), frn(5, 0), "alpha-needle.txt", false)
            .unwrap();
        volume
            .upsert(frn(13, 1), frn(5, 0), "needle-beta.txt", false)
            .unwrap();

        assert!(volume.search("", 10).is_empty());
        let results = volume.search("NEEDLE", 1);
        assert_eq!(results.len(), 1);
        assert!(results[0].name.to_lowercase().contains("needle"));
        assert!(!volume
            .search("needle", 10)
            .iter()
            .any(|item| item.name == "unrelated.txt"));
    }

    #[test]
    fn windows_installer_is_excluded_by_hierarchy_not_a_fake_flat_name() {
        let mut volume = volume();
        volume
            .upsert(frn(10, 1), frn(5, 0), "Windows", true)
            .unwrap();
        volume
            .upsert(frn(11, 1), frn(10, 1), "Installer", true)
            .unwrap();
        volume
            .upsert(frn(12, 1), frn(11, 1), "package.msi", false)
            .unwrap();
        assert!(volume.search("package", 10).is_empty());

        volume
            .upsert(frn(20, 1), frn(5, 0), "Installer", true)
            .unwrap();
        volume
            .upsert(frn(21, 1), frn(20, 1), "visible.msi", false)
            .unwrap();
        assert_eq!(volume.search("visible", 10).len(), 1);
    }

    #[test]
    fn user_exclusion_filters_candidates_before_top_k() {
        let mut volume = volume();
        volume
            .upsert(frn(10, 1), frn(5, 0), "excluded", true)
            .unwrap();
        volume
            .upsert(frn(11, 1), frn(10, 1), "needle", false)
            .unwrap();
        volume
            .upsert(frn(12, 1), frn(5, 0), "needle-visible", false)
            .unwrap();
        let state = IndexState {
            volumes: vec![volume],
            generation: 1,
            events_since_checkpoint: 0,
        };

        let outcome = state.search_with_exclusions("needle", 1, &[r"C:\excluded".into()]);
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(outcome.items[0].path, r"C:\needle-visible");
        assert_eq!(outcome.matched_count, 1);
    }

    #[test]
    fn broken_chain_cycle_and_record_overflow_fail() {
        let mut volume = volume();
        assert!(volume.upsert(frn(10, 1), frn(99, 1), "x", false).is_err());
        volume.upsert(frn(10, 1), frn(5, 0), "a", true).unwrap();
        volume.upsert(frn(11, 1), frn(10, 1), "b", true).unwrap();
        volume.nodes[10].parent_record = 11;
        assert!(volume.path_for(11).is_err());
        assert!(VolumeIndex::split_frn(0x0000_0001_0000_0000).is_err());
    }

    /// G4（FRESH-AUDIT-2）：在位槽位计数随 upsert/delete 增减，整算与之等值
    ///（ensure_slot 的稀疏度防线据此免全表扫描）。
    #[test]
    fn g4_present_slot_counter_tracks_upserts_and_deletes() {
        let mut volume = volume(); // 根节点 1 个在位
        volume.upsert(frn(10, 1), frn(5, 0), "a", true).unwrap();
        volume
            .upsert(frn(11, 1), frn(10, 1), "b.txt", false)
            .unwrap();
        assert_eq!(volume.present_slots, 3);
        volume.delete(frn(11, 1)).unwrap();
        assert_eq!(volume.present_slots, 2);
        // 复活同号槽：净计数不变。
        volume
            .upsert(frn(11, 1), frn(10, 1), "c.txt", false)
            .unwrap();
        assert_eq!(volume.present_slots, 3);
        let before = volume.present_slots;
        volume.recompute_derived_counters();
        assert_eq!(volume.present_slots, before);
    }

    /// 复审 M（2026-08-21 全仓重审）：载入侧基线恢复。initial_name_bytes 是
    /// serde skip——v5 缓存载入后必须等于当前池长，否则 needs_name_compact
    /// 的 fallback 阈值退化为常量 16MB，names 池超 16MB 的卷每次缓存命中
    /// 启动后的首个维护 tick 都触发一次无谓全量压缩。
    #[test]
    fn recompute_restores_name_pool_baseline() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "a", true).unwrap();
        volume.recompute_derived_counters(); // v5 载入路径
        assert_eq!(volume.initial_name_bytes, volume.names.len());
        assert!(!volume.needs_name_compact());
    }

    #[test]
    fn cache_validation_rejects_bad_parent_and_name_offset() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "file", false).unwrap();
        volume.nodes[10].parent_record = 99;
        assert!(volume.validate().is_err());

        volume.nodes[10].parent_record = 5;
        volume.nodes[10].name_off = 999;
        assert!(volume.validate().is_err());
    }

    /// M1（复审 2026-08-21）：祖先链上的**隔代**墓碑是 USN 重放的合法中间态
    /// （目录删除先落、后代删除未到）——validate 与 save 侧抽样都不得拒绝，
    /// 否则该状态写进缓存后每次启动全量重建。直接父墓碑容忍是既有行为。
    #[test]
    fn m1_validate_tolerates_tombstoned_ancestor_at_any_depth() {
        let mut volume = volume(); // 根=5
        volume.upsert(frn(10, 1), frn(5, 0), "dir", true).unwrap();
        volume
            .upsert(frn(11, 1), frn(10, 1), "child", true)
            .unwrap();
        volume
            .upsert(frn(12, 1), frn(11, 1), "grandchild.txt", false)
            .unwrap();

        // 直接父墓碑：record 12 的父 11 已删（既有容忍行为）。
        volume.nodes[11].flags &= !FLAG_PRESENT;
        assert!(volume.validate().is_ok());
        assert!(crate::index_cache::validate_before_save(&IndexState {
            volumes: vec![volume.clone()],
            generation: 1,
            events_since_checkpoint: 0,
        })
        .is_ok());

        // 隔代墓碑：父 11 在位、祖父 10 已删——record 12 的 path_for 会撞上
        // 墓碑祖先，修复前 validate 拒绝（缓存永久不可装载）。
        volume.nodes[11].flags |= FLAG_PRESENT;
        volume.nodes[10].flags &= !FLAG_PRESENT;
        assert!(volume.validate().is_ok());
        assert!(crate::index_cache::validate_before_save(&IndexState {
            volumes: vec![volume],
            generation: 1,
            events_since_checkpoint: 0,
        })
        .is_ok());
    }

    #[test]
    fn global_top_k_allows_late_volume_exact_match_to_win() {
        let mut first = volume();
        for record in 10..30 {
            first
                .upsert(
                    frn(record, 1),
                    frn(5, 0),
                    &format!("needle-extra-{record}.txt"),
                    false,
                )
                .unwrap();
        }
        let mut second = VolumeIndex::new(
            VolumeId {
                guid: "later-volume".into(),
                serial: 8,
            },
            "D:\\".into(),
            10,
            11,
            5,
        )
        .unwrap();
        second
            .upsert(frn(99, 1), frn(5, 0), "needle", false)
            .unwrap();
        let state = IndexState {
            volumes: vec![first, second],
            generation: 1,
            events_since_checkpoint: 0,
        };

        let outcome = state.search("needle", 8);
        assert_eq!(outcome.items.len(), 8);
        assert_eq!(outcome.items[0].path, r"D:\needle");
        assert!(outcome.is_truncated);
        assert_eq!(outcome.path_constructions, 8);
        assert_eq!(outcome.matched_count, 21);
    }

    #[test]
    fn max_one_thousand_has_no_hidden_intermediate_cap() {
        let mut volume = volume();
        for record in 10..1011 {
            volume
                .upsert(
                    frn(record, 1),
                    frn(5, 0),
                    &format!("item-{record:04}"),
                    false,
                )
                .unwrap();
        }
        let state = IndexState {
            volumes: vec![volume],
            generation: 2,
            events_since_checkpoint: 0,
        };

        let outcome = state.search("item", 1000);
        assert_eq!(outcome.items.len(), 1000);
        assert_eq!(outcome.path_constructions, 1000);
        assert_eq!(outcome.matched_count, 1001);
        assert!(outcome.is_truncated);
        assert_eq!(outcome.items[0].name, "item-0010");
    }

    #[test]
    fn stable_name_tie_break_does_not_follow_record_order() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "xb", false).unwrap();
        volume.upsert(frn(100, 1), frn(5, 0), "xa", false).unwrap();
        let state = IndexState {
            volumes: vec![volume],
            generation: 3,
            events_since_checkpoint: 0,
        };

        let names = state
            .search("x", 8)
            .items
            .into_iter()
            .map(|item| item.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["xa", "xb"]);
    }

    #[test]
    fn match_tiers_and_history_cannot_cross_locked_boundaries() {
        let rank = |kind, class, position, history_score| MatchMetadata {
            kind,
            class,
            position,
            score: 10,
            history_score,
        };
        let literal_contains = rank(MatchKind::Literal, 2, 9, 0);
        let full_exact_with_history = rank(MatchKind::FullPinyin, 0, 0, u32::MAX);
        let initials_exact_with_history = rank(MatchKind::Initials, 0, 0, u32::MAX);
        // S1（PRISM-IMPL-PLAN-4-2026-08-20）新契约：class 跨 kind——整名精确的
        // 拼音命中（class 0）压过字面子串噪声（class 2）；同 class 内 kind 锁死
        //（全拼先于首字母）。
        assert!(full_exact_with_history < literal_contains);
        assert!(full_exact_with_history < initials_exact_with_history);

        let prefix_without_history = rank(MatchKind::Literal, 1, 0, 0);
        let contains_with_history = rank(MatchKind::Literal, 2, 0, u32::MAX);
        assert!(prefix_without_history < contains_with_history);

        let same_tier_without_history = rank(MatchKind::FullPinyin, 1, 0, 0);
        let same_tier_with_history = rank(MatchKind::FullPinyin, 1, 0, 20);
        assert!(same_tier_with_history < same_tier_without_history);
    }

    /// S1（PRISM-IMPL-PLAN-4-2026-08-20）：`dy` 场景的排序层锚点——「抖音」
    /// （Initials，class 0）必须排在「Kennedy.docx」（Literal，class 2）之前；
    /// 同 class 决胜回到 kind：dy.txt（Literal，class 0）仍先于抖音。
    #[test]
    fn s1_strong_pinyin_class_beats_weak_literal_class() {
        let douyin = MatchMetadata {
            kind: MatchKind::Initials,
            class: 0,
            position: 0,
            score: 2,
            history_score: 0,
        };
        let kennedy = MatchMetadata {
            kind: MatchKind::Literal,
            class: 2,
            position: 5,
            score: 11,
            history_score: 0,
        };
        assert!(
            douyin < kennedy,
            "class 0 拼音命中必须压过 class 2 字面噪声"
        );
        let dy_txt = MatchMetadata {
            kind: MatchKind::Literal,
            class: 0,
            position: 0,
            score: 6,
            history_score: 0,
        };
        assert!(dy_txt < douyin, "同 class 内 kind 仍锁死：字面先于首字母");
    }

    #[test]
    fn usage_tier_crosses_position_but_not_class_or_kind() {
        let rank = |kind, class, position, history_score| MatchMetadata {
            kind,
            class,
            position,
            score: 10,
            history_score,
        };
        // 达到桶阈值（≥4）的高频使用越过 position：晚期匹配但常用的排前。
        assert!(rank(MatchKind::Literal, 2, 5, 100) < rank(MatchKind::Literal, 2, 0, 0));
        // 未达桶阈值（<4）的微弱历史不越过 position——同 position 才由它决胜。
        assert!(rank(MatchKind::Literal, 2, 0, 3) < rank(MatchKind::Literal, 2, 5, 3));
        // 桶不越过 class：前缀匹配无历史仍先于子串匹配满桶。
        assert!(rank(MatchKind::Literal, 1, 9, u32::MAX) < rank(MatchKind::Literal, 2, 0, 0));
        // S1：桶不越过 kind 收窄到同 class 内（class 2 的字面无历史仍先于
        // class 2 的拼音满桶）；跨 class 由 class 决胜（见 s1 测试）。
        assert!(rank(MatchKind::Literal, 2, 9, u32::MAX) < rank(MatchKind::FullPinyin, 2, 0, 0));
    }

    /// C:\project\{sub\deep.txt, near.txt} plus C:\other\deep.txt, and D:\project\deep.txt.
    fn root_fixture() -> IndexState {
        let mut first = volume();
        first
            .upsert(frn(10, 1), frn(5, 0), "project", true)
            .unwrap();
        first.upsert(frn(11, 1), frn(10, 1), "sub", true).unwrap();
        first
            .upsert(frn(12, 1), frn(11, 1), "deep.txt", false)
            .unwrap();
        first
            .upsert(frn(13, 1), frn(10, 1), "near.txt", false)
            .unwrap();
        first.upsert(frn(20, 1), frn(5, 0), "other", true).unwrap();
        first
            .upsert(frn(21, 1), frn(20, 1), "deep.txt", false)
            .unwrap();
        let mut second = VolumeIndex::new(
            VolumeId {
                guid: "second".into(),
                serial: 8,
            },
            "D:\\".into(),
            10,
            11,
            5,
        )
        .unwrap();
        second
            .upsert(frn(10, 1), frn(5, 0), "project", true)
            .unwrap();
        second
            .upsert(frn(11, 1), frn(10, 1), "deep.txt", false)
            .unwrap();
        IndexState {
            volumes: vec![first, second],
            generation: 1,
            events_since_checkpoint: 0,
        }
    }

    #[test]
    fn root_scope_matches_self_and_descendants_only() {
        let state = root_fixture();
        let volume = &state.volumes[0];
        assert!(
            volume.is_descendant_or_self(10, 10),
            "root is its own scope"
        );
        assert!(volume.is_descendant_or_self(12, 10), "grandchild is inside");
        assert!(!volume.is_descendant_or_self(21, 10), "sibling tree is out");
        assert!(
            volume.is_descendant_or_self(12, volume.root_record),
            "volume root contains everything"
        );
    }

    #[test]
    fn root_search_is_recursive_and_filters_before_top_k() {
        let state = root_fixture();
        let root = RootBound {
            volume_index: 0,
            root_record: 10,
        };

        let scoped = state.search_in_root("deep", 8, &[], Some(root));
        assert_eq!(scoped.items.len(), 1, "only the in-root file survives");
        assert_eq!(scoped.items[0].path, r"C:\project\sub\deep.txt");
        // matched_count is the root-scoped total, so truncation cannot describe records
        // that were never in scope.
        assert_eq!(scoped.matched_count, 1);
        assert!(!scoped.is_truncated);
        assert_eq!(scoped.path_constructions, 1);

        let global = state.search_in_root("deep", 8, &[], None);
        assert_eq!(global.items.len(), 3, "no root means global search");
        assert_eq!(global.matched_count, 3);
    }

    #[test]
    fn root_scope_never_crosses_volumes() {
        let state = root_fixture();
        let root = RootBound {
            volume_index: 1,
            root_record: 10,
        };
        let outcome = state.search_in_root("deep", 8, &[], Some(root));
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(outcome.items[0].path, r"D:\project\deep.txt");

        // The same record number on the other volume must not be accepted.
        let mut filter = RootFilter::new(root);
        assert!(!filter.accepts(0, &state.volumes[0], 12));
        assert!(filter.accepts(1, &state.volumes[1], 11));
    }

    #[test]
    fn root_top_k_holds_at_eight_and_one_thousand() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "root", true).unwrap();
        for record in 11..1012 {
            volume
                .upsert(
                    frn(record, 1),
                    frn(10, 1),
                    &format!("item-{record:04}"),
                    false,
                )
                .unwrap();
        }
        // Outside the root, and lexicographically ahead of every in-root name.
        for record in 2000..2100 {
            volume
                .upsert(
                    frn(record, 1),
                    frn(5, 0),
                    &format!("item-0000-{record}"),
                    false,
                )
                .unwrap();
        }
        let state = IndexState {
            volumes: vec![volume],
            generation: 1,
            events_since_checkpoint: 0,
        };
        let root = Some(RootBound {
            volume_index: 0,
            root_record: 10,
        });

        let first_page = state.search_in_root("item", 8, &[], root);
        assert_eq!(first_page.items.len(), 8);
        assert_eq!(first_page.matched_count, 1001);
        assert!(first_page.is_truncated);
        assert_eq!(first_page.path_constructions, 8);
        assert!(first_page
            .items
            .iter()
            .all(|item| item.path.starts_with(r"C:\root\")));

        let expanded = state.search_in_root("item", 1000, &[], root);
        assert_eq!(expanded.items.len(), 1000);
        assert_eq!(expanded.matched_count, 1001);
        assert!(expanded.is_truncated);
        assert!(expanded
            .items
            .iter()
            .all(|item| item.path.starts_with(r"C:\root\")));
    }

    #[test]
    fn root_verification_survives_cycles_missing_parents_and_tombstones() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "root", true).unwrap();
        volume.upsert(frn(11, 1), frn(10, 1), "mid", true).unwrap();
        volume
            .upsert(frn(12, 1), frn(11, 1), "needle-cycle.txt", false)
            .unwrap();
        volume.upsert(frn(20, 1), frn(5, 0), "away", true).unwrap();
        volume
            .upsert(frn(21, 1), frn(20, 1), "needle-missing.txt", false)
            .unwrap();
        volume
            .upsert(frn(22, 1), frn(20, 1), "needle-tombstone.txt", false)
            .unwrap();

        // A cycle above the candidate must reject instead of looping.
        volume.nodes[11].parent_record = 12;
        // A parent record outside the node table is a broken chain.
        volume.nodes[21].parent_record = 999_999;
        // A tombstoned parent is not a usable ancestor.
        volume.nodes[20].flags &= !FLAG_PRESENT;

        let state = IndexState {
            volumes: vec![volume],
            generation: 1,
            events_since_checkpoint: 0,
        };
        let outcome = state.search_in_root(
            "needle",
            8,
            &[],
            Some(RootBound {
                volume_index: 0,
                root_record: 10,
            }),
        );
        assert!(
            outcome.items.is_empty(),
            "broken chains never count as descendants: {:?}",
            outcome.items
        );
        assert_eq!(outcome.matched_count, 0);
    }

    #[test]
    fn root_verification_rejects_chains_deeper_than_the_limit() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "root", true).unwrap();
        let mut parent = 10u32;
        // The deepest directory sits exactly at the limit; its child is one hop too far.
        let deepest = 10 + MAX_ANCESTOR_DEPTH as u32;
        for record in 11..=deepest {
            volume
                .upsert(frn(record, 1), frn(parent, 1), &format!("d{record}"), true)
                .unwrap();
            parent = record;
        }
        let leaf = deepest + 1;
        volume
            .upsert(frn(leaf, 1), frn(parent, 1), "needle.txt", false)
            .unwrap();

        assert!(
            volume.is_descendant_or_self(deepest, 10),
            "exactly {MAX_ANCESTOR_DEPTH} hops below the root is still inside"
        );
        assert!(
            !volume.is_descendant_or_self(leaf, 10),
            "one hop past the ceiling is rejected"
        );
    }

    #[test]
    fn ancestor_depth_limit_matches_the_path_construction_budget() {
        // MAX_ANCESTOR_DEPTH is the deepest record `path_for` can still render, so a root
        // at the volume root accepts exactly the records that can produce a path.
        let mut volume = volume();
        let mut parent = volume.root_record;
        for hop in 1..=MAX_ANCESTOR_DEPTH as u32 + 1 {
            let record = 9 + hop;
            volume
                .upsert(
                    frn(record, 1),
                    frn(parent, if parent == volume.root_record { 0 } else { 1 }),
                    &format!("d{hop}"),
                    true,
                )
                .unwrap();
            parent = record;
        }
        let deepest_renderable = 9 + MAX_ANCESTOR_DEPTH as u32;
        assert!(volume.path_for(deepest_renderable).is_ok());
        assert!(volume.path_for(deepest_renderable + 1).is_err());
        assert!(volume.is_descendant_or_self(deepest_renderable, volume.root_record));
        assert!(!volume.is_descendant_or_self(deepest_renderable + 1, volume.root_record));
    }

    #[test]
    fn deep_candidate_does_not_poison_the_memo_for_shallower_siblings() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "root", true).unwrap();
        // A chain that runs past the ceiling, built before the candidate files so the
        // deep file gets the *lower* record number and is therefore scanned first.
        let mut parent = 10u32;
        let chain_len = MAX_ANCESTOR_DEPTH as u32 + 6;
        for hop in 1..=chain_len {
            let record = 99 + hop;
            volume
                .upsert(frn(record, 1), frn(parent, 1), &format!("d{hop}"), true)
                .unwrap();
            parent = record;
        }
        volume
            .upsert(frn(11, 1), frn(parent, 1), "needle-too-deep.txt", false)
            .unwrap();
        // Sits under a directory the too-deep walk passes through, but within the ceiling.
        let shallow_parent = 99 + MAX_ANCESTOR_DEPTH as u32 - 4;
        volume
            .upsert(
                frn(12, 1),
                frn(shallow_parent, 1),
                "needle-in-range.txt",
                false,
            )
            .unwrap();
        let state = IndexState {
            volumes: vec![volume],
            generation: 1,
            events_since_checkpoint: 0,
        };

        let outcome = state.search_in_root(
            "needle",
            8,
            &[],
            Some(RootBound {
                volume_index: 0,
                root_record: 10,
            }),
        );
        assert_eq!(
            outcome.items.len(),
            1,
            "the in-range file must survive the earlier too-deep walk: {:?}",
            outcome.items
        );
        assert!(outcome.items[0].name == "needle-in-range.txt");
        assert_eq!(outcome.matched_count, 1);
    }

    #[test]
    fn root_scope_handles_unicode_and_long_names() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "项目", true).unwrap();
        let long_name = "长".repeat(80);
        volume
            .upsert(frn(11, 1), frn(10, 1), &long_name, true)
            .unwrap();
        volume
            .upsert(frn(12, 1), frn(11, 1), "报告.txt", false)
            .unwrap();
        volume.upsert(frn(20, 1), frn(5, 0), "别处", true).unwrap();
        volume
            .upsert(frn(21, 1), frn(20, 1), "报告.txt", false)
            .unwrap();
        let state = IndexState {
            volumes: vec![volume],
            generation: 1,
            events_since_checkpoint: 0,
        };

        let outcome = state.search_in_root(
            "报告",
            8,
            &[],
            Some(RootBound {
                volume_index: 0,
                root_record: 10,
            }),
        );
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(
            outcome.items[0].path,
            format!(r"C:\项目\{long_name}\报告.txt")
        );
    }

    #[test]
    fn stale_root_record_yields_no_results_instead_of_everything() {
        let mut volume = volume();
        volume.upsert(frn(10, 1), frn(5, 0), "root", true).unwrap();
        volume
            .upsert(frn(11, 1), frn(10, 1), "needle.txt", false)
            .unwrap();
        // The root directory was deleted between resolve and search.
        volume.delete(frn(10, 1)).unwrap();
        let state = IndexState {
            volumes: vec![volume],
            generation: 1,
            events_since_checkpoint: 0,
        };

        let outcome = state.search_in_root(
            "needle",
            8,
            &[],
            Some(RootBound {
                volume_index: 0,
                root_record: 10,
            }),
        );
        assert!(outcome.items.is_empty());
        assert_eq!(outcome.matched_count, 0);
    }

    #[test]
    fn root_memo_does_not_grow_the_node_slot() {
        // The memo is request-scoped state on RootFilter, never per-node state.
        assert_eq!(std::mem::size_of::<NodeSlot>(), 12);
        let state = root_fixture();
        let mut filter = RootFilter::new(RootBound {
            volume_index: 0,
            root_record: 10,
        });
        assert!(filter.accepts(0, &state.volumes[0], 12));
        assert!(filter.accepts(0, &state.volumes[0], 12), "memo hit repeats");
        assert!(filter.accepts(0, &state.volumes[0], 13));
        assert!(!filter.accepts(0, &state.volumes[0], 21));
        assert!(
            !filter.accepts(0, &state.volumes[0], 21),
            "negative memo too"
        );
        assert_eq!(filter.bound().root_record, 10);
    }

    // --- G7: ext/path filter tests ------------------------------------------

    /// Builds a corpus: N files named `file-<i>.txt` + N files named `file-<i>.pdf`
    /// under C:\docs\, plus a subdirectory `C:\Project X\` with some files.
    fn filter_fixture() -> IndexState {
        let mut vol = volume();
        vol.upsert(frn(10, 1), frn(5, 0), "docs", true).unwrap();
        for i in 0..20u32 {
            vol.upsert(frn(100 + i, 1), frn(10, 1), &format!("file-{i}.txt"), false)
                .unwrap();
        }
        for i in 0..20u32 {
            vol.upsert(frn(200 + i, 1), frn(10, 1), &format!("file-{i}.pdf"), false)
                .unwrap();
        }
        // A directory to test that ext filters exclude directories.
        vol.upsert(frn(300, 1), frn(10, 1), "notes", true).unwrap();
        // Files under a directory with a space in the path.
        vol.upsert(frn(301, 1), frn(5, 0), "Project X", true)
            .unwrap();
        vol.upsert(frn(302, 1), frn(301, 1), "design.pdf", false)
            .unwrap();
        IndexState {
            volumes: vec![vol],
            generation: 1,
            events_since_checkpoint: 0,
        }
    }

    #[test]
    fn ext_filter_returns_only_matching_extension_before_top_k() {
        let state = filter_fixture();
        let filters = QueryFilters::new(vec!["pdf".into()], vec![]);
        let outcome = state.search_in_root_filtered("file", 8, &[], None, &filters);
        assert_eq!(
            outcome.items.len(),
            8,
            "should return full max=8 of pdf files"
        );
        assert!(
            outcome.items.iter().all(|item| item.name.ends_with(".pdf")),
            "all results must be pdf"
        );
        assert!(
            outcome.is_truncated,
            "more than 8 pdf files existed, so truncation must be true"
        );
    }

    #[test]
    fn ext_filter_or_with_multiple_extensions() {
        let state = filter_fixture();
        let filters = QueryFilters::new(vec!["txt".into(), "pdf".into()], vec![]);
        let outcome = state.search_in_root_filtered("file", 1000, &[], None, &filters);
        assert_eq!(outcome.items.len(), 40, "all 20 txt + 20 pdf should match");
    }

    #[test]
    fn ext_filter_excludes_directories() {
        let state = filter_fixture();
        let filters = QueryFilters::new(vec!["txt".into()], vec![]);
        let outcome = state.search_in_root_filtered("notes", 8, &[], None, &filters);
        // The directory "notes" matches the name query but has no extension.
        assert!(
            outcome.items.is_empty(),
            "directories must not pass ext filters"
        );
    }

    #[test]
    fn path_filter_substring_match_case_insensitive() {
        let state = filter_fixture();
        let filters = QueryFilters::new(vec![], vec!["project x".into()]);
        let outcome = state.search_in_root_filtered("design", 8, &[], None, &filters);
        assert_eq!(outcome.items.len(), 1);
        assert!(outcome.items[0].path.contains("Project X"));
    }

    #[test]
    fn ext_and_path_combined_and_before_top_k() {
        let state = filter_fixture();
        // ext:pdf AND path:"Project X" → only design.pdf under Project X
        let filters = QueryFilters::new(vec!["pdf".into()], vec!["Project X".into()]);
        let outcome = state.search_in_root_filtered("design", 8, &[], None, &filters);
        assert_eq!(outcome.items.len(), 1);
        assert!(outcome.items[0].name.contains("design"));
    }

    #[test]
    fn no_filters_returns_all_matching_candidates() {
        let state = filter_fixture();
        let outcome = state.search_in_root_filtered("file", 8, &[], None, &QueryFilters::none());
        assert_eq!(outcome.items.len(), 8);
    }

    #[test]
    fn empty_name_query_with_ext_filter_scans_all_nodes() {
        // G7: `ext:pdf` with no name token → name_query is empty, but the ext
        // filter should still return all pdf files. The empty-query guard must
        // not short-circuit when filters are present.
        let state = filter_fixture();
        let filters = QueryFilters::new(vec!["pdf".into()], vec![]);
        let outcome = state.search_in_root_filtered("", 8, &[], None, &filters);
        assert_eq!(outcome.items.len(), 8, "should return 8 pdf files");
        assert!(
            outcome.items.iter().all(|item| item.name.ends_with(".pdf")),
            "all results must be pdf"
        );
        assert!(outcome.is_truncated, "21 pdf files exist, must truncate");
    }

    #[test]
    fn file_extension_helper() {
        assert_eq!(file_extension("file.txt"), Some("txt"));
        assert_eq!(file_extension("archive.tar.gz"), Some("gz"));
        assert_eq!(file_extension("noext"), None);
        assert_eq!(file_extension(".gitignore"), Some("gitignore"));
        assert_eq!(file_extension("trailing."), None);
    }
}
