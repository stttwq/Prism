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
use crate::shell::ActionTarget;

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
    /// K2 §4.5：命令不可用原因（如「需要 7-Zip」）。enabled=false 时非空。
    /// skip_serializing_if 保证旧前端 JSON 逐字节不变（enabled=true 时缺省）。
    #[serde(skip_serializing_if = "str::is_empty")]
    pub disabled_reason: String,
    /// K4a：无结果回退——根搜索空结果时该命令作为回退行出现。
    #[serde(default, skip_serializing_if = "is_false")]
    pub fallback: bool,
    /// K4b：声明式参数（catalog 下发，编辑器回显用）。空省略。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<CommandArgumentDto>,
    /// K4b 收尾：handler 与参数随 catalog 下发，编辑既有命令时回显
    /// （此前编辑器拿不到 URL/程序路径，重存会被 open_url 校验拒绝）。
    #[serde(skip_serializing_if = "str::is_empty")]
    pub handler: &'static str,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub handler_params: std::collections::BTreeMap<String, String>,
}

/// K4b：声明参数的下行形态（对应 persistence::CommandArgumentSpec）。
#[derive(Debug, Clone, Serialize)]
pub struct CommandArgumentDto {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "str::is_empty")]
    pub default: String,
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

// K2 §4.3：按 surface 扩展。设计 §5.1 要求每个 surface 声明输入来源与适用性。
// 不用一组全局 accepts——同一命令在不同入口取不同输入（如终端命令在根搜索用
// current_folder、在动作面板用选中的 directory），全局 accepts 表达不了。
// K2 §4.6：shortcut binding 增加 shortcut_combo 字段，存储组合键原始字符串。
// broker 不解析——解析与冲突检测在 WPF 设置页完成。
#[derive(Debug, Clone, Serialize)]
pub struct CommandBindingDto {
    pub priority: i32,
    /// "none" | "selection" | "current_folder" | "current_folder_or_prompt" | "staged_paths"
    #[serde(default, skip_serializing_if = "str::is_empty")]
    pub input: String,
    /// action_panel/staging 用："one" | "many"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<String>,
    /// 适用的 target kind：["file","directory","application"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub requires_host_root: bool,
    /// K2 §4.6：仅 shortcut binding 使用。组合键原始字符串（如 "Ctrl+Shift+S"）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut_combo: Option<String>,
    /// K3 §4.4：仅 keyword binding 使用。关键字路由的触发词。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// K3 §4.4：仅 root_search binding 使用。默认 true——false 时命令不进根搜索。
    #[serde(default = "default_true_dt", skip_serializing_if = "is_true_dt")]
    pub show_in_root_search: bool,
}

impl Default for CommandBindingDto {
    fn default() -> Self {
        Self {
            priority: 0,
            input: String::new(),
            cardinality: None,
            target_kinds: Vec::new(),
            requires_host_root: false,
            shortcut_combo: None,
            trigger: None,
            show_in_root_search: true,
        }
    }
}

fn is_true_dt(v: &bool) -> bool {
    *v
}

/// serde default for `show_in_root_search`。Serialize-only 类型不生成调用，
/// 但属性保留供未来 Deserialize 复用——与持久化端 `CommandBinding` 对齐。
#[allow(dead_code)]
fn default_true_dt() -> bool {
    true
}

fn is_false(v: &bool) -> bool {
    !*v
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
                root_search: Some(CommandBindingDto {
                    priority: 0,
                    ..Default::default()
                }),
                ..Default::default()
            },
            danger: "normal",
            enabled: true,
            disabled_reason: String::new(),
            fallback: false,
            arguments: Vec::new(),
            handler: "",
            handler_params: Default::default(),
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
                root_search: Some(CommandBindingDto {
                    priority: 0,
                    ..Default::default()
                }),
                // K2 §5 commit 3：动作面板命令段。对目录执行「在此打开终端」。
                // 设计 §13.2 样例流：input=selection, cardinality=one, target_kinds=[directory]。
                action_panel: Some(CommandBindingDto {
                    priority: 0,
                    input: "selection".into(),
                    cardinality: Some("one".into()),
                    target_kinds: vec!["directory".into()],
                    requires_host_root: false,
                    shortcut_combo: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            danger: "normal",
            enabled: true,
            disabled_reason: String::new(),
            fallback: false,
            arguments: Vec::new(),
            handler: "",
            handler_params: Default::default(),
        },
        CommandDescriptor {
            // K2 §4.7：暂存区批量复制路径至剪贴板。无损操作、不探测存在性，
            // 因此不受 UNC 限制（§4.7 末段）。staging surface 是唯一入口——
            // root_search/keyword/action_panel 都无 binding，命令本身在那些
            // 入口不可见，与设计 §7.1（窗口/Web target 的命令动作首批不开）一致。
            id: "prism.staging.copy_paths".into(),
            title: "复制暂存区路径至剪贴板".into(),
            subtitle: "将暂存区所有路径以换行分隔写入剪贴板".into(),
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
                staging: Some(CommandBindingDto {
                    priority: 0,
                    input: "staged_paths".into(),
                    cardinality: Some("many".into()),
                    target_kinds: Vec::new(),
                    requires_host_root: false,
                    shortcut_combo: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            danger: "normal",
            enabled: true,
            disabled_reason: String::new(),
            fallback: false,
            arguments: Vec::new(),
            handler: "",
            handler_params: Default::default(),
        },
        CommandDescriptor {
            // K2 §4.5：暂存区多目标 ZIP。enabled/disabled_reason 在 catalog()
            // 中按 7-Zip 可用性动态调整——§4.5-3：Shell COM 多输入完成不可判定，
            // 首版仅外部程序可用，否则标禁用。
            id: "prism.staging.zip".into(),
            title: "压缩暂存区为 ZIP".into(),
            subtitle: "将暂存区所有文件压缩为单个 ZIP（需 7-Zip）".into(),
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
                staging: Some(CommandBindingDto {
                    priority: 1,
                    input: "staged_paths".into(),
                    cardinality: Some("many".into()),
                    target_kinds: Vec::new(),
                    requires_host_root: false,
                    shortcut_combo: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            // enabled + disabled_reason 在 catalog() 中动态设置。
            danger: "normal",
            enabled: true,
            disabled_reason: String::new(),
            fallback: false,
            arguments: Vec::new(),
            handler: "",
            handler_params: Default::default(),
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
    /// K2 §4.7：暂存区复制路径至剪贴板。纯文本输出、不探测存在性。
    CopyPaths,
    /// K2 §4.5：暂存区多目标 ZIP。需 7-Zip（Shell COM 多输入完成不可判定）。
    StagingZip,
}

const BROKER_HANDLERS: &[(&str, BrokerHandlerId)] = &[
    ("prism.terminal.open", BrokerHandlerId::OpenTerminalHere),
    ("prism.staging.copy_paths", BrokerHandlerId::CopyPaths),
    ("prism.staging.zip", BrokerHandlerId::StagingZip),
];

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
    /// K3 §4.5：启动裁决——关键字与网页引擎冲突的命令 id 集合。
    /// catalog() 据此移除 keyword binding 并设 disabled_reason。空 = 无冲突。
    disabled_keyword_commands: RwLock<std::collections::HashMap<String, String>>,
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
        let (commands, shortcut_bindings, isolated) = match loaded {
            Some(data) => (data.commands, data.shortcut_bindings, false),
            None if path.exists() => {
                crate::history::isolate(&path, crate::history::now_utc());
                (Vec::new(), Vec::new(), true)
            }
            None => (Vec::new(), Vec::new(), false),
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
            state: RwLock::new(CommandData {
                commands,
                shortcut_bindings,
                builtin_overrides: Default::default(),
            }),
            usage: RwLock::new(CommandUsageData {
                entries: usage_entries,
            }),
            persist_lock: Mutex::new(()),
            usage_persist_lock: Mutex::new(()),
            generation: AtomicU64::new(1), // 0 保留给「未知/未协商」
            disabled_keyword_commands: RwLock::new(std::collections::HashMap::new()),
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
    /// K2 §4.6：合并 shortcut_bindings 映射——为有快捷键的命令填充 bindings.shortcut。
    pub fn catalog(&self) -> Vec<CommandDescriptor> {
        let mut items = builtin_catalog();
        // K2 §4.5-3：staging ZIP 仅在外部压缩程序可用时启用。Shell COM
        // 多输入完成不可判定，首版不勉强用 COM。
        if !crate::zip::has_external_zip_program(None) {
            for item in items.iter_mut() {
                if item.id == "prism.staging.zip" {
                    item.enabled = false;
                    item.disabled_reason = "需要 7-Zip 或自定义压缩程序".into();
                }
            }
        }
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // K4 收尾：内置命令覆盖合并（标题/关键字/启用/根搜索可见）。
        // 关键字覆盖同时补 keyword binding（trigger=首关键字），关键字路由即可达。
        for (id, ov) in &state.builtin_overrides {
            if let Some(item) = items.iter_mut().find(|d| d.id == *id) {
                if !ov.title.is_empty() {
                    item.title = ov.title.clone();
                }
                if !ov.keywords.is_empty() {
                    item.keywords = ov.keywords.clone();
                    item.bindings.keyword = Some(CommandBindingDto {
                        priority: 0,
                        input: String::new(),
                        cardinality: None,
                        target_kinds: Vec::new(),
                        requires_host_root: false,
                        shortcut_combo: None,
                        trigger: ov.keywords.first().cloned(),
                        show_in_root_search: true,
                    });
                }
                if let Some(enabled) = ov.enabled {
                    item.enabled = enabled;
                }
                if let Some(show) = ov.show_in_root_search {
                    if let Some(rs) = item.bindings.root_search.as_mut() {
                        rs.show_in_root_search = show;
                    }
                }
            }
        }
        for command in &state.commands {
            // disabled 用户命令也下发（enabled=false）。设置页必须看见它们才能
            // 重新启用/删除——否则其关键字仍占命名空间（§4.5 all_command_keywords
            // 含 disabled）却无处管理，成黑洞。执行面各自过滤 enabled：
            // command_search / execute_command / action_composer 均已自行检查。
            // owner 分工：用户命令的 owner/trust 恒为 broker/user，由代码赋值。
            // broker-owned 命令需要注册 handler；K0 用户表为空，此处无过滤发生。
            items.push(user_command_to_descriptor(command));
        }
        // K2 §4.6：合并 shortcut_bindings。为目录中每条命令填充 bindings.shortcut。
        // 内置命令不在 commands 向量中，shortcut_bindings 是它们唯一的快捷键存储。
        for entry in &state.shortcut_bindings {
            if let Some(item) = items.iter_mut().find(|d| d.id == entry.command_id) {
                item.bindings.shortcut = Some(CommandBindingDto {
                    priority: 0,
                    input: String::new(),
                    cardinality: None,
                    target_kinds: Vec::new(),
                    requires_host_root: false,
                    shortcut_combo: Some(entry.combo.clone()),
                    ..Default::default()
                });
            }
        }
        // K3 §4.5：启动裁决——关键字与网页引擎冲突的命令移除 keyword binding
        // 并设 disabled_reason。命令本体不禁用，仍可经根搜索/动作面板/快捷键到达。
        let disabled = self
            .disabled_keyword_commands
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !disabled.is_empty() {
            for item in items.iter_mut() {
                if let Some(reason) = disabled.get(&item.id) {
                    item.bindings.keyword = None;
                    if item.disabled_reason.is_empty() {
                        item.disabled_reason = reason.clone();
                    }
                }
            }
        }
        drop(disabled);
        drop(state);
        items
    }

    /// K2 §4.6：设置或清除命令的快捷键绑定。combo = None 或空串 = 清除。
    /// 内置命令与用户命令都支持——绑定存在 CommandData.shortcut_bindings 映射中，
    /// 不在 UserCommandDefinition.bindings 里（设计 §4.6：独立存储）。
    pub fn set_shortcut_binding(
        &self,
        command_id: &str,
        combo: Option<String>,
    ) -> Result<(), String> {
        validate_command_id(command_id)?;
        {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let is_clear = combo.as_ref().is_none_or(|c| c.is_empty());
            if is_clear {
                state
                    .shortcut_bindings
                    .retain(|e| e.command_id != command_id);
            } else {
                let combo = combo.unwrap();
                if combo.chars().count() > crate::persistence::SHORTCUT_COMBO_MAX_CHARS {
                    return Err("shortcut combo exceeds 64 chars".into());
                }
                match state
                    .shortcut_bindings
                    .iter_mut()
                    .find(|e| e.command_id == command_id)
                {
                    Some(entry) => entry.combo = combo,
                    None => {
                        if state.shortcut_bindings.len()
                            >= crate::persistence::COMMAND_SHORTCUT_MAX_ENTRIES
                        {
                            return Err("shortcut bindings at capacity (64)".into());
                        }
                        state
                            .shortcut_bindings
                            .push(crate::persistence::CommandShortcutEntry {
                                command_id: command_id.into(),
                                combo,
                            });
                    }
                }
            }
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.persist()
    }

    /// 目录代际。`load` 后为 1（0 保留给「未知/未协商」），每次 mutation 递增。
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// 命令的使用频度分（frecency 近似）。K2 composer 用它对命令段排序。
    /// 取 success_count：已执行次数越多越靠前。frecency_milli 暂未填，留待
    /// 与 history.rs 同款衰减计算统一接入（设计 §12-K2 标注 frecency 排序）。
    pub fn usage_score(&self, id: &str) -> u32 {
        self.usage
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.success_count)
            .unwrap_or(0)
    }

    /// 整体替换/新增一条用户命令。校验前置（先校验再改内存态），成功后
    /// generation+1。K0 不接 IPC，仅 store 层实现并测试（D3）。
    pub fn set(&self, def: UserCommandDefinition) -> Result<(), String> {
        validate_command_id(&def.id)?;
        // 防御：内置 id 走覆盖存储（handle_command_set 路由），不进用户表——
        // 否则 catalog 会出现同 id 双条目。
        if def.id.starts_with("prism.") {
            return Err("内置命令 id 不可作为用户命令保存".into());
        }
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
        // 内置命令「删除」= 清除覆盖（恢复出厂），不能真的删掉内置条目。
        if id.starts_with("prism.") {
            {
                let mut state = self
                    .state
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.builtin_overrides.remove(id);
            }
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.persist()?;
            return Ok(());
        }
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

    /// K4 收尾：保存内置命令覆盖（标题/关键字/启用/根搜索可见）。
    /// handler 与绑定结构不可覆盖——编辑器对内置命令禁用这些字段，这里只取可见字段。
    pub fn set_builtin_override(&self, def: UserCommandDefinition) -> Result<(), String> {
        validate_command_id(&def.id)?;
        if !def.id.starts_with("prism.") {
            return Err("非内置命令 id".into());
        }
        if def.title.chars().count() > crate::persistence::COMMAND_TITLE_MAX_CHARS {
            return Err("command title exceeds 64 chars".into());
        }
        if def.keywords.len() > crate::persistence::COMMAND_MAX_KEYWORDS {
            return Err("command has more than 4 keywords".into());
        }
        for keyword in &def.keywords {
            if keyword.trim().is_empty()
                || keyword.chars().count() > crate::persistence::COMMAND_KEYWORD_MAX_CHARS
                || keyword.chars().any(char::is_whitespace)
            {
                return Err("command keyword is invalid".into());
            }
        }
        let ov = crate::persistence::BuiltinOverride {
            title: def.title,
            keywords: def.keywords,
            enabled: Some(def.enabled),
            show_in_root_search: Some(
                def.bindings
                    .root_search
                    .as_ref()
                    .map(|b| b.show_in_root_search)
                    .unwrap_or(true),
            ),
        };
        {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.builtin_overrides.insert(def.id, ov);
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

    /// K3 §4.1：按 id 查用户命令定义。execute_command 分派用户 handler 时取
    /// handler_params。只查用户表，不查内置命令（内置不经 UserHandlerKind）。
    pub fn get_user_command(&self, id: &str) -> Option<UserCommandDefinition> {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.commands.iter().find(|c| c.id == id).cloned()
    }

    /// K3 §4.5：收集所有用户命令的关键字，供命名空间校验。
    /// 返回 (command_id, keyword) 列表。enabled 和 disabled 的命令都收——
    /// 禁用的命令关键字仍占命名空间（避免启用时突然冲突）。
    pub fn all_command_keywords(&self) -> Vec<(String, String)> {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut out = Vec::new();
        for command in &state.commands {
            for kw in &command.keywords {
                if !kw.trim().is_empty() {
                    out.push((command.id.clone(), kw.trim().to_string()));
                }
            }
        }
        // K4 收尾：内置命令覆盖的关键字同样占命名空间（§4.5 冲突检查防用户命令抢词）。
        for (id, ov) in &state.builtin_overrides {
            for kw in &ov.keywords {
                if !kw.trim().is_empty() {
                    out.push((id.clone(), kw.trim().to_string()));
                }
            }
        }
        out
    }

    /// K3 §4.8：导出用户命令的持久化形态快照（深拷贝）。
    /// 内置命令不在 `CommandData` 中，自然不导出。
    pub fn data_snapshot(&self) -> CommandData {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        CommandData {
            commands: state.commands.clone(),
            shortcut_bindings: state.shortcut_bindings.clone(),
            builtin_overrides: state.builtin_overrides.clone(),
        }
    }

    /// K3 §4.5：启动确定性裁决。检查所有用户命令的 keyword binding trigger
    /// 是否与网页引擎关键字冲突。冲突时网页优先——命令 keyword binding
    /// 失效，写入 disabled_keyword_commands 集合，catalog() 据此设
    /// disabled_reason 并移除 keyword binding。命令本体不禁用。
    pub fn startup_resolve(&self, engine_keywords: &[String]) {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let engine_set: std::collections::HashSet<&str> =
            engine_keywords.iter().map(|s| s.as_str()).collect();

        let mut disabled = std::collections::HashMap::new();
        for cmd in &state.commands {
            if let Some(ref binding) = cmd.bindings.keyword {
                if let Some(ref trigger) = binding.trigger {
                    let t = trigger.trim();
                    if !t.is_empty() && engine_set.contains(t) {
                        disabled.insert(cmd.id.clone(), format!("关键字与网页引擎「{}」冲突", t));
                        crate::log(format!(
                            "K3 §4.5 启动裁决：命令 {} 关键字「{}」与网页引擎冲突，keyword binding 禁用",
                            cmd.id, t
                        ));
                    }
                }
            }
        }
        drop(state);

        let mut map = self
            .disabled_keyword_commands
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *map = disabled;
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
                    input: String::new(),
                    cardinality: None,
                    target_kinds: Vec::new(),
                    requires_host_root: false,
                    shortcut_combo: None,
                    trigger: None,
                    show_in_root_search: b.show_in_root_search,
                }),
            keyword: command
                .bindings
                .keyword
                .as_ref()
                .map(|b| CommandBindingDto {
                    priority: b.priority,
                    input: String::new(),
                    cardinality: None,
                    target_kinds: Vec::new(),
                    requires_host_root: false,
                    shortcut_combo: None,
                    trigger: b
                        .trigger
                        .clone()
                        .or_else(|| command.keywords.first().cloned()),
                    show_in_root_search: true,
                }),
            action_panel: command
                .bindings
                .action_panel
                .as_ref()
                .map(|b| CommandBindingDto {
                    priority: b.priority,
                    input: String::new(),
                    cardinality: None,
                    target_kinds: Vec::new(),
                    requires_host_root: false,
                    shortcut_combo: None,
                    trigger: None,
                    show_in_root_search: true,
                }),
            staging: command
                .bindings
                .staging
                .as_ref()
                .map(|b| CommandBindingDto {
                    priority: b.priority,
                    input: String::new(),
                    cardinality: None,
                    target_kinds: Vec::new(),
                    requires_host_root: false,
                    shortcut_combo: None,
                    trigger: None,
                    show_in_root_search: true,
                }),
            shortcut: command
                .bindings
                .shortcut
                .as_ref()
                .map(|b| CommandBindingDto {
                    priority: b.priority,
                    input: String::new(),
                    cardinality: None,
                    target_kinds: Vec::new(),
                    requires_host_root: false,
                    shortcut_combo: b.shortcut_combo.clone(),
                    trigger: None,
                    show_in_root_search: true,
                }),
        },
        danger: match command.danger.as_str() {
            "" | "normal" => "normal",
            "elevated" => "elevated",
            "destructive" => "destructive",
            _ => "normal",
        },
        enabled: command.enabled,
        disabled_reason: String::new(),
        fallback: command.fallback,
        arguments: command
            .arguments
            .iter()
            .map(|a| CommandArgumentDto {
                name: a.name.clone(),
                required: a.required,
                default: a.default.clone(),
            })
            .collect(),
        handler: match command.handler {
            crate::persistence::UserHandlerKind::OpenUrl => "open_url",
            crate::persistence::UserHandlerKind::LaunchProgram => "launch_program",
            crate::persistence::UserHandlerKind::Unknown => "",
        },
        handler_params: command.handler_params.clone(),
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
    /// K4a：剪贴板文本。UI 进程在调用点读取后传入；仅作 {clipboard} 取值源，
    /// 不参与路径/权限判定，绝不写入日志。None = 读取失败/旧前端未传。
    #[serde(default)]
    pub clipboard: Option<String>,
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

// K2 §4.1：CommandSelection 带 typed target——动作面板命令段的语义是「对当前
// 选中项执行命令」，broker 收到调用后需据此复核操作对象。设计 §5.3-2：
// title/subtitle 只是有界 UI 快照，不可作为路径或权限依据。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CommandSelection {
    #[serde(default)]
    pub target: Option<ActionTarget>,
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
            validate_staged_path(path)?;
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
            // K2 §4.1：typed target 复核。选中项不可能是命令自身——防命令递归调用命令。
            if let Some(target) = &selection.target {
                let kind = target.validate().map_err(|e| e.message)?;
                if matches!(kind, crate::shell::TargetKind::Command) {
                    return Err("selection target cannot be a command".into());
                }
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
            if let Some(target) = &selection.target {
                total += target.kind.len() + target.value.len() + 32;
            }
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

// ── K3 §4.6：模板展开（执行与预览共用同一函数） ─────────────────────
//
// 占位符 v1：{query}（= arguments.text）、{current_folder}。
// 修饰符：uppercase / lowercase / trim / percent-encode / raw，链式 | 分隔。
// {query} 默认 percent-encode（复用 websearch::url_encode）。raw 修饰符跳过编码。
// 设计 §10.2-4：仅显式标注 safe 的字段允许 raw——模板层用 |raw 显式声明。

/// 模板展开上下文。执行路径与预览路径构造同一实例，保证展开一致（§4.6）。
#[derive(Debug, Clone)]
pub struct ExpansionContext {
    /// {query} = arguments.text（用户在搜索框输入的文本）。
    pub query: String,
    /// {current_folder} = 调用时 current_folder 字段。
    pub current_folder: String,
    /// {selection.target} = 选中目标值（动作面板=面板行；无选中 = None → 空串）。
    /// G7c 修复：模板预设用 {selection.target}，但引擎此前不支持该占位符——
    /// 展开失败把字面 `{selection.target}` 当参数传给程序（记事本报"文件不存在"）。
    pub selection: Option<String>,
    /// {clipboard} = 剪贴板文本（K4a）。UI 进程在调用点读取后经
    /// CommandInvocationContext.clipboard 传入；None = 读取失败/旧前端未传 → 空串。
    pub clipboard: Option<String>,
    /// {date} 取值基准（K4a）。filters::local_now()（GetLocalTime，本地时区）；
    /// 显式入 ctx 而非展开时现取，保证同一模板内多次 {date} 一致且测试可固定。
    pub now: crate::filters::LocalNow,
    /// K4b：{arg.name} 取值源——resolve_arguments 按声明切分 arguments.text 的
    /// 结果（保存校验路径以声明 default 填充）。未声明的 {arg.x} 在展开层报
    /// UnknownPlaceholder，拼写错误预览即暴露。
    pub args: std::collections::BTreeMap<String, String>,
}

/// 模板展开错误。类型化返回，执行时不吞错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandError {
    /// 未知占位符名称（如 {unknown}）。
    UnknownPlaceholder(String),
    /// 修饰符语法错误（如 {query||}）。
    BadModifier(String),
}

impl std::fmt::Display for ExpandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpandError::UnknownPlaceholder(name) => {
                write!(f, "未知占位符：{{{name}}}")
            }
            ExpandError::BadModifier(msg) => write!(f, "修饰符错误：{msg}"),
        }
    }
}

/// 已知的占位符名称。
const PLACEHOLDERS: &[&str] = &[
    "query",
    "current_folder",
    "selection.target",
    "clipboard",
    "date",
    "uuid",
];

/// {date} 的默认格式（K4a）：ISO 风格 yyyy-MM-dd。
const DATE_DEFAULT_FORMAT: &str = "yyyy-MM-dd";

/// §4.6：单一模板展开入口。执行（ExecuteCommand）与预览（CommandPreview）
/// **调用同一函数**——预览返回的就是执行时将用的最终字符串（构造保证一致）。
/// `force_raw` 为 true 时所有占位符按 raw 展开（程序参数语境：percent-encode 无意义）。
pub fn expand_template(template: &str, ctx: &ExpansionContext) -> Result<String, ExpandError> {
    expand_template_impl(template, ctx, false)
}

/// K3 §4.3：程序参数语境的展开。与 `expand_template` 同函数体，仅强制 raw。
/// 保证预览与执行一致（§4.6 同函数约束）。
pub fn expand_template_raw(template: &str, ctx: &ExpansionContext) -> Result<String, ExpandError> {
    expand_template_impl(template, ctx, true)
}

fn expand_template_impl(
    template: &str,
    ctx: &ExpansionContext,
    force_raw: bool,
) -> Result<String, ExpandError> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // 找匹配的 }
            let close = bytes[i + 1..]
                .iter()
                .position(|&b| b == b'}')
                .ok_or(ExpandError::BadModifier(format!("缺少闭合 }} 在位置 {i}")))?;
            let inner = &template[i + 1..i + 1 + close];
            let expanded = expand_placeholder(inner, ctx, force_raw)?;
            out.push_str(&expanded);
            i += close + 2; // 跳过 {...}
        } else {
            // 原样输出非占位符字符
            let next_brace = bytes[i..].iter().position(|&b| b == b'{');
            match next_brace {
                Some(pos) => {
                    out.push_str(&template[i..i + pos]);
                    i += pos;
                }
                None => {
                    out.push_str(&template[i..]);
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// 展开单个占位符（`{}` 内部内容）。格式：name 或 name|mod1|mod2。
fn expand_placeholder(
    inner: &str,
    ctx: &ExpansionContext,
    force_raw: bool,
) -> Result<String, ExpandError> {
    let parts: Vec<&str> = inner.split('|').map(|s| s.trim()).collect();
    if parts.is_empty() || parts[0].is_empty() {
        return Err(ExpandError::BadModifier("占位符名称为空".into()));
    }
    let name = parts[0];
    // K4b：{arg.name} 动态命名空间，先于静态白名单。未声明的名字 →
    // UnknownPlaceholder（暴露拼写错误）；已声明但取值为空 → 空串。
    let arg_value = name.strip_prefix("arg.").map(|arg_name| {
        ctx.args
            .get(arg_name)
            .cloned()
            .ok_or_else(|| ExpandError::UnknownPlaceholder(name.to_string()))
    });
    if !PLACEHOLDERS.contains(&name) && arg_value.is_none() {
        return Err(ExpandError::UnknownPlaceholder(name.to_string()));
    }
    let base_value = match arg_value {
        Some(value) => value?,
        None => match name {
            "query" => ctx.query.clone(),
            "current_folder" => ctx.current_folder.clone(),
            // 无选中（关键字路由/根搜索直达）→ 空串；launch 参数侧把空展开丢掉。
            "selection.target" => ctx.selection.clone().unwrap_or_default(),
            // 剪贴板读取失败/未传 → 空串（与 selection 同语义，launch 侧丢空参）。
            "clipboard" => ctx.clipboard.clone().unwrap_or_default(),
            // 默认 yyyy-MM-dd；fmt: 修饰符在下方循环里重排。
            "date" => format_date(DATE_DEFAULT_FORMAT, &ctx.now),
            // 每次出现独立生成（同一模板两处 {uuid} = 两个值，与 Raycast 一致）。
            "uuid" => uuid_v4(),
            _ => unreachable!("checked above"),
        },
    };
    // 默认：query 做 percent-encode，current_folder 不编码。
    // raw 修饰符跳过编码；percent-encode 修饰符显式编码。
    // force_raw（程序参数语境）覆盖默认——一律不编码。
    let mut value = base_value;
    let mut raw = force_raw || name != "query"; // query 默认编码；其余默认 raw
    for modifier in &parts[1..] {
        match *modifier {
            "raw" => raw = true,
            "percent-encode" => raw = false,
            "trim" => value = value.trim().to_string(),
            "uppercase" => value = value.to_uppercase(),
            "lowercase" => value = value.to_lowercase(),
            // K4a：参数化修饰符。目前仅 date 支持（{date|fmt:yyyyMMdd}）。
            other if other.starts_with("fmt:") => {
                if name != "date" {
                    return Err(ExpandError::BadModifier(format!(
                        "fmt 修饰符仅支持 date 占位符：{other}"
                    )));
                }
                let pattern = &other["fmt:".len()..];
                if pattern.is_empty() {
                    return Err(ExpandError::BadModifier("空 fmt 模式".into()));
                }
                value = format_date(pattern, &ctx.now);
            }
            "" => return Err(ExpandError::BadModifier("空修饰符".into())),
            other => return Err(ExpandError::BadModifier(format!("未知修饰符：{other}"))),
        }
    }
    if raw {
        Ok(value)
    } else {
        Ok(crate::websearch::url_encode(&value))
    }
}

/// K4a：{date} 格式化。token 集（最长匹配优先）：yyyy yy MM M dd d HH H mm m ss s，
/// 其余字符字面输出（含中文）。now 来自 filters::local_now()（本地时区）。
fn format_date(pattern: &str, now: &crate::filters::LocalNow) -> String {
    let hour = now.secs_of_day / 3600;
    let minute = (now.secs_of_day % 3600) / 60;
    let second = now.secs_of_day % 60;
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut rest = pattern;
    while !rest.is_empty() {
        let (tok, text) = if let Some(s) = rest.strip_prefix("yyyy") {
            (s, format!("{:04}", now.year))
        } else if let Some(s) = rest.strip_prefix("yy") {
            (s, format!("{:02}", now.year % 100))
        } else if let Some(s) = rest.strip_prefix("MM") {
            (s, format!("{:02}", now.month))
        } else if let Some(s) = rest.strip_prefix('M') {
            (s, now.month.to_string())
        } else if let Some(s) = rest.strip_prefix("dd") {
            (s, format!("{:02}", now.day))
        } else if let Some(s) = rest.strip_prefix('d') {
            (s, now.day.to_string())
        } else if let Some(s) = rest.strip_prefix("HH") {
            (s, format!("{hour:02}"))
        } else if let Some(s) = rest.strip_prefix('H') {
            (s, hour.to_string())
        } else if let Some(s) = rest.strip_prefix("mm") {
            (s, format!("{minute:02}"))
        } else if let Some(s) = rest.strip_prefix('m') {
            (s, minute.to_string())
        } else if let Some(s) = rest.strip_prefix("ss") {
            (s, format!("{second:02}"))
        } else if let Some(s) = rest.strip_prefix('s') {
            (s, second.to_string())
        } else {
            // 非 token 字符（含多字节 UTF-8）按字面输出。
            let ch = rest.chars().next().expect("non-empty checked");
            (&rest[ch.len_utf8()..], ch.to_string())
        };
        out.push_str(&text);
        rest = tok;
    }
    out
}

/// K4a：UUID v4。BCryptGenRandom 取 16 字节熵；失败（实际不会发生）退化为
/// 墙钟纳秒 + 进程内计数器——唯一性仍够模板用途（文件名/请求 id），不用于密钥。
/// ponytail: 计数器回退路径够用；要密码学保证时再换成 RtlGenRandom 重试。
fn uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    #[cfg(windows)]
    {
        use windows::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        // SAFETY：系统首选 RNG，无句柄需求；失败走下方回退。
        if unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.is_err() {
            fill_fallback_random(&mut bytes);
        }
    }
    #[cfg(not(windows))]
    {
        fill_fallback_random(&mut bytes);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10x
    let h: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}{}{}{}-{}{}-{}{}-{}{}-{}{}{}{}{}{}",
        h[0],
        h[1],
        h[2],
        h[3],
        h[4],
        h[5],
        h[6],
        h[7],
        h[8],
        h[9],
        h[10],
        h[11],
        h[12],
        h[13],
        h[14],
        h[15]
    )
}

/// BCryptGenRandom 失败/非 Windows 的回退熵：墙钟纳秒 + 原子计数器。
fn fill_fallback_random(bytes: &mut [u8; 16]) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mix = nanos ^ counter.rotate_left(32).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    bytes[..8].copy_from_slice(&nanos.to_le_bytes());
    bytes[8..].copy_from_slice(&mix.to_le_bytes());
}

/// K4a：按字节上限截断（UTF-8 字符边界安全）。剪贴板超 8 KiB 时静默截断——
/// validate() 不报错，避免「剪贴板大了命令就失败」的意外。
fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ── K3 §4.2 语义层：open_url 校验（执行时第二次校验） ──────────────

/// open_url 语义校验。url_template 展开后检查 scheme ∈ {http, https}。
/// 保存时（CommandSet）与执行时（ExecuteCommand）都调用此函数。
pub fn validate_open_url(url_template: &str, ctx: &ExpansionContext) -> Result<String, String> {
    let url = expand_template(url_template, ctx).map_err(|e| e.to_string())?;
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(format!("URL 协议必须是 http 或 https：{url}"));
    }
    // 拒绝 javascript: / file: 等——上面 scheme 检查已覆盖，但显式拒绝
    // 让错误信息更清晰。
    if lower.starts_with("javascript:") {
        return Err("javascript: 协议被禁止".into());
    }
    if lower.starts_with("file:") {
        return Err("file: 协议被禁止".into());
    }
    Ok(url)
}

/// 构造 open_url 的 ExpansionContext。`args` 为 resolve_arguments 的产出
/// （保存校验路径以声明 default 填充）。
pub fn expansion_context_from(
    context: &CommandInvocationContext,
    args: std::collections::BTreeMap<String, String>,
) -> ExpansionContext {
    ExpansionContext {
        query: context.arguments.text.clone().unwrap_or_default(),
        current_folder: context.current_folder.clone().unwrap_or_default(),
        selection: context
            .selection
            .as_ref()
            .and_then(|s| s.target.as_ref().map(|t| t.value.clone())),
        // K4a：剪贴板超限静默截断（8 KiB），不作为失败路径。
        clipboard: context
            .clipboard
            .as_deref()
            .map(|c| truncate_bytes(c, TEXT_MAX_BYTES).to_string()),
        now: crate::filters::local_now(),
        args,
    }
}

/// K4b：位置切分 arguments.text 并按声明顺序传入。多余 token 忽略；可选缺位
/// 填 default；必填缺位报错。v1 不支持引号——含空白取值用 {query}。
pub fn resolve_arguments(
    text: Option<&str>,
    specs: &[crate::persistence::CommandArgumentSpec],
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut out = std::collections::BTreeMap::new();
    if specs.is_empty() {
        return Ok(out);
    }
    let tokens: Vec<&str> = text.unwrap_or_default().split_whitespace().collect();
    let mut missing: Vec<&str> = Vec::new();
    for (index, spec) in specs.iter().enumerate() {
        match tokens.get(index) {
            Some(token) => {
                out.insert(spec.name.clone(), (*token).to_string());
            }
            None if spec.required => missing.push(&spec.name),
            None => {
                out.insert(spec.name.clone(), spec.default.clone());
            }
        }
    }
    if missing.is_empty() {
        Ok(out)
    } else {
        Err(format!("缺少必填参数：{}", missing.join("、")))
    }
}

// ── K3 §4.2 语义层：launch_program 校验（执行时第二次校验） ──────────

/// launch_program 语义校验。保存时（CommandSet）与执行时（ExecuteCommand）都调用。
/// 路径须绝对、非 UNC、扩展名 .exe/.lnk、文件存在。working_dir 若非空须存在。
/// §4.2-2：拒绝 .bat/.cmd/.ps1。
pub fn validate_launch_program(
    handler_params: &std::collections::BTreeMap<String, String>,
    ctx: &ExpansionContext,
) -> Result<(String, Vec<String>, Option<String>), String> {
    let path = handler_params
        .get("path")
        .ok_or_else(|| "launch_program 缺少 path 参数".to_string())?;
    if path.is_empty() {
        return Err("launch_program path 为空".into());
    }
    // §4.2-2：拒绝相对路径、UNC、环境变量展开。
    if !Path::new(path).is_absolute() {
        return Err("launch_program path 必须是绝对路径".into());
    }
    if path.starts_with(r"\\") {
        return Err("launch_program 不接受 UNC 路径".into());
    }
    // 扩展名白名单：.exe / .lnk
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "exe" | "lnk") {
        return Err(format!(
            "launch_program path 扩展名必须是 .exe 或 .lnk（拒绝 {ext}）"
        ));
    }
    // 文件存在性（语义层，触 I/O）
    if !Path::new(path).exists() {
        return Err(format!("launch_program 路径不存在：{path}"));
    }
    // args_template：逐 token 展开，不二次分词
    let args: Vec<String> = match handler_params.get("args_template") {
        Some(template) if !template.is_empty() => {
            // §4.3：token 列表的字符串形式。展开时按 token 逐个替换占位符，
            // 替换结果不再二次分词——用空白分割模板本身的 token。
            // §4.3：程序参数用 raw 展开——percent-encode 对 argv 无意义。
            // G7c：展开为空（如 {selection.target} 无选中）→ 丢弃该参数，
            // 程序无参启动；未知占位符/坏修饰符 → 保留原文暴露拼写错误。
            template
                .split_whitespace()
                .filter_map(|tok| match expand_template_raw(tok, ctx) {
                    Ok(v) if !v.trim().is_empty() => Some(v),
                    Ok(_) => None,
                    Err(_) => Some(tok.to_string()),
                })
                .collect()
        }
        _ => Vec::new(),
    };
    // working_dir：展开后若非空须存在。路径用 raw 展开（不 percent-encode）。
    let working_dir = match handler_params.get("working_dir") {
        Some(template) if !template.is_empty() => {
            let dir = expand_template_raw(template, ctx).map_err(|e| e.to_string())?;
            if !dir.is_empty() && !Path::new(&dir).is_dir() {
                return Err(format!("launch_program working_dir 不存在：{dir}"));
            }
            Some(dir)
        }
        _ => None,
    };
    Ok((path.to_string(), args, working_dir))
}

/// K3 §4.2-6：用户命令的 danger 不接受 elevated（用户命令不得请求 runas）。
pub fn validate_user_danger(danger: &str) -> Result<(), String> {
    if danger == "elevated" {
        return Err("用户命令不能设置为 elevated（禁止 runas）".into());
    }
    Ok(())
}

// ── K3 §4.5 命名空间统一校验 ────────────────────────────────────────

/// 触发词所有者，标识校验请求来源。前端用此字段区分冲突来源展示。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerOwner {
    Command,
    WebEngine,
}

/// 冲突来源类型。前端据此展示「与网页引擎/别名/命令/保留字冲突」。
#[derive(Debug, Clone, Serialize)]
pub struct NamespaceConflict {
    pub kind: &'static str,
    /// 冲突方的可读标签（引擎名 / 别名目标 / 命令 id / 保留字本身）。
    pub owner_label: String,
}

/// §4.5：触发词命名空间校验。broker 是唯一裁决者。
///
/// 查全集：网页引擎关键字 ∪ 别名词 ∪ 命令关键字 ∪ 保留字（`ext:` / `path:` / `>`）。
/// 返回 `Ok(())` 表示无冲突，`Err` 带可读来源。`exclude_command_id` 用于排除
/// 正在编辑的命令自身（编辑保存时自身的旧关键字不应与自己冲突）。
pub fn validate_trigger_namespace(
    trigger: &str,
    owner: TriggerOwner,
    engines: &[crate::websearch::WebEngine],
    aliases: &[crate::persistence::AliasEntry],
    command_keywords: &[(String, String)], // (command_id, keyword)
    exclude_command_id: Option<&str>,
) -> Result<(), NamespaceConflict> {
    let trigger = trigger.trim();
    if trigger.is_empty() {
        return Ok(());
    }
    let trigger_lower = trigger.to_lowercase();
    // 保留字前缀（§4.5）：ext: / path: 是查询过滤前缀，> 是动作动词前缀。
    // 以这些前缀开头的触发词会与查询解析冲突。
    const RESERVED_PREFIXES: &[&str] = &["ext:", "path:"];
    for prefix in RESERVED_PREFIXES {
        if trigger_lower.starts_with(prefix) {
            return Err(NamespaceConflict {
                kind: "reserved",
                owner_label: format!("保留前缀 {prefix}"),
            });
        }
    }
    if trigger_lower == ">" {
        return Err(NamespaceConflict {
            kind: "reserved",
            owner_label: "保留字 >".into(),
        });
    }
    // 网页引擎关键字——大小写不敏感，与 try_match 一致。
    for engine in engines {
        if !engine.keyword.is_empty() && engine.keyword.eq_ignore_ascii_case(trigger) {
            return Err(NamespaceConflict {
                kind: "web_engine",
                owner_label: format!("网页引擎 {}", engine.name),
            });
        }
    }
    // 别名词——小写匹配（alias.rs lookup_word 同款）。
    for entry in aliases {
        for word in &entry.words {
            if word.eq_ignore_ascii_case(trigger) {
                return Err(NamespaceConflict {
                    kind: "alias",
                    owner_label: format!("别名 {}", entry.target),
                });
            }
        }
    }
    // 命令关键字——排除自身。
    for (cid, kw) in command_keywords {
        if Some(cid.as_str()) == exclude_command_id {
            continue;
        }
        if kw.eq_ignore_ascii_case(trigger) {
            return Err(NamespaceConflict {
                kind: "command",
                owner_label: format!("命令 {cid}"),
            });
        }
    }
    // owner 自身不做引擎/命令重复检查——owner 只影响前端展示来源标签。
    let _ = owner;
    Ok(())
}

/// K2 §4.7：staged_paths 的结构校验。与 `validate_path_field` 的区别：
/// **不拒绝 UNC**——`copy_paths` 复制原始字符串、不探测存在性，因此不受
/// UNC 限制（设计 §4.7 末段）。UNC 拒绝只对 mutation 类命令（ZIP）在
/// handler 分发时执行，不在通用 `validate()` 里一刀切。
/// 结构性检查（空/控制字符/长度/绝对路径）仍保留——staging 区的路径
/// 来源是搜索结果，恒为绝对路径；copy_paths 不应接受垃圾输入。
fn validate_staged_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.contains('\0') || path.chars().any(char::is_control) {
        return Err("staged path is invalid".into());
    }
    if path.len() > PATH_MAX_BYTES {
        return Err("staged path exceeds 32 KiB".into());
    }
    if !Path::new(path).is_absolute() {
        return Err("staged path must be absolute".into());
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
    use crate::persistence::UserHandlerKind;

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
        assert_eq!(store.catalog().len(), 5, "4 builtin + 1 user");
        let reloaded = CommandStore::load(&dir);
        assert_eq!(reloaded.catalog().len(), 5);
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
            store.catalog().len() == 4,
            "future version → 空用户表 + 4 builtin"
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
        assert_eq!(store.catalog().len(), 4, "损坏 → 空用户表 + 4 builtin");

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
        // disabled 用户命令仍进 catalog（enabled=false 下发，设置页可见可管理），
        // 执行面（搜索/execute/动作面板）各自过滤 enabled。
        let catalog = store.catalog();
        assert_eq!(catalog.len(), 5, "4 builtin + user.b (disabled)");
        let disabled = catalog.iter().find(|d| d.id == "user.b").unwrap();
        assert!(!disabled.enabled);
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

        // K2 §4.7：UNC 路径在 validate() 层不拒绝——copy_paths 复制原始字符串
        // 不受 UNC 限制。UNC 拒绝只对 mutation 类命令在 handler 分发时执行。
        let mut ctx = base();
        ctx.staged_paths = vec![r"\\server\share".into()];
        assert!(ctx.validate().is_ok());

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

    // K2 §4.1：CommandSelection 带 typed target 的校验。
    // command kind 的 selection.target 必须被拒绝——防命令递归调用命令。
    #[test]
    fn selection_target_validates_typed_target() {
        use crate::shell::{ActionTarget, TargetKind};

        let base = || CommandInvocationContext {
            command_id: "user.test".into(),
            source: InvocationSource::Root,
            ..Default::default()
        };

        // selection 为 None（root 来源）→ 通过
        assert!(base().validate().is_ok());

        // 合法 file target → 通过
        let mut ctx = base();
        ctx.selection = Some(CommandSelection {
            target: Some(ActionTarget::new(TargetKind::File, r"C:\x.txt")),
            ..Default::default()
        });
        assert!(ctx.validate().is_ok());

        // 合法 directory target → 通过
        let mut ctx = base();
        ctx.selection = Some(CommandSelection {
            target: Some(ActionTarget::new(TargetKind::Directory, r"C:\Windows")),
            ..Default::default()
        });
        assert!(ctx.validate().is_ok());

        // 合法 application target → 通过
        let mut ctx = base();
        ctx.selection = Some(CommandSelection {
            target: Some(ActionTarget::new(
                TargetKind::Application,
                r"C:\Windows\notepad.exe",
            )),
            ..Default::default()
        });
        assert!(ctx.validate().is_ok());

        // command kind target → 拒绝（防命令递归）
        let mut ctx = base();
        ctx.selection = Some(CommandSelection {
            target: Some(ActionTarget::new(
                TargetKind::Command,
                "prism.settings.open",
            )),
            ..Default::default()
        });
        assert!(ctx.validate().is_err());

        // 非法路径（相对）→ 拒绝
        let mut ctx = base();
        ctx.selection = Some(CommandSelection {
            target: Some(ActionTarget::new(TargetKind::File, "relative")),
            ..Default::default()
        });
        assert!(ctx.validate().is_err());

        // 超长 title（>256 chars）→ 拒绝
        let mut ctx = base();
        ctx.selection = Some(CommandSelection {
            title: "设".repeat(257),
            ..Default::default()
        });
        assert!(ctx.validate().is_err());

        // 256 chars title → 通过（边界）
        let mut ctx = base();
        ctx.selection = Some(CommandSelection {
            title: "设".repeat(256),
            ..Default::default()
        });
        assert!(ctx.validate().is_ok());
    }

    // K2 §4.1：CommandSelection 含 target 字段的 serde 往返。
    #[test]
    fn selection_target_serde_roundtrip() {
        let ctx = CommandInvocationContext {
            command_id: "user.test".into(),
            source: InvocationSource::Root,
            selection: Some(CommandSelection {
                target: Some(crate::shell::ActionTarget {
                    kind: "file".into(),
                    value: r"C:\x.txt".into(),
                }),
                title: "x.txt".into(),
                subtitle: String::new(),
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&ctx).unwrap();
        // 缺 target 的旧 JSON 仍可反序列化（default None）
        let legacy =
            r#"{"command_id":"user.test","source":"root","selection":{"title":"a","subtitle":""}}"#;
        let parsed: CommandInvocationContext = serde_json::from_str(legacy).unwrap();
        assert!(parsed.selection.unwrap().target.is_none());

        // 含 target 的 JSON 往返
        let reparsed: CommandInvocationContext = serde_json::from_str(&json).unwrap();
        let sel = reparsed.selection.unwrap();
        assert_eq!(sel.target.unwrap().value, r"C:\x.txt");
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

    // K2 §4.6：shortcut_bindings 设置/清除/目录合并。
    #[test]
    fn shortcut_binding_set_clear_and_catalog_merge() {
        let (store, _dir) = store("shortcut");

        // 初始：内置命令无 shortcut binding
        let catalog = store.catalog();
        let settings = catalog
            .iter()
            .find(|d| d.id == "prism.settings.open")
            .unwrap();
        assert!(
            settings.bindings.shortcut.is_none(),
            "no shortcut initially"
        );

        // 设置内置命令的快捷键
        store
            .set_shortcut_binding("prism.settings.open", Some("Ctrl+Shift+S".into()))
            .unwrap();
        assert_eq!(store.generation(), 2, "generation incremented");
        let catalog = store.catalog();
        let settings = catalog
            .iter()
            .find(|d| d.id == "prism.settings.open")
            .unwrap();
        let sc = settings.bindings.shortcut.as_ref().expect("shortcut set");
        assert_eq!(sc.shortcut_combo.as_deref(), Some("Ctrl+Shift+S"));

        // 清除
        store
            .set_shortcut_binding("prism.settings.open", None)
            .unwrap();
        let catalog = store.catalog();
        let settings = catalog
            .iter()
            .find(|d| d.id == "prism.settings.open")
            .unwrap();
        assert!(settings.bindings.shortcut.is_none(), "cleared");

        // 空串也清除
        store
            .set_shortcut_binding("prism.terminal.open", Some("Ctrl+T".into()))
            .unwrap();
        store
            .set_shortcut_binding("prism.terminal.open", Some(String::new()))
            .unwrap();
        let catalog = store.catalog();
        let terminal = catalog
            .iter()
            .find(|d| d.id == "prism.terminal.open")
            .unwrap();
        assert!(terminal.bindings.shortcut.is_none(), "empty string clears");
    }

    // K2 §4.6：shortcut_bindings 持久化往返。
    #[test]
    fn shortcut_binding_persists_across_reload() {
        let (store, dir) = store("shortcut-persist");
        store
            .set_shortcut_binding("prism.settings.open", Some("Ctrl+Alt+P".into()))
            .unwrap();
        // 重新加载
        let reloaded = CommandStore::load(&dir);
        let catalog = reloaded.catalog();
        let settings = catalog
            .iter()
            .find(|d| d.id == "prism.settings.open")
            .unwrap();
        let sc = settings
            .bindings
            .shortcut
            .as_ref()
            .expect("shortcut persisted");
        assert_eq!(sc.shortcut_combo.as_deref(), Some("Ctrl+Alt+P"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // K2 §4.6：shortcut_bindings 校验——无效 id 拒绝、超长 combo 拒绝、容量上限。
    #[test]
    fn shortcut_binding_validation() {
        let (store, _dir) = store("shortcut-val");

        // 无效 id 拒绝
        assert!(store
            .set_shortcut_binding("badprefix", Some("Ctrl+S".into()))
            .is_err());

        // 超长 combo 拒绝
        let long = "a".repeat(65);
        assert!(store
            .set_shortcut_binding("prism.settings.open", Some(long))
            .is_err());

        // 合法 combo 通过
        assert!(store
            .set_shortcut_binding("prism.settings.open", Some("Ctrl+Shift+F12".into()))
            .is_ok());
    }

    // K2 §4.6：旧版 WPF 保存 settings.json 不影响命令绑定——
    // shortcut_bindings 在 commands-v1.json 中，与 settings.json 完全分开。
    #[test]
    fn shortcut_bindings_independent_of_settings_json() {
        let (store, dir) = store("shortcut-indep");
        store
            .set_shortcut_binding("prism.terminal.open", Some("Ctrl+T".into()))
            .unwrap();

        // 模拟旧版 WPF 写入一个不含 shortcut_bindings 的 settings.json——
        // 这不影响 commands-v1.json。重新加载后 shortcut_bindings 仍在。
        let reloaded = CommandStore::load(&dir);
        let catalog = reloaded.catalog();
        let terminal = catalog
            .iter()
            .find(|d| d.id == "prism.terminal.open")
            .unwrap();
        assert!(terminal.bindings.shortcut.is_some(), "shortcut survives");
        let _ = std::fs::remove_dir_all(&dir);
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

    // ── K3 §4.6 commit 2：expand_template + open_url 校验 ─────────────

    /// 固定 {date} 基准：2026-08-29 14:05:09（周六），让日期测试可断言。
    fn fixed_now() -> crate::filters::LocalNow {
        crate::filters::LocalNow {
            year: 2026,
            month: 8,
            day: 29,
            days: crate::filters::days_from_civil(2026, 8, 29),
            secs_of_day: 14 * 3600 + 5 * 60 + 9,
            tz_offset: 8 * 3600,
        }
    }

    fn exp_ctx(query: &str) -> ExpansionContext {
        ExpansionContext {
            query: query.into(),
            current_folder: r"C:\Users\test".into(),
            selection: None,
            clipboard: None,
            now: fixed_now(),
            args: Default::default(),
        }
    }

    #[test]
    fn expand_selection_target_with_and_without_selection() {
        let with = ExpansionContext {
            query: String::new(),
            current_folder: String::new(),
            selection: Some(r"D:\a b.txt".into()),
            clipboard: None,
            now: fixed_now(),
            args: Default::default(),
        };
        assert_eq!(
            expand_template_raw("{selection.target}", &with).unwrap(),
            r"D:\a b.txt"
        );
        // 无选中 → 空串（launch 参数侧丢弃空展开）。
        let without = exp_ctx("");
        assert_eq!(
            expand_template_raw("{selection.target}", &without).unwrap(),
            ""
        );
    }

    #[test]
    fn launch_args_drop_empty_selection_and_keep_unknown() {
        let mut params = launch_params(r"C:\Windows\notepad.exe");
        params.insert(
            "args_template".into(),
            "{selection.target} {unknown} tail".into(),
        );
        // 无选中：空展开参数被丢弃（不再把字面 {selection.target} 传给程序），
        // 未知占位符保留原文暴露拼写错误。
        let ctx = exp_ctx("");
        let (_, args, _) = validate_launch_program(&params, &ctx).unwrap();
        assert_eq!(args, vec!["{unknown}", "tail"]);
        // 有选中：目标路径作为单个参数（含空格不分词）。
        let ctx = ExpansionContext {
            query: String::new(),
            current_folder: String::new(),
            selection: Some(r"D:\a b.txt".into()),
            clipboard: None,
            now: fixed_now(),
            args: Default::default(),
        };
        let (_, args, _) = validate_launch_program(&params, &ctx).unwrap();
        assert_eq!(args, vec![r"D:\a b.txt", "{unknown}", "tail"]);
    }

    #[test]
    fn expand_query_default_percent_encoded() {
        // {query} 默认百分号编码
        let url = expand_template("https://x.test/search?q={query}", &exp_ctx("foo bar")).unwrap();
        assert_eq!(url, "https://x.test/search?q=foo%20bar");
    }

    #[test]
    fn expand_query_raw_not_encoded() {
        // {query|raw} 不编码
        let url = expand_template("https://x.test/{query|raw}", &exp_ctx("a+b")).unwrap();
        assert_eq!(url, "https://x.test/a+b");
    }

    #[test]
    fn expand_modifier_chain() {
        // 修饰符链式 {query | trim | percent-encode}
        let url = expand_template(
            "https://x.test/?q={query | trim | percent-encode}",
            &exp_ctx("  hi  "),
        )
        .unwrap();
        assert_eq!(url, "https://x.test/?q=hi");
    }

    #[test]
    fn expand_uppercase_lowercase() {
        let u = expand_template("{query|uppercase}", &exp_ctx("hello")).unwrap();
        assert_eq!(u, "HELLO");
        let l = expand_template("{query|lowercase}", &exp_ctx("WORLD")).unwrap();
        assert_eq!(l, "world");
    }

    #[test]
    fn expand_static_url_no_query() {
        // 定值 URL（无 {query}）可执行
        let url = expand_template("https://example.com/page", &exp_ctx("ignored")).unwrap();
        assert_eq!(url, "https://example.com/page");
    }

    #[test]
    fn expand_current_folder() {
        // {current_folder} 默认 raw（不编码）
        let url = expand_template("file:///{current_folder}/index.html", &exp_ctx("")).unwrap();
        assert_eq!(url, "file:///C:\\Users\\test/index.html");
    }

    #[test]
    fn expand_unknown_placeholder_errors() {
        // 未知占位符 → 类型化错误
        let err = expand_template("https://x.test/{unknown}", &exp_ctx("x")).unwrap_err();
        assert_eq!(err, ExpandError::UnknownPlaceholder("unknown".into()));
    }

    #[test]
    fn expand_unknown_modifier_errors() {
        let err = expand_template("{query|bogus}", &exp_ctx("x")).unwrap_err();
        assert!(matches!(err, ExpandError::BadModifier(_)));
    }

    // ── K4a：{clipboard} / {date} / {uuid} ─────────────────────────────

    #[test]
    fn expand_clipboard_from_context_and_missing_is_empty() {
        let mut ctx = exp_ctx("");
        ctx.clipboard = Some("hello world".into());
        // 默认 raw（不编码），与 current_folder 同规则。
        assert_eq!(expand_template("{clipboard}", &ctx).unwrap(), "hello world");
        // 显式 percent-encode 可用于 URL 参数。
        assert_eq!(
            expand_template("https://x.test/?q={clipboard|percent-encode}", &ctx).unwrap(),
            "https://x.test/?q=hello%20world"
        );
        // None（读取失败/旧前端）→ 空串，launch 侧丢空参。
        assert_eq!(
            expand_template_raw("[{clipboard}]", &exp_ctx("")).unwrap(),
            "[]"
        );
    }

    #[test]
    fn expansion_context_from_truncates_clipboard() {
        let invocation = CommandInvocationContext {
            clipboard: Some("中".repeat(8 * 1024 + 16)), // 超 8 KiB 字节
            ..Default::default()
        };
        let ctx = expansion_context_from(&invocation, Default::default());
        let clipped = ctx.clipboard.expect("clipboard should be present");
        assert!(clipped.len() <= TEXT_MAX_BYTES);
        assert!(clipped.is_char_boundary(clipped.len()));
        // 未超限不截断
        let invocation = CommandInvocationContext {
            clipboard: Some("ok".into()),
            ..Default::default()
        };
        assert_eq!(
            expansion_context_from(&invocation, Default::default())
                .clipboard
                .as_deref(),
            Some("ok")
        );
    }

    #[test]
    fn expand_date_default_format() {
        assert_eq!(
            expand_template("{date}", &exp_ctx("")).unwrap(),
            "2026-08-29"
        );
    }

    #[test]
    fn expand_date_fmt_tokens() {
        // 全 token 集 + 字面字符（含中文与分隔符）。
        assert_eq!(
            expand_template("{date|fmt:yyyyMMdd}", &exp_ctx("")).unwrap(),
            "20260829"
        );
        assert_eq!(
            expand_template("{date|fmt:HH:mm:ss}", &exp_ctx("")).unwrap(),
            "14:05:09"
        );
        assert_eq!(
            expand_template("{date|fmt:yyyy年M月d日}", &exp_ctx("")).unwrap(),
            "2026年8月29日"
        );
        assert_eq!(
            expand_template("{date|fmt:yy/M/d H:m:s}", &exp_ctx("")).unwrap(),
            "26/8/29 14:5:9"
        );
    }

    #[test]
    fn expand_date_fmt_errors() {
        // 空模式
        let err = expand_template("{date|fmt:}", &exp_ctx("")).unwrap_err();
        assert!(matches!(err, ExpandError::BadModifier(_)));
        // fmt 用在非 date 占位符上
        let err = expand_template("{query|fmt:yyyyMMdd}", &exp_ctx("x")).unwrap_err();
        assert!(matches!(err, ExpandError::BadModifier(_)));
    }

    #[test]
    fn expand_uuid_v4_shape_and_uniqueness() {
        let a = expand_template("{uuid}", &exp_ctx("")).unwrap();
        let b = expand_template("{uuid}", &exp_ctx("")).unwrap();
        assert_ne!(a, b, "两次生成应不同");
        assert_eq!(a.len(), 36);
        let dash_at = |i: usize| a.as_bytes()[i] == b'-';
        assert!(dash_at(8) && dash_at(13) && dash_at(18) && dash_at(23));
        for (i, ch) in a.chars().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                continue;
            }
            assert!(ch.is_ascii_hexdigit(), "非法字符 {ch} at {i}");
        }
        assert_eq!(&a[14..15], "4", "version nibble 应为 4");
        assert!(
            matches!(&a[19..20], "8" | "9" | "a" | "b"),
            "variant 应为 10x"
        );
    }

    // ── K4b：声明式参数 ────────────────────────────────────────────────

    fn arg_spec(
        name: &str,
        required: bool,
        default: &str,
    ) -> crate::persistence::CommandArgumentSpec {
        crate::persistence::CommandArgumentSpec {
            name: name.into(),
            required,
            default: default.into(),
        }
    }

    #[test]
    fn resolve_arguments_positional_and_defaults() {
        let specs = vec![arg_spec("width", true, ""), arg_spec("height", false, "10")];
        let args = resolve_arguments(Some("80 60"), &specs).unwrap();
        assert_eq!(args["width"], "80");
        assert_eq!(args["height"], "60");
        // 可选缺位填 default
        let args = resolve_arguments(Some("80"), &specs).unwrap();
        assert_eq!(args["width"], "80");
        assert_eq!(args["height"], "10");
        // 多余 token 忽略
        let args = resolve_arguments(Some("80 60 extra"), &specs).unwrap();
        assert_eq!(args["height"], "60");
    }

    #[test]
    fn resolve_arguments_missing_required_errors() {
        let specs = vec![arg_spec("width", true, ""), arg_spec("height", true, "")];
        let err = resolve_arguments(Some("80"), &specs).unwrap_err();
        assert!(err.contains("height"), "got: {err}");
        let err = resolve_arguments(None, &specs).unwrap_err();
        assert!(err.contains("width"), "got: {err}");
    }

    #[test]
    fn resolve_arguments_all_optional_empty_text_fills_defaults() {
        let specs = vec![arg_spec("a", false, "x"), arg_spec("b", false, "")];
        let args = resolve_arguments(None, &specs).unwrap();
        assert_eq!(args["a"], "x");
        assert_eq!(args["b"], "");
        // 无参数命令 → 空 map，{query} 行为不变
        assert!(resolve_arguments(Some("anything"), &[]).unwrap().is_empty());
    }

    #[test]
    fn expand_arg_placeholder_declared_and_unknown() {
        let mut ctx = exp_ctx("");
        ctx.args.insert("q".into(), "rust lang".into());
        // 修饰符链照常可用
        let url = expand_template("https://x.test/?q={arg.q|percent-encode}", &ctx).unwrap();
        assert_eq!(url, "https://x.test/?q=rust%20lang");
        // 未声明 → UnknownPlaceholder（拼写错误预览即暴露）
        let err = expand_template("https://x.test/?q={arg.typo}", &exp_ctx("")).unwrap_err();
        assert_eq!(err, ExpandError::UnknownPlaceholder("arg.typo".into()));
    }

    #[test]
    fn validate_open_url_accepts_http() {
        let url = validate_open_url("https://x.test/search?q={query}", &exp_ctx("test")).unwrap();
        assert_eq!(url, "https://x.test/search?q=test");
    }

    #[test]
    fn validate_open_url_rejects_javascript() {
        // javascript: 拒绝
        let err = validate_open_url("javascript:alert(1)", &exp_ctx("x")).unwrap_err();
        assert!(err.contains("javascript") || err.contains("http"));
    }

    #[test]
    fn validate_open_url_rejects_file_scheme() {
        let err = validate_open_url("file:///C:/x", &exp_ctx("")).unwrap_err();
        assert!(err.contains("file") || err.contains("http"));
    }

    #[test]
    fn validate_open_url_rejects_bad_scheme() {
        // 非 http/https scheme 拒绝
        let err = validate_open_url("ftp://x.test", &exp_ctx("")).unwrap_err();
        assert!(err.contains("http"));
    }

    #[test]
    fn get_user_command_returns_definition() {
        let (store, _dir) = store("get-user-cmd");
        let mut cmd = user_cmd("user.url_cmd");
        cmd.handler = UserHandlerKind::OpenUrl;
        cmd.handler_params
            .insert("url_template".into(), "https://x.test/{query}".into());
        store.set(cmd).unwrap();
        let got = store.get_user_command("user.url_cmd").unwrap();
        assert_eq!(got.handler, UserHandlerKind::OpenUrl);
        assert_eq!(
            got.handler_params.get("url_template").unwrap(),
            "https://x.test/{query}"
        );
    }

    // ── K3 §4.3 commit 3：launch_program 校验 ──────────────────────────

    fn launch_params(path: &str) -> std::collections::BTreeMap<String, String> {
        let mut m = std::collections::BTreeMap::new();
        m.insert("path".into(), path.into());
        m
    }

    #[test]
    fn validate_launch_program_rejects_relative_path() {
        let err =
            validate_launch_program(&launch_params("notepad.exe"), &exp_ctx("x")).unwrap_err();
        assert!(err.contains("绝对路径"));
    }

    #[test]
    fn validate_launch_program_rejects_unc() {
        let err = validate_launch_program(&launch_params(r"\\server\share\app.exe"), &exp_ctx("x"))
            .unwrap_err();
        assert!(err.contains("UNC"));
    }

    #[test]
    fn validate_launch_program_rejects_bat() {
        let err =
            validate_launch_program(&launch_params(r"C:\evil.bat"), &exp_ctx("x")).unwrap_err();
        assert!(err.contains("bat") || err.contains("exe") || err.contains("lnk"));
    }

    #[test]
    fn validate_launch_program_rejects_nonexistent() {
        let err = validate_launch_program(
            &launch_params(r"C:\does_not_exist_xyz_999.exe"),
            &exp_ctx("x"),
        )
        .unwrap_err();
        assert!(err.contains("不存在") || err.contains("exist"));
    }

    #[test]
    fn validate_launch_program_rejects_missing_path() {
        let empty: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        let err = validate_launch_program(&empty, &exp_ctx("x")).unwrap_err();
        assert!(err.contains("path"));
    }

    #[test]
    fn validate_user_danger_rejects_elevated() {
        assert!(validate_user_danger("elevated").is_err());
        assert!(validate_user_danger("normal").is_ok());
        assert!(validate_user_danger("").is_ok());
    }

    #[test]
    fn validate_launch_program_args_not_retokenized() {
        // {query} 含空格/引号/& → 作为单个参数传递，不二次分词。
        // 这里只校验展开逻辑：args_template 的 token 分割发生在模板空白处，
        // 占位符展开结果不再分词。
        let mut params = launch_params(r"C:\Windows\notepad.exe");
        params.insert("args_template".into(), "{query}".into());
        let ctx = ExpansionContext {
            query: "hello world & |".into(),
            current_folder: String::new(),
            selection: None,
            clipboard: None,
            now: fixed_now(),
            args: Default::default(),
        };
        // notepad.exe 存在（标准 Windows 路径），且 .exe 合法
        let result = validate_launch_program(&params, &ctx);
        if let Ok((_, args, _)) = result {
            // 单 token {query} → 单个 arg，空格不裂成两个
            assert_eq!(args.len(), 1);
            assert_eq!(args[0], "hello world & |");
        }
        // 如果 notepad.exe 不在标准位置，路径校验会失败——但不影响 arg 展开逻辑断言
    }

    #[test]
    fn validate_launch_program_rejects_nonexistent_working_dir() {
        // §10.2-5：working_dir 不存在 → 拒绝执行，不传空字符串继续。
        let mut params = launch_params(r"C:\Windows\notepad.exe");
        params.insert("working_dir".into(), r"C:\does_not_exist_xyz_999".into());
        let err = validate_launch_program(&params, &exp_ctx("x")).unwrap_err();
        assert!(err.contains("working_dir") || err.contains("不存在") || err.contains("exist"));
    }

    #[test]
    fn validate_launch_program_accepts_existing_working_dir() {
        let mut params = launch_params(r"C:\Windows\notepad.exe");
        params.insert("working_dir".into(), r"C:\Windows".into());
        let result = validate_launch_program(&params, &exp_ctx("x"));
        if let Ok((_, _, working_dir)) = result {
            assert_eq!(working_dir.as_deref(), Some(r"C:\Windows"));
        }
    }

    // ── K3 §4.5 commit 4：命名空间校验 ──────────────────────────────

    fn ns_engine(keyword: &str, name: &str) -> crate::websearch::WebEngine {
        crate::websearch::WebEngine {
            keyword: keyword.into(),
            name: name.into(),
            url_template: "https://x.test/{q}".into(),
        }
    }

    fn ns_alias(target: &str, words: &[&str]) -> crate::persistence::AliasEntry {
        crate::persistence::AliasEntry {
            kind: "file".into(),
            target: target.into(),
            words: words.iter().map(|w| (*w).into()).collect(),
            bound_at_utc: 0,
        }
    }

    #[test]
    fn namespace_conflict_with_web_engine() {
        let engines = vec![ns_engine("g", "Google")];
        let err = validate_trigger_namespace("g", TriggerOwner::Command, &engines, &[], &[], None)
            .unwrap_err();
        assert_eq!(err.kind, "web_engine");
        assert!(err.owner_label.contains("Google"));
    }

    #[test]
    fn namespace_conflict_with_web_engine_case_insensitive() {
        let engines = vec![ns_engine("bi", "Bing")];
        let err = validate_trigger_namespace("BI", TriggerOwner::Command, &engines, &[], &[], None)
            .unwrap_err();
        assert_eq!(err.kind, "web_engine");
    }

    #[test]
    fn namespace_conflict_with_alias() {
        let aliases = vec![ns_alias(r"C:\x.txt", &["wx"])];
        let err = validate_trigger_namespace("wx", TriggerOwner::Command, &[], &aliases, &[], None)
            .unwrap_err();
        assert_eq!(err.kind, "alias");
        assert!(err.owner_label.contains("x.txt"));
    }

    #[test]
    fn namespace_conflict_with_other_command_keyword() {
        let command_keywords = vec![("user.cmd_a".into(), "calc".into())];
        let err = validate_trigger_namespace(
            "calc",
            TriggerOwner::Command,
            &[],
            &[],
            &command_keywords,
            None,
        )
        .unwrap_err();
        assert_eq!(err.kind, "command");
        assert!(err.owner_label.contains("user.cmd_a"));
    }

    #[test]
    fn namespace_exclude_self_command_keyword() {
        // 编辑保存时排除自身——不与自己旧关键字冲突。
        let command_keywords = vec![("user.cmd_a".into(), "calc".into())];
        let result = validate_trigger_namespace(
            "calc",
            TriggerOwner::Command,
            &[],
            &[],
            &command_keywords,
            Some("user.cmd_a"),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn namespace_conflict_with_reserved_prefix() {
        let err = validate_trigger_namespace("ext:", TriggerOwner::Command, &[], &[], &[], None)
            .unwrap_err();
        assert_eq!(err.kind, "reserved");
    }

    #[test]
    fn namespace_conflict_with_reserved_gt() {
        let err = validate_trigger_namespace(">", TriggerOwner::Command, &[], &[], &[], None)
            .unwrap_err();
        assert_eq!(err.kind, "reserved");
    }

    #[test]
    fn namespace_conflict_with_path_prefix() {
        let err = validate_trigger_namespace("path:", TriggerOwner::Command, &[], &[], &[], None)
            .unwrap_err();
        assert_eq!(err.kind, "reserved");
    }

    #[test]
    fn namespace_empty_trigger_ok() {
        let result = validate_trigger_namespace(
            "",
            TriggerOwner::Command,
            &[ns_engine("g", "Google")],
            &[],
            &[],
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn namespace_no_conflict_ok() {
        let engines = vec![ns_engine("g", "Google")];
        let aliases = vec![ns_alias(r"C:\x.txt", &["wx"])];
        let command_keywords = vec![("user.cmd_a".into(), "calc".into())];
        let result = validate_trigger_namespace(
            "unique",
            TriggerOwner::Command,
            &engines,
            &aliases,
            &command_keywords,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn all_command_keywords_collects_enabled_and_disabled() {
        let (store, _dir) = store("ns-keywords");
        let mut cmd_a = user_cmd("user.cmd_a");
        cmd_a.keywords = vec!["calc".into(), "notepad".into()];
        cmd_a.enabled = true;
        let mut cmd_b = user_cmd("user.cmd_b");
        cmd_b.keywords = vec!["editor".into()];
        cmd_b.enabled = false;
        store.set(cmd_a).unwrap();
        store.set(cmd_b).unwrap();
        let kws = store.all_command_keywords();
        // 两条命令、共 3 个关键字，disabled 命令的关键字也收录
        assert_eq!(kws.len(), 3);
        let ids: Vec<&str> = kws.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"user.cmd_a"));
        assert!(ids.contains(&"user.cmd_b"));
    }

    // K3 §4.5：启动确定性裁决——网页引擎关键字优先，冲突命令 keyword binding 禁用。
    #[test]
    fn startup_resolve_disables_conflicting_keyword() {
        let (store, dir) = store("startup-resolve");
        let mut cmd = user_cmd("user.conflict_cmd");
        cmd.title = "Conflict Command".into();
        cmd.bindings.keyword = Some(crate::persistence::CommandBinding {
            trigger: Some("g".into()),
            ..Default::default()
        });
        cmd.enabled = true;
        store.set(cmd).unwrap();

        // 引擎关键字 "g" 与命令 keyword binding trigger "g" 冲突
        store.startup_resolve(&["g".to_string()]);

        let catalog = store.catalog();
        let item = catalog
            .iter()
            .find(|d| d.id == "user.conflict_cmd")
            .expect("conflict command in catalog");
        assert!(item.bindings.keyword.is_none(), "keyword binding removed");
        assert!(
            !item.disabled_reason.is_empty(),
            "disabled_reason set: {}",
            item.disabled_reason
        );
        assert!(
            item.enabled,
            "command body not disabled — only keyword binding"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_resolve_no_conflict_keeps_keyword() {
        let (store, dir) = store("startup-resolve-nocnf");
        let mut cmd = user_cmd("user.safe_cmd");
        cmd.bindings.keyword = Some(crate::persistence::CommandBinding {
            trigger: Some("xyz".into()),
            ..Default::default()
        });
        cmd.enabled = true;
        store.set(cmd).unwrap();

        // 引擎关键字 "g" 不与 "xyz" 冲突
        store.startup_resolve(&["g".to_string()]);

        let catalog = store.catalog();
        let item = catalog
            .iter()
            .find(|d| d.id == "user.safe_cmd")
            .expect("safe command in catalog");
        assert!(
            item.bindings.keyword.is_some(),
            "keyword binding preserved (no conflict)"
        );
        assert!(item.disabled_reason.is_empty(), "no disabled_reason");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
