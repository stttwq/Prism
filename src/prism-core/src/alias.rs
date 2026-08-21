//! 文件别名系统（2026-08-21 设想文档）：精确触发、独立召回通道。
//!
//! 语义：
//! - 一个目标（file/directory/application）可绑多个词（`weixin.exe` 绑 `wx`
//!   和 `微信`）；持久化 `aliases-v1.json` 于用户数据目录（history 旁）。
//! - 查询（trim 后、大小写不敏感）与某词**完全相等**才触发；`weix` 不触发，
//!   走正常字面搜索。别名是额外召回行，与正常结果合并展示。
//! - 别名行 MatchMetadata 取 class 0（整名精确档）+ Literal kind，进入既有
//!   `MatchMetadata::cmp` 体系；同词多目标的仲裁由 history_score（frecency
//!   桶）与 score（绑定时间秒，冷启动倒序）在全局排序内完成。
//! - execute 带 query 走既有 history 记录路径——frecency 自动反映真实使用。
//! - 路径失效：命中时 `Path::exists()` 复验（history stale paths 教训），
//!   失效静默跳过。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use crate::persistence::{
    AliasData, AliasEntry, VersionedEnvelope, ALIAS_MAX_WORD_CHARS, ALIAS_MAX_WORDS_PER_TARGET,
};
use crate::shell::ActionTarget;

const ALIAS_FILE: &str = "aliases-v1.json";

pub struct AliasStore {
    path: PathBuf,
    state: RwLock<AliasData>,
    /// M2（复审 2026-08-21）：串行化 persist 的写盘段。set/delete 在
    /// spawn_blocking 里可并发，共享同一个 `aliases-v1.json.tmp` 无锁交错会
    /// 写出撕裂的 JSON 并被 atomic_replace 装上——load 侧吞掉解析失败，
    /// 别名整表静默清零。快照在锁内取：磁盘顺序与内存最后写入对齐。
    persist_lock: Mutex<()>,
}

/// set/delete 的结果：Ok 语义给协议层回执；Err 是校验/持久化失败。
pub type AliasMutationResult = Result<(), String>;

impl AliasStore {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(ALIAS_FILE);
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|bytes| {
                serde_json::from_slice::<VersionedEnvelope<AliasData>>(&bytes)
                    .ok()
                    .and_then(|envelope| envelope.into_compatible().ok())
            })
            .map(|data| data.entries)
            .unwrap_or_default();
        // 载入侧归一化词（trim + 小写）：存储历史可能有手工编辑的脏数据。
        let mut entries = entries;
        for entry in &mut entries {
            entry.words = normalized_words(&entry.words);
        }
        entries.retain(|entry| !entry.words.is_empty());
        Self {
            path,
            state: RwLock::new(AliasData { entries }),
            persist_lock: Mutex::new(()),
        }
    }

    /// 整体替换目标的词表。空词表 = 解绑（删除条目）。同步落盘（调用方在
    /// spawn_blocking 里）——别名变更是低频 UI 动作，不值得节流。
    /// M1（复审 2026-08-21）：目标值校验**前置**——persist 的
    /// VersionedEnvelope::new 会整表 validate，一个坏条目曾能把内存态
    /// 改成功但落盘失败，此后所有 set/delete 都卡在同一条坏数据上直到重启。
    pub fn set(&self, target: &ActionTarget, words: &[String], now_utc: u64) -> AliasMutationResult {
        let kind = target_kind(target)?;
        if target.value.is_empty()
            || target.value.contains('\0')
            || target.value.len() > 32 * 1024
            || !Path::new(&target.value).is_absolute()
        {
            return Err("别名目标必须是绝对路径（≤32KB、无空字节）".into());
        }
        let normalized = normalized_words(words);
        if words.is_empty() {
            // 显式清空 = 解绑。
            return self.remove_entry(kind, &target.value);
        }
        if normalized.is_empty() {
            return Err("别名词在 trim 后必须非空".into());
        }
        if normalized.len() > ALIAS_MAX_WORDS_PER_TARGET {
            return Err(format!("每个目标最多 {ALIAS_MAX_WORDS_PER_TARGET} 个别名词"));
        }
        {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let existing_word_count: usize = state
                .entries
                .iter()
                .filter(|entry| entry.words.iter().any(|word| normalized.contains(word)))
                .map(|entry| entry.words.len())
                .sum();
            let replacing = state
                .entries
                .iter()
                .any(|entry| entry.kind == kind && entry.target == target.value);
            if !replacing && state.entries.len() + existing_word_count >= crate::persistence::ALIAS_MAX_ENTRIES
            {
                return Err("别名总条数已达上限（2000）".into());
            }
            state.entries.retain(|entry| {
                !(entry.kind == kind && entry.target == target.value)
            });
            state.entries.push(AliasEntry {
                kind: kind.to_owned(),
                target: target.value.clone(),
                words: normalized,
                bound_at_utc: now_utc,
            });
        }
        self.persist()
    }

    pub fn delete(&self, target: &ActionTarget) -> AliasMutationResult {
        let kind = target_kind(target)?;
        self.remove_entry(kind, &target.value)
    }

    fn remove_entry(&self, kind: &str, value: &str) -> AliasMutationResult {
        {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let before = state.entries.len();
            state.entries.retain(|entry| !(entry.kind == kind && entry.target == value));
            if state.entries.len() == before {
                return Ok(()); // 本就不存在：幂等成功。
            }
        }
        self.persist()
    }

    /// 设置页列表（绑定时间倒序）。
    pub fn list(&self) -> Vec<AliasEntry> {
        let mut entries = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .clone();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.bound_at_utc));
        entries
    }

    /// 精确查词：返回绑定了该词的全部条目（冲突仲裁交给排序层）。
    pub fn lookup_word(&self, word: &str) -> Vec<AliasEntry> {
        let word = word.trim().to_lowercase();
        if word.is_empty() {
            return Vec::new();
        }
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .iter()
            .filter(|entry| entry.words.contains(&word))
            .cloned()
            .collect()
    }

    fn persist(&self) -> Result<(), String> {
        // M2：写盘段整体串行化，快照在锁内取——并发 set/delete 的持久化
        // 不再共享同一个 tmp 名交错写。
        let _guard = self
            .persist_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let envelope = VersionedEnvelope::new(snapshot)?;
        let bytes = serde_json::to_vec_pretty(&envelope)
            .map_err(|error| format!("serialize aliases: {error}"))?;
        let temporary = self.path.with_extension("json.tmp");
        std::fs::create_dir_all(self.path.parent().unwrap_or(Path::new(".")))
            .map_err(|error| format!("create alias directory: {error}"))?;
        std::fs::write(&temporary, &bytes).map_err(|error| format!("write aliases: {error}"))?;
        crate::fs_util::atomic_replace(&temporary, &self.path, "aliases")
    }
}

/// 词归一化：trim、小写、去空、去重、限长。返回可能为空（全部无效时）。
pub fn normalized_words(words: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::new();
    for word in words {
        let trimmed = word.trim().to_lowercase();
        if trimmed.is_empty()
            || trimmed.chars().count() > ALIAS_MAX_WORD_CHARS
            || trimmed.chars().any(char::is_whitespace)
            || normalized.contains(&trimmed)
        {
            continue;
        }
        normalized.push(trimmed);
    }
    normalized
}

/// 别名目标只接受 file/directory/application（web/window 无稳定路径语义）。
fn target_kind(target: &ActionTarget) -> Result<&'static str, String> {
    match target.kind.as_str() {
        "file" => Ok("file"),
        "directory" => Ok("directory"),
        "application" => Ok("application"),
        other => Err(format!("别名不支持该目标类型：{other}")),
    }
}

/// 检查目标值是否是可绑定的绝对路径（file/directory/application 的 value
/// 都是文件系统路径；相对路径拒绝——对齐 execute 的路径校验纪律）。
pub fn target_path_is_bindable(target: &ActionTarget) -> bool {
    target_kind(target).is_ok() && {
        let value = target.value.as_str();
        !value.is_empty()
            && !value.contains('\0')
            && Path::new(value).is_absolute()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (AliasStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("prism-alias-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (AliasStore::load(&dir), dir)
    }

    fn file_target(path: &str) -> ActionTarget {
        ActionTarget {
            kind: "file".into(),
            value: path.into(),
        }
    }

    #[test]
    fn set_lookup_delete_roundtrip_and_persistence() {
        let (store, dir) = store("roundtrip");
        let weixin = file_target(r"C:\Tools\weixin.exe");
        store
            .set(&weixin, &["wx".into(), "微信".into()], 1000)
            .unwrap();
        assert_eq!(store.lookup_word("wx").len(), 1);
        assert_eq!(store.lookup_word("WX").len(), 1, "查词大小写不敏感");
        assert_eq!(store.lookup_word("weix").len(), 0, "精确触发：前缀不命中");
        assert!(store.lookup_word("微信").iter().all(|e| e.target == weixin.value));

        // 重新 set 整体替换词表。
        store.set(&weixin, &["wx".into()], 2000).unwrap();
        assert_eq!(store.lookup_word("wx").len(), 1);
        assert_eq!(store.lookup_word("微信").len(), 0, "旧词随整体替换消失");

        // 持久化往返。
        let reloaded = AliasStore::load(&dir);
        assert_eq!(reloaded.lookup_word("wx").len(), 1);
        assert_eq!(reloaded.list().len(), 1);

        // 空词表 = 解绑；再解绑幂等成功。
        store.set(&weixin, &[], 3000).unwrap();
        assert!(store.lookup_word("wx").is_empty());
        store.delete(&weixin).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_falls_back_to_empty() {
        let (store, dir) = store("corrupt");
        let weixin = file_target(r"C:\Tools\weixin.exe");
        store.set(&weixin, &["wx".into()], 1).unwrap();
        std::fs::write(dir.join(ALIAS_FILE), b"{ not json").unwrap();
        let reloaded = AliasStore::load(&dir);
        assert!(reloaded.list().is_empty(), "损坏 → 空表重来，不 panic");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn word_and_target_validation() {
        let (store, _dir) = store("validate");
        let target = file_target(r"C:\a.exe");
        // 词内空白：精确匹配打不出来，拒绝。
        assert!(store.set(&target, &["w x".into()], 1).is_err());
        // 超 8 词拒绝。
        let nine: Vec<String> = (0..9).map(|i| format!("w{i}")).collect();
        assert!(store.set(&target, &nine, 1).is_err());
        // web 目标拒绝。
        let web = ActionTarget {
            kind: "web".into(),
            value: "https://example.com".into(),
        };
        assert!(store.set(&web, &["wx".into()], 1).is_err());
        // 词归一化：trim + 小写 + 去重 + 限长。
        assert_eq!(
            normalized_words(&["  WX ".into(), "wx".into(), "".into()]),
            vec!["wx".to_owned()]
        );
    }

    #[test]
    fn conflict_arbitration_orders_by_binding_time_desc() {
        let (store, _dir) = store("arbiter");
        let a = file_target(r"C:\a.exe");
        let b = file_target(r"C:\b.exe");
        store.set(&a, &["tool".into()], 100).unwrap();
        store.set(&b, &["tool".into()], 200).unwrap();
        let hits = store.lookup_word("tool");
        assert_eq!(hits.len(), 2);
        // list 按绑定时间倒序；排序层的仲裁由 history_score/score 完成，
        // 这里只锚定数据面：两个目标的词与时间都齐备。
        let listed = store.list();
        assert_eq!(listed[0].bound_at_utc, 200);
        assert_eq!(listed[1].bound_at_utc, 100);
    }
}
