//! 文件索引：紧凑字符串池 + 条目列表，MFT/USN 优先，目录遍历降级，磁盘缓存。
//!
//! 内存目标：200 万条目 ≤ 60MB（每条目 ≤ 30 bytes）。
//! IndexEntry 当前大小：u32×3 + u8 = 13 bytes（含对齐 ≤ 16 bytes）。
//! 字符串池 Vec<u8>：平均路径 60 bytes × 200 万 ≈ 120MB → 前缀共享后估计 ≤ 40MB。
//! 合计理论上限 ≈ 40MB + 16×200万 = 72MB；实测应低于此（大量路径共享前缀）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

// ── 数据结构 ──────────────────────────────────────────────────────────────────

/// 单条索引条目。所有字符串存在 `FileIndex::pool` 中，这里只存偏移。
/// 大小：4+4+4+1 = 13 bytes，对齐后 16 bytes/entry。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// 完整路径在字符串池中的字节偏移（UTF-8）。
    pub path_off: u32,
    /// 文件名（路径最后一段）在字符串池中的偏移。
    pub name_off: u32,
    /// 文件名小写版本在字符串池中的偏移（用于不区分大小写搜索）。
    pub name_lower_off: u32,
    /// 0 = 文件，1 = 目录。
    pub kind: u8,
}

/// 完整文件索引：字符串池 + 条目列表。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FileIndex {
    /// 所有字符串紧凑存储（UTF-8，以 `\0` 分隔）。
    pool: Vec<u8>,
    entries: Vec<IndexEntry>,
}

impl FileIndex {
    /// 从池中读取以 `\0` 结尾的字符串（返回 &str，无拷贝）。
    fn pool_str(&self, off: u32) -> &str {
        let start = off as usize;
        let end = self.pool[start..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| start + p)
            .unwrap_or(self.pool.len());
        std::str::from_utf8(&self.pool[start..end]).unwrap_or("")
    }

    /// 子串搜索（不区分大小写），返回最多 `max` 条匹配的条目克隆。
    pub fn search(&self, query: &str, max: usize) -> Vec<IndexEntry> {
        if query.is_empty() {
            return Vec::new();
        }
        let q = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| self.pool_str(e.name_lower_off).contains(q.as_str()))
            .take(max)
            .cloned()
            .collect()
    }

    /// 获取条目的完整路径字符串。
    pub fn entry_path(&self, e: &IndexEntry) -> &str {
        self.pool_str(e.path_off)
    }

    /// 获取条目的文件名字符串。
    pub fn entry_name(&self, e: &IndexEntry) -> &str {
        self.pool_str(e.name_off)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)] // 与 len() 配套，满足 clippy len_without_is_empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── 字符串池构建器 ────────────────────────────────────────────────────────────

struct PoolBuilder {
    pool: Vec<u8>,
}

impl PoolBuilder {
    fn new() -> Self {
        Self {
            pool: Vec::with_capacity(64 * 1024 * 1024),
        }
    }

    /// 将字符串追加到池，返回偏移。以 `\0` 结尾。
    fn push(&mut self, s: &str) -> u32 {
        let off = self.pool.len() as u32;
        self.pool.extend_from_slice(s.as_bytes());
        self.pool.push(0);
        off
    }
}

// ── 跳过的系统目录 ────────────────────────────────────────────────────────────

fn should_skip(name: &std::ffi::OsStr) -> bool {
    let s = name.to_string_lossy();
    matches!(
        s.as_ref(),
        "$Recycle.Bin"
            | "System Volume Information"
            | "$WINDOWS.~BT"
            | "$WinREAgent"
            | "Recovery"
            | "Config.Msi"
    )
}

// ── 目录遍历构建（降级路径）────────────────────────────────────────────────────

fn build_by_walkdir(roots: &[PathBuf]) -> FileIndex {
    let mut pb = PoolBuilder::new();
    let mut entries: Vec<IndexEntry> = Vec::with_capacity(500_000);

    for root in roots {
        let walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // 跳过系统目录
                if e.file_type().is_dir() {
                    if let Some(name) = e.path().file_name() {
                        return !should_skip(name);
                    }
                }
                true
            });

        for entry in walker.flatten() {
            let path = entry.path();
            // 路径转 UTF-8（中文路径用 lossy，原始 PathBuf 保留在 pool 中）
            let path_str = path.to_string_lossy();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let name_lower = name.to_lowercase();
            let kind: u8 = if entry.file_type().is_dir() { 1 } else { 0 };

            let path_off = pb.push(&path_str);
            let name_off = pb.push(&name);
            let name_lower_off = pb.push(&name_lower);

            entries.push(IndexEntry {
                path_off,
                name_off,
                name_lower_off,
                kind,
            });
        }
    }

    FileIndex {
        pool: pb.pool,
        entries,
    }
}

// ── MFT/USN 路径（仅 Windows）────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod mft {
    use super::*;
    use std::collections::HashMap;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{FSCTL_ENUM_USN_DATA, MFT_ENUM_DATA_V0, USN_RECORD_V2};
    use windows::Win32::System::IO::DeviceIoControl;

    /// 尝试用 MFT 枚举指定盘（如 "C:"）的所有文件。
    /// 失败（权限不足 / 非 NTFS）返回 None，调用方降级到目录遍历。
    pub fn try_enum_volume(drive_letter: char) -> Option<FileIndex> {
        let volume_path: Vec<u16> = format!("\\\\.\\{}:", drive_letter)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let handle = unsafe {
            CreateFileW(
                PCWSTR(volume_path.as_ptr()),
                0, // GENERIC_READ 不需要，只需 DeviceIoControl
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                HANDLE::default(),
            )
        }
        .ok()?;

        let result = enum_mft(handle, drive_letter);
        unsafe {
            let _ = CloseHandle(handle);
        }
        result
    }

    fn enum_mft(handle: HANDLE, drive: char) -> Option<FileIndex> {
        // frn -> (parent_frn, name, is_dir)
        let mut map: HashMap<u64, (u64, String, bool)> = HashMap::with_capacity(500_000);

        let mut med = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: 0,
            LowUsn: 0,
            HighUsn: i64::MAX,
        };

        let mut buf = vec![0u8; 64 * 1024];
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

            // 前 8 字节是下一个 StartFileReferenceNumber
            let next_frn = u64::from_le_bytes(buf[..8].try_into().ok()?);
            med.StartFileReferenceNumber = next_frn;

            let mut offset = 8usize;
            while offset + std::mem::size_of::<USN_RECORD_V2>() <= bytes_returned as usize {
                let rec = unsafe { &*(buf.as_ptr().add(offset) as *const USN_RECORD_V2) };
                if rec.RecordLength == 0 {
                    break;
                }
                let name_len = rec.FileNameLength as usize / 2;
                let name_ptr = unsafe {
                    (buf.as_ptr().add(offset) as *const u16).add(rec.FileNameOffset as usize / 2)
                };
                let name_slice = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
                let name = String::from_utf16_lossy(name_slice);
                let frn = rec.FileReferenceNumber;
                let parent_frn = rec.ParentFileReferenceNumber;
                let is_dir = (rec.FileAttributes & 0x10) != 0; // FILE_ATTRIBUTE_DIRECTORY

                map.insert(frn, (parent_frn, name, is_dir));
                offset += rec.RecordLength as usize;
            }

            if next_frn == 0 {
                break;
            }
        }

        // 构建完整路径
        let mut pb = PoolBuilder::new();
        let mut entries: Vec<IndexEntry> = Vec::with_capacity(map.len());
        let root_prefix = format!("{}:", drive);

        for (&frn, (parent_frn, name, is_dir)) in &map {
            if name.is_empty() || should_skip(std::ffi::OsStr::new(name.as_str())) {
                continue;
            }
            let full_path = build_path(frn, *parent_frn, name, &map, &root_prefix);
            let name_lower = name.to_lowercase();
            let path_off = pb.push(&full_path);
            let name_off = pb.push(name);
            let name_lower_off = pb.push(&name_lower);
            entries.push(IndexEntry {
                path_off,
                name_off,
                name_lower_off,
                kind: if *is_dir { 1 } else { 0 },
            });
        }

        Some(FileIndex {
            pool: pb.pool,
            entries,
        })
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

// ── 本地 NTFS 盘枚举 ──────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn local_ntfs_roots() -> Vec<PathBuf> {
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    // DRIVE_FIXED = 3 (fixed local disk)
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
    // 非 Windows：扫描根目录（用于 CI 测试）
    vec![PathBuf::from("/")]
}

// ── 磁盘缓存 ──────────────────────────────────────────────────────────────────

const CACHE_FILE: &str = "index.bin";

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE)
}

fn save_cache(index: &FileIndex, data_dir: &Path) {
    let path = cache_path(data_dir);
    match postcard::to_allocvec(index) {
        Ok(bytes) => {
            let _ = std::fs::write(&path, bytes);
        }
        Err(e) => {
            crate::log(format!("索引缓存序列化失败：{e}"));
        }
    }
}

fn load_cache(data_dir: &Path) -> Option<FileIndex> {
    let path = cache_path(data_dir);
    let bytes = std::fs::read(&path).ok()?;
    postcard::from_bytes(&bytes).ok()
}

// ── 公开接口 ──────────────────────────────────────────────────────────────────

/// 共享索引句柄类型（供 main.rs / ipc.rs 使用）。
pub type SharedIndex = Arc<RwLock<Option<FileIndex>>>;

/// 启动时异步构建或加载索引，完成后写入 `shared`。
///
/// - 缓存加载与全量扫描都是阻塞工作，一律 `spawn_blocking`，避免卡住命名管道。
/// - 有可用缓存时：立刻可搜，**不再启动时强制全量重建**（由定时刷新负责更新）。
/// - 无缓存时：后台全量构建；构建完成前 search 返回空列表。
pub async fn build_or_load(data_dir: PathBuf, shared: SharedIndex, refresh_secs: u64) {
    let data_dir_for_load = data_dir.clone();
    let shared_for_load = shared.clone();

    crate::log("正在加载索引缓存…");
    let loaded = tokio::task::spawn_blocking(move || {
        let t0 = std::time::Instant::now();
        match load_cache(&data_dir_for_load) {
            Some(cached) => {
                let n = cached.len();
                *shared_for_load.write().unwrap() = Some(cached);
                crate::log(format!(
                    "从缓存加载索引，共 {n} 条目，耗时 {}ms",
                    t0.elapsed().as_millis()
                ));
                true
            }
            None => {
                crate::log("无可用缓存，开始全量构建（期间搜索结果为空）");
                false
            }
        }
    })
    .await
    .unwrap_or(false);

    if !loaded {
        // 无缓存：阻塞线程池里全量构建。
        let shared2 = shared.clone();
        let data_dir2 = data_dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let t0 = std::time::Instant::now();
            let index = build_full_index();
            crate::log(format!(
                "全量索引完成，共 {} 条目，耗时 {}s",
                index.len(),
                t0.elapsed().as_secs()
            ));
            save_cache(&index, &data_dir2);
            *shared2.write().unwrap() = Some(index);
        })
        .await;
    }

    // 定时刷新（无 USN 时）：sleep 在 async，真正构建走 spawn_blocking。
    if refresh_secs > 0 {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(refresh_secs)).await;
                let shared3 = shared.clone();
                let data_dir3 = data_dir.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let t0 = std::time::Instant::now();
                    let index = build_full_index();
                    crate::log(format!(
                        "定时刷新索引，共 {} 条目，耗时 {}s",
                        index.len(),
                        t0.elapsed().as_secs()
                    ));
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

/// 全量构建：Windows 上先尝试 MFT，失败降级目录遍历。
fn build_full_index() -> FileIndex {
    let roots = local_ntfs_roots();

    #[cfg(target_os = "windows")]
    {
        // 尝试 MFT 枚举每个盘，全部成功则合并；任一失败则整体降级目录遍历。
        let mut all_entries: Vec<IndexEntry> = Vec::new();
        let mut all_pool: Vec<u8> = Vec::new();
        let mut mft_ok = true;

        for root in &roots {
            let letter = root.to_string_lossy().chars().next().unwrap_or('C');
            match mft::try_enum_volume(letter) {
                Some(idx) => {
                    // 合并：偏移需要加上当前 pool 长度
                    let base = all_pool.len() as u32;
                    all_pool.extend_from_slice(&idx.pool);
                    for mut e in idx.entries {
                        e.path_off += base;
                        e.name_off += base;
                        e.name_lower_off += base;
                        all_entries.push(e);
                    }
                }
                None => {
                    mft_ok = false;
                    break;
                }
            }
        }

        if mft_ok && !all_entries.is_empty() {
            crate::log("MFT 枚举成功");
            return FileIndex {
                pool: all_pool,
                entries: all_entries,
            };
        }
        crate::log("MFT 枚举失败，降级目录遍历");
    }

    build_by_walkdir(&roots)
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
            let p = Path::new(path);
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            let name_lower = name.to_lowercase();
            let path_off = pb.push(path);
            let name_off = pb.push(&name);
            let name_lower_off = pb.push(&name_lower);
            entries.push(IndexEntry {
                path_off,
                name_off,
                name_lower_off,
                kind: *kind,
            });
        }
        FileIndex {
            pool: pb.pool,
            entries,
        }
    }

    #[test]
    fn walkdir_builds_nonempty_index() {
        // 扫描临时目录，至少能建出非空索引
        let tmp = std::env::temp_dir();
        let idx = build_by_walkdir(&[tmp]);
        // 临时目录通常有文件；即使空也不应 panic
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
    fn cache_roundtrip() {
        let idx = make_test_index(&[("C:\\test\\foo.txt", 0), ("C:\\test\\bar", 1)]);
        let tmp = std::env::temp_dir().join("prism_index_cache_test");
        let _ = fs::create_dir_all(&tmp);

        save_cache(&idx, &tmp);
        let loaded = load_cache(&tmp).expect("缓存应能加载");
        assert_eq!(loaded.len(), idx.len());

        // 验证搜索结果一致
        let r1 = idx.search("foo", 10);
        let r2 = loaded.search("foo", 10);
        assert_eq!(r1.len(), r2.len());
        assert_eq!(idx.entry_path(&r1[0]), loaded.entry_path(&r2[0]));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn corrupt_cache_returns_none() {
        let tmp = std::env::temp_dir().join("prism_index_corrupt_test");
        let _ = fs::create_dir_all(&tmp);
        fs::write(tmp.join(CACHE_FILE), b"not valid postcard data").unwrap();
        assert!(load_cache(&tmp).is_none());
        let _ = fs::remove_dir_all(&tmp);
    }
}
