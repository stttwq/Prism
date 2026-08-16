//! Compact FRN-indexed hierarchy shared by MFT construction and USN replay.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use serde::{Deserialize, Serialize};

pub const FLAG_PRESENT: u16 = 0x0001;
pub const FLAG_DIRECTORY: u16 = 0x0002;
pub const FLAG_EXCLUDED: u16 = 0x0004;
/// 槽表按 MFT 记录号预分配（12 字节/槽，resize 全量落实）。
/// 上限 2M 会拒绝现代系统盘（2~8M 记录很常见），且 build 失败会让整个索引
/// 服务退出。16M ≈ 192MB 槽表上限，实际按卷真实记录数落实；病态稀疏卷
/// 仍由 prepare_initial_capacity 的 32× 密度检查拦截。
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

impl Ord for MatchMetadata {
    fn cmp(&self, other: &Self) -> Ordering {
        self.kind
            .cmp(&other.kind)
            .then(self.class.cmp(&other.class))
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
        })
    }

    pub fn finish_initial_build(&mut self) {
        self.nodes.shrink_to_fit();
        self.names.shrink_to_fit();
        self.initial_name_bytes = self.names.len();
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
        if required > 65_536 && required > live_records.max(1).saturating_mul(32) {
            return Err(format!(
                "MFT slot table is pathologically sparse: {required}/{live_records}"
            ));
        }
        self.nodes.resize(required, NodeSlot::default());
        Ok(())
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
    ) -> Result<ApplyOutcome, String> {
        let (record, sequence) = Self::split_frn(frn)?;
        let (parent_record, _) = Self::split_frn(parent_frn)?;
        if parent_record as usize >= self.nodes.len()
            || self.nodes[parent_record as usize].flags & FLAG_PRESENT == 0
        {
            return Err(format!("broken parent chain at record {record}"));
        }
        self.ensure_slot(record)?;

        let parent_excluded = self.nodes[parent_record as usize].flags & FLAG_EXCLUDED != 0;
        let excluded = parent_excluded
            || is_excluded_name(name)
            || (name.eq_ignore_ascii_case("Installer")
                && self.node_name_is(parent_record, "Windows"));
        let old = self.nodes[record as usize];
        let crossed_boundary =
            old.flags & FLAG_PRESENT != 0 && (old.flags & FLAG_EXCLUDED != 0) != excluded;
        if crossed_boundary && is_directory {
            return Ok(ApplyOutcome::RebuildRequired);
        }
        let keep_name = is_directory || !excluded;
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

    pub fn delete(&mut self, frn: u64) -> Result<(), String> {
        let (record, sequence) = Self::split_frn(frn)?;
        let Some(slot) = self.nodes.get_mut(record as usize) else {
            return Ok(());
        };
        if slot.flags & FLAG_PRESENT != 0 && slot.sequence == sequence {
            slot.flags &= !FLAG_PRESENT;
            slot.name_off = NO_NAME;
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
        search_volumes(std::slice::from_ref(self), query, max, &[], None, &QueryFilters::none()).items
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
            self.path_for(record as u32)?;
        }
        Ok(())
    }

    pub fn compact_names_if_needed(&mut self) -> Result<bool, String> {
        let threshold = (8 * 1024 * 1024usize).max(self.initial_name_bytes / 4);
        let live_bytes: usize = self
            .nodes
            .iter()
            .filter(|slot| slot.flags & FLAG_PRESENT != 0 && slot.name_off != NO_NAME)
            .filter_map(|slot| self.name_at(slot.name_off).ok())
            .map(|name| name.len() + 1)
            .sum();
        if self.names.len().saturating_sub(live_bytes) <= threshold {
            return Ok(false);
        }
        let mut replacement = Vec::with_capacity(live_bytes);
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
            slot.name_off = offset;
        }
        replacement.shrink_to_fit();
        self.names = replacement;
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
        })
    }

    pub(crate) fn rollback_mutations(&mut self, snapshot: MutationSnapshot) {
        self.nodes.truncate(snapshot.nodes_len);
        for (record, slot) in snapshot.slots {
            self.nodes[record as usize] = slot;
        }
        self.names.truncate(snapshot.names_len);
    }

    fn ensure_slot(&mut self, record: u32) -> Result<(), String> {
        let required = record as usize + 1;
        if required > MAX_RECORD_NUMBER {
            return Err(format!("MFT record {record} exceeds compact-index limit"));
        }
        if required > self.nodes.len() {
            let present = self
                .nodes
                .iter()
                .filter(|slot| slot.flags & FLAG_PRESENT != 0)
                .count()
                .max(1);
            if required > 65_536 && required > present.saturating_mul(32) {
                return Err(format!(
                    "MFT slot table is pathologically sparse: {required}/{present}"
                ));
            }
            self.nodes.resize(required, NodeSlot::default());
        }
        Ok(())
    }

    fn append_name(&mut self, name: &str) -> Result<u32, String> {
        if name.contains('\0') {
            return Err("file name contains NUL".into());
        }
        let offset = u32::try_from(self.names.len()).map_err(|_| "name pool exceeds u32")?;
        self.names.extend_from_slice(name.as_bytes());
        self.names.push(0);
        Ok(offset)
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

fn match_metadata(name: &str, query_lower: &str) -> Option<MatchMetadata> {
    let byte_position = find_case_insensitive(name, query_lower)?;
    let name_lower = if name.is_ascii() {
        None
    } else {
        Some(name.to_lowercase())
    };
    let normalized = name_lower.as_deref().unwrap_or(name);
    let class = if normalized.len() == query_lower.len() {
        0
    } else if byte_position == 0 {
        1
    } else {
        2
    };
    let position = normalized[..byte_position].encode_utf16().count() as u32;
    Some(MatchMetadata {
        kind: MatchKind::Literal,
        class,
        position,
        score: name.encode_utf16().count() as u32,
        history_score: 0,
    })
}

fn find_case_insensitive(name: &str, query_lower: &str) -> Option<usize> {
    if query_lower.is_empty() {
        // G7: an empty name query matches every candidate (used when ext:/path:
        // filters are the only criteria). Position 0 means "match at start".
        return Some(0);
    }
    if name.is_ascii() && query_lower.is_ascii() {
        name.as_bytes()
            .windows(query_lower.len())
            .position(|window| {
                window
                    .iter()
                    .zip(query_lower.as_bytes())
                    .all(|(left, right)| left.to_ascii_lowercase() == *right)
            })
    } else {
        name.to_lowercase().find(query_lower)
    }
}

pub(crate) fn is_literal_match(name: &str, query: &str) -> bool {
    find_case_insensitive(name, &query.to_lowercase()).is_some()
}

/// Case-insensitive whole-name comparison that also works for non-ASCII names.
pub(crate) fn name_eq_ignore_case(left: &str, right: &str) -> bool {
    if left.is_ascii() && right.is_ascii() {
        left.eq_ignore_ascii_case(right)
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
        ext.is_some_and(|e| self.exts.iter().any(|target| target.eq_ignore_ascii_case(e)))
    }

    /// High-cost path check: requires a constructed full path. All path substrings must
    /// match (AND), case-insensitive.
    pub(crate) fn path_matches(&self, path: &str) -> bool {
        if self.paths.is_empty() {
            return true;
        }
        self.paths
            .iter()
            .all(|needle| path.to_lowercase().contains(&needle.to_lowercase()))
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

fn search_volumes(
    volumes: &[VolumeIndex],
    query: &str,
    max: usize,
    exclusion_paths: &[String],
    root: Option<RootBound>,
    filters: &QueryFilters,
) -> SearchOutcome {
    // G7: an empty name query normally means "no search", but when ext:/path:
    // filters are present the empty name matches every candidate (match_metadata
    // returns Some for empty query), so we must not short-circuit.
    if (query.is_empty() && filters.is_empty()) || max == 0 {
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

    let query_lower = query.to_lowercase();
    let exclusions: Vec<_> = exclusion_paths
        .iter()
        .filter_map(|path| NormalizedExclusion::parse(path))
        .collect();
    let mut heap = BinaryHeap::with_capacity(max);
    let mut root_filter = root.map(RootFilter::new);
    let mut scanned_nodes = 0u64;
    let mut name_candidates = 0u64;
    let mut matched_count = 0u64;
    let mut entered_top_k = 0u64;
    let mut path_constructions = 0u64;
    let has_path_filter = filters.has_path_filter();
    for (volume_index, volume) in volumes.iter().enumerate() {
        for (record, slot) in volume.nodes.iter().enumerate() {
            scanned_nodes = scanned_nodes.saturating_add(1);
            if slot.flags & (FLAG_PRESENT | FLAG_EXCLUDED) != FLAG_PRESENT
                || slot.name_off == NO_NAME
            {
                continue;
            }
            name_candidates = name_candidates.saturating_add(1);
            let Ok(name) = volume.name_at(slot.name_off) else {
                continue;
            };
            let Some(metadata) = match_metadata(name, &query_lower) else {
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
                path_constructions = path_constructions.saturating_add(1);
                let path = match volume.path_for(record as u32) {
                    Ok(path) => path,
                    Err(_) => continue,
                };
                if !filters.path_matches(&path) {
                    continue;
                }
            }
            matched_count = matched_count.saturating_add(1);
            let candidate = RankedCandidate {
                volume_index,
                mount_path: &volume.mount_path,
                record: record as u32,
                name,
                is_directory,
                metadata,
            };
            if heap.len() < max {
                heap.push(candidate);
                entered_top_k = entered_top_k.saturating_add(1);
            } else if heap.peek().is_some_and(|worst| candidate < *worst) {
                heap.pop();
                heap.push(candidate);
                entered_top_k = entered_top_k.saturating_add(1);
            }
        }
    }

    let ranked = heap.into_sorted_vec();
    // Path construction for surviving candidates: counted separately from the path-filter
    // constructions above (which only ran when a path filter was present).
    path_constructions = path_constructions.saturating_add(ranked.len() as u64);
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
        is_truncated: matched_count > max as u64,
        scanned_nodes,
        name_candidates,
        matched_count,
        entered_top_k,
        path_constructions,
    }
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
        assert!(volume.prepare_initial_capacity(16_777_216, 16_777_216).is_err());
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
        assert!(literal_contains < full_exact_with_history);
        assert!(full_exact_with_history < initials_exact_with_history);

        let prefix_without_history = rank(MatchKind::Literal, 1, 0, 0);
        let contains_with_history = rank(MatchKind::Literal, 2, 0, u32::MAX);
        assert!(prefix_without_history < contains_with_history);

        let same_tier_without_history = rank(MatchKind::FullPinyin, 1, 0, 0);
        let same_tier_with_history = rank(MatchKind::FullPinyin, 1, 0, 20);
        assert!(same_tier_with_history < same_tier_without_history);
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
        vol.upsert(frn(301, 1), frn(5, 0), "Project X", true).unwrap();
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
        assert_eq!(outcome.items.len(), 8, "should return full max=8 of pdf files");
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
        assert_eq!(
            outcome.items.len(),
            40,
            "all 20 txt + 20 pdf should match"
        );
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
