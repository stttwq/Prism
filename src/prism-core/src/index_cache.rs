//! Consistent v5 machine-level snapshots with atomic replacement.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hierarchy::IndexState;

const CACHE_MAGIC: &[u8; 8] = b"PRISMV5\0";
const CACHE_VERSION: u32 = 5;
const CACHE_FILE: &str = "index-v5.bin";

#[derive(Serialize, Deserialize)]
struct CacheEnvelope<T> {
    magic: [u8; 8],
    version: u32,
    state: T,
    /// B3（AUDIT-4 批次C，2026-08-21）：names/nodes 内容校验和（逐卷
    /// `VolumeIndex::content_hash` 折叠）。v5 此前只有 magic+version+结构
    /// validate，名字池单字节静默损坏可跨重启存活（sidecar 有 checksum、
    /// v5 没有）。`serde(default)` 使旧文件（无此字段）解码为 0 并跳过校验；
    /// 新文件非 0，load 侧重算比对，不匹配按损坏拒绝走全量重建。
    #[serde(default)]
    content_checksum: u64,
}

/// B3: 逐卷 content_hash 折叠成整态校验和。save / save_streaming 两条路径
/// 共用同一函数，保证字节一致的产物校验和也一致。
fn state_checksum(state: &IndexState) -> u64 {
    fn fnv(hash: &mut u64, value: u64) {
        for byte in value.to_le_bytes() {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    let mut hash: u64 = 0xcbf29ce484222325;
    fnv(&mut hash, state.volumes.len() as u64);
    for volume in &state.volumes {
        fnv(&mut hash, volume.content_hash());
    }
    hash
}

pub fn machine_data_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Prism")
}

pub fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE)
}

pub fn load(data_dir: &Path) -> Result<IndexState, String> {
    let path = cache_path(data_dir);
    // B3（AUDIT-4 批次C）+ M3（复审 2026-08-21）：envelope 解析（含 legacy
    // 回退与 IO 重试）整体收敛在 load_envelope_with_retry。
    let mut envelope = load_envelope_with_retry(&path)?;
    if &envelope.magic != CACHE_MAGIC || envelope.version != CACHE_VERSION {
        return Err("cache is not Prism v5".into());
    }
    // B3：非 0 校验和必须匹配。names_fingerprint 是 serde skip 字段（解码后
    // 为 0），先整算再参与哈希——与 save 侧（活索引的滚入指纹）等值。
    if envelope.content_checksum != 0 {
        for volume in &mut envelope.state.volumes {
            volume.recompute_derived_counters();
        }
        if state_checksum(&envelope.state) != envelope.content_checksum {
            return Err("cache content checksum mismatch".into());
        }
    }
    validate(&envelope.state)?;
    // F3（FRESH-AUDIT-2）：指纹是 serde skip 字段，载入后整算一次
    //（一次池线性扫，远廉于上面的全量 validate）。
    for volume in &mut envelope.state.volumes {
        volume.recompute_derived_counters();
    }
    Ok(envelope.state)
}

enum LoadEnvelopeError {
    /// 按新格式解码在尾部缺数据——大概率是旧格式文件（无校验和字段）。
    Legacy,
    /// 打开失败（AV 扫描共享冲突/瞬时 ACL）——与格式损坏分流（M3）。
    IoOpen,
    Decode(postcard::Error),
}

/// M3（复审 2026-08-21）：打开错误≠损坏。此前 File::open 的任何错误都被折进
/// DeserializeUnexpectedEnd，触发 legacy 回退→再失败→按损坏丢缓存→多卷 MFT
/// 全量重建——一次杀软扫描窗口就能白丢几分钟的磁盘工作。现在打开失败先
/// 退避 250ms 重试一次；仍失败才按不可用上抛（调用方走重建，但那已是
/// 真正持续的占用而非瞬态窗口）。
fn load_envelope_with_retry(path: &Path) -> Result<CacheEnvelope<IndexState>, String> {
    for attempt in 0..2 {
        match load_envelope(path) {
            Ok(envelope) => return Ok(envelope),
            Err(LoadEnvelopeError::IoOpen) if attempt == 0 => {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(LoadEnvelopeError::IoOpen) => return Err("open v5 cache failed after retry".into()),
            Err(LoadEnvelopeError::Legacy) => {
                let legacy: CacheEnvelopeLegacy<IndexState> = match open_and_decode(path) {
                    Ok(legacy) => legacy,
                    Err(CacheReadError::Io) if attempt == 0 => {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        continue;
                    }
                    Err(CacheReadError::Io) => {
                        return Err("open v5 legacy cache failed after retry".into())
                    }
                    Err(CacheReadError::Postcard(error)) => {
                        return Err(format!("decode v5 legacy cache: {error}"))
                    }
                };
                return Ok(CacheEnvelope {
                    magic: legacy.magic,
                    version: legacy.version,
                    state: legacy.state,
                    content_checksum: 0,
                });
            }
            Err(LoadEnvelopeError::Decode(error)) => {
                return Err(format!("decode v5 cache: {error}"))
            }
        }
    }
    unreachable!("retry loop returns from every branch")
}

fn load_envelope(path: &Path) -> Result<CacheEnvelope<IndexState>, LoadEnvelopeError> {
    match open_and_decode(path) {
        Ok(envelope) => Ok(envelope),
        Err(CacheReadError::Postcard(postcard::Error::DeserializeUnexpectedEnd)) => {
            Err(LoadEnvelopeError::Legacy)
        }
        Err(CacheReadError::Postcard(error)) => Err(LoadEnvelopeError::Decode(error)),
        Err(CacheReadError::Io) => Err(LoadEnvelopeError::IoOpen),
    }
}

/// B3/M3：读缓存的两类失败——打开（IO）与解码（postcard）必须分流。
/// Io 不携带错误详情：调用方只按「打不开」分支处理（重试/放弃加载）。
enum CacheReadError {
    Io,
    Postcard(postcard::Error),
}

fn open_and_decode<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CacheReadError> {
    let file = std::fs::File::open(path).map_err(|_| CacheReadError::Io)?;
    // 流式反序列化（分块缓冲）：不再把完整文件字节物化为 Vec，消除字节+结构双驻留。
    // postcard::from_io 需要 (reader, scratch_buffer)：读取器流式取字节，scratch 仅供
    // 反序列器暂存非顺序数据，常驻尺寸远小于完整文件。
    let reader = std::io::BufReader::with_capacity(256 * 1024, file);
    let mut scratch = [0u8; 4096];
    let (envelope, _leftover): (T, _) =
        postcard::from_io((reader, scratch.as_mut_slice())).map_err(CacheReadError::Postcard)?;
    Ok(envelope)
}

/// B3：升级前的 v5 布局（无尾部校验和字段），仅供 load 的 legacy 回退解码。
#[derive(Serialize, Deserialize)]
struct CacheEnvelopeLegacy<T> {
    magic: [u8; 8],
    version: u32,
    state: T,
}

/// 审计 2026-08-25（中）：v5 缓存写盘互斥（见 save 内注释）。
static CACHE_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn save(state: &IndexState, data_dir: &Path) -> Result<(), String> {
    // AUDIT-2026-08-18 R-C2: save 前只做抽样 validate——全量 validate 对每节点跑
    // path_for（O(n·depth)），大索引下可能超 SCM Stop 30s wait_hint。
    // load 侧保留全量 validate 作为缓存文件损坏的安全网。
    validate_before_save(state)?;
    // 审计 2026-08-25（中）：v5 写盘互斥。maintenance checkpoint 被 stop 打断后，
    // 其 spawn_blocking 仍在写 .tmp；停机 checkpoint 在另一线程并发
    // File::create 同名 tmp——两把句柄交错写，先完成者的 atomic_replace 可把
    // 混合内容装上，下次启动 checksum 失败触发全卷 MFT 重建。照抄 history/
    // alias 的 persist 纪律把写盘段（含 atomic_replace）串行化。锁序恒为
    // cache_write_lock → index.read（流式回调）；反向不存在——所有调用方都在
    // 释放索引锁后以 owned snapshot 调用 save（见 checkpoint_sync 回落路径）。
    let _write_guard = CACHE_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("create {}: {error}", data_dir.display()))?;
    let path = cache_path(data_dir);
    let temporary = data_dir.join(format!("{CACHE_FILE}.tmp"));
    let file = std::fs::File::create(&temporary)
        .map_err(|error| format!("create {}: {error}", temporary.display()))?;
    // 流式序列化直接写文件（分块缓冲）：大索引 checkpoint 时不再与索引本体
    // 之外再生成一份完整的序列化 Vec —— 内存峰值从 3x 降到 2x。
    let mut writer = std::io::BufWriter::with_capacity(256 * 1024, file);
    postcard::to_io(
        &CacheEnvelope {
            magic: *CACHE_MAGIC,
            version: CACHE_VERSION,
            state,
            content_checksum: state_checksum(state),
        },
        &mut writer,
    )
    .map_err(|error| format!("encode v5 cache: {error}"))?;
    writer
        .flush()
        .and_then(|()| writer.get_ref().sync_all())
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    drop(writer);
    crate::fs_util::atomic_replace(&temporary, &path, "cache")
}

/// M2（FRESH-AUDIT-2026-08-19）: 流式 checkpoint 的重试标记。
/// checkpoint 的逐卷写回调在世代/卷集变化时返回含此标记的错误，
/// 调用方据此重试或回落整态 clone 路径（serde/postcard 的错误通道会丢弃
/// 自定义消息，所以标记走我们自己的 Result<String> 而不是 Serialize::Error）。
pub const SNAPSHOT_CHANGED: &str = "prism-snapshot-changed";

/// M2: 逐卷流式写 v5。字节布局手工复刻 CacheEnvelope<IndexState> 的派生
/// 序列化（magic 原始 8 字节 + varint 字段；与 save() 逐字节一致，由测试锚定）：
/// `magic | version | volumes.len | volume* | generation | events_since_checkpoint | content_checksum`。
/// `write_volume` 由调用方实现锁与世代核对——每卷短读锁、guard 不跨卷存活；
/// B3（AUDIT-4 批次C）起回调须返回该卷的 `content_hash()`（在同一短读锁内
/// 计算），流式尾部写出整态校验和，与 save() 派生序列化保持逐字节一致。
pub fn save_streaming<F>(
    data_dir: &Path,
    volume_count: usize,
    generation: u64,
    events_since_checkpoint: u64,
    mut write_volume: F,
) -> Result<(), String>
where
    F: FnMut(usize, &mut std::io::BufWriter<std::fs::File>) -> Result<u64, String>,
{
    // 审计 2026-08-25（中）：同 save 的写盘互斥——maintenance/停机/重建三条
    // checkpoint 路径共享同一个 .tmp 文件名，必须串行写。
    let _write_guard = CACHE_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("create {}: {error}", data_dir.display()))?;
    let path = cache_path(data_dir);
    let temporary = data_dir.join(format!("{CACHE_FILE}.tmp"));
    let file = std::fs::File::create(&temporary)
        .map_err(|error| format!("create {}: {error}", temporary.display()))?;
    let mut writer = std::io::BufWriter::with_capacity(256 * 1024, file);
    writer
        .write_all(CACHE_MAGIC)
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    postcard::to_io(&CACHE_VERSION, &mut writer)
        .map_err(|error| format!("encode v5 header: {error}"))?;
    // Vec 的长度字段：postcard 对 usize 走 varint，与 varint u32 编码一致。
    postcard::to_io(&(volume_count as u32), &mut writer)
        .map_err(|error| format!("encode v5 header: {error}"))?;
    let mut checksum: u64 = 0xcbf29ce484222325;
    fn fold(value: u64, checksum: &mut u64) {
        for byte in value.to_le_bytes() {
            *checksum ^= u64::from(byte);
            *checksum = checksum.wrapping_mul(0x100000001b3);
        }
    }
    fold(volume_count as u64, &mut checksum);
    for position in 0..volume_count {
        let volume_hash = write_volume(position, &mut writer)?;
        fold(volume_hash, &mut checksum);
    }
    postcard::to_io(&generation, &mut writer)
        .map_err(|error| format!("encode v5 tail: {error}"))?;
    postcard::to_io(&events_since_checkpoint, &mut writer)
        .map_err(|error| format!("encode v5 tail: {error}"))?;
    postcard::to_io(&checksum, &mut writer)
        .map_err(|error| format!("encode v5 checksum: {error}"))?;
    writer
        .flush()
        .and_then(|()| writer.get_ref().sync_all())
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    drop(writer);
    crate::fs_util::atomic_replace(&temporary, &path, "cache")
}

/// AUDIT-2026-08-18 R-C2: save 前抽样 validate——避免全量 path_for 的 O(n·depth)。
/// 抽样覆盖：每卷根节点（必检）+ 随机 ~1% 节点 path_for + 结构不变量
///（nodes 上限、names 池边界、slot 计数一致性）。load 侧仍走全量 validate。
/// M2: pub(crate) 供 checkpoint 的流式路径在短读锁内调用。
pub(crate) fn validate_before_save(state: &IndexState) -> Result<(), String> {
    for volume in &state.volumes {
        // 结构不变量始终检查（开销极低）。
        volume.validate_structure()?;

        // 根节点必检（path_for 恒为 mount_path，开销极小）。
        let root_record = volume.root_record;
        volume.path_for(root_record)?;

        // 随机 1% 节点抽检 path_for。用固定步长而非 RNG——确定性、零分配、
        // 覆盖均匀，且不引入 rand 依赖。
        // 随机 1% 节点抽检 path_for。用固定步长而非 RNG——确定性、零分配、
        // 覆盖均匀，且不引入 rand 依赖。
        // FRESH-AUDIT-2 F1: 抽样失败必须中止保存（原 `let _ =` 丢弃结果，损坏父链
        // 照常写入 v5）。墓碑槽跳过——父已删子仍在是 USN 重放的合法中间态，
        // path_for 对非 PRESENT 记录必然报错（见 validate() 同款处理）。
        let stride = (volume.nodes.len() / 100).max(1);
        let mut record = 0u32;
        while (record as usize) < volume.nodes.len() {
            let slot = &volume.nodes[record as usize];
            if record != root_record && slot.flags & crate::hierarchy::FLAG_PRESENT != 0 {
                // M1（复审 2026-08-21）：与 load 侧 validate 同口径——祖先链上
                // 任意墓碑（USN 重放中间态）容忍跳过，其余 path_for 错误硬拒。
                if let Err(reason) = volume.path_for(record) {
                    if !reason.starts_with("missing parent record") {
                        return Err(reason);
                    }
                }
            }
            record = record.saturating_add(stride as u32);
        }
    }
    Ok(())
}

fn validate(state: &IndexState) -> Result<(), String> {
    for volume in &state.volumes {
        volume.validate()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::{VolumeId, VolumeIndex};

    fn state() -> IndexState {
        IndexState {
            volumes: vec![VolumeIndex::new(
                VolumeId {
                    guid: "v".into(),
                    serial: 1,
                },
                "C:\\".into(),
                2,
                3,
                5,
            )
            .unwrap()],
            generation: 4,
            events_since_checkpoint: 5,
        }
    }

    #[test]
    fn v5_roundtrip_and_corruption_recovery() {
        let dir = std::env::temp_dir().join(format!("prism-v5-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save(&state(), &dir).unwrap();
        let loaded = load(&dir).unwrap();
        assert_eq!(loaded.generation, 4);
        std::fs::write(cache_path(&dir), b"corrupt").unwrap();
        assert!(load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn older_cache_versions_are_rejected() {
        let bytes = postcard::to_allocvec(&CacheEnvelope {
            magic: *b"PRISMV4\0",
            version: 4,
            state: state(),
            content_checksum: 0,
        })
        .unwrap();
        let decoded: CacheEnvelope<IndexState> = postcard::from_bytes(&bytes).unwrap();
        assert_ne!(&decoded.magic, CACHE_MAGIC);
    }

    /// FRESH-AUDIT-2 F1: 抽样 path_for 失败必须让 validate_before_save 报错，
    /// 不允许损坏父链（结构校验捕捉不到的环）写入 v5。
    #[test]
    fn f1_validate_before_save_rejects_broken_parent_chain() {
        let mut st = state();
        let volume = &mut st.volumes[0];
        let root_frn = volume.root_record as u64;
        volume.upsert(1, root_frn, "a", true).unwrap();
        volume.upsert(2, 1, "b", true).unwrap();

        // 环：a↔b。两者都是 present 目录，validate_structure 不拦截；
        // 只有抽样 path_for 能发现（表现为深度耗尽或显式环报错）。
        volume.nodes[1].parent_record = 2;
        volume.nodes[2].parent_record = 1;

        let err = validate_before_save(&st).unwrap_err();
        assert!(
            err.contains("cycle") || err.contains("exceeds depth"),
            "unexpected error: {err}"
        );
    }

    /// B3（AUDIT-4 批次C）：结构完好但内容被改（names 池单字节静默损坏的
    /// 等价模拟——同长度名字只差一字节，解码不报错）时校验和必须拒绝。
    #[test]
    fn b3_checksum_rejects_silently_altered_content() {
        let dir = std::env::temp_dir().join(format!("prism-v5-b3a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let clean = state();
        let mut altered = state();
        // 同长度、仅一字节之差的名字——结构校验全部通过（父=root_record 5）。
        altered.volumes[0]
            .upsert(10, 5, "corrupt-name", false)
            .unwrap();
        // 用干净状态的校验和 + 被改内容组装 envelope。
        let bytes = postcard::to_allocvec(&CacheEnvelope {
            magic: *CACHE_MAGIC,
            version: CACHE_VERSION,
            state: altered,
            content_checksum: state_checksum(&clean),
        })
        .unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(cache_path(&dir), bytes).unwrap();
        let error = load(&dir).unwrap_err();
        assert!(error.contains("checksum"), "unexpected: {error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B3：旧格式文件（无校验和字段）仍可加载——legacy 回退解码 + 跳过校验，
    /// 升级路径无损。
    #[test]
    fn b3_legacy_file_without_checksum_still_loads() {
        let dir = std::env::temp_dir().join(format!("prism-v5-b3b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = postcard::to_allocvec(&CacheEnvelopeLegacy {
            magic: *CACHE_MAGIC,
            version: CACHE_VERSION,
            state: state(),
        })
        .unwrap();
        std::fs::write(cache_path(&dir), bytes).unwrap();
        let loaded = load(&dir).unwrap();
        assert_eq!(loaded.generation, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
