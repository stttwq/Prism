//! Compact, versioned pinyin sidecar owned by the indexer service.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};
#[cfg(not(windows))]
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::hierarchy::{
    ExclusionMatcher, IndexState, MatchKind, MatchMetadata, QueryFilters, RootBound, RootFilter,
    FLAG_DIRECTORY, FLAG_PRESENT,
};
use crate::pinyin::{
    encode_compact, match_compact_normalized, normalize_query, PinyinMatch, PinyinMatchKind,
    PINYIN_DICTIONARY_VERSION,
};

const MAGIC: [u8; 8] = *b"PRPYG2\0\0";
const SCHEMA_VERSION: u32 = 1;
const FILE_NAME: &str = "pinyin-v1.bin";
const MAX_DELTA_RECORDS: usize = 4096;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct RecordKey {
    volume: u16,
    record: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct SidecarRecord {
    key: RecordKey,
    offset: u32,
    length: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SidecarDisk {
    magic: [u8; 8],
    schema_version: u32,
    dictionary_version: String,
    index_generation: u64,
    index_identity: u64,
    volume_count: u16,
    records: Vec<SidecarRecord>,
    payload: Vec<u8>,
    checksum: u64,
}

#[derive(Debug)]
pub struct PinyinSidecar {
    disk: SidecarDisk,
    delta: BTreeMap<RecordKey, Option<Vec<u8>>>,
    #[cfg(windows)]
    mapping: Option<MappedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinyinHit {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub match_metadata: MatchMetadata,
    pub match_spans: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinyinSearchOutcome {
    pub items: Vec<PinyinHit>,
    pub matched_count: u64,
    pub path_constructions: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadErrorKind {
    Missing,
    Corrupt,
    VersionMismatch,
    IndexMismatch,
    Io,
}

#[derive(Debug, Clone)]
pub struct LoadError {
    pub kind: LoadErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrototypeReport {
    pub corpus_names: usize,
    pub pinyin_records: usize,
    pub build_ms: f64,
    pub encoded_bytes: usize,
    pub resident_bytes: usize,
    pub ascii_no_hit_p95_ms: f64,
    pub pinyin_p95_ms: f64,
    pub iterations: usize,
}

pub fn prototype_names(names: &[String], iterations: usize) -> Result<PrototypeReport, String> {
    let started = Instant::now();
    let mut records = Vec::new();
    let mut payload = Vec::new();
    for (record, name) in names.iter().enumerate() {
        let Some(encoded) = encode_compact(name) else {
            continue;
        };
        let offset = u32::try_from(payload.len()).map_err(|_| "prototype payload exceeds u32")?;
        let length = u16::try_from(encoded.len()).map_err(|_| "prototype record exceeds u16")?;
        records.push(SidecarRecord {
            key: RecordKey {
                volume: 0,
                record: record as u32,
            },
            offset,
            length,
        });
        payload.extend_from_slice(&encoded);
    }
    records.shrink_to_fit();
    payload.shrink_to_fit();
    let mut disk = SidecarDisk {
        magic: MAGIC,
        schema_version: SCHEMA_VERSION,
        dictionary_version: PINYIN_DICTIONARY_VERSION.to_owned(),
        index_generation: 1,
        index_identity: 1,
        volume_count: 1,
        records,
        payload,
        checksum: 0,
    };
    disk.checksum = content_checksum(&disk);
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;
    let encoded_bytes = postcard::to_allocvec(&disk)
        .map_err(|error| error.to_string())?
        .len();
    let resident_bytes = disk.records.capacity() * std::mem::size_of::<SidecarRecord>()
        + disk.payload.capacity()
        + encoded_bytes;

    let ascii = normalize_query("prismg2nohit").ok_or("invalid ASCII probe")?;
    let pinyin = normalize_query("weixin").ok_or("invalid pinyin probe")?;
    let iterations = iterations.max(1);
    let ascii_no_hit_p95_ms = prototype_p95(&disk, ascii.as_bytes(), iterations);
    let pinyin_p95_ms = prototype_p95(&disk, pinyin.as_bytes(), iterations);
    Ok(PrototypeReport {
        corpus_names: names.len(),
        pinyin_records: disk.records.len(),
        build_ms,
        encoded_bytes,
        resident_bytes,
        ascii_no_hit_p95_ms,
        pinyin_p95_ms,
        iterations,
    })
}

fn prototype_p95(disk: &SidecarDisk, query: &[u8], iterations: usize) -> f64 {
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        let mut matches = 0usize;
        for record in &disk.records {
            let start = record.offset as usize;
            let end = start + record.length as usize;
            if disk
                .payload
                .get(start..end)
                .and_then(|bytes| match_compact_normalized(bytes, query))
                .is_some()
            {
                matches = matches.saturating_add(1);
            }
        }
        std::hint::black_box(matches);
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    samples[((samples.len() * 95).saturating_sub(1)) / 100]
}

impl PinyinSidecar {
    pub fn build(index: &IndexState) -> Result<Self, String> {
        let volume_count = u16::try_from(index.volumes.len())
            .map_err(|_| "pinyin sidecar has too many volumes")?;
        let mut records = Vec::new();
        let mut payload = Vec::new();
        for (volume_number, volume) in index.volumes.iter().enumerate() {
            let volume_number =
                u16::try_from(volume_number).map_err(|_| "pinyin volume number exceeds u16")?;
            for (record, slot) in volume.nodes.iter().enumerate() {
                if slot.flags & FLAG_PRESENT == 0 {
                    continue;
                }
                let Ok(name) = volume.name_at(slot.name_off) else {
                    continue;
                };
                let Some(encoded) = encode_compact(name) else {
                    continue;
                };
                let offset =
                    u32::try_from(payload.len()).map_err(|_| "pinyin payload exceeds u32")?;
                let length =
                    u16::try_from(encoded.len()).map_err(|_| "one pinyin record exceeds u16")?;
                records.push(SidecarRecord {
                    key: RecordKey {
                        volume: volume_number,
                        record: record as u32,
                    },
                    offset,
                    length,
                });
                payload.extend_from_slice(&encoded);
            }
        }
        let mut disk = SidecarDisk {
            magic: MAGIC,
            schema_version: SCHEMA_VERSION,
            dictionary_version: PINYIN_DICTIONARY_VERSION.to_owned(),
            index_generation: index.generation,
            index_identity: index_identity(index),
            volume_count,
            records,
            payload,
            checksum: 0,
        };
        disk.checksum = content_checksum(&disk);
        Ok(Self {
            disk,
            delta: BTreeMap::new(),
            #[cfg(windows)]
            mapping: None,
        })
    }

    pub fn load(data_dir: &Path, index: &IndexState) -> Result<Self, LoadError> {
        let path = path(data_dir);
        #[cfg(windows)]
        let mapping = MappedFile::open(&path)?;
        #[cfg(windows)]
        let bytes = mapping.bytes();
        #[cfg(not(windows))]
        let bytes = {
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .map_err(io_load_error)?
                .read_to_end(&mut bytes)
                .map_err(io_load_error)?;
            bytes
        };
        let disk: SidecarDisk = postcard::from_bytes(bytes).map_err(|error| LoadError {
            kind: LoadErrorKind::Corrupt,
            message: error.to_string(),
        })?;
        validate_disk(&disk, index)?;
        Ok(Self {
            disk,
            delta: BTreeMap::new(),
            #[cfg(windows)]
            mapping: Some(mapping),
        })
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        if !self.delta.is_empty() {
            return Err("pinyin sidecar must be rebuilt before saving".into());
        }
        std::fs::create_dir_all(data_dir)
            .map_err(|error| format!("create pinyin directory: {error}"))?;
        let bytes = postcard::to_allocvec(&self.disk)
            .map_err(|error| format!("encode pinyin sidecar: {error}"))?;
        let destination = path(data_dir);
        let temporary = data_dir.join(format!("{FILE_NAME}.tmp"));
        let mut file = std::fs::File::create(&temporary)
            .map_err(|error| format!("create pinyin temporary file: {error}"))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("write pinyin temporary file: {error}"))?;
        drop(file);
        atomic_replace(&temporary, &destination)
    }

    pub fn encoded_bytes(&self) -> Result<usize, String> {
        postcard::to_allocvec(&self.disk)
            .map(|bytes| bytes.len())
            .map_err(|error| error.to_string())
    }

    pub fn resident_bytes(&self) -> usize {
        let heap = self.disk.records.capacity() * std::mem::size_of::<SidecarRecord>()
            + self.disk.payload.capacity()
            + self
                .delta
                .values()
                .filter_map(Option::as_ref)
                .map(Vec::capacity)
                .sum::<usize>();
        #[cfg(windows)]
        {
            heap + self.mapping.as_ref().map(MappedFile::len).unwrap_or(0)
        }
        #[cfg(not(windows))]
        {
            heap
        }
    }

    pub fn apply_delta(
        &mut self,
        volume: usize,
        record: u32,
        name: Option<&str>,
    ) -> Result<bool, String> {
        let volume = u16::try_from(volume).map_err(|_| "pinyin volume exceeds u16")?;
        let key = RecordKey { volume, record };
        if !self.delta.contains_key(&key) && self.delta.len() >= MAX_DELTA_RECORDS {
            return Ok(true);
        }
        let value = name.and_then(encode_compact);
        self.delta.insert(key, value);
        Ok(self.delta.len() >= MAX_DELTA_RECORDS)
    }

    pub fn search(&self, index: &IndexState, query: &str, max: usize) -> PinyinSearchOutcome {
        self.search_with_exclusions(index, query, max, &[])
    }

    pub fn search_with_exclusions(
        &self,
        index: &IndexState,
        query: &str,
        max: usize,
        exclusion_paths: &[String],
    ) -> PinyinSearchOutcome {
        self.search_in_root(index, query, max, exclusion_paths, None, &QueryFilters::none())
    }

    /// Pinyin candidates are filtered by the same root rule as the literal path, before
    /// the Top-K heap, so a current-directory search never leaks matches from elsewhere.
    /// G7: ext/path filters are applied here too, before the Top-K heap.
    pub fn search_in_root(
        &self,
        index: &IndexState,
        query: &str,
        max: usize,
        exclusion_paths: &[String],
        root: Option<RootBound>,
        filters: &QueryFilters,
    ) -> PinyinSearchOutcome {
        let Some(normalized) = normalize_query(query) else {
            return PinyinSearchOutcome {
                items: Vec::new(),
                matched_count: 0,
                path_constructions: 0,
            };
        };
        let exclusions = ExclusionMatcher::new(exclusion_paths);
        let mut root_filter = root.map(RootFilter::new);
        let mut heap = BinaryHeap::with_capacity(max);
        let mut matched_count = 0u64;
        let mut path_constructions = 0u64;
        let has_path_filter = filters.has_path_filter();
        for record in &self.disk.records {
            if self.delta.contains_key(&record.key) {
                continue;
            }
            if key_is_excluded(index, record.key, &exclusions) {
                continue;
            }
            if !key_is_in_root(index, record.key, root_filter.as_mut()) {
                continue;
            }
            let start = record.offset as usize;
            let end = start.saturating_add(record.length as usize);
            let Some(bytes) = self.disk.payload.get(start..end) else {
                continue;
            };
            if let Some(matched) = match_compact_normalized(bytes, normalized.as_bytes()) {
                if key_is_literal(index, record.key, query) {
                    continue;
                }
                if !key_passes_filters(index, record.key, filters, has_path_filter, &mut path_constructions) {
                    continue;
                }
                matched_count = matched_count.saturating_add(1);
                push_candidate(&mut heap, index, record.key, matched, max, &exclusions);
            }
        }
        for (key, encoded) in &self.delta {
            let Some(bytes) = encoded else {
                continue;
            };
            if key_is_excluded(index, *key, &exclusions) {
                continue;
            }
            if !key_is_in_root(index, *key, root_filter.as_mut()) {
                continue;
            }
            if let Some(matched) = match_compact_normalized(bytes, normalized.as_bytes()) {
                if key_is_literal(index, *key, query) {
                    continue;
                }
                if !key_passes_filters(index, *key, filters, has_path_filter, &mut path_constructions) {
                    continue;
                }
                matched_count = matched_count.saturating_add(1);
                push_candidate(&mut heap, index, *key, matched, max, &exclusions);
            }
        }

        let ranked = heap.into_sorted_vec();
        path_constructions = path_constructions.saturating_add(ranked.len() as u64);
        let items = ranked
            .into_iter()
            .filter_map(|candidate| candidate.into_hit(index))
            .collect();
        PinyinSearchOutcome {
            items,
            matched_count,
            path_constructions,
        }
    }
}

fn key_is_literal(index: &IndexState, key: RecordKey, query: &str) -> bool {
    index
        .volumes
        .get(key.volume as usize)
        .and_then(|volume| {
            volume
                .nodes
                .get(key.record as usize)
                .map(|slot| (volume, slot))
        })
        .and_then(|(volume, slot)| volume.name_at(slot.name_off).ok())
        .is_some_and(|name| crate::hierarchy::is_literal_match(name, query))
}

#[derive(Debug, Eq)]
struct Candidate {
    key: RecordKey,
    name: String,
    mount_path: String,
    is_directory: bool,
    matched: PinyinMatch,
}

impl Candidate {
    fn metadata(&self) -> MatchMetadata {
        MatchMetadata {
            kind: match self.matched.kind {
                PinyinMatchKind::Full => MatchKind::FullPinyin,
                PinyinMatchKind::Initials => MatchKind::Initials,
            },
            class: self.matched.class,
            position: self.matched.position,
            score: self.matched.score,
            history_score: 0,
        }
    }

    fn into_hit(self, index: &IndexState) -> Option<PinyinHit> {
        let volume = index.volumes.get(self.key.volume as usize)?;
        let path = volume.path_for(self.key.record).ok()?;
        let match_metadata = self.metadata();
        Some(PinyinHit {
            name: self.name,
            path,
            is_directory: self.is_directory,
            match_metadata,
            match_spans: self.matched.spans,
        })
    }
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.metadata()
            .cmp(&other.metadata())
            .then_with(|| self.name.cmp(&other.name))
            .then_with(|| self.mount_path.cmp(&other.mount_path))
            .then(self.key.cmp(&other.key))
            .then(self.is_directory.cmp(&other.is_directory))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn push_candidate(
    heap: &mut BinaryHeap<Candidate>,
    index: &IndexState,
    key: RecordKey,
    matched: PinyinMatch,
    max: usize,
    exclusions: &ExclusionMatcher,
) {
    if max == 0 {
        return;
    }
    if key_is_excluded(index, key, exclusions) {
        return;
    }
    let Some(volume) = index.volumes.get(key.volume as usize) else {
        return;
    };
    let Some(slot) = volume.nodes.get(key.record as usize) else {
        return;
    };
    if slot.flags & FLAG_PRESENT == 0 {
        return;
    }
    let Ok(name) = volume.name_at(slot.name_off) else {
        return;
    };
    let candidate = Candidate {
        key,
        name: name.to_owned(),
        mount_path: volume.mount_path.clone(),
        is_directory: slot.flags & FLAG_DIRECTORY != 0,
        matched,
    };
    if heap.len() < max {
        heap.push(candidate);
    } else if heap.peek().is_some_and(|worst| candidate < *worst) {
        heap.pop();
        heap.push(candidate);
    }
}

fn key_is_excluded(index: &IndexState, key: RecordKey, exclusions: &ExclusionMatcher) -> bool {
    index
        .volumes
        .get(key.volume as usize)
        .is_none_or(|volume| exclusions.matches(volume, key.record))
}

fn key_is_in_root(
    index: &IndexState,
    key: RecordKey,
    root_filter: Option<&mut RootFilter>,
) -> bool {
    let Some(filter) = root_filter else {
        return true;
    };
    index
        .volumes
        .get(key.volume as usize)
        .is_some_and(|volume| filter.accepts(key.volume as usize, volume, key.record))
}

/// G7: applies ext/path filters to a pinyin candidate before it enters the Top-K heap.
/// Ext is a low-cost name check; path requires constructing the full path (high cost),
/// counted in `path_constructions`. Returns true when the candidate passes all filters.
fn key_passes_filters(
    index: &IndexState,
    key: RecordKey,
    filters: &QueryFilters,
    has_path_filter: bool,
    path_constructions: &mut u64,
) -> bool {
    let Some(volume) = index.volumes.get(key.volume as usize) else {
        return false;
    };
    let Some(slot) = volume.nodes.get(key.record as usize) else {
        return false;
    };
    let Ok(name) = volume.name_at(slot.name_off) else {
        return false;
    };
    let is_directory = slot.flags & FLAG_DIRECTORY != 0;
    if !filters.ext_matches(name, is_directory) {
        return false;
    }
    if has_path_filter {
        *path_constructions = path_constructions.saturating_add(1);
        match volume.path_for(key.record) {
            Ok(path) => {
                if !filters.path_matches(&path) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

fn validate_disk(disk: &SidecarDisk, index: &IndexState) -> Result<(), LoadError> {
    if disk.magic != MAGIC || disk.schema_version != SCHEMA_VERSION {
        return Err(LoadError {
            kind: LoadErrorKind::VersionMismatch,
            message: "pinyin sidecar schema mismatch".into(),
        });
    }
    if disk.dictionary_version != PINYIN_DICTIONARY_VERSION {
        return Err(LoadError {
            kind: LoadErrorKind::VersionMismatch,
            message: "pinyin dictionary version mismatch".into(),
        });
    }
    if disk.index_identity != index_identity(index)
        || usize::from(disk.volume_count) != index.volumes.len()
    {
        return Err(LoadError {
            kind: LoadErrorKind::IndexMismatch,
            message: "pinyin sidecar index identity mismatch".into(),
        });
    }
    if disk.checksum != content_checksum(disk) {
        return Err(LoadError {
            kind: LoadErrorKind::Corrupt,
            message: "pinyin sidecar checksum mismatch".into(),
        });
    }
    let mut previous = None;
    for record in &disk.records {
        if previous.is_some_and(|key| key >= record.key)
            || record.key.volume >= disk.volume_count
            || record.offset as usize + record.length as usize > disk.payload.len()
        {
            return Err(LoadError {
                kind: LoadErrorKind::Corrupt,
                message: "pinyin sidecar record table is invalid".into(),
            });
        }
        previous = Some(record.key);
    }
    Ok(())
}

fn index_identity(index: &IndexState) -> u64 {
    let mut hash = FNV_OFFSET;
    for volume in &index.volumes {
        hash_bytes(&mut hash, volume.volume_id.guid.as_bytes());
        hash_bytes(&mut hash, &volume.volume_id.serial.to_le_bytes());
        hash_bytes(&mut hash, &volume.root_record.to_le_bytes());
        hash_bytes(&mut hash, &(volume.nodes.len() as u64).to_le_bytes());
        hash_bytes(&mut hash, &(volume.names.len() as u64).to_le_bytes());
        hash_bytes(&mut hash, &volume.names);
    }
    hash
}

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn content_checksum(disk: &SidecarDisk) -> u64 {
    let mut hash = FNV_OFFSET;
    for record in &disk.records {
        hash_bytes(&mut hash, &record.key.volume.to_le_bytes());
        hash_bytes(&mut hash, &record.key.record.to_le_bytes());
        hash_bytes(&mut hash, &record.offset.to_le_bytes());
        hash_bytes(&mut hash, &record.length.to_le_bytes());
    }
    hash_bytes(&mut hash, &disk.payload);
    hash
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn io_load_error(error: std::io::Error) -> LoadError {
    LoadError {
        kind: if error.kind() == std::io::ErrorKind::NotFound {
            LoadErrorKind::Missing
        } else {
            LoadErrorKind::Io
        },
        message: error.to_string(),
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct MappedFile {
    _file: std::fs::File,
    mapping: windows::Win32::Foundation::HANDLE,
    view: windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
}

#[cfg(windows)]
unsafe impl Send for MappedFile {}
#[cfg(windows)]
unsafe impl Sync for MappedFile {}

#[cfg(windows)]
impl MappedFile {
    fn open(path: &Path) -> Result<Self, LoadError> {
        use std::os::windows::io::AsRawHandle;
        use windows::core::Error;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::Memory::{
            CreateFileMappingW, MapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
        };

        let file = std::fs::File::open(path).map_err(io_load_error)?;
        let len = usize::try_from(file.metadata().map_err(io_load_error)?.len()).map_err(|_| {
            LoadError {
                kind: LoadErrorKind::Corrupt,
                message: "pinyin sidecar is too large for this process".into(),
            }
        })?;
        if len == 0 {
            return Err(LoadError {
                kind: LoadErrorKind::Corrupt,
                message: "pinyin sidecar is empty".into(),
            });
        }
        let file_handle = HANDLE(file.as_raw_handle());
        let mapping = unsafe { CreateFileMappingW(file_handle, None, PAGE_READONLY, 0, 0, None) }
            .map_err(|error| LoadError {
            kind: LoadErrorKind::Io,
            message: error.to_string(),
        })?;
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, len) };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(mapping);
            }
            return Err(LoadError {
                kind: LoadErrorKind::Io,
                message: Error::from_win32().to_string(),
            });
        }
        Ok(Self {
            _file: file,
            mapping,
            view,
            len,
        })
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.view.Value.cast::<u8>(), self.len) }
    }

    fn len(&self) -> usize {
        self.len
    }
}

#[cfg(windows)]
impl Drop for MappedFile {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Memory::UnmapViewOfFile;
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.mapping);
        }
    }
}

#[cfg(windows)]
fn atomic_replace(temporary: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};

    if !destination.exists() {
        return std::fs::rename(temporary, destination)
            .map_err(|error| format!("install initial pinyin sidecar: {error}"));
    }
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let temporary: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        ReplaceFileW(
            PCWSTR(destination.as_ptr()),
            PCWSTR(temporary.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    }
    .map_err(|error| format!("replace pinyin sidecar: {error}"))
}

#[cfg(not(windows))]
fn atomic_replace(temporary: &Path, destination: &Path) -> Result<(), String> {
    std::fs::rename(temporary, destination).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::{VolumeId, VolumeIndex};

    fn state() -> IndexState {
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "volume".into(),
                serial: 7,
            },
            "C:\\".into(),
            10,
            20,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "微信2026", false).unwrap();
        volume.upsert(11, 5, "重庆", true).unwrap();
        volume.upsert(12, 5, "literal.txt", false).unwrap();
        IndexState {
            volumes: vec![volume],
            generation: 9,
            events_since_checkpoint: 0,
        }
    }

    #[test]
    fn compact_sidecar_matches_and_maps_utf16_spans() {
        let sidecar = PinyinSidecar::build(&state()).unwrap();
        assert_eq!(sidecar.disk.records.len(), 2);
        let outcome = sidecar.search(&state(), "wx2026", 8);
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(outcome.items[0].path, "C:\\微信2026");
        assert_eq!(outcome.items[0].match_spans, [0, 6]);
        assert_eq!(outcome.items[0].match_metadata.kind, MatchKind::Initials);
    }

    #[test]
    fn root_scope_filters_pinyin_hits_from_outside_the_root() {
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "volume".into(),
                serial: 7,
            },
            "C:\\".into(),
            10,
            20,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "项目", true).unwrap();
        volume.upsert(11, 10, "微信", false).unwrap();
        volume.upsert(12, 5, "微信", false).unwrap();
        volume.upsert(13, 5, "别处", true).unwrap();
        volume.upsert(14, 13, "微信", false).unwrap();
        let index = IndexState {
            volumes: vec![volume],
            generation: 9,
            events_since_checkpoint: 0,
        };
        let mut sidecar = PinyinSidecar::build(&index).unwrap();
        let root = Some(RootBound {
            volume_index: 0,
            root_record: 10,
        });

        assert_eq!(sidecar.search(&index, "wx", 8).items.len(), 3);
        let scoped = sidecar.search_in_root(&index, "wx", 8, &[], root, &QueryFilters::none());
        assert_eq!(scoped.items.len(), 1, "{:?}", scoped.items);
        assert_eq!(scoped.items[0].path, "C:\\项目\\微信");
        assert_eq!(
            scoped.matched_count, 1,
            "out-of-root candidates never enter Top-K accounting"
        );

        // The delta path applies the same rule: a rename inside the root stays visible and
        // a rename outside it does not leak in.
        sidecar.apply_delta(0, 11, Some("支付宝")).unwrap();
        sidecar.apply_delta(0, 12, Some("支付宝")).unwrap();
        let delta_scoped = sidecar.search_in_root(&index, "zfb", 8, &[], root, &QueryFilters::none());
        assert_eq!(delta_scoped.items.len(), 1, "{:?}", delta_scoped.items);
        assert_eq!(delta_scoped.items[0].path, "C:\\项目\\微信");
        assert_eq!(delta_scoped.matched_count, 1);
    }

    #[test]
    fn delta_rename_and_tombstone_override_main_records() {
        let index = state();
        let mut sidecar = PinyinSidecar::build(&index).unwrap();
        sidecar.apply_delta(0, 10, Some("支付宝")).unwrap();
        sidecar.apply_delta(0, 11, None).unwrap();
        assert!(sidecar.search(&index, "wx", 8).items.is_empty());
        assert_eq!(sidecar.search(&index, "zfb", 8).items.len(), 1);
        assert!(sidecar.search(&index, "cq", 8).items.is_empty());
    }

    #[test]
    fn persisted_sidecar_rejects_corruption_dictionary_and_index_mismatch() {
        let index = state();
        let sidecar = PinyinSidecar::build(&index).unwrap();
        let dir = std::env::temp_dir().join(format!("prism-pinyin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        sidecar.save(&dir).unwrap();
        let loaded = PinyinSidecar::load(&dir, &index).unwrap();
        assert_eq!(loaded.disk.records.len(), 2);
        #[cfg(windows)]
        assert!(loaded.mapping.is_some());
        drop(loaded);

        let mut different = index.clone();
        different.volumes[0].upsert(13, 5, "新增", false).unwrap();
        assert_eq!(
            PinyinSidecar::load(&dir, &different).unwrap_err().kind,
            LoadErrorKind::IndexMismatch
        );
        let mut wrong_dictionary = sidecar.disk.clone();
        wrong_dictionary.dictionary_version = "future-dictionary".into();
        std::fs::write(
            path(&dir),
            postcard::to_allocvec(&wrong_dictionary).unwrap(),
        )
        .unwrap();
        assert_eq!(
            PinyinSidecar::load(&dir, &index).unwrap_err().kind,
            LoadErrorKind::VersionMismatch
        );
        std::fs::write(path(&dir), b"corrupt").unwrap();
        assert_eq!(
            PinyinSidecar::load(&dir, &index).unwrap_err().kind,
            LoadErrorKind::Corrupt
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn node_slot_size_is_unchanged_by_sidecar() {
        assert_eq!(std::mem::size_of::<crate::hierarchy::NodeSlot>(), 12);
    }

    #[test]
    fn exclusions_apply_before_pinyin_top_k() {
        let mut index = state();
        index.volumes[0].upsert(20, 5, "秘密", true).unwrap();
        index.volumes[0].upsert(21, 20, "微信", false).unwrap();
        let sidecar = PinyinSidecar::build(&index).unwrap();
        let outcome = sidecar.search_with_exclusions(&index, "wx", 8, &["C:\\秘密".to_owned()]);
        assert!(outcome
            .items
            .iter()
            .all(|item| item.path != "C:\\秘密\\微信"));
        assert_eq!(outcome.matched_count, 1);
    }

    #[test]
    fn pinyin_does_not_match_a_child_through_its_parent_path() {
        let mut index = state();
        index.volumes[0].upsert(20, 5, "微信目录", true).unwrap();
        index.volumes[0].upsert(21, 20, "notes.txt", false).unwrap();
        let sidecar = PinyinSidecar::build(&index).unwrap();
        let outcome = sidecar.search(&index, "wx", 8);
        assert!(outcome.items.iter().any(|item| item.name == "微信目录"));
        assert!(outcome.items.iter().all(|item| item.name != "notes.txt"));
    }

    #[test]
    fn delta_never_grows_past_the_rebuild_threshold() {
        let mut sidecar = PinyinSidecar::build(&state()).unwrap();
        for record in 0..MAX_DELTA_RECORDS as u32 {
            let rebuild = sidecar.apply_delta(0, record, Some("微信")).unwrap();
            assert_eq!(rebuild, record as usize + 1 >= MAX_DELTA_RECORDS);
        }
        assert_eq!(sidecar.delta.len(), MAX_DELTA_RECORDS);
        assert!(sidecar
            .apply_delta(0, MAX_DELTA_RECORDS as u32, Some("微信"))
            .unwrap());
        assert_eq!(sidecar.delta.len(), MAX_DELTA_RECORDS);
    }
}
