//! Compact FRN-indexed hierarchy shared by MFT construction and USN replay.

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
        self.ensure_slot(record)?;
        if parent_record as usize >= self.nodes.len()
            || self.nodes[parent_record as usize].flags & FLAG_PRESENT == 0
        {
            return Err(format!("broken parent chain at record {record}"));
        }

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
        if query.is_empty() || max == 0 {
            return Vec::new();
        }
        let query = query.to_lowercase();
        let mut hits = Vec::with_capacity(max.min(64));
        for (record, slot) in self.nodes.iter().enumerate() {
            if slot.flags & (FLAG_PRESENT | FLAG_EXCLUDED) != FLAG_PRESENT
                || slot.name_off == NO_NAME
            {
                continue;
            }
            let Ok(name) = self.name_at(slot.name_off) else {
                continue;
            };
            if !contains_case_insensitive(name, &query) {
                continue;
            }
            let Ok(path) = self.path_for(record as u32) else {
                continue;
            };
            hits.push(IndexHit {
                name: name.to_owned(),
                path,
                is_directory: slot.flags & FLAG_DIRECTORY != 0,
            });
            if hits.len() == max {
                break;
            }
        }
        hits
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
    pub fn search(&self, query: &str, max: usize) -> Vec<IndexHit> {
        let mut hits = Vec::with_capacity(max.min(64));
        for volume in &self.volumes {
            hits.extend(volume.search(query, max.saturating_sub(hits.len())));
            if hits.len() == max {
                break;
            }
        }
        hits
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

fn contains_case_insensitive(name: &str, query_lower: &str) -> bool {
    if name.is_ascii() && query_lower.is_ascii() {
        name.as_bytes().windows(query_lower.len()).any(|window| {
            window
                .iter()
                .zip(query_lower.as_bytes())
                .all(|(left, right)| left.to_ascii_lowercase() == *right)
        })
    } else {
        name.to_lowercase().contains(query_lower)
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
}
