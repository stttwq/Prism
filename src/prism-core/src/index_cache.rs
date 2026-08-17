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
    let (envelope, _leftover): (CacheEnvelope<IndexState>, _) =
        postcard::from_io((reader, scratch.as_mut_slice()))
            .map_err(|error| format!("decode v5 cache: {error}"))?;
    if &envelope.magic != CACHE_MAGIC || envelope.version != CACHE_VERSION {
        return Err("cache is not Prism v5".into());
    }
    validate(&envelope.state)?;
    Ok(envelope.state)
}

pub fn save(state: &IndexState, data_dir: &Path) -> Result<(), String> {
    validate(state)?;
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
}
