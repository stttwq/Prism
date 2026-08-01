//! Compact FRN-indexed hierarchy shared by MFT construction and USN replay.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use serde::{Deserialize, Serialize};

pub const FLAG_PRESENT: u16 = 0x0001;
pub const FLAG_DIRECTORY: u16 = 0x0002;
pub const FLAG_EXCLUDED: u16 = 0x0004;
const MAX_RECORD_NUMBER: usize = 2_000_000;
const MAX_PATH_DEPTH: usize = 64;
const NO_NAME: u32 = u32::MAX;

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MatchMetadata {
    pub class: u8,
    pub position: u32,
    pub score: u32,
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
        search_volumes(std::slice::from_ref(self), query, max, &[]).items
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
            if parent.flags & (FLAG_PRESENT | FLAG_DIRECTORY) != FLAG_PRESENT | FLAG_DIRECTORY {
                return Err(format!("cache parent is not a present directory: {record}"));
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
            let end = self.names[start..]
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
        for frn in frns {
            let (record, _) = Self::split_frn(frn)?;
            if record as usize >= self.nodes.len()
                || slots.iter().any(|(saved, _)| *saved == record)
            {
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

    fn name_at(&self, offset: u32) -> Result<&str, String> {
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
        search_volumes(&self.volumes, query, max, exclusion_paths)
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
        class,
        position,
        score: name.encode_utf16().count() as u32,
    })
}

fn find_case_insensitive(name: &str, query_lower: &str) -> Option<usize> {
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

fn search_volumes(
    volumes: &[VolumeIndex],
    query: &str,
    max: usize,
    exclusion_paths: &[String],
) -> SearchOutcome {
    if query.is_empty() || max == 0 {
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
    let mut scanned_nodes = 0u64;
    let mut name_candidates = 0u64;
    let mut matched_count = 0u64;
    let mut entered_top_k = 0u64;
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
            if exclusions
                .iter()
                .any(|exclusion| exclusion.matches(volume, record as u32))
            {
                continue;
            }
            matched_count = matched_count.saturating_add(1);
            let candidate = RankedCandidate {
                volume_index,
                mount_path: &volume.mount_path,
                record: record as u32,
                name,
                is_directory: slot.flags & FLAG_DIRECTORY != 0,
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
    let path_constructions = ranked.len() as u64;
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
}
