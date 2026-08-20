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
    ExclusionMatcher, IndexState, MatchKind, MatchMetadata, NameTerms, QueryFilters, RootBound,
    RootFilter, FLAG_DIRECTORY, FLAG_PRESENT,
};
use crate::pinyin::{
    encode_compact, match_compact_normalized, normalize_query, PinyinMatch, PinyinMatchKind,
    PINYIN_DICTIONARY_VERSION,
};

const MAGIC: [u8; 8] = *b"PRPYG2\0\0";
const SCHEMA_VERSION: u32 = 1;
const FILE_NAME: &str = "pinyin-v1.bin";
const MAX_DELTA_RECORDS: usize = 4096;
/// M3（FRESH-AUDIT-3-2026-08-20）：delta 总条数硬上限。纯英文条目不触发重建
///（不占重建阈值），但它们仍要留在 delta 里掩蔽陈旧编码，总量必须封顶兜底内存
///（约 32k 条 × BTreeMap 节点 ≈ 数 MB），到达即重建冲刷。
const MAX_DELTA_TOTAL_RECORDS: usize = 32 * 1024;

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

/// A1（AUDIT-4 批次B，2026-08-21）：增量表从 [`PinyinSidecar`] 拆出。
/// 此前 delta 活在 sidecar 里，USN 批次写入用 `Arc::make_mut` COW 深克隆整份
/// sidecar（几十 MB）——击键搜索几乎总在飞，浏览器/WU 持续写盘时每批次克隆一次，
/// 分配 churn + 瞬时 2× 常驻。拆出后主表 `Arc` 快照永不变异，watcher 只写这张
/// 独立的小表；字节格式不变（delta 本就不序列化）。
#[derive(Debug, Default)]
pub struct PinyinDelta {
    entries: BTreeMap<RecordKey, Option<Vec<u8>>>,
    /// M3（FRESH-AUDIT-3-2026-08-20）：含汉字编码的 delta 条数。纯英文名变更
    /// （npm install / Windows Update 风暴）不计入重建阈值，杜绝"每 4096 条
    /// 英文变更触发一次全量重建"的周期性整索引 clone + 全节点编码 + fsync。
    chinese_count: usize,
}

impl PinyinDelta {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.chinese_count = 0;
    }

    pub fn resident_bytes(&self) -> usize {
        self.entries
            .values()
            .filter_map(Option::as_ref)
            .map(Vec::capacity)
            .sum()
    }

    /// 记录一次名字变更（`name=None` 表示删除）。返回 true 表示到达重建阈值，
    /// 调用方应卸载主表并排队全量重建。
    pub fn apply(
        &mut self,
        volume: usize,
        record: u32,
        name: Option<&str>,
    ) -> Result<bool, String> {
        let volume = u16::try_from(volume).map_err(|_| "pinyin volume exceeds u16")?;
        let key = RecordKey { volume, record };
        let value = name.and_then(encode_compact);
        let counts_chinese = value.is_some();
        if let Some(existing) = self.entries.get(&key) {
            // 覆盖既有条目：按旧新取值增减汉字计数（中文→英文 rename 释放容量）。
            let counted_before = existing.is_some();
            self.chinese_count = self
                .chinese_count
                .saturating_add(usize::from(counts_chinese))
                .saturating_sub(usize::from(counted_before));
        } else {
            // M3（FRESH-AUDIT-3-2026-08-20）：重建阈值只看含汉字编码的条数——
            // 纯英文名变更（encode_compact=None）不再触发周期性全量重建；
            // 总条数硬上限兜底内存（纯英文条目也要留下掩蔽陈旧编码）。
            if counts_chinese && self.chinese_count >= MAX_DELTA_RECORDS {
                return Ok(true);
            }
            if self.entries.len() >= MAX_DELTA_TOTAL_RECORDS {
                return Ok(true);
            }
            self.chinese_count += usize::from(counts_chinese);
        }
        self.entries.insert(key, value);
        Ok(self.chinese_count >= MAX_DELTA_RECORDS
            || self.entries.len() >= MAX_DELTA_TOTAL_RECORDS)
    }
}

/// 主表（磁盘字节格式的内存映像）。不可变共享：搜索只拿 `Arc` 快照，
/// 增量变更全部走 [`PinyinDelta`]（A1，AUDIT-4 批次B）。
#[derive(Debug, Clone)]
pub struct PinyinSidecar {
    disk: SidecarDisk,
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
        Ok(Self { disk })
    }

    pub fn load(data_dir: &Path, index: &IndexState) -> Result<Self, LoadError> {
        let identity = index_identity(index);
        let volume_count = u16::try_from(index.volumes.len()).unwrap_or(u16::MAX);
        Self::load_with_identity(data_dir, identity, volume_count)
    }

    /// Loads and validates the sidecar without holding the index read lock
    /// during the expensive mmap + deserialization phase.
    ///
    /// Callers compute `identity` and `volume_count` under the read lock, then
    /// release the lock before calling this.  If the index changed between the
    /// snapshot and installation the delta mechanism catches up, so a strict
    /// generation match at install time is not required — only the identity
    /// hash must match the on-disk sidecar.
    pub fn load_with_identity(
        data_dir: &Path,
        identity: u64,
        volume_count: u16,
    ) -> Result<Self, LoadError> {
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
        validate_disk_with(&disk, identity, volume_count)?;
        // 反序列化完成后 mapping 立即随本地变量 drop：mmap 与堆拷贝双份常驻
        // 等于白占一份内存（大卷可达百 MB），而这里的字节此后再无读取。
        Ok(Self { disk })
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        // A1（AUDIT-4 批次B）：delta 已拆出本类型，save 不再校验 delta——
        // 调用方（rebuild 路径）总是从活索引全新构建，无 delta 可言。
        std::fs::create_dir_all(data_dir)
            .map_err(|error| format!("create pinyin directory: {error}"))?;
        let destination = path(data_dir);
        let temporary = data_dir.join(format!("{FILE_NAME}.tmp"));
        let file = std::fs::File::create(&temporary)
            .map_err(|error| format!("create pinyin temporary file: {error}"))?;
        // 流式序列化直接写文件（分块缓冲），不再在堆上生成完整序列化 Vec。
        let mut writer = std::io::BufWriter::with_capacity(256 * 1024, file);
        postcard::to_io(&self.disk, &mut writer)
            .map_err(|error| format!("encode pinyin sidecar: {error}"))?;
        writer
            .flush()
            .and_then(|()| writer.get_ref().sync_all())
            .map_err(|error| format!("write pinyin temporary file: {error}"))?;
        drop(writer);
        crate::fs_util::atomic_replace(&temporary, &destination, "pinyin sidecar")
    }

    pub fn encoded_bytes(&self) -> Result<usize, String> {
        postcard::to_allocvec(&self.disk)
            .map(|bytes| bytes.len())
            .map_err(|error| error.to_string())
    }

    pub fn resident_bytes(&self) -> usize {
        self.disk.records.capacity() * std::mem::size_of::<SidecarRecord>()
            + self.disk.payload.capacity()
    }

    pub fn search(
        &self,
        delta: &PinyinDelta,
        index: &IndexState,
        query: &str,
        max: usize,
    ) -> PinyinSearchOutcome {
        self.search_with_exclusions(delta, index, query, max, &[])
    }

    pub fn search_with_exclusions(
        &self,
        delta: &PinyinDelta,
        index: &IndexState,
        query: &str,
        max: usize,
        exclusion_paths: &[String],
    ) -> PinyinSearchOutcome {
        self.search_in_root(delta, index, query, max, exclusion_paths, None, &QueryFilters::none())
    }

    /// Pinyin candidates are filtered by the same root rule as the literal path, before
    /// the Top-K heap, so a current-directory search never leaks matches from elsewhere.
    /// G7: ext/path filters are applied here too, before the Top-K heap.
    ///
    /// A1（AUDIT-4 批次B）：`delta` 是独立于主表的增量表——主表条目被 delta
    /// 键掩蔽后跳过，delta 条目自己作为候选（`Some`）参与；掩蔽语义与拆分前
    /// 逐条对齐。
    // clippy: 8 参数是搜索入口的既有形状（与字面路径 search_in_root_filtered
    // 同族），再包一层参数结构体只会增加一层解包噪声。
    #[allow(clippy::too_many_arguments)]
    pub fn search_in_root(
        &self,
        delta: &PinyinDelta,
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
        // N1 + S4：字面去重查询在循环外分词一次（口径与字面路径一致）。
        let terms = NameTerms::parse(query);
        for record in &self.disk.records {
            if delta.entries.contains_key(&record.key) {
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
                // matched_count 修正（FRESH-AUDIT-3-2026-08-20）：FLAG_PRESENT 检查
                // 前移——sidecar 陈旧条目（索引侧已删、delta 未覆盖的重启窗口）
                // 不得计入 matched_count，is_truncated 不再虚高。
                if !key_is_live_present(index, record.key) {
                    continue;
                }
                if key_is_literal(index, record.key, &terms) {
                    continue;
                }
                if !key_passes_filters(
                    index,
                    record.key,
                    filters,
                    has_path_filter,
                    &mut path_constructions,
                ) {
                    continue;
                }
                matched_count = matched_count.saturating_add(1);
                push_candidate(&mut heap, index, record.key, matched, max, &exclusions);
            }
        }
        for (key, encoded) in &delta.entries {
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
                if !key_is_live_present(index, *key) {
                    continue;
                }
                if key_is_literal(index, *key, &terms) {
                    continue;
                }
                if !key_passes_filters(
                    index,
                    *key,
                    filters,
                    has_path_filter,
                    &mut path_constructions,
                ) {
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

/// S4（PRISM-IMPL-PLAN-4-2026-08-20）：判「已被字面命中」的口径与字面路径一致
///（NameTerms AND），否则同一条结果会同时以字面项与拼音项双出行或漏行。
/// terms 已降幂（N1：调用方在扫描循环外分词一次）。
fn key_is_literal(index: &IndexState, key: RecordKey, terms: &NameTerms) -> bool {
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
        .is_some_and(|name| {
            terms
                .iter()
                .all(|term| crate::hierarchy::find_case_insensitive(name, term).is_some())
        })
}

/// matched_count 修正（FRESH-AUDIT-3-2026-08-20）：候选在 live 索引里必须仍然
/// 存在（FLAG_PRESENT）。与 push_candidate 的拒绝口径对齐，已删记录不再计入。
fn key_is_live_present(index: &IndexState, key: RecordKey) -> bool {
    index
        .volumes
        .get(key.volume as usize)
        .and_then(|volume| volume.nodes.get(key.record as usize))
        .is_some_and(|slot| slot.flags & FLAG_PRESENT != 0)
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

fn validate_disk_with(
    disk: &SidecarDisk,
    identity: u64,
    volume_count: u16,
) -> Result<(), LoadError> {
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
    if disk.index_identity != identity || disk.volume_count != volume_count {
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

/// Computes the identity hash for an index state.
///
/// Made `pub(crate)` so callers can pre-compute the hash under a read lock
/// and then release the lock before the expensive mmap + deserialization.
///
/// F3（FRESH-AUDIT-2）：names 池不再逐字节滚入（几十~上百 MB 池在 index.read()
/// 内持锁 ~100ms 级，阻塞 USN 写者）——改用 VolumeIndex 增量维护的
/// `names_fingerprint`（append 滚入 / compact 与缓存载入整算，见 hierarchy），
/// 并叠加 journal_id。升级后首次与旧 sidecar 必然失配，走一轮重建（一次性）。
pub(crate) fn index_identity(index: &IndexState) -> u64 {
    let mut hash = FNV_OFFSET;
    for volume in &index.volumes {
        hash_bytes(&mut hash, volume.volume_id.guid.as_bytes());
        hash_bytes(&mut hash, &volume.volume_id.serial.to_le_bytes());
        hash_bytes(&mut hash, &volume.journal_id.to_le_bytes());
        hash_bytes(&mut hash, &volume.root_record.to_le_bytes());
        hash_bytes(&mut hash, &(volume.nodes.len() as u64).to_le_bytes());
        hash_bytes(&mut hash, &(volume.names.len() as u64).to_le_bytes());
        hash_bytes(&mut hash, &volume.names_fingerprint.to_le_bytes());
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
        let outcome = sidecar.search(&PinyinDelta::new(), &state(), "wx2026", 8);
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(outcome.items[0].path, "C:\\微信2026");
        assert_eq!(outcome.items[0].match_spans, [0, 6]);
        assert_eq!(outcome.items[0].match_metadata.kind, MatchKind::Initials);
    }

    /// A1（AUDIT-4 批次B）：delta 是独立表——主表 Arc 快照永不变异，
    /// 掩蔽全部经 delta 发生。这是拆锁后「主表只读共享」的锚。
    #[test]
    fn a1_delta_masks_without_mutating_the_shared_snapshot() {
        use std::sync::Arc;
        let index = state();
        let sidecar = Arc::new(PinyinSidecar::build(&index).unwrap());
        let mut delta = PinyinDelta::new();
        delta.apply(0, 10, Some("支付宝")).unwrap();
        // 同一不可变主表快照 + delta → 掩蔽生效（wx 不再命中已改名的记录）。
        assert!(sidecar.search(&delta, &index, "wx", 8).items.is_empty());
        assert_eq!(sidecar.search(&delta, &index, "zfb", 8).items.len(), 1);
        // 同一快照不带 delta：主表内容原样（从未被变异）。
        assert_eq!(
            sidecar.search(&PinyinDelta::new(), &index, "wx", 8).items.len(),
            1
        );
    }

    /// F3（FRESH-AUDIT-2）：identity 指纹的稳定性与敏感性。
    /// - 相同追加序列 → 相同 identity（缓存载入后的整算指纹与增量滚入等值）；
    /// - 名字池内容变化（rename 追加新名）→ identity 必变；
    /// - v5 缓存 save/load 往返后 identity 不变（serde skip + 载入整算）。
    #[test]
    fn f3_index_identity_fingerprint_is_stable_and_sensitive() {
        let first = state();
        let mut same = state();
        // 重建等价索引：相同追加顺序（upsert 相同名字）→ 相同指纹与 identity。
        assert_eq!(index_identity(&first), index_identity(&same));

        // 载入侧整算指纹与增量滚入等值。
        same.volumes[0].recompute_derived_counters();
        assert_eq!(index_identity(&first), index_identity(&same));

        // rename 追加新名 → 内容变化 → identity 必变。
        let mut renamed = state();
        renamed.volumes[0].upsert(10, 5, "微信2027", false).unwrap();
        assert_ne!(index_identity(&first), index_identity(&renamed));

        // v5 往返：serde skip 使指纹归零，载入整算后 identity 与活索引一致。
        let dir =
            std::env::temp_dir().join(format!("prism-f3-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::index_cache::save(&first, &dir).unwrap();
        let loaded = crate::index_cache::load(&dir).unwrap();
        assert_eq!(index_identity(&first), index_identity(&loaded));
        let _ = std::fs::remove_dir_all(&dir);
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
        let sidecar = PinyinSidecar::build(&index).unwrap();
        let mut delta = PinyinDelta::new();
        let root = Some(RootBound {
            volume_index: 0,
            root_record: 10,
        });

        assert_eq!(sidecar.search(&delta, &index, "wx", 8).items.len(), 3);
        let scoped = sidecar.search_in_root(&delta, &index, "wx", 8, &[], root, &QueryFilters::none());
        assert_eq!(scoped.items.len(), 1, "{:?}", scoped.items);
        assert_eq!(scoped.items[0].path, "C:\\项目\\微信");
        assert_eq!(
            scoped.matched_count, 1,
            "out-of-root candidates never enter Top-K accounting"
        );

        // The delta path applies the same rule: a rename inside the root stays visible and
        // a rename outside it does not leak in.
        delta.apply(0, 11, Some("支付宝")).unwrap();
        delta.apply(0, 12, Some("支付宝")).unwrap();
        let delta_scoped =
            sidecar.search_in_root(&delta, &index, "zfb", 8, &[], root, &QueryFilters::none());
        assert_eq!(delta_scoped.items.len(), 1, "{:?}", delta_scoped.items);
        assert_eq!(delta_scoped.items[0].path, "C:\\项目\\微信");
        assert_eq!(delta_scoped.matched_count, 1);
    }

    #[test]
    fn delta_rename_and_tombstone_override_main_records() {
        let index = state();
        let sidecar = PinyinSidecar::build(&index).unwrap();
        let mut delta = PinyinDelta::new();
        delta.apply(0, 10, Some("支付宝")).unwrap();
        delta.apply(0, 11, None).unwrap();
        assert!(sidecar.search(&delta, &index, "wx", 8).items.is_empty());
        assert_eq!(sidecar.search(&delta, &index, "zfb", 8).items.len(), 1);
        assert!(sidecar.search(&delta, &index, "cq", 8).items.is_empty());
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
        let outcome =
            sidecar.search_with_exclusions(&PinyinDelta::new(), &index, "wx", 8, &["C:\\秘密".to_owned()]);
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
        let outcome = sidecar.search(&PinyinDelta::new(), &index, "wx", 8);
        assert!(outcome.items.iter().any(|item| item.name == "微信目录"));
        assert!(outcome.items.iter().all(|item| item.name != "notes.txt"));
    }

    #[test]
    fn delta_never_grows_past_the_rebuild_threshold() {
        let mut delta = PinyinDelta::new();
        for record in 0..MAX_DELTA_RECORDS as u32 {
            let rebuild = delta.apply(0, record, Some("微信")).unwrap();
            assert_eq!(rebuild, record as usize + 1 >= MAX_DELTA_RECORDS);
        }
        assert_eq!(delta.entries.len(), MAX_DELTA_RECORDS);
        assert!(delta.apply(0, MAX_DELTA_RECORDS as u32, Some("微信")).unwrap());
        assert_eq!(delta.entries.len(), MAX_DELTA_RECORDS);
    }

    /// M3（FRESH-AUDIT-3-2026-08-20）：纯英文名风暴（npm install / Windows
    /// Update 类）不触发重建——重建阈值只看含汉字编码的条数。
    #[test]
    fn m3_english_name_storm_does_not_trigger_rebuild() {
        let mut delta = PinyinDelta::new();
        for record in 0..(MAX_DELTA_RECORDS as u32 * 2) {
            let rebuild = delta.apply(0, record, Some("body-styles.css")).unwrap();
            assert!(!rebuild, "纯英文名变更不得触发全量重建");
        }
        assert_eq!(delta.chinese_count, 0);
    }

    /// M3：中文→英文 rename 后计数回落，释放的容量可继续容纳新的中文条目。
    #[test]
    fn m3_chinese_to_english_rename_releases_quota() {
        let mut delta = PinyinDelta::new();
        delta.apply(0, 10, Some("微信")).unwrap();
        assert_eq!(delta.chinese_count, 1);
        delta.apply(0, 10, Some("english-name")).unwrap();
        assert_eq!(delta.chinese_count, 0, "中文→英文 rename 计数回落");
        // 释放后不再触发（0 < 阈值），即使 delta 条目本身还在。
        assert!(!delta.apply(0, 11, Some("支付宝")).unwrap());
    }

    /// M3：delta 总条数硬上限兜底内存——纯英文条目也要留下掩蔽陈旧编码，
    /// 但总量到 32k 即触发重建冲刷。
    #[test]
    fn m3_delta_total_entries_have_a_hard_cap() {
        let mut delta = PinyinDelta::new();
        let mut crossed = false;
        for record in 0..MAX_DELTA_TOTAL_RECORDS as u32 {
            if delta.apply(0, record, Some("english")).unwrap() {
                crossed = true;
            }
        }
        assert!(crossed, "总条数到达硬上限必须触发重建");
        assert!(delta
            .apply(0, MAX_DELTA_TOTAL_RECORDS as u32 + 7, Some("more"))
            .unwrap());
    }

    /// matched_count 修正（FRESH-AUDIT-3-2026-08-20）：sidecar 陈旧条目
    ///（索引侧已删、delta 未覆盖——重启后 delta 为空的窗口）不得计入
    /// matched_count，is_truncated 不虚高；items 亦不出现（FLAG_PRESENT）。
    #[test]
    fn matched_count_skips_stale_deleted_records() {
        let mut index = state();
        let sidecar = PinyinSidecar::build(&index).unwrap();
        index.volumes[0].delete(10).unwrap();
        let outcome = sidecar.search(&PinyinDelta::new(), &index, "wx", 8);
        assert!(outcome.items.is_empty());
        assert_eq!(
            outcome.matched_count, 0,
            "已删记录不得计入 matched_count"
        );
    }

    /// S4（PRISM-IMPL-PLAN-4-2026-08-20）：拼音去重口径与字面路径一致——
    /// 名字含**全部** term 的拼音命中被字面路径吸收（不双出行）；只含部分
    /// term 的仍作为拼音结果出行。拼音侧查询经 normalize_query 剥空格成
    /// 单串（「wx zfb」→「wxzfb」首字母整串），去重判定用字面 AND 口径。
    #[test]
    fn s4_pinyin_dedup_uses_multi_term_literal_rule() {
        let mut index = state();
        // 「微信支付宝」拼音首字母 = wxzfb 命中，但字面不含「wx」/「zfb」→ 出行。
        index.volumes[0].upsert(20, 5, "微信支付宝", false).unwrap();
        // 「wxzfb微信」：ASCII 段 wxzfb 全拼命中拼音，同时字面含全部 term → 吸收。
        index.volumes[0].upsert(21, 5, "wxzfb微信", false).unwrap();
        let sidecar = PinyinSidecar::build(&index).unwrap();
        let outcome = sidecar.search(&PinyinDelta::new(), &index, "wx zfb", 8);
        let names: Vec<&str> = outcome.items.iter().map(|item| item.name.as_str()).collect();
        assert!(
            names.contains(&"微信支付宝"),
            "字面 AND 不命中的拼音命中应出行：{names:?}"
        );
        assert!(
            !names.contains(&"wxzfb微信"),
            "全部 term 字面命中的条目不得以拼音项双出行：{names:?}"
        );
    }
}
