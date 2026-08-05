//! Current-directory (root) scope: path normalization, index mapping and the
//! structured rejections that make every fallback to a global search explainable.
//!
//! The scope only ever describes *where* to search. Resolving a root never mutates the
//! index and never adds per-node state: the 12-byte [`NodeSlot`](crate::hierarchy::NodeSlot)
//! stays untouched and descendant checks walk the existing `parent_record` chain.

use serde::{Deserialize, Serialize};

use crate::hierarchy::{
    name_eq_ignore_case, IndexState, RootBound, VolumeIndex, FLAG_DIRECTORY, FLAG_EXCLUDED,
    FLAG_PRESENT, MAX_ANCESTOR_DEPTH,
};

/// Windows extended-length path budget. A longer string cannot name a real directory.
pub const MAX_ROOT_PATH_BYTES: usize = 32_767;

/// Root depth ceiling, in components below the volume root. Equal to the hierarchy's
/// ancestor/path budget, so any root that resolves can also have its own path rendered.
pub const MAX_ROOT_DEPTH: usize = MAX_ANCESTOR_DEPTH;

/// Why a requested root cannot be used. Every variant is a *degradation reason*: the
/// caller keeps working by searching globally and telling the user which root was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootRejection {
    /// Relative, drive-relative (`C:docs`) or empty input.
    NotAbsolute,
    /// A path form Prism does not index: UNC shares, `\\?\UNC\`, device paths, and any
    /// volume that is not a mounted drive letter (non-NTFS volumes never enter the index,
    /// so they surface here as well).
    Unsupported,
    /// Longer than [`MAX_ROOT_PATH_BYTES`].
    TooLong,
    /// More than [`MAX_ROOT_DEPTH`] components below the volume root.
    TooDeep,
    /// The drive is not part of the index (not indexed yet, removed, or not NTFS).
    VolumeNotIndexed,
    /// The volume is indexed but the directory is not in it.
    NotFound,
    /// The path resolves to a file, not a directory.
    NotADirectory,
    /// The host reported that the directory cannot be read. Never derived from the index;
    /// callers pass it in when their own probe failed.
    AccessDenied,
}

impl RootRejection {
    /// Stable machine-readable reason, matching the serialized form.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotAbsolute => "not_absolute",
            Self::Unsupported => "unsupported",
            Self::TooLong => "too_long",
            Self::TooDeep => "too_deep",
            Self::VolumeNotIndexed => "volume_not_indexed",
            Self::NotFound => "not_found",
            Self::NotADirectory => "not_a_directory",
            Self::AccessDenied => "access_denied",
        }
    }

    /// Short explanation for logs and the UI fallback notice.
    pub fn message(self) -> &'static str {
        match self {
            Self::NotAbsolute => "root must be an absolute drive path",
            Self::Unsupported => "root path form is not indexed",
            Self::TooLong => "root path is too long",
            Self::TooDeep => "root path is too deep",
            Self::VolumeNotIndexed => "root volume is not indexed",
            Self::NotFound => "root directory is not in the index",
            Self::NotADirectory => "root path is not a directory",
            Self::AccessDenied => "root directory cannot be read",
        }
    }
}

/// A syntactically valid root: an uppercase drive prefix plus the components below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedRoot {
    volume_prefix: String,
    components: Vec<String>,
}

impl NormalizedRoot {
    /// Normalizes case of the drive letter, separators, redundant `.`/`..` and trailing
    /// slashes, and rejects every path form the index cannot describe.
    pub fn parse(raw: &str) -> Result<Self, RootRejection> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(RootRejection::NotAbsolute);
        }
        if trimmed.len() > MAX_ROOT_PATH_BYTES {
            return Err(RootRejection::TooLong);
        }
        if trimmed.contains('\0') || trimmed.chars().any(char::is_control) {
            return Err(RootRejection::Unsupported);
        }
        let unified = trimmed.replace('/', "\\");
        let body = strip_extended_prefix(&unified)?;
        let mut chars = body.chars();
        let drive = chars.next().ok_or(RootRejection::NotAbsolute)?;
        if !drive.is_ascii_alphabetic() || chars.next() != Some(':') {
            return Err(RootRejection::NotAbsolute);
        }
        let rest = &body[2..];
        if !rest.is_empty() && !rest.starts_with('\\') {
            // `C:docs` is drive-relative, not absolute.
            return Err(RootRejection::NotAbsolute);
        }

        let mut components: Vec<String> = Vec::new();
        for part in rest.split('\\').filter(|part| !part.is_empty()) {
            match part {
                "." => continue,
                ".." => {
                    if components.pop().is_none() {
                        return Err(RootRejection::NotAbsolute);
                    }
                }
                _ => components.push(part.to_owned()),
            }
        }
        if components.len() > MAX_ROOT_DEPTH {
            return Err(RootRejection::TooDeep);
        }
        Ok(Self {
            volume_prefix: format!("{}:", drive.to_ascii_uppercase()),
            components,
        })
    }

    /// Canonical display form, always without a trailing separator below the drive root.
    pub fn display(&self) -> String {
        if self.components.is_empty() {
            return format!("{}\\", self.volume_prefix);
        }
        format!("{}\\{}", self.volume_prefix, self.components.join("\\"))
    }

    pub fn volume_prefix(&self) -> &str {
        &self.volume_prefix
    }

    pub fn components(&self) -> &[String] {
        &self.components
    }
}

/// A root mapped onto a live index: the volume slot plus the directory record that every
/// candidate must descend from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootScope {
    normalized: NormalizedRoot,
    bound: RootBound,
}

impl RootScope {
    /// Normalizes `raw` and maps it to an indexed volume and directory record.
    pub fn resolve(index: &IndexState, raw: &str) -> Result<Self, RootRejection> {
        let normalized = NormalizedRoot::parse(raw)?;
        let (volume_index, volume) = index
            .volumes
            .iter()
            .enumerate()
            .find(|(_, volume)| {
                volume
                    .mount_path
                    .trim_end_matches(['\\', '/'])
                    .eq_ignore_ascii_case(&normalized.volume_prefix)
            })
            .ok_or(RootRejection::VolumeNotIndexed)?;
        let record = resolve_record(volume, normalized.components())?;
        Ok(Self {
            normalized,
            bound: RootBound {
                volume_index,
                root_record: record,
            },
        })
    }

    pub fn bound(&self) -> RootBound {
        self.bound
    }

    pub fn display(&self) -> String {
        self.normalized.display()
    }

    pub fn normalized(&self) -> &NormalizedRoot {
        &self.normalized
    }
}

fn strip_extended_prefix(path: &str) -> Result<&str, RootRejection> {
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        // `\\?\UNC\server\share` and `\\?\Volume{...}` are not drive-letter roots.
        if rest.len() < 2 || !rest.as_bytes()[0].is_ascii_alphabetic() || rest.as_bytes()[1] != b':'
        {
            return Err(RootRejection::Unsupported);
        }
        return Ok(rest);
    }
    if path.starts_with(r"\\") {
        // UNC shares and `\\.\` device paths are never indexed.
        return Err(RootRejection::Unsupported);
    }
    Ok(path)
}

/// Maps path components to a directory record with one bounded pass over the node table.
///
/// Only the last component is compared against every node; matches are then confirmed by
/// walking the existing `parent_record` chain upwards, so no child index is needed.
///
/// Directories inside an excluded subtree (`node_modules`, `.git`, `WinSxS`, ...) are not
/// searchable, so they are reported as absent from the index instead of resolving to a
/// scope that could only ever return an unexplained empty result.
fn resolve_record(volume: &VolumeIndex, components: &[String]) -> Result<u32, RootRejection> {
    let Some(leaf) = components.last() else {
        return Ok(volume.root_record);
    };
    let mut saw_file = false;
    for (record, slot) in volume.nodes.iter().enumerate() {
        if slot.flags & (FLAG_PRESENT | FLAG_EXCLUDED) != FLAG_PRESENT {
            continue;
        }
        let Ok(name) = volume.name_at(slot.name_off) else {
            continue;
        };
        if name.is_empty() || !name_eq_ignore_case(name, leaf) {
            continue;
        }
        if !chain_matches(volume, record as u32, components) {
            continue;
        }
        if slot.flags & FLAG_DIRECTORY != 0 {
            return Ok(record as u32);
        }
        saw_file = true;
    }
    Err(if saw_file {
        RootRejection::NotADirectory
    } else {
        RootRejection::NotFound
    })
}

fn chain_matches(volume: &VolumeIndex, record: u32, components: &[String]) -> bool {
    let mut current = record;
    for expected in components.iter().rev() {
        let Some(slot) = volume.nodes.get(current as usize) else {
            return false;
        };
        if slot.flags & FLAG_PRESENT == 0 {
            return false;
        }
        let Ok(name) = volume.name_at(slot.name_off) else {
            return false;
        };
        if !name_eq_ignore_case(name, expected) {
            return false;
        }
        if slot.parent_record == current {
            // A self-parent below the volume root is a broken chain, not a match.
            return false;
        }
        current = slot.parent_record;
    }
    current == volume.root_record
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::{VolumeId, VolumeIndex};

    fn frn(record: u32, sequence: u16) -> u64 {
        (u64::from(sequence) << 48) | u64::from(record)
    }

    fn volume(mount: &str, guid: &str) -> VolumeIndex {
        VolumeIndex::new(
            VolumeId {
                guid: guid.into(),
                serial: 7,
            },
            mount.into(),
            9,
            10,
            5,
        )
        .unwrap()
    }

    /// C:\项目\sub\file.txt plus C:\项目\report.txt, and an unrelated D: volume.
    fn index() -> IndexState {
        let mut first = volume("C:\\", "first");
        first.upsert(frn(10, 1), frn(5, 0), "项目", true).unwrap();
        first.upsert(frn(11, 1), frn(10, 1), "sub", true).unwrap();
        first
            .upsert(frn(12, 1), frn(11, 1), "file.txt", false)
            .unwrap();
        first
            .upsert(frn(13, 1), frn(10, 1), "report.txt", false)
            .unwrap();
        let mut second = volume("D:\\", "second");
        second.upsert(frn(10, 1), frn(5, 0), "data", true).unwrap();
        IndexState {
            volumes: vec![first, second],
            generation: 1,
            events_since_checkpoint: 0,
        }
    }

    #[test]
    fn normalization_canonicalizes_case_separators_and_dot_segments() {
        let parsed = NormalizedRoot::parse("  c:/Users\\me\\.\\docs\\  ").unwrap();
        assert_eq!(parsed.display(), r"C:\Users\me\docs");
        assert_eq!(parsed.components(), ["Users", "me", "docs"]);

        assert_eq!(
            NormalizedRoot::parse(r"C:\Users\me\..\other")
                .unwrap()
                .display(),
            r"C:\Users\other"
        );
        assert_eq!(NormalizedRoot::parse("C:").unwrap().display(), "C:\\");
        assert_eq!(NormalizedRoot::parse(r"C:\\").unwrap().display(), "C:\\");
        assert_eq!(
            NormalizedRoot::parse(r"\\?\D:\Projects").unwrap().display(),
            r"D:\Projects"
        );
    }

    #[test]
    fn normalization_rejects_every_unusable_form() {
        for (raw, expected) in [
            ("", RootRejection::NotAbsolute),
            ("   ", RootRejection::NotAbsolute),
            ("docs\\sub", RootRejection::NotAbsolute),
            (r"\docs", RootRejection::NotAbsolute),
            ("C:docs", RootRejection::NotAbsolute),
            (r"C:\..", RootRejection::NotAbsolute),
            (r"\\server\share", RootRejection::Unsupported),
            (r"\\.\C:", RootRejection::Unsupported),
            (r"\\?\UNC\server\share", RootRejection::Unsupported),
            (r"\\?\Volume{0}\dir", RootRejection::Unsupported),
            ("C:\\a\0b", RootRejection::Unsupported),
        ] {
            assert_eq!(
                NormalizedRoot::parse(raw).unwrap_err(),
                expected,
                "unexpected verdict for {raw:?}"
            );
        }
    }

    #[test]
    fn normalization_bounds_length_and_depth() {
        let too_long = format!("C:\\{}", "a".repeat(MAX_ROOT_PATH_BYTES));
        assert_eq!(
            NormalizedRoot::parse(&too_long).unwrap_err(),
            RootRejection::TooLong
        );

        let deep = format!("C:\\{}", vec!["d"; MAX_ROOT_DEPTH].join("\\"));
        assert_eq!(
            NormalizedRoot::parse(&deep).unwrap().components().len(),
            MAX_ROOT_DEPTH
        );
        let too_deep = format!("C:\\{}", vec!["d"; MAX_ROOT_DEPTH + 1].join("\\"));
        assert_eq!(
            NormalizedRoot::parse(&too_deep).unwrap_err(),
            RootRejection::TooDeep
        );
    }

    #[test]
    fn long_but_legal_unicode_root_maps_to_its_record() {
        let mut first = volume("C:\\", "first");
        let long = "长".repeat(90);
        first.upsert(frn(10, 1), frn(5, 0), "项目", true).unwrap();
        first.upsert(frn(11, 1), frn(10, 1), &long, true).unwrap();
        let index = IndexState {
            volumes: vec![first],
            generation: 1,
            events_since_checkpoint: 0,
        };

        let scope = RootScope::resolve(&index, &format!("c:/项目/{long}/")).unwrap();
        assert_eq!(scope.bound().volume_index, 0);
        assert_eq!(scope.bound().root_record, 11);
        assert_eq!(scope.display(), format!(r"C:\项目\{long}"));
    }

    #[test]
    fn mapping_resolves_volume_root_nested_dirs_and_case_differences() {
        let index = index();
        let root = RootScope::resolve(&index, "C:\\").unwrap();
        assert_eq!(root.bound().root_record, index.volumes[0].root_record);

        let nested = RootScope::resolve(&index, r"c:\项目\SUB").unwrap();
        assert_eq!(nested.bound().volume_index, 0);
        assert_eq!(nested.bound().root_record, 11);
        assert_eq!(nested.display(), r"C:\项目\SUB");

        let other_volume = RootScope::resolve(&index, r"D:\data").unwrap();
        assert_eq!(other_volume.bound().volume_index, 1);
        assert_eq!(other_volume.bound().root_record, 10);
    }

    #[test]
    fn mapping_reports_structured_degradation_reasons() {
        let index = index();
        assert_eq!(
            RootScope::resolve(&index, r"E:\somewhere").unwrap_err(),
            RootRejection::VolumeNotIndexed,
            "an unindexed or non-NTFS volume degrades explicitly"
        );
        assert_eq!(
            RootScope::resolve(&index, r"C:\项目\missing").unwrap_err(),
            RootRejection::NotFound
        );
        assert_eq!(
            RootScope::resolve(&index, r"C:\项目\report.txt").unwrap_err(),
            RootRejection::NotADirectory
        );
        assert_eq!(
            RootScope::resolve(&index, r"C:\sub").unwrap_err(),
            RootRejection::NotFound,
            "a nested name must not match at the wrong depth"
        );
    }

    #[test]
    fn deleted_root_directory_no_longer_resolves() {
        let mut index = index();
        index.volumes[0].delete(frn(11, 1)).unwrap();
        assert_eq!(
            RootScope::resolve(&index, r"C:\项目\sub").unwrap_err(),
            RootRejection::NotFound
        );
    }

    #[test]
    fn root_inside_an_excluded_subtree_is_reported_as_absent() {
        let mut first = volume("C:\\", "first");
        first.upsert(frn(10, 1), frn(5, 0), "项目", true).unwrap();
        first
            .upsert(frn(11, 1), frn(10, 1), "node_modules", true)
            .unwrap();
        first.upsert(frn(12, 1), frn(11, 1), "pkg", true).unwrap();
        let index = IndexState {
            volumes: vec![first],
            generation: 1,
            events_since_checkpoint: 0,
        };

        // Excluded subtrees hold no searchable entries, so scoping into one must degrade
        // explicitly instead of resolving to a root that can only return nothing.
        assert_eq!(
            RootScope::resolve(&index, r"C:\项目\node_modules").unwrap_err(),
            RootRejection::NotFound
        );
        assert_eq!(
            RootScope::resolve(&index, r"C:\项目\node_modules\pkg").unwrap_err(),
            RootRejection::NotFound
        );
        assert_eq!(
            RootScope::resolve(&index, r"C:\项目")
                .unwrap()
                .bound()
                .root_record,
            10
        );
    }

    #[test]
    fn rejection_reasons_are_stable_strings() {
        assert_eq!(RootRejection::AccessDenied.reason(), "access_denied");
        assert_eq!(
            serde_json::to_string(&RootRejection::VolumeNotIndexed).unwrap(),
            "\"volume_not_indexed\""
        );
        assert!(!RootRejection::TooDeep.message().is_empty());
    }
}
