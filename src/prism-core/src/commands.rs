//! 统一命令系统（K0：兼容地基）。
//!
//! K0 只放类型、存储、能力协商的目录读路径，不执行任何命令、不路由关键字、
//! 不改动排序。命令身份 (`TargetKind::Command`) 在 broker 每条既有执行路径上
//! 被显式拒绝——命令 id 一旦被当成路径交给 Shell 层，能力 gating 就白做了。
//!
//! 存储照抄 `AliasStore` 家规：`RwLock<Data>` + `Mutex<()> persist_lock` +
//! tmp 文件 + `write_all` + `sync_all` + `atomic_replace` + 损坏隔离留档
//! 并回写合法空表。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::persistence::{
    validate_command_id, CommandData, CommandUsageData, UserCommandDefinition, VersionedEnvelope,
    COMMAND_MAX_ENTRIES, COMMAND_USAGE_MAX_ENTRIES,
};

const COMMANDS_FILE: &str = "commands-v1.json";
const COMMAND_USAGE_FILE: &str = "command-usage-v1.json";

// ── 内置命令目录（D5） ─────────────────────────────────────────────
//
// K0 放 1 条 `prism.settings.open`：owner=ui、danger=normal、所有 binding 为
// null（结构上不可达）。全 binding 为 null 使它在 K0 不可达，同时给测试提供
// 真实数据。K1 填 root_search binding，使两条命令在根搜索可见、可执行。

/// broker 下发的命令描述（Serialize-only）。与持久化用的 `UserCommandDefinition`
/// （Deserialize）是两个类型——类型收口保证导入 JSON 无法获得内置特权 handler。
#[derive(Debug, Clone, Serialize)]
pub struct CommandDescriptor {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub subtitle: String,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub icon_glyph: String,
    /// broker | ui
    pub owner: &'static str,
    /// builtin | user
    pub trust: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    pub input: CommandInputDto,
    pub bindings: CommandBindingsDto,
    pub danger: &'static str,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandInputDto {
    pub kind: &'static str,
    pub required: bool,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub prompt: String,
}

/// 各 surface 的绑定状态。K0 全 null（不可达），K1 填 root_search/keyword。
#[derive(Debug, Clone, Default, Serialize)]
pub struct CommandBindingsDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_search: Option<CommandBindingDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword: Option<CommandBindingDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_panel: Option<CommandBindingDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staging: Option<CommandBindingDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<CommandBindingDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandBindingDto {
    pub priority: i32,
}

/// K1 两条内置命令，均带 root_search binding（在根搜索中可见、可执行）。
/// `prism.settings.open` owner=ui → broker 返回 UiCommand 交给前端执行。
/// `prism.terminal.open` owner=broker → broker 直接打开终端。
fn builtin_catalog() -> Vec<CommandDescriptor> {
    vec![
        CommandDescriptor {
            id: "prism.settings.open".into(),
            title: "打开设置".into(),
            subtitle: String::new(),
            icon_glyph: String::new(),
            owner: "ui",
            trust: "builtin",
            keywords: Vec::new(),
            input: CommandInputDto {
                kind: "none",
                required: false,
                prompt: String::new(),
            },
            bindings: CommandBindingsDto {
                root_search: Some(CommandBindingDto { priority: 0 }),
                ..Default::default()
            },
            danger: "normal",
            enabled: true,
        },
        CommandDescriptor {
            id: "prism.terminal.open".into(),
            title: "在此处打开终端".into(),
            subtitle: String::new(),
            icon_glyph: String::new(),
            owner: "broker",
            trust: "builtin",
            keywords: Vec::new(),
            input: CommandInputDto {
                kind: "none",
                required: false,
                prompt: String::new(),
            },
            bindings: CommandBindingsDto {
                root_search: Some(CommandBindingDto { priority: 0 }),
                ..Default::default()
            },
            danger: "normal",
            enabled: true,
        },
    ]
}

// ── T8: handler registry ──────────────────────────────────────────
//
// K1 填入 prism.terminal.open → OpenTerminalHere。prism.settings.open owner=ui，
// 不经 broker handler（broker 返回 UiCommand 交给前端）。

/// K1 起使用的内置 handler 标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerHandlerId {
    OpenTerminalHere,
    SystemLock,
}

const BROKER_HANDLERS: &[(&str, BrokerHandlerId)] =
    &[("prism.terminal.open", BrokerHandlerId::OpenTerminalHere)];

/// 查 broker-owned 命令是否有对应 handler。
pub fn broker_handler(id: &str) -> Option<BrokerHandlerId> {
    BROKER_HANDLERS
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, h)| *h)
}

// ── CommandStore ──────────────────────────────────────────────────

/// 命令目录与使用记录共用一个 store（P1：不重构六跳穿参）。两把 persist 锁：
/// 目录写与使用记录写没有共享不变式，共用一把锁会让 K1 高频 usage 写阻塞
/// 目录读写。
pub struct CommandStore {
    path: PathBuf,
    usage_path: PathBuf,
    state: RwLock<CommandData>,
    usage: RwLock<CommandUsageData>,
    persist_lock: Mutex<()>,
    usage_persist_lock: Mutex<()>,
    generation: AtomicU64,
}

impl CommandStore {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(COMMANDS_FILE);
        let usage_path = data_dir.join(COMMAND_USAGE_FILE);

        let loaded = std::fs::read(&path).ok().and_then(|bytes| {
            serde_json::from_slice::<VersionedEnvelope<CommandData>>(&bytes)
                .ok()
                .and_then(|envelope| envelope.into_compatible().ok())
        });
        let (commands, isolated) = match loaded {
            Some(data) => (data.commands, false),
            None if path.exists() => {
                crate::history::isolate(&path, crate::history::now_utc());
                (Vec::new(), true)
            }
            None => (Vec::new(), false),
        };

        let usage_loaded = std::fs::read(&usage_path).ok().and_then(|bytes| {
            serde_json::from_slice::<VersionedEnvelope<CommandUsageData>>(&bytes)
                .ok()
                .and_then(|envelope| envelope.into_compatible().ok())
        });
        let (usage_entries, usage_isolated) = match usage_loaded {
            Some(data) => (data.entries, false),
            None if usage_path.exists() => {
                crate::history::isolate(&usage_path, crate::history::now_utc());
                (Vec::new(), true)
            }
            None => (Vec::new(), false),
        };

        let store = Self {
            path,
            usage_path,
            state: RwLock::new(CommandData { commands }),
            usage: RwLock::new(CommandUsageData {
                entries: usage_entries,
            }),
            persist_lock: Mutex::new(()),
            usage_persist_lock: Mutex::new(()),
            generation: AtomicU64::new(1), // 0 保留给「未知/未协商」
        };

        // 隔离分支：原件已归档，立即回写合法空表让文件回到盘上。
        if isolated {
            let _ = store.persist();
        }
        if usage_isolated {
            let _ = store.persist_usage();
        }
        store
    }

    /// 内置静态表 + 用户表合并。过滤 `enabled=false`；broker-owned 项还需有
    /// 注册 handler 才进目录（owner 分工：UI-owned 原样下发，WPF 侧过滤）。
    pub fn catalog(&self) -> Vec<CommandDescriptor> {
        let mut items = builtin_catalog();
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for command in &state.commands {
            if !command.enabled {
                continue;
            }
            // owner 分工：用户命令的 owner/trust 恒为 broker/user，由代码赋值。
            // broker-owned 命令需要注册 handler；K0 用户表为空，此处无过滤发生。
            items.push(user_command_to_descriptor(command));
        }
        items
    }

    /// 目录代际。`load` 后为 1（0 保留给「未知/未协商」），每次 mutation 递增。
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// 整体替换/新增一条用户命令。校验前置（先校验再改内存态），成功后
    /// generation+1。K0 不接 IPC，仅 store 层实现并测试（D3）。
    pub fn set(&self, def: UserCommandDefinition) -> Result<(), String> {
        validate_command_id(&def.id)?;
        {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // 上限口径：只数 entries.len()，不算同 id 替换。
            let replacing = state.commands.iter().any(|c| c.id == def.id);
            if !replacing && state.commands.len() >= COMMAND_MAX_ENTRIES {
                return Err("命令总条数已达上限（512）".into());
            }
            state.commands.retain(|c| c.id != def.id);
            state.commands.push(def);
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.persist()
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        validate_command_id(id)?;
        {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let before = state.commands.len();
            state.commands.retain(|c| c.id != id);
            if state.commands.len() == before {
                return Ok(()); // 幂等成功
            }
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.persist()
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        validate_command_id(id)?;
        {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(command) = state.commands.iter_mut().find(|c| c.id == id) else {
                return Ok(()); // 不存在：幂等成功
            };
            command.enabled = enabled;
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.persist()
    }

    /// 清空使用记录。经 `ClearHistory` 联动（K0 已接）。清内存 + 删/重写文件。
    pub fn clear_usage(&self) -> Result<(), String> {
        {
            let mut usage = self
                .usage
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            usage.entries.clear();
        }
        // 重写为合法空表（与 history.clear 删除文件不同：命令使用记录文件留空表
        // 而非删除，避免下次 record 时重建文件名竞争）。
        self.persist_usage()
    }

    /// 记录命令执行成功。K0 不接 IPC（K1 消费者），仅留签名与单测。
    /// 写盘受调用方传入的 `history_enabled` 门控。
    pub fn record_success(&self, id: &str, now: u64, history_enabled: bool) -> Result<(), String> {
        if !history_enabled {
            return Ok(());
        }
        validate_command_id(id)?;
        {
            let mut usage = self
                .usage
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = usage.entries.iter_mut().find(|e| e.id == id);
            match entry {
                Some(entry) => {
                    entry.success_count = entry.success_count.saturating_add(1);
                    entry.last_success_utc = now;
                }
                None => {
                    if usage.entries.len() >= COMMAND_USAGE_MAX_ENTRIES {
                        // 容量 LRU：按 last_success 降序截断
                        usage
                            .entries
                            .sort_by_key(|a| std::cmp::Reverse(a.last_success_utc));
                        usage.entries.truncate(COMMAND_USAGE_MAX_ENTRIES - 1);
                    }
                    usage.entries.push(crate::persistence::CommandUsageEntry {
                        id: id.to_owned(),
                        success_count: 1,
                        last_success_utc: now,
                        frecency_milli: 0,
                    });
                }
            }
        }
        self.persist_usage()
    }

    fn persist(&self) -> Result<(), String> {
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
        let bytes =
            serde_json::to_vec_pretty(&envelope).map_err(|e| format!("serialize commands: {e}"))?;
        let temporary = self.path.with_extension("json.tmp");
        std::fs::create_dir_all(self.path.parent().unwrap_or(Path::new(".")))
            .map_err(|e| format!("create commands directory: {e}"))?;
        {
            let mut file =
                std::fs::File::create(&temporary).map_err(|e| format!("write commands: {e}"))?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|e| format!("write commands: {e}"))?;
        }
        crate::fs_util::atomic_replace(&temporary, &self.path, "commands")
    }

    fn persist_usage(&self) -> Result<(), String> {
        let _guard = self
            .usage_persist_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = self
            .usage
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let envelope = VersionedEnvelope::new(snapshot)?;
        let bytes = serde_json::to_vec_pretty(&envelope)
            .map_err(|e| format!("serialize command usage: {e}"))?;
        let temporary = self.usage_path.with_extension("json.tmp");
        std::fs::create_dir_all(self.usage_path.parent().unwrap_or(Path::new(".")))
            .map_err(|e| format!("create command usage directory: {e}"))?;
        {
            let mut file = std::fs::File::create(&temporary)
                .map_err(|e| format!("write command usage: {e}"))?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|e| format!("write command usage: {e}"))?;
        }
        crate::fs_util::atomic_replace(&temporary, &self.usage_path, "command-usage")
    }
}

fn user_command_to_descriptor(command: &UserCommandDefinition) -> CommandDescriptor {
    CommandDescriptor {
        id: command.id.clone(),
        title: command.title.clone(),
        subtitle: command.subtitle.clone(),
        icon_glyph: command.icon_glyph.clone(),
        owner: "broker",
        trust: "user",
        keywords: command.keywords.clone(),
        input: CommandInputDto {
            kind: if command.input.kind.is_empty() {
                "none"
            } else {
                // validate 已保证 kind 合法；此处用 leak-free 的方式返回 &'static str
                // 不现实，改为固定映射。
                match command.input.kind.as_str() {
                    "text" => "text",
                    "destination" => "destination",
                    "output_path" => "output_path",
                    _ => "none",
                }
            },
            required: command.input.required,
            prompt: command.input.prompt.clone(),
        },
        bindings: CommandBindingsDto {
            root_search: command
                .bindings
                .root_search
                .as_ref()
                .map(|b| CommandBindingDto {
                    priority: b.priority,
                }),
            keyword: command
                .bindings
                .keyword
                .as_ref()
                .map(|b| CommandBindingDto {
                    priority: b.priority,
                }),
            action_panel: command
                .bindings
                .action_panel
                .as_ref()
                .map(|b| CommandBindingDto {
                    priority: b.priority,
                }),
            staging: command
                .bindings
                .staging
                .as_ref()
                .map(|b| CommandBindingDto {
                    priority: b.priority,
                }),
            shortcut: command
                .bindings
                .shortcut
                .as_ref()
                .map(|b| CommandBindingDto {
                    priority: b.priority,
                }),
        },
        danger: match command.danger.as_str() {
            "" | "normal" => "normal",
            "elevated" => "elevated",
            "destructive" => "destructive",
            _ => "normal",
        },
        enabled: command.enabled,
    }
}

// ── T7: CommandInvocationContext（类型 + 校验，不执行） ──────────
//
// K0 交付 validate() + 全部边界单测。512 KiB 的线路级检查随 K1 的
// ExecuteCommand 请求一起落地（K1-3）。

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CommandInvocationContext {
    pub command_id: String,
    pub source: InvocationSource,
    #[serde(default)]
    pub arguments: CommandArguments,
    #[serde(default)]
    pub selection: Option<CommandSelection>,
    #[serde(default)]
    pub staged_paths: Vec<String>,
    #[serde(default)]
    pub current_folder: Option<String>,
    #[serde(default)]
    pub host_kind: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvocationSource {
    #[default]
    Root,
    Keyword,
    ActionPanel,
    Staging,
    Shortcut,
}

/// 严格解析：deny_unknown_fields 只加在这一层（K1→K2 新 WPF 加外层字段时
/// 旧 broker 不会整体拒绝），防「把 ZIP 输出路径塞进自由文本」这类越权。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandArguments {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub output_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CommandSelection {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
}

const TEXT_MAX_BYTES: usize = 8 * 1024;
const PATH_MAX_BYTES: usize = 32 * 1024;
const STAGED_MAX: usize = 128;
const SELECTION_TITLE_MAX_CHARS: usize = 256;
const APPROX_MAX_JSON: usize = 512 * 1024;

impl CommandInvocationContext {
    /// 覆盖设计 §5.3 全部上限。512 KiB 的线路级检查（与 MAX_REQUEST_LINE_BYTES
    /// 叠加）随 K1 的 ExecuteCommand 落地（K1-3）。
    pub fn validate(&self) -> Result<(), String> {
        validate_command_id(&self.command_id)?;

        if self.staged_paths.len() > STAGED_MAX {
            return Err("staged_paths exceeds 128 items".into());
        }
        for path in &self.staged_paths {
            validate_path_field(path)?;
        }
        if let Some(folder) = &self.current_folder {
            validate_path_field(folder)?;
        }
        if let Some(text) = &self.arguments.text {
            if text.len() > TEXT_MAX_BYTES {
                return Err("text exceeds 8 KiB".into());
            }
        }
        if let Some(dest) = &self.arguments.destination {
            validate_path_field(dest)?;
        }
        if let Some(out) = &self.arguments.output_path {
            validate_path_field(out)?;
        }
        if let Some(selection) = &self.selection {
            if selection.title.chars().count() > SELECTION_TITLE_MAX_CHARS {
                return Err("selection title exceeds 256 chars".into());
            }
            if selection.subtitle.chars().count() > SELECTION_TITLE_MAX_CHARS {
                return Err("selection subtitle exceeds 256 chars".into());
            }
        }
        if self.approximate_json_len() > APPROX_MAX_JSON {
            return Err("command invocation exceeds 512 KiB".into());
        }
        Ok(())
    }

    /// 粗估序列化后字节数。不真正序列化——只量字段体积。
    fn approximate_json_len(&self) -> usize {
        let mut total = 256; // 结构开销估值
        total += self.command_id.len();
        if let Some(text) = &self.arguments.text {
            total += text.len();
        }
        if let Some(dest) = &self.arguments.destination {
            total += dest.len();
        }
        if let Some(out) = &self.arguments.output_path {
            total += out.len();
        }
        for path in &self.staged_paths {
            total += path.len() + 4; // 引号+逗号
        }
        if let Some(selection) = &self.selection {
            total += selection.title.len() + selection.subtitle.len();
        }
        total
    }
}

fn validate_path_field(path: &str) -> Result<(), String> {
    if path.is_empty() || path.contains('\0') || path.chars().any(char::is_control) {
        return Err("path field is invalid".into());
    }
    if path.len() > PATH_MAX_BYTES {
        return Err("path field exceeds 32 KiB".into());
    }
    if path.starts_with(r"\\") {
        return Err("UNC paths are not allowed".into());
    }
    if !Path::new(path).is_absolute() {
        return Err("path field must be absolute".into());
    }
    Ok(())
}

// ── T5: SearchCommandContext（broker 侧解析 + 校验，不消费） ──────
//
// K0 上线发送（仅协商后连接），broker 解析 + 校验 + 丢弃。未协商连接的
// search payload 逐字节旧格式。

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchCommandContext {
    #[serde(default)]
    pub current_folder: Option<String>,
    #[serde(default)]
    pub host_kind: Option<String>,
    #[serde(default)]
    pub host_capabilities: Vec<String>,
}

const HOST_KIND_WHITELIST: &[&str] = &["none", "explorer", "system_file_dialog", "directory_opus"];
const HOST_CAPS_MAX: usize = 8;
const HOST_CAP_MAX_BYTES: usize = 32;
const CURRENT_FOLDER_MAX_BYTES: usize = 32 * 1024;

/// 连接级能力开关。Copy：Search 分支 spawn 到独立任务，按值捕获无需 Arc。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConnectionCaps {
    pub commands_v1: bool,
}

impl SearchCommandContext {
    /// 在进 search_service 之前执行。未协商 → 整体丢弃（单点门控）。
    /// K0 消费者为空：search_service 收下后 `let _ = command_context;`。
    pub fn sanitize(self, caps: ConnectionCaps) -> Option<Self> {
        if !caps.commands_v1 {
            return None;
        }
        let current_folder = self.current_folder.filter(|folder| {
            !folder.is_empty()
                && !folder.contains('\0')
                && !folder.contains('"')
                && !folder.chars().any(char::is_control)
                && folder.len() <= CURRENT_FOLDER_MAX_BYTES
                && !folder.starts_with(r"\\")
                && Path::new(folder).is_absolute()
        });
        let host_kind = self
            .host_kind
            .filter(|kind| HOST_KIND_WHITELIST.contains(&kind.as_str()));
        let host_capabilities = self
            .host_capabilities
            .into_iter()
            .filter(|cap| {
                !cap.is_empty()
                    && cap.len() <= HOST_CAP_MAX_BYTES
                    && !cap.contains('\0')
                    && !cap.chars().any(char::is_control)
            })
            .take(HOST_CAPS_MAX)
            .collect();
        Some(Self {
            current_folder,
            host_kind,
            host_capabilities,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (CommandStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("prism-cmd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (CommandStore::load(&dir), dir)
    }

    fn user_cmd(id: &str) -> UserCommandDefinition {
        UserCommandDefinition {
            id: id.into(),
            title: "Test".into(),
            enabled: true,
            ..Default::default()
        }
    }

    // R1: set → persist → load 往返
    #[test]
    fn set_persist_load_roundtrip() {
        let (store, dir) = store("roundtrip");
        store.set(user_cmd("user.test")).unwrap();
        assert_eq!(store.catalog().len(), 3, "2 builtin + 1 user");
        let reloaded = CommandStore::load(&dir);
        assert_eq!(reloaded.catalog().len(), 3);
        assert_eq!(reloaded.generation(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // R2: 未来版本拒绝
    #[test]
    fn future_schema_rejected_on_load() {
        let dir = std::env::temp_dir().join(format!("prism-cmd-future-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(COMMANDS_FILE),
            r#"{"schema_version":999,"data":{"commands":[]}}"#,
        )
        .unwrap();
        let store = CommandStore::load(&dir);
        assert!(
            store.catalog().len() == 2,
            "future version → 空用户表 + 2 builtin"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // R3: 损坏文件隔离留档
    #[test]
    fn corrupt_file_isolated_and_rewritten() {
        let dir = std::env::temp_dir().join(format!("prism-cmd-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(COMMANDS_FILE);
        std::fs::write(&path, b"{ not json").unwrap();

        let store = CommandStore::load(&dir);
        assert_eq!(store.catalog().len(), 2, "损坏 → 空用户表 + 2 builtin");

        let mut isolated = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if name.starts_with("commands-v1.corrupt-") {
                isolated += 1;
            }
        }
        assert_eq!(isolated, 1, "损坏原件留档");
        assert!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).is_ok(),
            "盘上回到合法 JSON"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // R4: 边界校验
    #[test]
    fn set_rejects_invalid_commands() {
        let (store, _dir) = store("validate");
        // 无效 id
        assert!(store.set(user_cmd("badprefix.x")).is_err());
        // 超长 title（65 字符）
        let mut cmd = user_cmd("user.long");
        cmd.title = "设".repeat(65);
        assert!(store.set(cmd).is_err());
        // 5 个关键字
        let mut cmd = user_cmd("user.kw");
        cmd.keywords = (0..5).map(|i| format!("k{i}")).collect();
        assert!(store.set(cmd).is_err());
    }

    // R5: generation 单调递增
    #[test]
    fn generation_starts_at_one_and_increments() {
        let (store, _dir) = store("gen");
        assert_eq!(store.generation(), 1, "load 后为 1");
        store.set(user_cmd("user.a")).unwrap();
        assert_eq!(store.generation(), 2);
        store.set(user_cmd("user.b")).unwrap();
        assert_eq!(store.generation(), 3);
        store.delete("user.a").unwrap();
        assert_eq!(store.generation(), 4);
        store.set_enabled("user.b", false).unwrap();
        assert_eq!(store.generation(), 5);
        // enabled=false 不进 catalog，只剩 builtin
        assert_eq!(store.catalog().len(), 2, "2 builtin only (user.b disabled)");
    }

    // R6: 并发 mutation 不撕裂
    #[test]
    fn concurrent_mutations_do_not_tear() {
        let (store, dir) = store("concurrent");
        std::thread::scope(|scope| {
            for i in 0..8 {
                let store = &store;
                scope.spawn(move || {
                    for j in 0..20 {
                        let id = format!("user.t{i}_{j}");
                        let _ = store.set(user_cmd(&id));
                    }
                });
            }
            for i in 0..4 {
                let store = &store;
                scope.spawn(move || {
                    for j in 0..10 {
                        let id = format!("user.t{i}_{j}");
                        let _ = store.delete(&id);
                    }
                });
            }
        });
        // 文件可解析
        let bytes = std::fs::read(dir.join(COMMANDS_FILE)).unwrap();
        let envelope: VersionedEnvelope<CommandData> = serde_json::from_slice(&bytes).unwrap();
        let _ = envelope.into_compatible().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // R16: CommandInvocationContext::validate
    #[test]
    fn invocation_context_validates_bounds() {
        let base = || CommandInvocationContext {
            command_id: "user.test".into(),
            source: InvocationSource::Root,
            ..Default::default()
        };

        // staged 129 项拒绝
        let mut ctx = base();
        ctx.staged_paths = (0..129).map(|i| format!(r"C:\p{i}")).collect();
        assert!(ctx.validate().is_err());

        // 单路径 UNC 拒绝
        let mut ctx = base();
        ctx.staged_paths = vec![r"\\server\share".into()];
        assert!(ctx.validate().is_err());

        // 单路径相对拒绝
        let mut ctx = base();
        ctx.staged_paths = vec!["relative".into()];
        assert!(ctx.validate().is_err());

        // text 8KiB+1 拒绝
        let mut ctx = base();
        ctx.arguments.text = Some("x".repeat(8 * 1024 + 1));
        assert!(ctx.validate().is_err());

        // CommandArguments 含未知字段拒绝
        let json = r#"{"command_id":"user.test","source":"root","arguments":{"unknown":"x"}}"#;
        assert!(serde_json::from_str::<CommandInvocationContext>(json).is_err());

        // 合法
        let mut ctx = base();
        ctx.staged_paths = vec![r"C:\a".into(), r"C:\b".into()];
        ctx.arguments.text = Some("hello".into());
        assert!(ctx.validate().is_ok());

        // 聚合 >512 KiB 拒绝
        let mut ctx = base();
        ctx.arguments.text = Some("x".repeat(500 * 1024));
        ctx.staged_paths = vec!["y".repeat(20 * 1024)];
        assert!(ctx.validate().is_err());
    }

    // R15: SearchCommandContext::sanitize
    #[test]
    fn command_context_sanitize() {
        let caps = ConnectionCaps { commands_v1: true };
        let no_caps = ConnectionCaps { commands_v1: false };

        // 未协商 → 整体丢弃
        let ctx = SearchCommandContext {
            current_folder: Some(r"C:\Users".into()),
            ..Default::default()
        };
        assert!(ctx.clone().sanitize(no_caps).is_none());

        // 协商 → 保留
        let sanitized = ctx.sanitize(caps).unwrap();
        assert_eq!(sanitized.current_folder.as_deref(), Some(r"C:\Users"));

        // 相对路径 → None
        let ctx = SearchCommandContext {
            current_folder: Some("relative".into()),
            ..Default::default()
        };
        assert!(ctx.sanitize(caps).unwrap().current_folder.is_none());

        // UNC → None
        let ctx = SearchCommandContext {
            current_folder: Some(r"\\server\share".into()),
            ..Default::default()
        };
        assert!(ctx.sanitize(caps).unwrap().current_folder.is_none());

        // 未知 host_kind → None
        let ctx = SearchCommandContext {
            host_kind: Some("future".into()),
            ..Default::default()
        };
        assert!(ctx.sanitize(caps).unwrap().host_kind.is_none());

        // 合法 host_kind → 保留
        let ctx = SearchCommandContext {
            host_kind: Some("explorer".into()),
            ..Default::default()
        };
        assert_eq!(
            ctx.sanitize(caps).unwrap().host_kind.as_deref(),
            Some("explorer")
        );

        // host_capabilities 超 8 截断
        let ctx = SearchCommandContext {
            host_capabilities: (0..10).map(|i| format!("cap{i}")).collect(),
            ..Default::default()
        };
        assert_eq!(ctx.sanitize(caps).unwrap().host_capabilities.len(), 8);

        // host_capabilities 含控制字符丢弃
        let ctx = SearchCommandContext {
            host_capabilities: vec!["ok".into(), "ba\td".into()],
            ..Default::default()
        };
        assert_eq!(ctx.sanitize(caps).unwrap().host_capabilities, vec!["ok"]);
    }

    // R17: clear_usage 联动
    #[test]
    fn clear_usage_empties_file() {
        let (store, dir) = store("clear_usage");
        store.record_success("user.test", 100, true).unwrap();
        assert!(!store.usage.read().unwrap().entries.is_empty());

        store.clear_usage().unwrap();
        assert!(store.usage.read().unwrap().entries.is_empty());

        // 盘上是合法空表
        let bytes = std::fs::read(dir.join(COMMAND_USAGE_FILE)).unwrap();
        let envelope: VersionedEnvelope<CommandUsageData> = serde_json::from_slice(&bytes).unwrap();
        assert!(envelope.into_compatible().unwrap().entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_success_respects_history_disabled() {
        let (store, _dir) = store("disabled");
        store.record_success("user.test", 100, false).unwrap();
        assert!(store.usage.read().unwrap().entries.is_empty());
    }

    // T8: handler registry —— K1 填入 terminal handler
    #[test]
    fn broker_handler_resolves_terminal() {
        assert_eq!(
            broker_handler("prism.terminal.open"),
            Some(BrokerHandlerId::OpenTerminalHere)
        );
        // ui-owned 命令无 broker handler
        assert!(broker_handler("prism.settings.open").is_none());
        assert!(broker_handler("unknown").is_none());
    }

    // D5: 内置目录含 prism.settings.open + prism.terminal.open
    #[test]
    fn builtin_catalog_has_both_commands() {
        let (store, _dir) = store("builtin");
        let catalog = store.catalog();
        let settings = catalog
            .iter()
            .find(|d| d.id == "prism.settings.open")
            .unwrap();
        assert_eq!(settings.owner, "ui");
        assert_eq!(settings.trust, "builtin");
        assert!(settings.enabled);
        // K1 root_search binding 已填
        assert!(settings.bindings.root_search.is_some());

        let terminal = catalog
            .iter()
            .find(|d| d.id == "prism.terminal.open")
            .unwrap();
        assert_eq!(terminal.owner, "broker");
        assert_eq!(terminal.trust, "builtin");
        assert!(terminal.enabled);
        assert!(terminal.bindings.root_search.is_some());
    }

    // R7: ActionTarget{kind:"command"} validate
    #[test]
    fn command_target_validate_accepts_and_rejects() {
        use crate::shell::{ActionTarget, ShellErrorKind, TargetKind};

        // 合法 id 通过
        let t = ActionTarget {
            kind: "command".into(),
            value: "prism.settings.open".into(),
        };
        assert_eq!(t.validate().unwrap(), TargetKind::Command);

        // 129 字节拒绝
        let long = format!("user.{}", "a".repeat(130));
        let t = ActionTarget {
            kind: "command".into(),
            value: long,
        };
        assert_eq!(
            t.validate().unwrap_err().kind,
            ShellErrorKind::TargetInvalid
        );

        // 大写拒绝
        let t = ActionTarget {
            kind: "command".into(),
            value: "prism.Settings".into(),
        };
        assert_eq!(
            t.validate().unwrap_err().kind,
            ShellErrorKind::TargetInvalid
        );

        // 含 / 拒绝
        let t = ActionTarget {
            kind: "command".into(),
            value: "prism.settings/open".into(),
        };
        assert_eq!(
            t.validate().unwrap_err().kind,
            ShellErrorKind::TargetInvalid
        );

        // 含空白拒绝
        let t = ActionTarget {
            kind: "command".into(),
            value: "prism.set tings".into(),
        };
        assert_eq!(
            t.validate().unwrap_err().kind,
            ShellErrorKind::TargetInvalid
        );

        // 无前缀拒绝
        let t = ActionTarget {
            kind: "command".into(),
            value: "settings.open".into(),
        };
        assert_eq!(
            t.validate().unwrap_err().kind,
            ShellErrorKind::TargetInvalid
        );
    }

    // R8: 命令 target 被逐一拒绝（五处拒绝点）
    #[test]
    fn command_target_rejected_everywhere() {
        use crate::shell::{ActionTarget, ShellErrorKind};

        let cmd = ActionTarget {
            kind: "command".into(),
            value: "prism.exit".into(),
        };

        // actions::list_actions → Err(Unsupported)
        let err = crate::actions::list_actions(&cmd).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);

        // shell::execute_run_action 拒绝（pub(crate) 纯函数）
        let err =
            crate::shell::execute_run_action(cmd.clone(), "copy".into(), Default::default(), None)
                .unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);

        // history::is_recordable == false（history-v2.json 不被污染的唯一防线）
        assert!(!crate::history::is_recordable(&cmd));
    }

    // R9: IPC 守卫验证 TargetKind::parse 识别 command
    #[test]
    fn ipc_guard_recognizes_command_target() {
        // 验证 TargetKind::parse 对 "command" 返回 Command
        assert_eq!(
            crate::shell::TargetKind::parse("command"),
            Some(crate::shell::TargetKind::Command)
        );
        // 验证 resolve_target 不回退 command kind 为 File（target 已显式提供）
        let target = crate::ipc::resolve_target(
            Some(crate::shell::ActionTarget {
                kind: "command".into(),
                value: "prism.exit".into(),
            }),
            None,
            None,
        );
        assert_eq!(target.kind, "command");
    }
}
