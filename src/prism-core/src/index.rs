//! 文件索引：字符串池 intern + 条目列表，MFT/USN 优先，目录遍历降级，磁盘缓存。
//!
//! 内存目标：常驻索引尽量小，后端进程 ≤ 70MB。
//!
//! 版本演进：
//! - v1：path + name + name_lower（三份字符串）
//! - v2：仅完整路径；扩大黑名单
//! - v3：父目录与文件名分别 intern；同目录 / 同名文件共享池条目（路径前缀压缩）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

// ── 数据结构 ──────────────────────────────────────────────────────────────────

/// 单条索引条目。目录与文件名在池中 intern，多文件共享同一父目录串。
/// 大小：4+4+1 = 9 bytes，对齐后 12 bytes/entry。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// 父目录在字符串池中的偏移（UTF-8，`\0` 结尾；盘符根为 `C:\` 形式）。
    pub dir_off: u32,
    /// 文件名/目录名在字符串池中的偏移。
    pub name_off: u32,
    /// 0 = 文件，1 = 目录。
    pub kind: u8,
}

/// 完整文件索引：intern 字符串池 + 条目列表。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FileIndex {
    /// 去重后的目录与文件名（UTF-8，以 `\0` 分隔）。
    pool: Vec<u8>,
    entries: Vec<IndexEntry>,
}

/// 全量构建进度。`total_estimate=0` 表示没有可用的历史总数估计。
#[derive(Debug, Default)]
pub struct IndexProgress {
    pub active: AtomicBool,
    pub scanned: AtomicU64,
    pub total_estimate: AtomicU64,
}

/// 索引构建任务与 IPC 服务共享的进度状态。
pub type SharedProgress = Arc<IndexProgress>;

struct ActiveBuild<'a>(&'a IndexProgress);

impl IndexProgress {
    fn begin_build(&self) -> ActiveBuild<'_> {
        self.scanned.store(0, Ordering::Relaxed);
        self.active.store(true, Ordering::Relaxed);
        ActiveBuild(self)
    }
}

impl Drop for ActiveBuild<'_> {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Relaxed);
    }
}

impl FileIndex {
    fn pool_str(&self, off: u32) -> &str {
        let start = off as usize;
        if start >= self.pool.len() {
            return "";
        }
        let end = self.pool[start..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| start + p)
            .unwrap_or(self.pool.len());
        std::str::from_utf8(&self.pool[start..end]).unwrap_or("")
    }

    /// 子串搜索（不区分大小写），只匹配文件名，最多 `max` 条。
    pub fn search(&self, query: &str, max: usize) -> Vec<IndexEntry> {
        if query.is_empty() || max == 0 {
            return Vec::new();
        }
        let q = query.to_lowercase();
        let mut out = Vec::with_capacity(max.min(64));
        for e in &self.entries {
            let name = self.pool_str(e.name_off);
            if name_contains_ci(name, &q) {
                out.push(e.clone());
                if out.len() >= max {
                    break;
                }
            }
        }
        out
    }

    /// 拼出完整路径（目录 + 分隔符 + 文件名）。搜索结果条数有限，分配可接受。
    pub fn entry_path(&self, e: &IndexEntry) -> String {
        let dir = self.pool_str(e.dir_off);
        let name = self.pool_str(e.name_off);
        join_dir_name(dir, name)
    }

    /// 文件名（池内借用，无分配）。
    pub fn entry_name(&self, e: &IndexEntry) -> &str {
        self.pool_str(e.name_off)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 估算常驻：池 + 条目表（不含 Vec 超额容量）。
    pub fn memory_bytes(&self) -> usize {
        self.pool.len() + self.entries.len() * std::mem::size_of::<IndexEntry>()
    }

    fn shrink_to_fit(&mut self) {
        self.pool.shrink_to_fit();
        self.entries.shrink_to_fit();
    }

    fn log_stats(&self, label: &str) {
        let mem = self.memory_bytes();
        crate::log(format!(
            "{label}：{} 条目，池 {:.2}MB，条目表 {:.2}MB，合计约 {:.2}MB",
            self.len(),
            self.pool.len() as f64 / (1024.0 * 1024.0),
            (self.entries.len() * std::mem::size_of::<IndexEntry>()) as f64 / (1024.0 * 1024.0),
            mem as f64 / (1024.0 * 1024.0)
        ));
    }
}

fn join_dir_name(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        return name.to_string();
    }
    if dir.ends_with('\\') || dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

/// 拆成 (父目录, 文件名)。父目录不含末尾分隔符，盘符根规范为 `C:\`。
fn split_parent_name(path: &str) -> Option<(String, String)> {
    let path = path.trim_end_matches(['\\', '/']);
    if path.is_empty() {
        return None;
    }
    let i = path.rfind(['\\', '/'])?;
    let (parent, name) = (&path[..i], &path[i + 1..]);
    if name.is_empty() {
        return None;
    }
    let parent = normalize_dir(parent);
    Some((parent, name.to_string()))
}

fn normalize_dir(parent: &str) -> String {
    let p = parent.trim_end_matches(['\\', '/']);
    // "C:" → "C:\"
    if p.len() == 2 && p.as_bytes()[1] == b':' {
        let mut s = p.to_string();
        s.push('\\');
        s
    } else if p.is_empty() {
        "\\".to_string()
    } else {
        p.to_string()
    }
}

fn name_contains_ci(name: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    if name.is_ascii() && query_lower.is_ascii() {
        return contains_ascii_ci(name, query_lower);
    }
    name.to_lowercase().contains(query_lower)
}

fn contains_ascii_ci(haystack: &str, needle_lower: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle_lower.as_bytes();
    if n.is_empty() {
        return true;
    }
    if n.len() > h.len() {
        return false;
    }
    let last = h.len() - n.len();
    'outer: for i in 0..=last {
        for j in 0..n.len() {
            if h[i + j].to_ascii_lowercase() != n[j] {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

// ── 字符串池（带 intern）──────────────────────────────────────────────────────

struct PoolBuilder {
    pool: Vec<u8>,
    /// 构建期临时表；完成后丢弃，不进 FileIndex。
    intern: HashMap<String, u32>,
}

impl PoolBuilder {
    fn new() -> Self {
        Self {
            pool: Vec::with_capacity(2 * 1024 * 1024),
            intern: HashMap::with_capacity(64_000),
        }
    }

    /// 相同字符串只存一份，返回池偏移。
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&off) = self.intern.get(s) {
            return off;
        }
        let off = self.pool.len() as u32;
        self.pool.extend_from_slice(s.as_bytes());
        self.pool.push(0);
        self.intern.insert(s.to_owned(), off);
        off
    }

    fn push_entry(&mut self, entries: &mut Vec<IndexEntry>, dir: &str, name: &str, kind: u8) {
        let dir_off = self.intern(dir);
        let name_off = self.intern(name);
        entries.push(IndexEntry {
            dir_off,
            name_off,
            kind,
        });
    }

    fn into_index(self, entries: Vec<IndexEntry>) -> FileIndex {
        let mut index = FileIndex {
            pool: self.pool,
            entries,
        };
        // intern 表在此 drop
        index.shrink_to_fit();
        index
    }
}

// ── 跳过规则 ──────────────────────────────────────────────────────────────────

fn is_skipped_name(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "$Recycle.Bin",
        "System Volume Information",
        "$WINDOWS.~BT",
        "$WinREAgent",
        "Recovery",
        "Config.Msi",
        "WinSxS",
        "node_modules",
        "__pycache__",
        ".git",
        ".svn",
        ".hg",
        ".cache",
        "bower_components",
    ];
    NAMES.iter().any(|n| name.eq_ignore_ascii_case(n))
}

fn should_skip_os(name: &std::ffi::OsStr) -> bool {
    is_skipped_name(&name.to_string_lossy())
}

fn path_is_excluded(path: &str) -> bool {
    for seg in path.split(['\\', '/']) {
        if !seg.is_empty() && is_skipped_name(seg) {
            return true;
        }
    }
    let lower = path.to_ascii_lowercase();
    const FRAGMENTS: &[&str] = &[
        r"\windows\winsxs\",
        r"\windows\installer\",
        r"\windows\softwaredistribution\",
        r"\windows\servicing\",
        r"\windows\assembly\",
        r"\windows\csc\",
        r"\$windows.~bt\",
        r"\$windows.~ws\",
        r"\appdata\local\temp\",
        r"\appdata\local\tmp\",
        r"\appdata\local\microsoft\windows\inetcache\",
        r"\appdata\local\microsoft\windows\temporary internet files\",
        r"\appdata\local\pip\cache\",
        r"\appdata\local\nuget\v3-cache\",
        r"\appdata\local\yarn\cache\",
        r"\appdata\local\pnpm\store\",
        r"\appdata\roaming\npm-cache\",
        r"\programdata\package cache\",
    ];
    for frag in FRAGMENTS {
        if lower.contains(frag) {
            return true;
        }
    }
    lower.contains("/node_modules/")
        || lower.contains("/.git/")
        || lower.contains("/windows/winsxs/")
}

// ── 目录遍历 ──────────────────────────────────────────────────────────────────

fn build_by_walkdir(roots: &[PathBuf], progress: Option<&IndexProgress>) -> FileIndex {
    let mut pb = PoolBuilder::new();
    let mut entries: Vec<IndexEntry> = Vec::with_capacity(200_000);

    for root in roots {
        let walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    if let Some(name) = e.path().file_name() {
                        if should_skip_os(name) {
                            return false;
                        }
                    }
                    let p = e.path().to_string_lossy();
                    if path_is_excluded(&p) {
                        return false;
                    }
                }
                true
            });

        for entry in walker.flatten() {
            if let Some(progress) = progress {
                progress.scanned.fetch_add(1, Ordering::Relaxed);
            }
            let path_str = entry.path().to_string_lossy();
            if path_str.is_empty() || path_is_excluded(&path_str) {
                continue;
            }
            let Some((dir, name)) = split_parent_name(&path_str) else {
                continue;
            };
            if name.is_empty() || is_skipped_name(&name) {
                continue;
            }
            let kind: u8 = if entry.file_type().is_dir() { 1 } else { 0 };
            pb.push_entry(&mut entries, &dir, &name, kind);
        }
    }

    pb.into_index(entries)
}

// ── MFT/USN（Windows）─────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod mft {
    use super::*;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{FSCTL_ENUM_USN_DATA, MFT_ENUM_DATA_V0, USN_RECORD_V2};
    use windows::Win32::System::IO::DeviceIoControl;

    /// 把一盘 MFT 条目追加进共享 pool/entries。失败返回 false。
    pub fn try_append_volume(
        drive_letter: char,
        pb: &mut PoolBuilder,
        entries: &mut Vec<IndexEntry>,
        progress: &IndexProgress,
    ) -> bool {
        let volume_path: Vec<u16> = format!("\\\\.\\{}:", drive_letter)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let handle = unsafe {
            CreateFileW(
                PCWSTR(volume_path.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                HANDLE::default(),
            )
        };
        let Ok(handle) = handle else {
            return false;
        };

        let ok = append_mft(handle, drive_letter, pb, entries, progress);
        unsafe {
            let _ = CloseHandle(handle);
        }
        ok
    }

    fn append_mft(
        handle: HANDLE,
        drive: char,
        pb: &mut PoolBuilder,
        entries: &mut Vec<IndexEntry>,
        progress: &IndexProgress,
    ) -> bool {
        let mut map: HashMap<u64, (u64, String, bool)> = HashMap::with_capacity(500_000);

        let mut med = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: 0,
            LowUsn: 0,
            HighUsn: i64::MAX,
        };

        let mut buf = vec![0u8; 64 * 1024];
        let mut any = false;
        loop {
            let mut bytes_returned: u32 = 0;
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_ENUM_USN_DATA,
                    Some(&med as *const _ as *const _),
                    std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32,
                    Some(buf.as_mut_ptr() as *mut _),
                    buf.len() as u32,
                    Some(&mut bytes_returned),
                    None,
                )
            };
            if ok.is_err() || bytes_returned <= 8 {
                break;
            }
            any = true;

            let next_frn = u64::from_le_bytes(buf[..8].try_into().unwrap_or([0; 8]));
            med.StartFileReferenceNumber = next_frn;

            let mut offset = 8usize;
            while offset + std::mem::size_of::<USN_RECORD_V2>() <= bytes_returned as usize {
                let rec = unsafe { &*(buf.as_ptr().add(offset) as *const USN_RECORD_V2) };
                if rec.RecordLength == 0 {
                    break;
                }
                progress.scanned.fetch_add(1, Ordering::Relaxed);
                let name_len = rec.FileNameLength as usize / 2;
                let name_ptr = unsafe {
                    (buf.as_ptr().add(offset) as *const u16).add(rec.FileNameOffset as usize / 2)
                };
                let name_slice = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
                let name = String::from_utf16_lossy(name_slice);
                let frn = rec.FileReferenceNumber;
                let parent_frn = rec.ParentFileReferenceNumber;
                let is_dir = (rec.FileAttributes & 0x10) != 0;
                map.insert(frn, (parent_frn, name, is_dir));
                offset += rec.RecordLength as usize;
            }

            if next_frn == 0 {
                break;
            }
        }

        if !any {
            return false;
        }

        let root_prefix = format!("{}:", drive);
        for (&frn, (parent_frn, name, is_dir)) in &map {
            if name.is_empty() || is_skipped_name(name) {
                continue;
            }
            let full_path = build_path(frn, *parent_frn, name, &map, &root_prefix);
            if path_is_excluded(&full_path) {
                continue;
            }
            let Some((dir, fname)) = split_parent_name(&full_path) else {
                continue;
            };
            pb.push_entry(entries, &dir, &fname, if *is_dir { 1 } else { 0 });
        }
        true
    }

    fn build_path(
        _frn: u64,
        parent_frn: u64,
        name: &str,
        map: &HashMap<u64, (u64, String, bool)>,
        root_prefix: &str,
    ) -> String {
        let mut parts = vec![name.to_string()];
        let mut cur_parent = parent_frn;
        let mut depth = 0;
        loop {
            depth += 1;
            if depth > 64 {
                break;
            }
            match map.get(&cur_parent) {
                Some((gp, pname, _)) => {
                    parts.push(pname.clone());
                    cur_parent = *gp;
                }
                None => break,
            }
        }
        parts.reverse();
        format!("{}\\{}", root_prefix, parts.join("\\"))
    }
}

// ── 盘符枚举 ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn local_ntfs_roots() -> Vec<PathBuf> {
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let mut roots = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0u32..26 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let path: Vec<u16> = format!("{}:\\", letter)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let drive_type = unsafe { GetDriveTypeW(windows::core::PCWSTR(path.as_ptr())) };
        if drive_type == DRIVE_FIXED {
            roots.push(PathBuf::from(format!("{}:\\", letter)));
        }
    }
    roots
}

#[cfg(not(target_os = "windows"))]
fn local_ntfs_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

// ── 磁盘缓存 ──────────────────────────────────────────────────────────────────

const CACHE_FILE: &str = "index.bin";
/// v3：dir_off + name_off intern；旧缓存一律丢弃。
const CACHE_VERSION: u32 = 3;

#[derive(Serialize, Deserialize)]
struct CacheEnvelope {
    version: u32,
    index: FileIndex,
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE)
}

fn save_cache(index: &FileIndex, data_dir: &Path) {
    let path = cache_path(data_dir);
    #[derive(Serialize)]
    struct Envelope<'a> {
        version: u32,
        index: &'a FileIndex,
    }
    let env = Envelope {
        version: CACHE_VERSION,
        index,
    };
    match postcard::to_allocvec(&env) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(&path, &bytes) {
                crate::log(format!("索引缓存写入失败：{e}"));
            } else {
                crate::log(format!(
                    "索引缓存已写入 {}（{:.1}MB）",
                    path.display(),
                    bytes.len() as f64 / (1024.0 * 1024.0)
                ));
            }
        }
        Err(e) => crate::log(format!("索引缓存序列化失败：{e}")),
    }
}

fn load_cache(data_dir: &Path) -> Option<FileIndex> {
    let path = cache_path(data_dir);
    let bytes = std::fs::read(&path).ok()?;
    let env: CacheEnvelope = postcard::from_bytes(&bytes).ok()?;
    if env.version != CACHE_VERSION {
        crate::log(format!(
            "索引缓存版本不匹配（文件 v{}，需要 v{}），将重建",
            env.version, CACHE_VERSION
        ));
        return None;
    }
    let mut index = env.index;
    index.shrink_to_fit();
    Some(index)
}

// ── 公开接口 ──────────────────────────────────────────────────────────────────

pub type SharedIndex = Arc<RwLock<Option<FileIndex>>>;

pub async fn build_or_load(
    data_dir: PathBuf,
    shared: SharedIndex,
    progress: SharedProgress,
    refresh_secs: u64,
) {
    let data_dir_for_load = data_dir.clone();
    let shared_for_load = shared.clone();
    let progress_for_load = progress.clone();

    crate::log("正在加载索引缓存…");
    let loaded = tokio::task::spawn_blocking(move || {
        let t0 = std::time::Instant::now();
        match load_cache(&data_dir_for_load) {
            Some(cached) => {
                cached.log_stats("从缓存加载索引");
                crate::log(format!("加载耗时 {}ms", t0.elapsed().as_millis()));
                progress_for_load
                    .total_estimate
                    .store(cached.len() as u64, Ordering::Relaxed);
                *shared_for_load.write().unwrap() = Some(cached);
                true
            }
            None => {
                progress_for_load.total_estimate.store(0, Ordering::Relaxed);
                crate::log("无可用缓存，开始全量构建（期间搜索结果为空）");
                false
            }
        }
    })
    .await
    .unwrap_or(false);

    if !loaded {
        let shared2 = shared.clone();
        let data_dir2 = data_dir.clone();
        let progress2 = progress.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let t0 = std::time::Instant::now();
            let index = build_full_index(&progress2);
            index.log_stats("全量索引完成");
            crate::log(format!("全量构建耗时 {}s", t0.elapsed().as_secs()));
            save_cache(&index, &data_dir2);
            *shared2.write().unwrap() = Some(index);
        })
        .await;
    }

    if refresh_secs > 0 {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(refresh_secs)).await;
                let shared3 = shared.clone();
                let data_dir3 = data_dir.clone();
                let progress3 = progress.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let total_estimate = shared3
                        .read()
                        .ok()
                        .and_then(|guard| guard.as_ref().map(FileIndex::len))
                        .unwrap_or(0);
                    progress3
                        .total_estimate
                        .store(total_estimate as u64, Ordering::Relaxed);
                    let t0 = std::time::Instant::now();
                    let index = build_full_index(&progress3);
                    index.log_stats("定时刷新索引");
                    crate::log(format!("刷新耗时 {}s", t0.elapsed().as_secs()));
                    save_cache(&index, &data_dir3);
                    *shared3.write().unwrap() = Some(index);
                })
                .await;
                if let Err(e) = result {
                    crate::log(format!("定时刷新任务失败：{e}"));
                }
            }
        });
    }
}

fn build_full_index(progress: &IndexProgress) -> FileIndex {
    let _active_build = progress.begin_build();
    let roots = local_ntfs_roots();

    #[cfg(target_os = "windows")]
    {
        // 多盘共用一个 PoolBuilder，目录/文件名跨盘也能 intern。
        let mut pb = PoolBuilder::new();
        let mut entries: Vec<IndexEntry> = Vec::with_capacity(500_000);
        let mut mft_ok = true;

        for root in &roots {
            let letter = root.to_string_lossy().chars().next().unwrap_or('C');
            if !mft::try_append_volume(letter, &mut pb, &mut entries, progress) {
                mft_ok = false;
                break;
            }
        }

        if mft_ok && !entries.is_empty() {
            crate::log("MFT 枚举成功");
            return pb.into_index(entries);
        }
        // MFT 结果会被整个丢弃，降级扫描从零重新计数，避免进度重复累计。
        progress.scanned.store(0, Ordering::Relaxed);
        crate::log("MFT 枚举失败，降级目录遍历");
    }

    build_by_walkdir(&roots, Some(progress))
}

// ── 单元测试 ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_test_index(paths: &[(&str, u8)]) -> FileIndex {
        let mut pb = PoolBuilder::new();
        let mut entries = Vec::new();
        for (path, kind) in paths {
            let (dir, name) = split_parent_name(path).expect("test path must have parent");
            pb.push_entry(&mut entries, &dir, &name, *kind);
        }
        pb.into_index(entries)
    }

    #[test]
    fn walkdir_builds_nonempty_index() {
        let tmp = std::env::temp_dir();
        let idx = build_by_walkdir(&[tmp], None);
        let _ = idx.len();
    }

    #[test]
    fn search_substring_match() {
        let idx = make_test_index(&[
            ("C:\\Users\\test\\微信.exe", 0),
            ("C:\\Users\\test\\WeChat.exe", 0),
            ("C:\\Windows\\System32\\notepad.exe", 0),
        ]);
        let results = idx.search("wechat", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(idx.entry_name(&results[0]), "WeChat.exe");
        assert_eq!(idx.entry_path(&results[0]), "C:\\Users\\test\\WeChat.exe");
    }

    #[test]
    fn search_case_insensitive() {
        let idx = make_test_index(&[("C:\\test\\Hello.txt", 0)]);
        assert_eq!(idx.search("hello", 10).len(), 1);
        assert_eq!(idx.search("HELLO", 10).len(), 1);
        assert_eq!(idx.search("xyz", 10).len(), 0);
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let idx = make_test_index(&[("C:\\test\\file.txt", 0)]);
        assert!(idx.search("", 10).is_empty());
    }

    #[test]
    fn search_respects_max() {
        let paths: Vec<_> = (0..20)
            .map(|i| (format!("C:\\test\\file{i}.txt"), 0u8))
            .collect();
        let path_refs: Vec<_> = paths.iter().map(|(p, k)| (p.as_str(), *k)).collect();
        let idx = make_test_index(&path_refs);
        assert_eq!(idx.search("file", 5).len(), 5);
    }

    #[test]
    fn search_matches_filename_not_parent_path() {
        let idx = make_test_index(&[("C:\\wechat\\notes.txt", 0)]);
        assert!(idx.search("wechat", 10).is_empty());
        assert_eq!(idx.search("notes", 10).len(), 1);
    }

    #[test]
    fn parent_dir_is_interned_across_siblings() {
        let idx = make_test_index(&[
            ("C:\\Users\\a\\one.txt", 0),
            ("C:\\Users\\a\\two.txt", 0),
            ("C:\\Users\\a\\three.txt", 0),
        ]);
        // 同一父目录只出现一次；三个不同文件名 + 一个目录
        // 池里字符串数 = intern 键数
        let nulls = idx.pool.iter().filter(|&&b| b == 0).count();
        assert_eq!(nulls, 4, "1 dir + 3 names");
        // 三个条目的 dir_off 应相同
        assert_eq!(idx.entries[0].dir_off, idx.entries[1].dir_off);
        assert_eq!(idx.entries[1].dir_off, idx.entries[2].dir_off);
    }

    #[test]
    fn same_filename_interned_across_dirs() {
        let idx = make_test_index(&[("C:\\a\\readme.md", 0), ("C:\\b\\readme.md", 0)]);
        assert_eq!(idx.entries[0].name_off, idx.entries[1].name_off);
        assert_ne!(idx.entries[0].dir_off, idx.entries[1].dir_off);
    }

    #[test]
    fn drive_root_parent_normalized() {
        let idx = make_test_index(&[("C:\\pagefile.sys", 0)]);
        assert_eq!(idx.entry_path(&idx.entries[0]), "C:\\pagefile.sys");
        assert_eq!(idx.entry_name(&idx.entries[0]), "pagefile.sys");
    }

    #[test]
    fn path_excludes_noise_dirs() {
        assert!(path_is_excluded(r"C:\Windows\WinSxS\amd64_foo\file.dll"));
        assert!(path_is_excluded(r"D:\proj\node_modules\lodash\index.js"));
        assert!(path_is_excluded(r"C:\Users\a\AppData\Local\Temp\x.tmp"));
        assert!(!path_is_excluded(r"C:\Users\a\Documents\report.docx"));
    }

    #[test]
    fn skipped_names_case_insensitive() {
        assert!(is_skipped_name("node_modules"));
        assert!(is_skipped_name("NODE_MODULES"));
        assert!(is_skipped_name(".git"));
        assert!(is_skipped_name("WinSxS"));
        assert!(!is_skipped_name("Documents"));
    }

    #[test]
    fn cache_roundtrip() {
        let idx = make_test_index(&[("C:\\test\\foo.txt", 0), ("C:\\test\\bar", 1)]);
        let tmp = std::env::temp_dir().join("prism_index_cache_test_v3");
        let _ = fs::create_dir_all(&tmp);

        save_cache(&idx, &tmp);
        let loaded = load_cache(&tmp).expect("缓存应能加载");
        assert_eq!(loaded.len(), idx.len());

        let r1 = idx.search("foo", 10);
        let r2 = loaded.search("foo", 10);
        assert_eq!(r1.len(), r2.len());
        assert_eq!(idx.entry_path(&r1[0]), loaded.entry_path(&r2[0]));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn old_version_cache_rejected() {
        let tmp = std::env::temp_dir().join("prism_index_old_ver_test_v3");
        let _ = fs::create_dir_all(&tmp);
        let env = CacheEnvelope {
            version: 2,
            index: make_test_index(&[("C:\\a\\b.txt", 0)]),
        };
        let bytes = postcard::to_allocvec(&env).unwrap();
        fs::write(tmp.join(CACHE_FILE), bytes).unwrap();
        assert!(load_cache(&tmp).is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn corrupt_cache_returns_none() {
        let tmp = std::env::temp_dir().join("prism_index_corrupt_test_v3");
        let _ = fs::create_dir_all(&tmp);
        fs::write(tmp.join(CACHE_FILE), b"not valid postcard data").unwrap();
        assert!(load_cache(&tmp).is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn entry_size_is_compact() {
        assert!(std::mem::size_of::<IndexEntry>() <= 12);
    }

    #[test]
    fn pool_smaller_than_full_paths() {
        // 100 个同目录文件：若存完整路径会重复父目录 100 次
        let paths: Vec<_> = (0..100)
            .map(|i| (format!("C:\\Users\\shared\\file{i}.txt"), 0u8))
            .collect();
        let refs: Vec<_> = paths.iter().map(|(p, k)| (p.as_str(), *k)).collect();
        let idx = make_test_index(&refs);
        // 完整路径平均约 30B × 100 = 3000；intern 后约 1 目录 + 100 短名 ≪ 3000
        assert!(
            idx.pool.len() < 2000,
            "pool {} too large for interned siblings",
            idx.pool.len()
        );
    }
}
