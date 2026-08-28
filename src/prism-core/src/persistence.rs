//! Version contracts shared by user-data persistence features.

use serde::{Deserialize, Serialize};

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const HISTORY_SCHEMA_VERSION: u32 = 2;
pub const FAVICON_METADATA_SCHEMA_VERSION: u32 = 1;
pub const ALIAS_SCHEMA_VERSION: u32 = 1;
pub const COMMANDS_SCHEMA_VERSION: u32 = 1;
pub const COMMAND_USAGE_SCHEMA_VERSION: u32 = 1;

pub trait VersionedData {
    const SCHEMA_VERSION: u32;
    fn validate(&self) -> Result<(), String>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionedEnvelope<T> {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub data: T,
}

impl<T: VersionedData + Default> VersionedEnvelope<T> {
    pub fn new(data: T) -> Result<Self, String> {
        data.validate()?;
        Ok(Self {
            schema_version: T::SCHEMA_VERSION,
            data,
        })
    }

    pub fn into_compatible(self) -> Result<T, String> {
        if self.schema_version > T::SCHEMA_VERSION {
            return Err(format!(
                "schema version {} is newer than supported version {}",
                self.schema_version,
                T::SCHEMA_VERSION
            ));
        }
        self.data.validate()?;
        Ok(self.data)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryData {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

/// v2：记录某个规范化查询串选中过该 target（写入侧在 history.rs 里做键归一化）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryStat {
    pub query: String,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub last_used_utc: u64,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub execute_count: u32,
    #[serde(default)]
    pub reveal_count: u32,
    #[serde(default)]
    pub destination_count: u32,
    #[serde(default)]
    pub last_used_utc: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub first_used_utc: u64,
    /// 衰减加权分 ×1000 定点存储：事件时更新、读取时按 last_utc 惰性衰减。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub frecency_milli: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queries: Vec<QueryStat>,
}

impl VersionedData for HistoryData {
    const SCHEMA_VERSION: u32 = HISTORY_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.entries.len() > 5000 {
            return Err("history contains more than 5000 entries".into());
        }
        for entry in &self.entries {
            if !matches!(
                entry.kind.as_str(),
                "file" | "directory" | "application" | "window"
            ) {
                return Err("history contains an unsupported target kind".into());
            }
            if entry.target.is_empty()
                || entry.target.contains('\0')
                || entry.target.len() > 32 * 1024
            {
                return Err("history contains an invalid target".into());
            }
            if entry.queries.len() > 8 {
                return Err("history entry contains more than 8 query stats".into());
            }
            for stat in &entry.queries {
                if stat.query.is_empty() || stat.query.contains('\0') || stat.query.len() > 128 {
                    return Err("history entry contains an invalid query stat".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaviconMetadata {
    #[serde(default)]
    pub entries: Vec<serde_json::Value>,
}

/// 别名系统（2026-08-21 设想）：一个目标（file/directory/application）绑定
/// 多个词；查询与词**精确相等**时目标以 class 0 行加入合并。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AliasData {
    #[serde(default)]
    pub entries: Vec<AliasEntry>,
}

/// 单条绑定：目标 → 词表（set 整体替换）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AliasEntry {
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub words: Vec<String>,
    #[serde(default)]
    pub bound_at_utc: u64,
}

/// 别名词上限：trim 后非空、≤32 字符、不含空白（精确匹配单 token）。
pub const ALIAS_MAX_WORDS_PER_TARGET: usize = 8;
pub const ALIAS_MAX_WORD_CHARS: usize = 32;
pub const ALIAS_MAX_ENTRIES: usize = 2000;

impl VersionedData for AliasData {
    const SCHEMA_VERSION: u32 = ALIAS_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.entries.len() > ALIAS_MAX_ENTRIES {
            return Err("alias store contains more than 2000 entries".into());
        }
        for entry in &self.entries {
            if !matches!(entry.kind.as_str(), "file" | "directory" | "application") {
                return Err("alias entry has an unsupported target kind".into());
            }
            if entry.target.is_empty()
                || entry.target.contains('\0')
                || entry.target.len() > 32 * 1024
            {
                return Err("alias entry has an invalid target".into());
            }
            if entry.words.is_empty() || entry.words.len() > ALIAS_MAX_WORDS_PER_TARGET {
                return Err("alias entry must bind 1..=8 words".into());
            }
            for word in &entry.words {
                if word.is_empty()
                    || word.chars().count() > ALIAS_MAX_WORD_CHARS
                    || word.contains('\0')
                    || word.chars().any(char::is_whitespace)
                {
                    return Err("alias entry has an invalid word".into());
                }
            }
        }
        Ok(())
    }
}

impl VersionedData for FaviconMetadata {
    const SCHEMA_VERSION: u32 = FAVICON_METADATA_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.entries.len() > 1_000 {
            return Err("favicon metadata contains more than 1000 entries".into());
        }
        Ok(())
    }
}

// ── 命令系统持久化（K0） ───────────────────────────────────────────
//
// 独立命名上限常量，不复用 ALIAS_*：别名词是用户词表（≤32 字符/≤8 个、精确整词
// 匹配），命令关键字进与网页引擎共享的独占路由命名空间（≤16 字符/≤4 个）。
// 共享常量会让一侧调参波及另一侧路由语义。title/subtitle 按 chars().count()：
// 中文为主的展示文本，字节上限会让正常标题在 UTF-8 下提前触顶。

pub const COMMAND_MAX_ENTRIES: usize = 512;
pub const COMMAND_ID_MAX_BYTES: usize = 128;
pub const COMMAND_MAX_KEYWORDS: usize = 4;
pub const COMMAND_KEYWORD_MAX_CHARS: usize = 16;
pub const COMMAND_TITLE_MAX_CHARS: usize = 64;
pub const COMMAND_SUBTITLE_MAX_CHARS: usize = 128;
/// K2 §4.6：shortcut_combo 字段最大字符数（组合键串足够）。
pub const SHORTCUT_COMBO_MAX_CHARS: usize = 64;
/// K2 §4.6：shortcut_bindings 最多 64 条（内置命令 ≤ 6 + 用户命令 ≤ 512，取交集上限）。
pub const COMMAND_SHORTCUT_MAX_ENTRIES: usize = 64;
pub const COMMAND_USAGE_MAX_ENTRIES: usize = 2000;
/// K3 §4.1：handler_params 键数上限。
pub const COMMAND_HANDLER_PARAMS_MAX_KEYS: usize = 8;
/// K3 §4.1：handler_params 单值字符上限。
pub const COMMAND_HANDLER_PARAM_MAX_CHARS: usize = 1024;
/// K3 §4.2：handler_params 键名字符上限（`[a-z_]+`，保留扩展空间）。
pub const COMMAND_HANDLER_PARAM_KEY_MAX_CHARS: usize = 32;

/// 用户命令目录。`UserCommandDefinition` 刻意不含 `owner`：用户命令的 owner 恒为
/// broker、trust 恒为 user，由代码赋值，不从 JSON 读。
/// K2 §4.6：`shortcut_bindings` 是独立的 command_id → combo 映射，broker 独占。
/// 与 WPF 的 `settings.json` 的 `ActionHotkeys` 分开存储（设计 §7.2 + §14-R10）：
/// 旧版 WPF 保存 settings.json 时会丢弃它不认识的字段，命令绑定写进去就没了。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandData {
    #[serde(default)]
    pub commands: Vec<UserCommandDefinition>,
    /// K2 §4.6：命令快捷键绑定。key = command_id, value = combo string。
    /// 空字符串 = 清除。内置命令也在此映射中存（它们不在 `commands` 向量中）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shortcut_bindings: Vec<CommandShortcutEntry>,
}

/// K2 §4.6：快捷键绑定条目。用 Vec 而非 HashMap 保证 JSON 稳定。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandShortcutEntry {
    pub command_id: String,
    pub combo: String,
}

/// 持久化形态的用户命令定义（Deserialize 端）。与 broker 下发的 `CommandDescriptor`
/// （Serialize 端）是两个类型——类型收口保证导入 JSON 无法获得内置特权 handler
/// （owner 字段不在用户侧类型上）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserCommandDefinition {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub icon_glyph: String,
    /// 关键字：独占路由命名空间。trim 后非空、≤16 字符、无空白、≤4 个。
    #[serde(default)]
    pub keywords: Vec<String>,
    /// 输入要求。
    #[serde(default)]
    pub input: CommandInputSpec,
    /// 各 surface 的绑定。K0 用户表为空，字段保留供 K1+。
    #[serde(default)]
    pub bindings: CommandBindings,
    /// normal | elevated | destructive。
    #[serde(default)]
    pub danger: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// K3 §4.1：用户态 handler。反序列化为独立枚举，与 `commands::BrokerHandlerId`
    /// **没有任何转换路径**——类型层面保证导入 JSON 无法获得内置特权能力。
    /// 未知字符串 → `Unknown`（条目不禁用整文件，仅自身标不可用）。
    #[serde(default)]
    pub handler: UserHandlerKind,
    /// K3 §4.1：handler 参数。键 ≤8、单值 ≤1024 字符、键名 `[a-z_]+`。
    /// 必选键由 handler 决定：open_url → `url_template`；launch_program → `path`。
    #[serde(default)]
    pub handler_params: std::collections::BTreeMap<String, String>,
}

/// K3 §4.1：用户命令可用的 handler 全集。**刻意不含内置 handler**，且不提供任何到
/// `commands::BrokerHandlerId` 的转换——类型层面保证导入 JSON 无法获得特权能力。
/// `Unknown` 是反序列化默认值：未知/缺失 handler 不使整文件失败，仅该条目标记不可用。
/// 未知字符串（如 `staging_zip`）在自定义 Deserialize 里降级为 `Unknown` 而非报错
/// （§4.1 A4 验收：类型无转换路径）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserHandlerKind {
    #[default]
    Unknown,
    OpenUrl,
    LaunchProgram,
}

impl<'de> Deserialize<'de> for UserHandlerKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer).unwrap_or_default();
        match raw.as_str() {
            "open_url" => Ok(UserHandlerKind::OpenUrl),
            "launch_program" => Ok(UserHandlerKind::LaunchProgram),
            // 未知/缺失 → Unknown，不报错（§4.1 单条降级语义）。
            _ => Ok(UserHandlerKind::Unknown),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandInputSpec {
    /// none | text | destination | output_path。
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub prompt: String,
}

/// 各 surface 的绑定集合。K0 用户命令无绑定，字段保留。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandBindings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_search: Option<CommandBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<CommandBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_panel: Option<CommandBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging: Option<CommandBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<CommandBinding>,
}

/// 单条 binding。K0 为占位结构；K2 §4.6 在 shortcut surface 增加 `shortcut_combo`
/// 存组合键字符串（如 "Ctrl+Shift+S"），其余 surface 不使用此字段。
/// K3 §4.4 在 keyword surface 增加 `trigger`（关键字路由触发词，可复用 keywords
/// 首项），在 root_search surface 增加 `show_in_root_search`（默认 true——false
/// 时命令不进根搜索 lane，只能经关键字/快捷键/动作面板到达）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandBinding {
    #[serde(default)]
    pub priority: i32,
    /// K2 §4.6：仅 shortcut binding 使用。组合键原始字符串，broker 不解析——
    /// 解析与冲突检测在 WPF 设置页完成，broker 只存储与下发。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut_combo: Option<String>,
    /// K3 §4.4：仅 keyword binding 使用。关键字路由的触发词，可复用 `keywords`
    /// 首项。None 时关键字路由不可达（需在设置页显式指定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// K3 §4.4：仅 root_search binding 使用。默认 true——false 时命令不进根搜索
    /// lane，只能经关键字/快捷键/动作面板到达。设计 §4.4 原文。
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub show_in_root_search: bool,
}

impl Default for CommandBinding {
    fn default() -> Self {
        Self {
            priority: 0,
            shortcut_combo: None,
            trigger: None,
            show_in_root_search: true,
        }
    }
}

/// 命令使用记录。不存参数、不存 query（设计 §14 R14）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandUsageData {
    #[serde(default)]
    pub entries: Vec<CommandUsageEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandUsageEntry {
    pub id: String,
    #[serde(default)]
    pub success_count: u32,
    #[serde(default)]
    pub last_success_utc: u64,
    #[serde(default)]
    pub frecency_milli: u32,
}

fn default_true() -> bool {
    true
}

/// skip_serializing_if helper：show_in_root_search 默认 true，省略时不序列化。
fn is_true(value: &bool) -> bool {
    *value
}

impl VersionedData for CommandData {
    const SCHEMA_VERSION: u32 = COMMANDS_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.commands.len() > COMMAND_MAX_ENTRIES {
            return Err("command store contains more than 512 entries".into());
        }
        for command in &self.commands {
            validate_command_id(&command.id)?;
            if command.title.chars().count() > COMMAND_TITLE_MAX_CHARS {
                return Err("command title exceeds 64 chars".into());
            }
            if command.subtitle.chars().count() > COMMAND_SUBTITLE_MAX_CHARS {
                return Err("command subtitle exceeds 128 chars".into());
            }
            if command.icon_glyph.chars().count() > 16 {
                return Err("command icon_glyph exceeds 16 chars".into());
            }
            if command.keywords.len() > COMMAND_MAX_KEYWORDS {
                return Err("command has more than 4 keywords".into());
            }
            for keyword in &command.keywords {
                if keyword.trim().is_empty()
                    || keyword.chars().count() > COMMAND_KEYWORD_MAX_CHARS
                    || keyword.chars().any(char::is_whitespace)
                {
                    return Err("command keyword is invalid".into());
                }
            }
            if command.input.kind.is_empty() {
                // 缺省视为 none
            } else if !matches!(
                command.input.kind.as_str(),
                "none" | "text" | "destination" | "output_path"
            ) {
                return Err("command input has an unsupported kind".into());
            }
            if command.input.prompt.chars().count() > 128 {
                return Err("command input prompt exceeds 128 chars".into());
            }
            if !matches!(
                command.danger.as_str(),
                "" | "normal" | "elevated" | "destructive"
            ) {
                return Err("command has an unsupported danger level".into());
            }
            // K3 §4.2 结构层：handler_params 键数 / 值长 / 键名格式（无 I/O）。
            // 必选键存在性与 scheme/路径校验属语义层，在 CommandSet（commit 4）执行。
            if command.handler_params.len() > COMMAND_HANDLER_PARAMS_MAX_KEYS {
                return Err("handler_params exceeds 8 keys".into());
            }
            for (key, value) in &command.handler_params {
                if key.is_empty() || key.chars().count() > COMMAND_HANDLER_PARAM_KEY_MAX_CHARS {
                    return Err("handler_params key is empty or exceeds 32 chars".into());
                }
                if !key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    return Err("handler_params key contains invalid characters".into());
                }
                if value.chars().count() > COMMAND_HANDLER_PARAM_MAX_CHARS {
                    return Err("handler_params value exceeds 1024 chars".into());
                }
            }
            // K2 §4.6：shortcut_combo 长度上限（组合键串如 "Ctrl+Shift+F12" ≤ 32 足够）。
            if let Some(combo) = command.bindings.shortcut.as_ref() {
                if let Some(c) = &combo.shortcut_combo {
                    if c.chars().count() > SHORTCUT_COMBO_MAX_CHARS {
                        return Err("shortcut_combo exceeds 64 chars".into());
                    }
                }
            }
        }
        // K2 §4.6：shortcut_bindings 上限与长度校验。
        if self.shortcut_bindings.len() > COMMAND_SHORTCUT_MAX_ENTRIES {
            return Err("shortcut_bindings exceeds 64 entries".into());
        }
        for entry in &self.shortcut_bindings {
            validate_command_id(&entry.command_id)?;
            if entry.combo.chars().count() > SHORTCUT_COMBO_MAX_CHARS {
                return Err("shortcut_binding combo exceeds 64 chars".into());
            }
        }
        Ok(())
    }
}

impl VersionedData for CommandUsageData {
    const SCHEMA_VERSION: u32 = COMMAND_USAGE_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.entries.len() > COMMAND_USAGE_MAX_ENTRIES {
            return Err("command usage contains more than 2000 entries".into());
        }
        for entry in &self.entries {
            validate_command_id(&entry.id)?;
            if entry.id.len() > COMMAND_ID_MAX_BYTES {
                return Err("command usage id exceeds 128 bytes".into());
            }
        }
        Ok(())
    }
}

/// 命令 id 语法校验：`prism.` 或 `user.` 前缀，其余字符仅 [a-z0-9._-]，无空白。
/// 复用于 CommandStore 与 Shell 校验，保证存储与执行面同一把尺。
pub fn validate_command_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > COMMAND_ID_MAX_BYTES {
        return Err("command id is empty or exceeds 128 bytes".into());
    }
    if id.contains('\0') {
        return Err("command id contains NUL".into());
    }
    // "prism." = 6 bytes, "user." = 5 bytes. 两种前缀长度不同，分开判定。
    let rest = if let Some(rest) = id.strip_prefix("prism.") {
        rest
    } else if let Some(rest) = id.strip_prefix("user.") {
        rest
    } else {
        return Err("command id must start with 'prism.' or 'user.'".into());
    };
    if rest.is_empty() {
        return Err("command id has no name after prefix".into());
    }
    for byte in rest.bytes() {
        let ok = byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || byte == b'.'
            || byte == b'_'
            || byte == b'-';
        if !ok {
            return Err("command id contains invalid characters".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_version_and_fields_use_safe_defaults() {
        let envelope: VersionedEnvelope<HistoryData> =
            serde_json::from_str(r#"{"data":{}}"#).unwrap();
        assert_eq!(envelope.schema_version, 0);
        assert!(envelope.into_compatible().unwrap().entries.is_empty());
    }

    #[test]
    fn future_schema_is_rejected() {
        let envelope: VersionedEnvelope<FaviconMetadata> =
            serde_json::from_str(r#"{"schema_version":999,"data":{"entries":[]}}"#).unwrap();
        assert!(envelope.into_compatible().is_err());
    }

    #[test]
    fn writer_validates_before_serializing_new_shape() {
        let data = HistoryData {
            entries: vec![HistoryEntry::default(); 5001],
        };
        assert!(VersionedEnvelope::new(data).is_err());
    }

    #[test]
    fn query_stats_are_bounded_and_nonempty() {
        let entry = |queries: Vec<QueryStat>| HistoryData {
            entries: vec![HistoryEntry {
                kind: "file".into(),
                target: r"C:\x".into(),
                queries,
                ..HistoryEntry::default()
            }],
        };
        let nine = (0..9)
            .map(|index| QueryStat {
                query: format!("q{index}"),
                count: 1,
                last_used_utc: 0,
            })
            .collect();
        assert!(VersionedEnvelope::new(entry(nine)).is_err());

        let empty = vec![QueryStat::default()];
        assert!(VersionedEnvelope::new(entry(empty)).is_err());

        let overlong = vec![QueryStat {
            query: "x".repeat(129),
            ..QueryStat::default()
        }];
        assert!(VersionedEnvelope::new(entry(overlong)).is_err());

        let valid = vec![QueryStat {
            query: "conf".into(),
            count: 2,
            last_used_utc: 42,
        }];
        assert!(VersionedEnvelope::new(entry(valid)).is_ok());
    }

    #[test]
    fn v1_entries_decode_into_the_v2_shape_with_defaults() {
        let envelope: VersionedEnvelope<HistoryData> = serde_json::from_str(
            r#"{"schema_version":1,"data":{"entries":[{"kind":"file","target":"C:\\a","execute_count":2,"last_used_utc":9}]}}"#,
        )
        .unwrap();
        let data = envelope.into_compatible().unwrap();
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].frecency_milli, 0);
        assert!(data.entries[0].queries.is_empty());
        assert_eq!(data.entries[0].first_used_utc, 0);
    }

    // ── K0 命令 schema ───────────────────────────────────────────────

    #[test]
    fn command_id_syntax_accepts_valid_and_rejects_invalid() {
        assert!(validate_command_id("prism.settings.open").is_ok());
        assert!(validate_command_id("user.my_cmd.v2").is_ok());
        assert!(validate_command_id("user.copy").is_ok());
        // 无前缀
        assert!(validate_command_id("settings.open").is_err());
        // 大写
        assert!(validate_command_id("prism.Settings").is_err());
        // 含斜杠
        assert!(validate_command_id("prism.settings/open").is_err());
        // 含空白
        assert!(validate_command_id("prism.set tings").is_err());
        // 仅前缀
        assert!(validate_command_id("prism.").is_err());
        // 超长
        assert!(validate_command_id(&format!("user.{}", "a".repeat(130))).is_err());
        // NUL
        assert!(validate_command_id("prism.set\u{0}tings").is_err());
        // 空串
        assert!(validate_command_id("").is_err());
    }

    #[test]
    fn command_data_future_schema_rejected() {
        let envelope: VersionedEnvelope<CommandData> =
            serde_json::from_str(r#"{"schema_version":999,"data":{"commands":[]}}"#).unwrap();
        assert!(envelope.into_compatible().is_err());
    }

    #[test]
    fn command_data_validates_bounds() {
        // 超 512 条
        let over = CommandData {
            commands: vec![
                UserCommandDefinition {
                    id: "user.x".into(),
                    title: "t".into(),
                    ..Default::default()
                };
                513
            ],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(over).is_err());

        // 中文标题按 chars 计，64 字符合法
        let title64: String = "设".repeat(64);
        let ok = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.title64".into(),
                title: title64,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(ok).is_ok());

        // 65 字符拒绝
        let title65: String = "设".repeat(65);
        let over_title = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.title65".into(),
                title: title65,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(over_title).is_err());

        // 5 个关键字拒绝
        let five_kw: Vec<String> = (0..5).map(|i| format!("k{i}")).collect();
        let over_kw = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.kw".into(),
                title: "t".into(),
                keywords: five_kw,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(over_kw).is_err());

        // 关键字含空白拒绝
        let space_kw = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.space".into(),
                title: "t".into(),
                keywords: vec!["has space".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(space_kw).is_err());

        // 未知 input kind 拒绝
        let bad_input = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.input".into(),
                title: "t".into(),
                input: CommandInputSpec {
                    kind: "magic".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(bad_input).is_err());

        // 未知 danger 拒绝
        let bad_danger = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.danger".into(),
                title: "t".into(),
                danger: "apocalypse".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(bad_danger).is_err());

        // 无效 id 拒绝
        let bad_id = CommandData {
            commands: vec![UserCommandDefinition {
                id: "badprefix.x".into(),
                title: "t".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(bad_id).is_err());
    }

    #[test]
    fn command_usage_data_validates_bounds() {
        // 超 2000 条
        let over = CommandUsageData {
            entries: vec![
                CommandUsageEntry {
                    id: "user.x".into(),
                    ..Default::default()
                };
                2001
            ],
        };
        assert!(VersionedEnvelope::new(over).is_err());

        // 无效 id
        let bad = CommandUsageData {
            entries: vec![CommandUsageEntry {
                id: "nope".into(),
                ..Default::default()
            }],
        };
        assert!(VersionedEnvelope::new(bad).is_err());

        // 合法
        let ok = CommandUsageData {
            entries: vec![CommandUsageEntry {
                id: "user.x".into(),
                success_count: 3,
                last_success_utc: 100,
                frecency_milli: 500,
            }],
        };
        assert!(VersionedEnvelope::new(ok).is_ok());
    }

    // ── K3 §4.1 commit 1：UserHandlerKind + handler_params ─────────────

    #[test]
    fn user_handler_kind_default_is_unknown() {
        let h: UserHandlerKind = serde_json::from_str("null").unwrap_or_default();
        assert_eq!(h, UserHandlerKind::Unknown);
    }

    #[test]
    fn user_handler_kind_unknown_string_decodes_to_unknown() {
        // 未知 handler 字符串 → Unknown，不使整个文件失败（§4.1 Unknown 降级语义）。
        let json = r#"{"schema_version":1,"data":{
            "commands":[
                {"id":"user.good","title":"ok","handler":"open_url","handler_params":{"url_template":"https://x.test/{query}"}},
                {"id":"user.bad","title":"bad","handler":"staging_zip","handler_params":{"path":"C:\\x"}}
            ]
        }}"#;
        let envelope: VersionedEnvelope<CommandData> = serde_json::from_str(json).unwrap();
        let data = envelope.into_compatible().unwrap();
        assert_eq!(data.commands.len(), 2);
        assert_eq!(data.commands[0].handler, UserHandlerKind::OpenUrl);
        assert_eq!(data.commands[1].handler, UserHandlerKind::Unknown);
    }

    #[test]
    fn user_handler_kind_missing_field_defaults_unknown() {
        // 现有 commands-v1.json 无 handler 字段 → Unknown，shortcut_bindings 不受影响。
        let json = r#"{"schema_version":1,"data":{
            "commands":[{"id":"user.legacy","title":"legacy"}],
            "shortcut_bindings":[{"command_id":"user.legacy","combo":"Ctrl+Shift+T"}]
        }}"#;
        let envelope: VersionedEnvelope<CommandData> = serde_json::from_str(json).unwrap();
        let data = envelope.into_compatible().unwrap();
        assert_eq!(data.commands[0].handler, UserHandlerKind::Unknown);
        assert!(data.commands[0].handler_params.is_empty());
        assert_eq!(data.shortcut_bindings.len(), 1);
        assert_eq!(data.shortcut_bindings[0].command_id, "user.legacy");
    }

    #[test]
    fn user_handler_kind_serde_roundtrip() {
        let def = UserCommandDefinition {
            id: "user.roundtrip".into(),
            title: "RT".into(),
            handler: UserHandlerKind::LaunchProgram,
            handler_params: {
                let mut m = std::collections::BTreeMap::new();
                m.insert("path".into(), r"C:\Windows\notepad.exe".into());
                m.insert("args_template".into(), "{query}".into());
                m
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&def).unwrap();
        let back: UserCommandDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn user_handler_kind_rejects_staging_zip_string() {
        // A4 验收：含 staging_zip 字符串的导入 JSON → 落 Unknown（类型无转换路径）。
        let json = r#"{"schema_version":1,"data":{
            "commands":[{"id":"user.evil","title":"evil","handler":"staging_zip"}]
        }}"#;
        let envelope: VersionedEnvelope<CommandData> = serde_json::from_str(json).unwrap();
        let data = envelope.into_compatible().unwrap();
        assert_eq!(data.commands[0].handler, UserHandlerKind::Unknown);
    }

    #[test]
    fn handler_params_too_many_keys_rejected() {
        let mut params = std::collections::BTreeMap::new();
        for i in 0..9 {
            params.insert(format!("k{i}"), "v".into());
        }
        let bad = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.too_many_keys".into(),
                title: "t".into(),
                handler_params: params,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(bad).is_err());
    }

    #[test]
    fn handler_params_value_too_long_rejected() {
        let mut params = std::collections::BTreeMap::new();
        params.insert("url_template".into(), "x".repeat(1025));
        let bad = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.long_val".into(),
                title: "t".into(),
                handler_params: params,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(bad).is_err());
    }

    #[test]
    fn handler_params_bad_key_name_rejected() {
        let mut params = std::collections::BTreeMap::new();
        params.insert("URL_Template".into(), "https://x.test".into());
        let bad = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.bad_key".into(),
                title: "t".into(),
                handler_params: params,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(bad).is_err());
    }

    #[test]
    fn handler_params_valid_bounds_accepted() {
        let mut params = std::collections::BTreeMap::new();
        params.insert("url_template".into(), "https://x.test/{query}".into());
        let ok = CommandData {
            commands: vec![UserCommandDefinition {
                id: "user.ok_handler".into(),
                title: "t".into(),
                handler: UserHandlerKind::OpenUrl,
                handler_params: params,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(VersionedEnvelope::new(ok).is_ok());
    }
}
