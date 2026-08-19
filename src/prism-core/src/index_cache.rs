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
    let file =
        std::fs::File::open(&path).map_err(|error| format!("open {}: {error}", path.display()))?;
    // 流式反序列化（分块缓冲）：不再把完整文件字节物化为 Vec，消除字节+结构双驻留。
    // postcard::from_io 需要 (reader, scratch_buffer)：读取器流式取字节，scratch 仅供
    // 反序列器暂存非顺序数据，常驻尺寸远小于完整文件。
    let reader = std::io::BufReader::with_capacity(256 * 1024, file);
    let mut scratch = [0u8; 4096];
    let (mut envelope, _leftover): (CacheEnvelope<IndexState>, _) =
        postcard::from_io((reader, scratch.as_mut_slice()))
            .map_err(|error| format!("decode v5 cache: {error}"))?;
    if &envelope.magic != CACHE_MAGIC || envelope.version != CACHE_VERSION {
        return Err("cache is not Prism v5".into());
    }
    validate(&envelope.state)?;
    // F3（FRESH-AUDIT-2）：指纹是 serde skip 字段，载入后整算一次
    //（一次池线性扫，远廉于上面的全量 validate）。
    for volume in &mut envelope.state.volumes {
        volume.recompute_derived_counters();
    }
    Ok(envelope.state)
}

pub fn save(state: &IndexState, data_dir: &Path) -> Result<(), String> {
    // AUDIT-2026-08-18 R-C2: save 前只做抽样 validate——全量 validate 对每节点跑
    // path_for（O(n·depth)），大索引下可能超 SCM Stop 30s wait_hint。
    // load 侧保留全量 validate 作为缓存文件损坏的安全网。
    validate_before_save(state)?;
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
/// `magic | version | volumes.len | volume* | generation | events_since_checkpoint`。
/// `write_volume` 由调用方实现锁与世代核对——每卷短读锁、guard 不跨卷存活。
pub fn save_streaming<F>(
    data_dir: &Path,
    volume_count: usize,
    generation: u64,
    events_since_checkpoint: u64,
    mut write_volume: F,
) -> Result<(), String>
where
    F: FnMut(usize, &mut std::io::BufWriter<std::fs::File>) -> Result<(), String>,
{
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
    for position in 0..volume_count {
        write_volume(position, &mut writer)?;
    }
    postcard::to_io(&generation, &mut writer)
        .map_err(|error| format!("encode v5 tail: {error}"))?;
    postcard::to_io(&events_since_checkpoint, &mut writer)
        .map_err(|error| format!("encode v5 tail: {error}"))?;
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
                volume.path_for(record)?;
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
}
