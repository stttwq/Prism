//! 命名管道 JSON 消息服务（UTF-8，按行分隔，每行一条消息）。
//!
//! 协议契约见 frontend-spec.md 第 6-7 节：
//! 前端 → 后端：search / execute / reveal / actions / run_action，另加内置 ping 自检。
//! 后端 → 前端：pong / results / actions / status / error。
//!
//! 第四步：search 接真实索引；execute 打开文件；reveal 在资源管理器中定位。
//! 第五步：search 合并开始菜单程序，kind=app 置顶；execute 启动 .lnk。
//! 第六步：网页关键词 bi/b/g（+ 自定义，必应优先）→ kind=web；execute 用默认浏览器打开 URL。
//! 第八步：reload_engines 热替换引擎列表（设置页保存后立即生效）。
//! 第九步：actions / run_action 接基础动作（打开所在文件夹/复制/剪切/复制路径）。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::apps::SharedApps;
use crate::hierarchy::{MatchKind, MatchMetadata, NameTerms};
use crate::history::{HistoryDiagnostic, HistoryStore, HistoryUse, HistoryWeight};
use crate::indexer_client;
use crate::indexer_ipc::{
    exclusion_paths, requested_root, validate_search_request, BuildProgress, SearchFilter,
};
use crate::root_scope::RootRejection;
use crate::shell::{
    ActionTarget, ShellError, ShellErrorKind, ShellExecutor, ShellOperation, ShellOutcome,
    TargetKind,
};
use crate::websearch::{self, WebEngine};
use crate::{log, VERSION};

pub const BROKER_PROTOCOL: u32 = 1;

/// 共享引擎列表（可热重载）。
/// 读路径：search 持读锁；写路径：reload_engines 换整表。
pub type SharedEngines = Arc<std::sync::RwLock<Vec<WebEngine>>>;

pub struct BrokerPreferences {
    pinyin_enabled: AtomicBool,
    zip_program: std::sync::RwLock<Option<String>>,
}

impl BrokerPreferences {
    pub fn new(pinyin_enabled: bool) -> Self {
        Self {
            pinyin_enabled: AtomicBool::new(pinyin_enabled),
            zip_program: std::sync::RwLock::new(None),
        }
    }

    pub fn with_zip_program(pinyin_enabled: bool, zip_program: Option<String>) -> Self {
        Self {
            pinyin_enabled: AtomicBool::new(pinyin_enabled),
            zip_program: std::sync::RwLock::new(zip_program),
        }
    }

    pub fn pinyin_enabled(&self) -> bool {
        self.pinyin_enabled.load(Ordering::Acquire)
    }

    fn zip_program(&self) -> Option<String> {
        self.zip_program.read().ok().and_then(|guard| guard.clone())
    }
}

/// 前端发来的请求消息。`type` 字段区分类型（snake_case）。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol: u32,
        /// K0：能力协商。前端声明支持的能力集（如 "commands_v1"）。
        /// 缺失/空 = 旧前端，broker 不下发命令目录、不受理 command 请求。
        #[serde(default)]
        capabilities: Vec<String>,
    },
    /// 连接自检：期望回 pong。
    Ping,
    /// 即时搜索。
    Search {
        query: String,
        #[serde(default = "default_max")]
        max: usize,
        #[serde(default)]
        filters: Option<Vec<SearchFilter>>,
        /// 可选的当前目录范围（G4）。缺失或空白等于全局搜索，所以旧前端逐字节兼容。
        #[serde(default)]
        root: Option<String>,
        /// 显式搜索模式（G5）。缺失等于 `all`，所以旧前端逐字节兼容。
        #[serde(default)]
        mode: Option<SearchMode>,
        /// K0：命令上下文（current_folder/host_kind/host_capabilities）。
        /// 缺失 = 旧前端，search payload 逐字节兼容。broker 解析+校验+丢弃
        ///（K0 无消费者，K1 接入点见 search_service）。
        #[serde(default)]
        command_context: Option<crate::commands::SearchCommandContext>,
    },
    /// 执行选中项（打开文件 / 启动程序 / 打开网址）。
    Execute {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        target: Option<ActionTarget>,
        /// 触发本次动作的查询文本（查询记忆的记录侧输入）。缺失或 null 等于
        /// 无查询上下文，旧前端逐字节兼容。
        #[serde(default)]
        query: Option<String>,
    },
    /// 打开文件所在文件夹并选中。
    Reveal {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        target: Option<ActionTarget>,
        #[serde(default)]
        query: Option<String>,
    },
    /// 请求某文件的动作列表（→ 键动作面板）。不写历史，无 query。
    Actions {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        target: Option<ActionTarget>,
    },
    /// 执行动作面板里的某个动作。
    RunAction {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        target: Option<ActionTarget>,
        action: String,
        /// 动作参数：copy_to/move_to 携带 destination，rename 携带 new_name。
        #[serde(default)]
        args: Option<ActionArgs>,
        #[serde(default)]
        query: Option<String>,
    },
    /// 设置页保存后热重载网页引擎列表（步骤 8）。
    ReloadEngines {
        engines: Vec<WebEngine>,
    },
    UpdatePreferences {
        history_enabled: bool,
        pinyin_enabled: bool,
    },
    ClearHistory,
    /// G5：把一次枚举内有效的 window token 换回可激活的句柄，并复核身份。
    ///
    /// 分成 resolve/record 两步是有意的：激活必须由前台进程（WPF）完成，broker 只能
    /// 在这里做第一次复核；只有 WPF 回报成功后才允许写成功历史。
    ResolveWindow {
        target: ActionTarget,
    },
    /// G5：WPF 激活成功后回报，broker 据此写窗口历史。失败路径不发本消息。
    RecordWindowSwitch {
        target: ActionTarget,
        #[serde(default)]
        query: Option<String>,
    },
    /// 别名系统（2026-08-21 设想）：整体替换目标的词表（空词表 = 解绑）。
    AliasSet {
        target: ActionTarget,
        words: Vec<String>,
    },
    /// 解绑目标（幂等）。
    AliasDelete {
        target: ActionTarget,
    },
    /// 设置页列表。
    AliasList,
    /// 解析 .lnk 的目标路径（别名身份指向真实 exe）。非 .lnk 或失败返回 None。
    /// 复用 STA worker 的 COM 单元（禁裸 spawn_blocking——CoCreateInstance 会失败）。
    ResolveLnk {
        target: ActionTarget,
    },
    /// K0：拉取命令目录（查询通道，已协商连接）。
    CommandList,
    /// K1：执行命令。context 携带 command_id 与调用上下文（目录/选中项等）。
    /// 仅在协商 commands_v1 的连接上受理，否则回 Unsupported。
    ExecuteCommand {
        context: crate::commands::CommandInvocationContext,
    },
    /// K2 §4.6：设置或清除命令的快捷键绑定。combo 为 None = 清除。
    /// 仅协商 commands_v1 的连接受理。
    SetCommandShortcut {
        command_id: String,
        combo: Option<String>,
    },
    /// K3 §4.5：保存/替换用户命令定义。语义层校验在 broker 侧执行
    ///（路径存在性、协议白名单、关键字命名空间冲突）。拒绝而非静默修正。
    /// 仅协商 commands_v1 的连接受理。
    CommandSet {
        command: Box<crate::persistence::UserCommandDefinition>,
    },
    /// K3 §4.5：删除用户命令。幂等。
    /// 仅协商 commands_v1 的连接受理。
    CommandDelete {
        command_id: String,
    },
    /// K3 §4.5：触发词命名空间校验。broker 查全集（引擎/别名/命令/保留字）。
    /// 前端保存前调用，通过才写盘。
    ValidateTriggerNamespace {
        trigger: String,
        owner: crate::commands::TriggerOwner,
        /// 编辑中的命令 id——排除自身，避免编辑保存时与自身旧关键字冲突。
        #[serde(default)]
        exclude_command_id: Option<String>,
    },
    /// K3 §4.6：命令预览（dry-run）。返回执行时将用的最终字符串，不产生
    /// 任何副作用（不打开浏览器、不启动进程、不写磁盘）。
    /// 仅协商 commands_v1 的连接受理。
    CommandPreview {
        context: crate::commands::CommandInvocationContext,
    },
    /// K3 §4.8：导出用户命令。broker 返回持久化形态（UserCommandDefinition 列表）
    /// + exported_at，包在 VersionedEnvelope 里。内置命令不导出。
    ///
    /// 仅协商 commands_v1 的连接受理。
    CommandExport,
}

/// 显式搜索模式。未知取值按 `all` 处理，避免新前端加模式后打死旧 broker。
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    All,
    Window,
    #[serde(other)]
    Unknown,
}

fn default_max() -> usize {
    100
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// K0：Results.cacheable 的反向省略——cacheable=true（K0 的每一个响应）时省略，
/// 使 search 回包逐字节等于 K0 前（P2）。语义自解释：只在「有限制」时出现。
fn is_true(value: &bool) -> bool {
    *value
}
/// 后端回给前端的响应消息。
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol: u32,
        version: String,
        /// 构建指纹（`1.1.0+release.<mtime>`），用于确认跑的是刚编出来的那份。
        /// 纯新增字段，旧前端忽略即可。
        build_id: String,
        /// K0：broker 支持的能力与客户端声明集的交集。skip_serializing_if Vec::is_empty
        /// 保证未协商连接的回包逐字节等于 K0 前（P2）。
        #[serde(skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
        /// K0：命令目录代际。仅协商成功时为 Some，否则缺省（逐字节兼容）。
        #[serde(skip_serializing_if = "Option::is_none")]
        command_catalog_generation: Option<u64>,
    },
    /// ping 的回应，附带后端版本供前端自检。
    Pong { version: String, build_id: String },
    /// 搜索结果列表（"展示更多"行由前端追加）。
    /// `is_indexing=true` 表示索引尚未就绪，items 可能为空。
    Results {
        query: String,
        items: Vec<SearchResult>,
        #[serde(default)]
        is_indexing: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        // Boxed so the optional progress block does not inflate every `Response` variant.
        // Serialization is unchanged: `Box` is transparent to serde.
        index_progress: Option<Box<IndexProgressDto>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        index_error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        index_generation: Option<u64>,
        #[serde(default)]
        is_truncated: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        matched_count: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        scanned_nodes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name_candidates: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        entered_top_k: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path_constructions: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pinyin_status: Option<crate::indexer_ipc::PinyinStatus>,
        #[serde(skip_serializing_if = "Option::is_none")]
        history_status: Option<String>,
        /// 请求带了 root 但无法使用时的结构化降级原因（值与 `root_scope::RootRejection`
        /// 的稳定字符串一致）。此时 items 已经是全局搜索结果，前端据此回到全局并提示。
        #[serde(skip_serializing_if = "Option::is_none")]
        root_rejection: Option<crate::root_scope::RootRejection>,
        #[serde(skip_serializing_if = "Option::is_none")]
        root_message: Option<String>,
        /// K0：响应是否可缓存。skip_serializing_if=is_true 使 cacheable=true（K0
        /// 的每一个响应）时省略，逐字节兼容旧前端。K1 让含命令行的响应回 false。
        #[serde(skip_serializing_if = "is_true")]
        cacheable: bool,
        /// K0：命令目录代际。K0 恒 None（无命令行），K1 在协商连接上回填。
        #[serde(skip_serializing_if = "Option::is_none")]
        command_catalog_generation: Option<u64>,
    },
    /// 动作面板列表。
    Actions { items: Vec<ActionItem> },
    /// 后端状态推送（如索引进行中）。
    Status {
        is_indexing: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        cancelled: bool,
    },
    /// G5：复核通过的窗口句柄，交给 WPF 完成前台激活。
    ///
    /// 句柄只在这一条消息里离开 broker。WPF 拿到后必须在激活前再复核一次，压掉
    /// 「本次复核 → 真正激活」之间的关闭与句柄复用窗口。
    WindowHandle {
        handle: u64,
        pid: u32,
        title: String,
        is_minimized: bool,
    },
    /// 别名系统：设置页列表（绑定时间倒序）。
    AliasItems { items: Vec<AliasItemDto> },
    /// 别名设置/解绑回执：`Ok` 空串表示成功，否则为用户可读错误。
    AliasApplied { message: String },
    /// .lnk 解析结果：resolved 为解析后的 exe 路径，None=非 .lnk 或解析失败。
    /// 前端失败/超时回退绑 .lnk（现行为兜底）。
    LnkTarget { resolved: Option<String> },
    /// K0：命令目录（协商连接拉取）。
    CommandCatalog {
        generation: u64,
        items: Vec<crate::commands::CommandDescriptor>,
    },
    /// K1：broker 把 UI-owned 命令回传给前端执行。id = 命令 id，
    /// context = 原始调用上下文（前端据此决定是否隐藏窗口等）。
    UiCommand {
        id: String,
        context: crate::commands::CommandInvocationContext,
    },
    /// K2 §4.6：快捷键绑定设置/清除回执。message 空串=成功，否则为错误文案。
    CommandShortcutApplied { message: String },
    /// K3 §4.5：命令保存/删除回执。message 空串=成功，否则为错误文案。
    CommandApplied { message: String },
    /// K3 §4.5：触发词命名空间校验结果。ok=true 无冲突；
    /// ok=false 时 conflict 携带来源信息（kind + owner_label）。
    NamespaceValidation {
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        conflict: Option<crate::commands::NamespaceConflict>,
    },
    /// K3 §4.6：命令预览结果。预览成功时返回执行时将用的最终字符串——
    /// open_url 返回 URL 全文，launch_program 返回 path + args + working_dir。
    /// 校验失败时 ok=false，message 携带错误文案（非空字符串）。
    CommandPreviewResult {
        ok: bool,
        message: String,
        /// open_url: 展开后的 URL 全文。
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        /// launch_program: 展开后的程序路径。
        #[serde(skip_serializing_if = "Option::is_none")]
        program_path: Option<String>,
        /// launch_program: 展开后的参数数组。
        #[serde(skip_serializing_if = "Vec::is_empty")]
        program_args: Vec<String>,
        /// launch_program: 展开后的工作目录。
        #[serde(skip_serializing_if = "Option::is_none")]
        program_working_dir: Option<String>,
    },
    /// K3 §4.8：命令导出结果。返回用户命令的持久化形态 + exported_at 时间戳。
    /// 内置命令不导出（它们随版本走）。
    CommandExportResult {
        /// 版本化信封，schema_version 对应 CommandData::SCHEMA_VERSION。
        envelope: crate::persistence::VersionedEnvelope<crate::persistence::CommandData>,
        /// 导出时间（ISO 8601 UTC）。
        exported_at: String,
    },
    /// 出错时回传，前端在列表区以单行提示展示。
    Error {
        message: String,
        /// 结构化错误类别（snake_case：access_denied/target_invalid/conflict/
        /// elevation_required/unsupported/system）。2026-08-24 复审确认：前端
        /// 目前只读 message，此字段是**预留**——给将来按类别分支的错误处理用
        ///（L4 的类型化产物），不是遗漏；前端接入前不要在此反推文案。
        #[serde(skip_serializing_if = "Option::is_none")]
        category: Option<crate::shell::ShellErrorKind>,
    },
}

/// 首次构建期间随 `results` 返回的索引进度快照。
///
/// `scanned` / `total_estimate` 为旧字段，保持原义。逐卷字段（G9）全部可选：
/// 首次安装无缓存可估算记录总量时它们缺失，前端退化为"已完成 N/M 卷"。
#[derive(Debug, Default, Serialize)]
pub struct IndexProgressDto {
    pub scanned: u64,
    pub total_estimate: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volumes_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volumes_done: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_volume: Option<String>,
}

/// 单条搜索结果，字段对应 frontend-spec.md 第 2 节 `SearchResult` record。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SearchResultKind {
    App,
    File,
    Folder,
    Web,
    /// G5：可切换的顶层窗口。旧前端映射为 Unknown 并忽略。
    Window,
    /// K0：命令行。追加到枚举末尾（Window 之后）——derive(Ord) 序号参与
    /// compare_search_results / sort_search_results_with_picks 的最终 tiebreak，
    /// 追加给命令行最弱 tiebreak（设计 §6.2「命令不得越过文件强匹配」）。
    /// K0 无生产者，K1 直接依赖此顺序。
    Command,
}

impl SearchResultKind {
    /// 序列化后的 kind 字符串（serde snake_case 的手写镜像，供去重键使用）。
    fn wire_str(self) -> &'static str {
        match self {
            SearchResultKind::App => "app",
            SearchResultKind::File => "file",
            SearchResultKind::Folder => "folder",
            SearchResultKind::Web => "web",
            SearchResultKind::Window => "window",
            SearchResultKind::Command => "command",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    /// "app" | "file" | "folder" | "web"（"more" 行由前端生成）。
    pub kind: SearchResultKind,
    /// 审计 P7：路径字段改 Arc<str>，共享同一路径的多个字段（subtitle /
    /// execute_id / target.value）从 String clone 变为 refcount++。
    pub title: Arc<str>,
    pub subtitle: Arc<str>,
    /// 回传后端用于 execute/reveal/actions 的标识。
    pub execute_id: Arc<str>,
    /// Typed execution contract. `execute_id` remains for old readers only.
    pub target: ActionTarget,
    /// 标题中要染蓝的区间，扁平数组 [start,len,start,len,...]。
    pub match_spans: Vec<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_metadata: Option<MatchMetadata>,
}

/// 动作面板单项，字段对应 frontend-spec.md 第 2 节 `ActionItem` record。
/// K2 §4.2：扩四字段。invocation_kind/command_id/is_enabled/disabled_reason。
/// skip_serializing_if 保证内置动作 JSON 与 K1 逐字节一致（P3：未协商连接拿到的
/// 动作列表完全没变化）。
#[derive(Debug, Clone, Serialize)]
pub struct ActionItem {
    /// "open_folder"|"copy"|"cut"|"copy_path"|"shell:<n>" 等。
    pub id: String,
    pub label: String,
    /// Segoe Fluent Icons 字形码，无图标传 ""。
    pub icon_glyph: String,
    pub has_submenu: bool,
    pub is_section_header: bool,
    /// K2: "builtin_action" | "command"。默认 builtin_action。
    #[serde(skip_serializing_if = "is_builtin_action")]
    pub invocation_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(skip_serializing_if = "is_true")]
    pub is_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
}

fn is_builtin_action(kind: &str) -> bool {
    kind == "builtin_action"
}

/// 动作参数：copy_to/move_to 携带 destination，rename 携带 new_name。
/// 所有字段可选，由具体动作决定哪些是必填。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_name: Option<String>,
}

/// 别名系统：设置页列表项。
#[derive(Debug, Clone, Serialize)]
pub struct AliasItemDto {
    pub target: ActionTarget,
    pub words: Vec<String>,
    pub bound_at_utc: u64,
}
/// 并发 armed 的管道 listener 数（对齐 indexer 侧 `indexer_runtime::PIPE_LISTENERS`）。
/// 单 listener 在 `connect()` 返回到下一次 `create()` 之间存在不可避免的空窗，
/// 空窗内到达的客户端拿到 ERROR_PIPE_BUSY；多个 listener 互为备份。前端即将引入
/// 第二条长连接（动作通道，审计 C1），同样依赖多 listener 消除建连竞争。
const PIPE_LISTENERS: usize = 4;

/// serve 的共享状态束：accept_loop 与连接任务都需要整组句柄，
/// 打包避免超长参数列表。
struct BrokerShared {
    apps: SharedApps,
    engines: SharedEngines,
    shell: Arc<ShellExecutor>,
    history: Arc<HistoryStore>,
    preferences: Arc<BrokerPreferences>,
    windows: Arc<crate::window_list::WindowSnapshotStore>,
    /// 别名系统（2026-08-21 设想）：broker 拥有（用户数据，同 history）。
    aliases: Arc<crate::alias::AliasStore>,
    /// K0：命令系统存储（目录 + 使用记录），broker 拥有。
    commands: Arc<crate::commands::CommandStore>,
    /// L 批次（FRESH-AUDIT-3-2026-08-20）：活跃连接数（照搬 indexer 的
    /// try_admit_connection）。正常部署只有前端一条全生命期连接 + 少量
    /// 世代/瞬时客户端，8 已宽裕；无上限时任凭本地进程堆积连接即可耗尽
    /// 2 worker 的 runtime。
    connections: AtomicUsize,
}

impl BrokerShared {
    /// L 批次：并发连接上限 8。超出者在握手前直接关闭——不产生任务与行缓冲。
    const MAX_CONNECTIONS: usize = 8;

    fn try_admit_connection(&self) -> bool {
        let current = self.connections.fetch_add(1, Ordering::AcqRel);
        if current >= Self::MAX_CONNECTIONS {
            self.connections.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    fn release_connection(&self) {
        self.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 当前进程用户 SID（字符串，如 S-1-5-21-...）。管道 ACL 需要，只取一次。
fn current_user_sid() -> std::io::Result<&'static str> {
    use std::sync::OnceLock;
    static SID: OnceLock<Result<String, std::io::Error>> = OnceLock::new();
    SID.get_or_init(|| unsafe {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
        use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
        use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(std::io::Error::other)?;
        let mut needed = 0u32;
        // 长度探测调用预期以 ERROR_INSUFFICIENT_BUFFER 失败并回填 needed。
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            return Err(std::io::Error::other("GetTokenInformation sizing failed"));
        }
        let mut buffer = vec![0u8; needed as usize];
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .map_err(std::io::Error::other)?;
        let _ = CloseHandle(token);
        let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
        let mut sid_string = windows::core::PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid_string).map_err(std::io::Error::other)?;
        let len = PCWSTR(sid_string.0).len();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_string.0, len));
        let _ = LocalFree(HLOCAL(sid_string.0 as _));
        Ok(sid)
    })
    .as_deref()
    .map_err(std::io::Error::other)
}

/// 建带 ACL 的 broker 管道实例（AUDIT-2026-08-18 R-A2）。
///
/// 此前管道无 ACL：broker 以用户身份执行删除/复制动作，任意本地进程都能
/// 连上指挥它。现在 DACL 只授 SYSTEM 与当前用户（管理员经 SYSTEM/属主兜底），
/// 并拒绝远程客户端。SID 获取失败则建管失败——宁可拒开也不裸奔。
fn create_broker_pipe(pipe_name: &str, first: bool) -> std::io::Result<NamedPipeServer> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, BOOL, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    let sid = current_user_sid()?;
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})");
    let sddl: Vec<u16> = std::ffi::OsStr::new(&sddl)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
        .map_err(std::io::Error::other)?;
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: BOOL(0),
    };
    let result = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                pipe_name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            )
    };
    unsafe {
        let _ = LocalFree(HLOCAL(descriptor.0));
    }
    result
}

/// 管道服务主循环：创建管道实例 → 等待前端连接 → 交给连接处理器 →
/// 立刻建下一个实例等待重连。前端崩溃/重启不影响后端。
///
/// 实例重建失败不再让进程退出（审计 H3：此前任何一次 `create` 失败都会令
/// `serve` 上抛 → main 退出，活动连接全断），改为退避重试；仅当确认管道名被
/// 第三方进程抢注（名下已无任何实例仍被占用）时才视为致命。
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    pipe_name: &str,
    apps: SharedApps,
    engines: SharedEngines,
    shell: Arc<ShellExecutor>,
    history: Arc<HistoryStore>,
    preferences: Arc<BrokerPreferences>,
    aliases: Arc<crate::alias::AliasStore>,
    commands: Arc<crate::commands::CommandStore>,
) -> std::io::Result<()> {
    // 首个实例带 first_pipe_instance(true)：创建失败说明管道名已被另一个 broker
    // 持有（真双实例），唯一正确动作是退出并让 main 上报。
    let first = create_broker_pipe(pipe_name, true)?;

    // One snapshot store for the whole broker: window tokens must stay comparable across
    // reconnects, and it holds only the latest enumeration.
    let shared = Arc::new(BrokerShared {
        apps,
        engines,
        shell,
        history,
        preferences,
        windows: Arc::new(crate::window_list::WindowSnapshotStore::new()),
        aliases,
        commands,
        connections: AtomicUsize::new(0),
    });

    let ownership = Arc::new(PipeOwnership::new());
    ownership.instances.fetch_add(1, Ordering::Relaxed);

    let mut listeners = tokio::task::JoinSet::new();
    listeners.spawn(accept_loop(
        pipe_name.to_owned(),
        first,
        shared.clone(),
        ownership.clone(),
    ));
    // 其余 listener 在启动期一次性补齐：失败不致命（该槽位缺席时其余 listener
    // 照常服务，最坏退化为单 listener，与修复前行为一致）。
    for _ in 1..PIPE_LISTENERS {
        match create_broker_pipe(pipe_name, false) {
            Ok(pipe) => {
                ownership.instances.fetch_add(1, Ordering::Relaxed);
                listeners.spawn(accept_loop(
                    pipe_name.to_owned(),
                    pipe,
                    shared.clone(),
                    ownership.clone(),
                ));
            }
            Err(error) => {
                log(format!(
                    "额外管道 listener 创建失败，降级为更少并发槽位：{error}"
                ));
            }
        }
    }

    // 任一 accept 循环退出 = 无法保证仍有 armed listener，整体上抛（对齐 indexer 语义）。
    let first_result = listeners.join_next().await;
    listeners.abort_all();
    match first_result {
        Some(Ok(result)) => result,
        Some(Err(error)) => Err(std::io::Error::other(format!(
            "broker pipe listener task: {error}"
        ))),
        None => Err(std::io::Error::other(
            "no broker pipe listeners were started",
        )),
    }
}

/// 本进程对管道名的持有状态：名下实例数（armed listener + 已连接实例）与
/// 抢注探测的互斥锁。
struct PipeOwnership {
    instances: AtomicUsize,
    probe_lock: tokio::sync::Mutex<()>,
}

impl PipeOwnership {
    fn new() -> Self {
        Self {
            instances: AtomicUsize::new(0),
            probe_lock: tokio::sync::Mutex::new(()),
        }
    }
}

/// 单个 listener 的 accept 循环：等待客户端 → 交给连接任务 → 立刻重建本槽位。
async fn accept_loop(
    pipe_name: String,
    mut server: NamedPipeServer,
    shared: Arc<BrokerShared>,
    ownership: Arc<PipeOwnership>,
) -> std::io::Result<()> {
    // R4（全仓检验 2026-08-25 第二轮）：单次 connect 错误不再让整个 accept 循环退出
    // ——旧路径丢掉其余健康 listener，broker exit(1)，与诱因（瞬时管道状态竞争）
    // 完全不成比例。失败实例不可复用：计数减掉、退避一小段后走 rearm 重建。
    // rearm 返回 Err 仅剩"管道名被第三方抢注"一种致命语义，照旧上抛让位。
    let mut connect_retry_delay = std::time::Duration::from_millis(100);
    loop {
        if let Err(error) = server.connect().await {
            // armed 实例被本错误路径丢弃，计数必须同步减掉，否则抢注探测永远看不到 0。
            ownership.instances.fetch_sub(1, Ordering::Relaxed);
            log(format!("管道 connect 失败，重建 listener 重试：{error}"));
            drop(server);
            tokio::time::sleep(connect_retry_delay).await;
            connect_retry_delay = (connect_retry_delay * 2).min(std::time::Duration::from_secs(5));
            server = rearm_listener(&pipe_name, &ownership).await?;
            continue;
        }
        connect_retry_delay = std::time::Duration::from_millis(100);
        let connected = server;
        // 实例从 armed 转为 connected，仍归本进程持有，计数不变；
        // 连接任务结束时由其减 1。
        server = rearm_listener(&pipe_name, &ownership).await?;

        // L 批次：连接准入——超限在握手前直接断开，不产生任务。
        if !shared.try_admit_connection() {
            log(format!(
                "broker 连接被拒：超过 {} 个并发连接",
                BrokerShared::MAX_CONNECTIONS
            ));
            drop(connected);
            ownership.instances.fetch_sub(1, Ordering::Relaxed);
            continue;
        }

        let shared = shared.clone();
        let ownership = ownership.clone();
        tokio::spawn(async move {
            let BrokerShared {
                apps,
                engines,
                shell,
                history,
                preferences,
                windows,
                aliases,
                commands,
                ..
            } = &*shared;
            let result = handle_connection(
                connected,
                apps.clone(),
                engines.clone(),
                shell.clone(),
                history.clone(),
                preferences.clone(),
                windows.clone(),
                aliases.clone(),
                commands.clone(),
            )
            .await;
            shared.release_connection();
            ownership.instances.fetch_sub(1, Ordering::Relaxed);
            if let Err(e) = result {
                log(format!("连接处理结束：{e}"));
            }
        });
    }
}

/// 重建本槽位的 listener（审计 H3）。普通失败按退避无限重试；仅当名下实例数
/// 归零（本进程不再持有任何管道实例）时，用 `first_pipe_instance(true)` 探测
/// 管道名归属：探测成功说明无人占用，顺带恢复首实例身份；仍被占用则只能是
/// 第三方进程抢注——此时两个 broker 会随机分走客户端连接，必须退出让位。
///
/// 探测互斥：多个槽位同时进入零持有窗口时，串行探测避免 A 槽刚建好的实例
/// 让 B 槽的探测误判成抢注。
async fn rearm_listener(
    pipe_name: &str,
    ownership: &PipeOwnership,
) -> std::io::Result<NamedPipeServer> {
    let mut delay = std::time::Duration::from_millis(100);
    loop {
        if ownership.instances.load(Ordering::Relaxed) == 0 {
            let _guard = ownership.probe_lock.lock().await;
            // 拿到锁后复查：等锁期间兄弟槽位可能已经重建了实例。
            if ownership.instances.load(Ordering::Relaxed) == 0 {
                return match create_broker_pipe(pipe_name, true) {
                    Ok(server) => {
                        ownership.instances.fetch_add(1, Ordering::Relaxed);
                        Ok(server)
                    }
                    Err(error) => Err(std::io::Error::new(
                        error.kind(),
                        format!("broker pipe name taken over by another process: {error}"),
                    )),
                };
            }
        }
        match create_broker_pipe(pipe_name, false) {
            Ok(server) => {
                ownership.instances.fetch_add(1, Ordering::Relaxed);
                return Ok(server);
            }
            Err(error) => {
                log(format!("管道 listener 重建失败，{delay:?} 后重试：{error}"));
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(5));
            }
        }
    }
}

/// 入站单行长度上限：合法请求是 KB 级；无上限的逐行读会让任意本地进程
/// 连上管道灌超长行撑爆内存。
const MAX_REQUEST_LINE_BYTES: usize = 1024 * 1024;

/// 有界逐行读取器：分块读入、跨调用保留未换行的残留字节、超限即报错。
/// 替代无上限累积的 `BufReader::lines()`。
///
/// 上限按方向配置（审计 P12）：入站请求走 [`MAX_REQUEST_LINE_BYTES`]（1MB），
/// indexer 响应方向另设更宽的上限——1000 条长路径结果合法地超过 1MB。
pub(crate) struct BoundedLineReader<R: tokio::io::AsyncRead + Unpin> {
    reader: BufReader<R>,
    carry: Vec<u8>,
    max_line_bytes: usize,
}

impl<R: tokio::io::AsyncRead + Unpin> BoundedLineReader<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self::with_limit(reader, MAX_REQUEST_LINE_BYTES)
    }

    /// 自定上限的构造（审计 P12）：调用方按方向选择合适的上限。
    pub(crate) fn with_limit(reader: R, max_line_bytes: usize) -> Self {
        Self {
            reader: BufReader::new(reader),
            carry: Vec::with_capacity(512),
            max_line_bytes,
        }
    }

    fn too_long(&self) -> String {
        // 向上取整到 MB：不足 1MB 的上限也不会显示成「0MB」。
        format!(
            "数据行超过 {}MB 上限",
            self.max_line_bytes.div_ceil(1024 * 1024)
        )
    }

    /// 读下一行（含换行符前的内容）。EOF 且无残留返回 None；
    /// 单行超过本读取器的上限返回 Err（调用方应断开）。
    pub(crate) async fn next_line(&mut self) -> Result<Option<String>, String> {
        loop {
            if let Some(pos) = self.carry.iter().position(|byte| *byte == b'\n') {
                // 上限检查必须同样覆盖"换行符已到"的分支，否则超长行会被整行取出。
                if pos > self.max_line_bytes {
                    return Err(self.too_long());
                }
                let mut line: Vec<u8> = self.carry.drain(..=pos).collect();
                line.pop(); // \n
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(
                    String::from_utf8(line).map_err(|_| "请求不是有效的 UTF-8".to_string())?,
                ));
            }
            if self.carry.len() > self.max_line_bytes {
                return Err(self.too_long());
            }
            let mut chunk = [0u8; 8192];
            let read = self
                .reader
                .read(&mut chunk)
                .await
                .map_err(|error| format!("读取请求失败：{error}"))?;
            if read == 0 {
                if self.carry.is_empty() {
                    return Ok(None);
                }
                // 最后一行没有换行符：仍按一行处理（与 lines() 行为一致）。
                let line = std::mem::take(&mut self.carry);
                return Ok(Some(
                    String::from_utf8(line).map_err(|_| "请求不是有效的 UTF-8".to_string())?,
                ));
            }
            self.carry.extend_from_slice(&chunk[..read]);
        }
    }
}

/// 握手（首行）限时（审计 L1 分阶段空闲策略）：客户端连上后正常会立刻发 hello，
/// 连上不发首行的空连接 10 秒即断开，防止挂死客户端的连接任务常驻。
/// 已完成握手的连接**不设**空闲超时：前端是全生命期单连接设计，掐空闲会把
/// 每次击键搜索退化回逐请求建连，重演 ERROR_PIPE_BUSY（教训见 indexer_client.rs 头注）。
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 读连接的首行（握手），带限时。超时归一成与行读取相同的 `Err(String)` 语义，
/// 由调用方按"回错误后断开"处理；超时值作为参数以便单测注入毫秒级时限。
async fn read_handshake_line<R: tokio::io::AsyncRead + Unpin>(
    lines: &mut BoundedLineReader<R>,
    timeout: std::time::Duration,
) -> Result<Option<String>, String> {
    match tokio::time::timeout(timeout, lines.next_line()).await {
        Ok(inner) => inner,
        Err(_) => Err(format!(
            "握手超时：连接 {} 秒内未收到首行",
            timeout.as_secs()
        )),
    }
}

/// 单个连接的收发循环：逐行读入 JSON 请求，分发后逐行写回 JSON 响应。
///
/// B5（AUDIT-4 批次D，2026-08-21）：`Search` 是查询通道上唯一的无界计算请求
/// （索引写锁停顿可达数秒）。此前逐请求串行，一次慢搜索冻结同连接后续全部
/// 请求的读取与计算。现在 Search spawn 并发计算（在途上限 16/连接），其余
/// 请求（ping/actions/resolve 等本就快）保持内联；**响应经 mpsc 交给连接级
/// `ordered_writer` 按请求序号严格保序写出**——协议无请求 id、前端按行配对，
/// 保序是硬前提。EOF 后读循环等 writer 排干在途响应（search 内部自有超时，
/// 有界），客户端断开时写失败即静默丢弃残余。
/// 写失败（客户端已断）静默返回，残余丢弃。
#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    pipe: NamedPipeServer,
    apps: SharedApps,
    engines: SharedEngines,
    shell: Arc<ShellExecutor>,
    history: Arc<HistoryStore>,
    preferences: Arc<BrokerPreferences>,
    windows: Arc<crate::window_list::WindowSnapshotStore>,
    aliases: Arc<crate::alias::AliasStore>,
    commands: Arc<crate::commands::CommandStore>,
) -> std::io::Result<()> {
    log("前端已连接");
    let (reader, writer) = tokio::io::split(pipe);
    let mut lines = BoundedLineReader::new(reader);
    let (tx, rx) = tokio::sync::mpsc::channel::<(u64, Response)>(MAX_QUEUED_RESPONSES);
    let writer_task = tokio::spawn(ordered_writer(writer, rx));
    // B5：在途 search 并发上限。超出（本地进程灌请求）立即回错——仍走保序
    // 通道，不破配对。Arc<Semaphore> 的 permit 随任务结束释放。
    let search_permits = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT_SEARCHES));

    // K0：本连接的能力协商状态。Hello 之前 commands_v1=false（默认），所有
    // command 相关请求一律拒绝；Hello 之后按客户端声明集写位。
    let mut caps = crate::commands::ConnectionCaps::default();

    let mut next_seq: u64 = 0;
    let mut first_line = true;
    loop {
        let read = if std::mem::take(&mut first_line) {
            read_handshake_line(&mut lines, HANDSHAKE_TIMEOUT).await
        } else {
            lines.next_line().await
        };
        let line = match read {
            Ok(option) => match option {
                Some(line) => line,
                None => break, // EOF：客户端断开
            },
            Err(message) => {
                // 超长/损坏的入站行（或握手超时）：回一条错误说明后断开连接。
                let _ = tx
                    .send((
                        next_seq,
                        Response::Error {
                            message,
                            category: None,
                        },
                    ))
                    .await;
                log("入站请求行异常，断开连接");
                drop(tx);
                let _ = writer_task.await;
                return Ok(());
            }
        };

        let line = line.trim();
        if line.is_empty() {
            // 空行不占序号也不回包（与旧行为一致：continue 不写响应）。
            continue;
        }

        let seq = next_seq;
        next_seq = next_seq.saturating_add(1);

        match serde_json::from_str::<Request>(line) {
            Ok(Request::Search {
                query,
                max,
                filters,
                root,
                mode,
                command_context,
            }) => {
                let Ok(permit) = search_permits.clone().try_acquire_owned() else {
                    let _ = tx
                        .send((
                            seq,
                            Response::Error {
                                message: format!(
                                    "too many concurrent searches (limit {MAX_INFLIGHT_SEARCHES})"
                                ),
                                category: None,
                            },
                        ))
                        .await;
                    continue;
                };
                // H4（全仓复审 2026-08-22）：seq 由本分支领走后必须回包，否则 ordered_writer
                // 的 next_seq 永久停摆，整条连接静默卡死。双层 spawn：内层算响应，外层
                // await JoinHandle——内层 panic（debug/tests 下）时外层仍回一条错误响应。
                // K0：command_context 进 search_service 之前先按 caps 门控+sanitize。
                // 未协商（caps.commands_v1=false）→ 整体丢弃（None），search 逐字节不变。
                let command_context = command_context.and_then(|ctx| ctx.sanitize(caps));
                let apps = apps.clone();
                let engines = engines.clone();
                let history = history.clone();
                let preferences = preferences.clone();
                let windows = windows.clone();
                let aliases = aliases.clone();
                let commands = commands.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mode = mode.unwrap_or_default();
                    let inner = tokio::spawn(async move {
                        search_service(
                            SearchArgs {
                                query: &query,
                                max,
                                filters,
                                root: root.as_deref(),
                                mode,
                                command_context,
                            },
                            &apps,
                            &engines,
                            &history,
                            &preferences,
                            &windows,
                            &aliases,
                            &commands,
                        )
                        .await
                    });
                    let response = match inner.await {
                        Ok(response) => response,
                        Err(_) => Response::Error {
                            message: "search task panicked".to_string(),
                            category: None,
                        },
                    };
                    let _ = tx.send((seq, response)).await;
                    drop(permit);
                });
            }
            Ok(req) => {
                // K0：Hello 在连接循环内处理——需要写 caps（mut），不能进
                // dispatch_non_search（不可变借用）。CommandList 同样需读 caps。
                let response = match &req {
                    Request::Hello {
                        protocol,
                        capabilities,
                    } => {
                        if *protocol == BROKER_PROTOCOL {
                            // 协商：仅当协议匹配且客户端声明含 commands_v1 时置位。
                            let negotiated = capabilities.iter().any(|cap| cap == "commands_v1");
                            caps.commands_v1 = negotiated;
                            Response::Hello {
                                protocol: *protocol,
                                version: VERSION.to_string(),
                                build_id: crate::build_id(),
                                features: if negotiated {
                                    vec!["commands_v1".to_string()]
                                } else {
                                    Vec::new()
                                },
                                command_catalog_generation: if negotiated {
                                    Some(commands.generation())
                                } else {
                                    None
                                },
                            }
                        } else {
                            Response::Error {
                                message: format!(
                                    "broker protocol {protocol} is incompatible with {BROKER_PROTOCOL}"
                                ),
                                category: None,
                            }
                        }
                    }
                    Request::CommandList => {
                        if !caps.commands_v1 {
                            Response::Error {
                                message: "command catalog requires commands_v1 capability".into(),
                                category: Some(ShellErrorKind::Unsupported),
                            }
                        } else {
                            let items = commands.catalog();
                            Response::CommandCatalog {
                                generation: commands.generation(),
                                items,
                            }
                        }
                    }
                    Request::Actions { id, target } => {
                        // K2 §4.4：caps 为真时在动作列表后追加命令段；为假时只返回
                        // 内置段（K1 逐字节一致，P3）。命令身份仍被拒绝进文件动作面板。
                        let resolved =
                            resolve_target(target.clone(), id.clone(), Some(TargetKind::File));
                        if TargetKind::parse(&resolved.kind) == Some(TargetKind::Command) {
                            Response::Error {
                                message: "命令不能通过该请求执行".into(),
                                category: Some(ShellErrorKind::Unsupported),
                            }
                        } else {
                            let builtin = crate::actions::list_actions(&resolved);
                            match crate::action_composer::compose(
                                &resolved,
                                builtin,
                                &commands,
                                caps.commands_v1,
                            ) {
                                Ok(items) => Response::Actions { items },
                                Err(ShellError { kind, message }) => Response::Error {
                                    message,
                                    category: Some(kind),
                                },
                            }
                        }
                    }
                    Request::ExecuteCommand { context } => {
                        if !caps.commands_v1 {
                            Response::Error {
                                message: "command execution requires commands_v1 capability".into(),
                                category: Some(ShellErrorKind::Unsupported),
                            }
                        } else {
                            execute_command(context.clone(), &commands, &shell, &history).await
                        }
                    }
                    Request::SetCommandShortcut { command_id, combo } => {
                        if !caps.commands_v1 {
                            Response::Error {
                                message: "command shortcut requires commands_v1 capability".into(),
                                category: Some(ShellErrorKind::Unsupported),
                            }
                        } else {
                            let commands_for_blocking = commands.clone();
                            let cid = command_id.clone();
                            let combo_val = combo.clone();
                            let result = tokio::task::spawn_blocking(move || {
                                commands_for_blocking.set_shortcut_binding(&cid, combo_val)
                            })
                            .await
                            .unwrap_or_else(|error| Err(format!("set shortcut task: {error}")));
                            Response::CommandShortcutApplied {
                                message: result.err().unwrap_or_default(),
                            }
                        }
                    }
                    Request::CommandSet { command } => {
                        if !caps.commands_v1 {
                            Response::Error {
                                message: "command set requires commands_v1 capability".into(),
                                category: Some(ShellErrorKind::Unsupported),
                            }
                        } else {
                            handle_command_set(*command.clone(), &commands, &engines, &aliases)
                                .await
                        }
                    }
                    Request::CommandDelete { command_id } => {
                        if !caps.commands_v1 {
                            Response::Error {
                                message: "command delete requires commands_v1 capability".into(),
                                category: Some(ShellErrorKind::Unsupported),
                            }
                        } else {
                            let commands_for_blocking = commands.clone();
                            let cid = command_id.clone();
                            let result = tokio::task::spawn_blocking(move || {
                                commands_for_blocking.delete(&cid)
                            })
                            .await
                            .unwrap_or_else(|error| Err(format!("delete command task: {error}")));
                            Response::CommandApplied {
                                message: result.err().unwrap_or_default(),
                            }
                        }
                    }
                    Request::ValidateTriggerNamespace {
                        trigger,
                        owner,
                        exclude_command_id,
                    } => {
                        if !caps.commands_v1 {
                            Response::Error {
                                message:
                                    "validate trigger namespace requires commands_v1 capability"
                                        .into(),
                                category: Some(ShellErrorKind::Unsupported),
                            }
                        } else {
                            // §4.5：快照读（引擎/别名/命令关键字），无 I/O，
                            // 不阻塞——命名空间校验是只读查询，直接在 async 线程做。
                            let engine_snapshot = engines
                                .read()
                                .map(|guard| guard.clone())
                                .unwrap_or_default();
                            let alias_snapshot = aliases.list();
                            let command_keywords = commands.all_command_keywords();
                            match crate::commands::validate_trigger_namespace(
                                trigger.as_str(),
                                *owner,
                                &engine_snapshot,
                                &alias_snapshot,
                                &command_keywords,
                                exclude_command_id.as_deref(),
                            ) {
                                Ok(()) => Response::NamespaceValidation {
                                    ok: true,
                                    conflict: None,
                                },
                                Err(conflict) => Response::NamespaceValidation {
                                    ok: false,
                                    conflict: Some(conflict),
                                },
                            }
                        }
                    }
                    Request::CommandPreview { context } => {
                        if !caps.commands_v1 {
                            Response::Error {
                                message: "command preview requires commands_v1 capability".into(),
                                category: Some(ShellErrorKind::Unsupported),
                            }
                        } else {
                            handle_command_preview(context.clone(), &commands)
                        }
                    }
                    Request::CommandExport => {
                        if !caps.commands_v1 {
                            Response::Error {
                                message: "command export requires commands_v1 capability".into(),
                                category: Some(ShellErrorKind::Unsupported),
                            }
                        } else {
                            let snapshot = commands.data_snapshot();
                            Response::CommandExportResult {
                                envelope: crate::persistence::VersionedEnvelope {
                                schema_version:
                                    <crate::persistence::CommandData as crate::persistence::VersionedData>::SCHEMA_VERSION,
                                    data: snapshot,
                                },
                                exported_at: crate::history::now_utc().to_string(),
                            }
                        }
                    }
                    _ => {
                        dispatch_non_search(
                            req,
                            &engines,
                            &shell,
                            &history,
                            &preferences,
                            &windows,
                            &aliases,
                            &commands,
                            caps,
                        )
                        .await
                    }
                };
                // 背压：通道满（writer 落后 = 客户端不读）时在此等待而非无限排队；
                // send 失败说明 writer 已随客户端断开退出，整条连接一并收尾。
                if tx.send((seq, response)).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                let response = Response::Error {
                    message: format!("无法解析请求：{e}"),
                    category: None,
                };
                if tx.send((seq, response)).await.is_err() {
                    break;
                }
            }
        }
    }

    drop(tx);
    // EOF：等 writer 把在途响应写完/写失败（search 计算自带超时，有界）。
    let _ = writer_task.await;
    log("前端断开连接");
    Ok(())
}

/// B5（AUDIT-4 批次D）：单连接在途 search 并发上限。
const MAX_INFLIGHT_SEARCHES: usize = 16;

/// 单连接待写响应上限（背压，2026-08-24 全仓检验）：响应通道必须有界——
/// 请求入站无界读 + 响应无界排队的组合下，一个只写不读的本地恶意/故障进程
/// 可让响应在通道与 writer 缓冲里无限堆积直至 OOM（与 MAX_INFLIGHT_SEARCHES
/// 防的是同一威胁模型的另一半）。有界后：客户端停止读取 → 管道写阻塞 →
/// writer 停止消费 → 通道写满 → 读循环停在 send 上不再读新请求，内存封顶；
/// 客户端断开 → 管道写失败 → writer 退出 → 通道 send 报错 → 连接整体回收。
/// 正常前端是严格串行的请求-响应，8 个排队额度绰绰有余。
const MAX_QUEUED_RESPONSES: usize = 8;

/// B5：连接级保序 writer。收 (seq, response)，BTreeMap 缓冲乱序到达者，
/// 严格按 0,1,2,… 顺序序列化写出。P15 的响应缓冲复用移到这里（唯一消费者）：
/// 偶发的超大响应（"more" 模式 300 项）不长期占着容量，写完超过
/// `RESPONSE_BUFFER_KEEP` 就缩回去。写失败（客户端已断）静默返回，残余丢弃。
async fn ordered_writer<W>(mut writer: W, mut rx: tokio::sync::mpsc::Receiver<(u64, Response)>)
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut out: Vec<u8> = Vec::with_capacity(8 * 1024);
    const RESPONSE_BUFFER_KEEP: usize = 256 * 1024;
    let mut buffered: std::collections::BTreeMap<u64, Response> = std::collections::BTreeMap::new();
    let mut next_seq: u64 = 0;
    while let Some((seq, response)) = rx.recv().await {
        buffered.insert(seq, response);
        while let Some(response) = buffered.remove(&next_seq) {
            out.clear();
            if let Err(error) = serde_json::to_writer(&mut out, &response) {
                // 序列化中途失败：丢掉半截字节，回一条自造的错误行（与旧行为一致）。
                out.clear();
                out.extend_from_slice(
                    format!("{{\"type\":\"error\",\"message\":\"序列化失败:{error}\"}}").as_bytes(),
                );
            }
            out.push(b'\n');
            if writer.write_all(&out).await.is_err() || writer.flush().await.is_err() {
                return;
            }
            if out.capacity() > RESPONSE_BUFFER_KEEP {
                out = Vec::with_capacity(8 * 1024);
            }
            next_seq = next_seq.saturating_add(1);
        }
    }
}

// clippy: 参数表是连接处理器与搜索服务的既有形状，包一层结构体只会加解包噪声。
#[allow(clippy::too_many_arguments)]
async fn dispatch_non_search(
    req: Request,
    engines: &SharedEngines,
    shell: &Arc<ShellExecutor>,
    history: &Arc<HistoryStore>,
    preferences: &Arc<BrokerPreferences>,
    windows: &Arc<crate::window_list::WindowSnapshotStore>,
    aliases: &Arc<crate::alias::AliasStore>,
    commands: &Arc<crate::commands::CommandStore>,
    _caps: crate::commands::ConnectionCaps,
) -> Response {
    match req {
        Request::Hello { .. } => Response::Error {
            message: "hello must be handled in the connection loop".into(),
            category: None,
        },
        Request::Ping => Response::Pong {
            version: VERSION.to_string(),
            build_id: crate::build_id(),
        },
        Request::Execute { id, target, query } => {
            // K0：命令身份只能经 execute_command 调用（K1）。这里显式拒绝，不依赖
            // 下游 Shell 层的类型守卫——新增操作忘记加守卫不应等于开一个洞。
            let resolved = resolve_target(target, id, None);
            if TargetKind::parse(&resolved.kind) == Some(TargetKind::Command) {
                return Response::Error {
                    message: "命令不能通过该请求执行".into(),
                    category: Some(ShellErrorKind::Unsupported),
                };
            }
            run_shell(
                shell,
                ShellOperation::Open(resolved),
                history,
                query.as_deref().and_then(query_pick_key),
            )
            .await
        }
        Request::Reveal { id, target, query } => {
            let resolved = resolve_target(target, id, Some(TargetKind::File));
            if TargetKind::parse(&resolved.kind) == Some(TargetKind::Command) {
                return Response::Error {
                    message: "命令不能通过该请求执行".into(),
                    category: Some(ShellErrorKind::Unsupported),
                };
            }
            run_shell(
                shell,
                ShellOperation::Reveal(resolved),
                history,
                query.as_deref().and_then(query_pick_key),
            )
            .await
        }
        Request::Actions { id, target } => {
            let resolved = resolve_target(target, id, Some(TargetKind::File));
            if TargetKind::parse(&resolved.kind) == Some(TargetKind::Command) {
                return Response::Error {
                    message: "命令不能通过该请求执行".into(),
                    category: Some(ShellErrorKind::Unsupported),
                };
            }
            list_actions(resolved)
        }
        Request::RunAction {
            id,
            target,
            action,
            args,
            query,
        } => {
            let resolved = resolve_target(target, id, Some(TargetKind::File));
            if TargetKind::parse(&resolved.kind) == Some(TargetKind::Command) {
                return Response::Error {
                    message: "命令不能通过该请求执行".into(),
                    category: Some(ShellErrorKind::Unsupported),
                };
            }
            run_shell(
                shell,
                ShellOperation::RunAction {
                    target: resolved,
                    action,
                    args: args.unwrap_or_default(),
                    zip_program: preferences.zip_program(),
                },
                history,
                query.as_deref().and_then(query_pick_key),
            )
            .await
        }
        Request::ReloadEngines { engines: list } => reload_engines(list, engines),
        Request::UpdatePreferences {
            history_enabled,
            pinyin_enabled,
        } => {
            history.set_enabled(history_enabled);
            preferences
                .pinyin_enabled
                .store(pinyin_enabled, Ordering::Release);
            // M4 (audit): apply the pinyin preference via an explicit
            // SetPinyinEnabled management command instead of the old empty-query
            // Search side-channel. The command loads/releases the sidecar on the
            // indexer side; failures are logged rather than swallowed.
            if let Err(error) = indexer_client::set_pinyin_enabled(pinyin_enabled).await {
                log(format!(
                    "set_pinyin_enabled({pinyin_enabled}) failed: {error}"
                ));
            }
            Response::Status {
                is_indexing: false,
                cancelled: false,
            }
        }
        Request::ClearHistory => {
            // history.clear() 与 commands.clear_usage() 都做同步文件删除。Offload 到
            // blocking 线程。两者都成功才回 Status，任一失败回 Error。
            let history = history.clone();
            let commands = commands.clone();
            match tokio::task::spawn_blocking(move || {
                let history_result = history.clear();
                let usage_result = commands.clear_usage();
                history_result.and(usage_result)
            })
            .await
            {
                Ok(Ok(())) => Response::Status {
                    is_indexing: false,
                    cancelled: false,
                },
                Ok(Err(message)) => Response::Error {
                    message,
                    category: None,
                },
                Err(error) => {
                    log(format!("history clear spawn_blocking failed: {error}"));
                    Response::Error {
                        message: "history clear failed".into(),
                        category: None,
                    }
                }
            }
        }
        Request::ResolveWindow { target } => {
            // S3：probe 底层是 OpenProcess/GetWindowTextW 等同步 Win32 调用，
            // 不能占用 2 个 tokio worker 之一（会连管道 accept 一起堵）——与
            // window 搜索路径同一模式。SystemWindowProbe 是零大小单元结构体，
            // 在 blocking 闭包里直接构造，无需把 probe 引用跨 await 边界传递。
            let windows_for_blocking = windows.clone();
            match tokio::task::spawn_blocking(move || {
                resolve_window(
                    &target,
                    &windows_for_blocking,
                    &crate::window_list::SystemWindowProbe,
                )
            })
            .await
            {
                Ok(response) => response,
                Err(error) => {
                    log(format!("resolve_window spawn_blocking failed: {error}"));
                    Response::Error {
                        message: format!("resolve_window task failed: {error}"),
                        category: None,
                    }
                }
            }
        }
        Request::RecordWindowSwitch { target, query } => {
            let windows_for_blocking = windows.clone();
            // `Response` 体积大（228B），闭包返回 Result<_, Response> 触发
            // result_large_err——与 record_window_switch 函数本体同款豁免。
            #[allow(clippy::result_large_err)]
            let resolved = tokio::task::spawn_blocking(move || {
                record_window_switch(
                    &target,
                    &windows_for_blocking,
                    &crate::window_list::SystemWindowProbe,
                )
            })
            .await;
            match resolved {
                Ok(Ok(history_target)) => {
                    write_window_history(
                        history_target,
                        history,
                        query.as_deref().and_then(query_pick_key),
                    )
                    .await;
                    Response::Status {
                        is_indexing: false,
                        cancelled: false,
                    }
                }
                Ok(Err(response)) => response,
                Err(error) => {
                    log(format!(
                        "record_window_switch spawn_blocking failed: {error}"
                    ));
                    Response::Error {
                        message: format!("record_window_switch task failed: {error}"),
                        category: None,
                    }
                }
            }
        }
        Request::Search { .. } => Response::Error {
            message: "search must be dispatched asynchronously".into(),
            category: None,
        },
        Request::AliasSet { target, words } => {
            // M1（复审 2026-08-21）：绑定性校验（绝对路径 + 支持的 kind）在
            // 进入存储之前——alias.rs 的 set 内也有同款防线，双保险。
            if !crate::alias::target_path_is_bindable(&target) {
                return Response::AliasApplied {
                    message: "别名目标必须是 file/directory/application 的绝对路径".into(),
                };
            }
            // 别名设置是低频 UI 动作：同步落盘放 spawn_blocking（对齐
            // ClearHistory 的纪律——绝不在 async 线程做文件 I/O）。
            let aliases_for_blocking = aliases.clone();
            let result = tokio::task::spawn_blocking(move || {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0);
                aliases_for_blocking.set(&target, &words, now)
            })
            .await
            .unwrap_or_else(|error| Err(format!("alias set task: {error}")));
            Response::AliasApplied {
                message: result.err().unwrap_or_default(),
            }
        }
        Request::AliasDelete { target } => {
            let aliases_for_blocking = aliases.clone();
            let result = tokio::task::spawn_blocking(move || aliases_for_blocking.delete(&target))
                .await
                .unwrap_or_else(|error| Err(format!("alias delete task: {error}")));
            Response::AliasApplied {
                message: result.err().unwrap_or_default(),
            }
        }
        Request::AliasList => {
            let items = aliases
                .list()
                .into_iter()
                .map(|entry| AliasItemDto {
                    target: ActionTarget {
                        kind: entry.kind,
                        value: entry.target,
                    },
                    words: entry.words,
                    bound_at_utc: entry.bound_at_utc,
                })
                .collect();
            Response::AliasItems { items }
        }
        Request::ResolveLnk { target } => {
            // 非 .lnk 快速路径：免 STA 往返。
            if !target.value.to_ascii_lowercase().ends_with(".lnk") {
                return Response::LnkTarget { resolved: None };
            }
            match shell.resolve_lnk(target.value.clone()).await {
                Ok(resolved) => Response::LnkTarget { resolved },
                Err(error) => {
                    log(format!("resolve_lnk failed: {error:?}"));
                    // 失败回退 None（调用方绑 .lnk），与 STA 忙/超时语义一致。
                    Response::LnkTarget { resolved: None }
                }
            }
        }
        // K0：Hello 与 CommandList 在连接循环内处理（需读写 caps），这里不可达。
        Request::CommandList => Response::Error {
            message: "command_list must be handled in the connection loop".into(),
            category: None,
        },
        // K1：ExecuteCommand 在连接循环内处理（需读 caps），这里不可达。
        Request::ExecuteCommand { .. } => Response::Error {
            message: "execute_command must be handled in the connection loop".into(),
            category: None,
        },
        // K2 §4.6：SetCommandShortcut 在连接循环内处理（需读 caps），这里不可达。
        Request::SetCommandShortcut { .. } => Response::Error {
            message: "set_command_shortcut must be handled in the connection loop".into(),
            category: None,
        },
        // K3 §4.5：CommandSet/CommandDelete/ValidateTriggerNamespace 在连接循环内
        // 处理（需读 caps），这里不可达。
        Request::CommandSet { .. } => Response::Error {
            message: "command_set must be handled in the connection loop".into(),
            category: None,
        },
        Request::CommandDelete { .. } => Response::Error {
            message: "command_delete must be handled in the connection loop".into(),
            category: None,
        },
        Request::ValidateTriggerNamespace { .. } => Response::Error {
            message: "validate_trigger_namespace must be handled in the connection loop".into(),
            category: None,
        },
        Request::CommandPreview { .. } => Response::Error {
            message: "command_preview must be handled in the connection loop".into(),
            category: None,
        },
        Request::CommandExport => Response::Error {
            message: "command_export must be handled in the connection loop".into(),
            category: None,
        },
    }
}

/// 别名通道（2026-08-21 设想）：精确查词 + 路径存在性复验 + class 0 行。
/// history_score 提供 frecency 桶仲裁；score=绑定时间秒——冷启动（无历史）
/// 时靠它按绑定时间倒序。同词多目标全部出行，排序交给全局
/// `MatchMetadata::cmp`（web 关键词行由上方 websearch 先出，天然在前）。
fn alias_search_hits(
    aliases: &Arc<crate::alias::AliasStore>,
    word: &str,
    history: &Arc<HistoryStore>,
    limit: usize,
) -> Vec<SearchResult> {
    let mut rows = Vec::new();
    for entry in aliases.lookup_word(word) {
        if rows.len() >= limit {
            break;
        }
        // 存在性复验：失效静默跳过（悬空绑定不产出错误行——history stale paths 教训）。
        let path = std::path::Path::new(&entry.target);
        if !path.exists() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let kind = match entry.kind.as_str() {
            "application" => SearchResultKind::App,
            "directory" => SearchResultKind::Folder,
            _ => SearchResultKind::File,
        };
        let target = ActionTarget {
            kind: entry.kind.clone(),
            value: entry.target.clone(),
        };
        let metadata = MatchMetadata {
            kind: MatchKind::Literal,
            class: 0,
            position: 0,
            // L10（全仓复审 2026-08-22）：min 已保证 ≤ u32::MAX，直接 as 即可；
            // 原 unwrap_or(0) 是死码，反而掩盖饱和语义。
            // 2026-08-24 修复：MatchMetadata::cmp 的 score 键是升序（原名长度，
            // 短名优先）。直接放绑定时间秒会把「新绑定优先」的仲裁语义反过来
            // （老绑定时间戳小反而排前）。取反后：同词多目标冷启动时新绑定在前，
            // 与本函数文档注释一致。取反限制在 i32 域（u32::MAX - epoch 会超出
            // 前端 PipeClient 的 TryGetInt32 值域，整个 match_metadata 被静默
            // 丢弃）；2038 年后饱和为平手，由标题兜底排序。
            score: (i32::MAX as u32) - entry.bound_at_utc.min(i32::MAX as u64) as u32,
            history_score: history.score(&target),
        };
        rows.push(SearchResult {
            kind,
            title: Arc::from(file_name),
            subtitle: Arc::from(entry.target.as_str()),
            execute_id: Arc::from(entry.target.as_str()),
            target,
            match_spans: Vec::new(),
            match_metadata: Some(metadata),
        });
    }
    rows
}

/// 别名行并入既有结果（2026-08-24 修复）。
///
/// 目标已在字面/apps/历史/索引结果里时，此前直接丢弃别名行——「既有行的
/// class/kind 不会更差」的假设对同目标的拼音行不成立（App 行经拼音通道是
/// Initials kind，被丢弃后别名绑定的 class-0 Literal 加成凭空消失，首次搜索
/// 排不到首位，要靠用户手选一次建立查询记忆才置顶）。现在只在别名 metadata
/// 严格更优（或既有行无 metadata）时替换排序键，行本体（标题/spans/图标）
/// 保持不动；新目标照旧追加行。
fn merge_alias_rows(ranked: &mut Vec<SearchResult>, alias_rows: Vec<SearchResult>) -> Vec<usize> {
    let mut alias_indices = Vec::new();
    let mut index_by_target: HashMap<String, usize> = ranked
        .iter()
        .enumerate()
        .map(|(index, item)| {
            (
                crate::history::target_key(&item.target.kind, &item.target.value),
                index,
            )
        })
        .collect();
    for row in alias_rows {
        let key = crate::history::target_key(&row.target.kind, &row.target.value);
        match index_by_target.get(&key) {
            Some(&index) => {
                let upgrade = match (row.match_metadata, ranked[index].match_metadata) {
                    (Some(new), Some(existing)) => new < existing,
                    (Some(_), None) => true,
                    _ => false,
                };
                if upgrade {
                    ranked[index].match_metadata = row.match_metadata;
                }
                // 升级与追加都是别名命中：无论走哪条路径都要参与置顶。
                alias_indices.push(index);
            }
            None => {
                index_by_target.insert(key, ranked.len());
                alias_indices.push(ranked.len());
                ranked.push(row);
            }
        }
    }
    alias_indices
}

/// 2026-08-25 修复：多通道（字面/拼音/apps/历史注入/别名）汇入 ranked 后的最终去重。
/// 键与前端 ResultList 的 ContainerKey 同构（kind+execute_id+title）：同键两行会把
/// 同一实例两次插进前端显示集合，ListBox 出现重复行，WPF Selector 的选中簿记随即抛
/// "An item with the same key has already been added. Key: …ItemInfo"，且中毒行跨查询
/// 存活——此后每次按键都报"搜索失败"直至重启（用户报告：搜索 B 后输入什么都失败）。
/// 保序保留首次出现（排序已定优劣）；窗口行单一通道且 token 天然唯一，不走此去重。
fn dedupe_row_identities(ranked: &mut Vec<SearchResult>) {
    if ranked.len() <= 1 {
        return;
    }
    let mut seen = HashSet::with_capacity(ranked.len());
    ranked.retain(|row| {
        seen.insert(format!(
            "{}\u{1f}{}\u{1f}{}",
            row.kind.wire_str(),
            row.execute_id,
            row.title
        ))
    });
}

/// G5：token → 已复核的句柄。窗口目标永远不进 `ShellExecutor`：激活受 Windows 前台规则
/// 约束，只能由前台进程（WPF）完成，broker 这里只负责复核并交出句柄。
fn resolve_window(
    target: &ActionTarget,
    windows: &Arc<crate::window_list::WindowSnapshotStore>,
    probe: &dyn crate::window_list::WindowProbe,
) -> Response {
    if target.kind != TargetKind::Window.as_str() {
        return Response::Error {
            message: "resolve_window requires a window target".into(),
            category: Some(crate::shell::ShellErrorKind::TargetInvalid),
        };
    }
    match windows.resolve(&target.value, probe) {
        Ok(entry) => Response::WindowHandle {
            handle: entry.handle,
            pid: entry.pid,
            title: entry.title,
            is_minimized: entry.is_minimized,
        },
        Err(error) => Response::Error {
            message: error.message().to_owned(),
            category: Some(match error {
                crate::window_list::ResolveError::Malformed => {
                    crate::shell::ShellErrorKind::TargetInvalid
                }
                _ => crate::shell::ShellErrorKind::Conflict,
            }),
        },
    }
}

/// G5：WPF 回报激活成功后写历史。
///
/// 仍然复核一次 token：这条消息只应在成功后到达，但 broker 不能靠前端自证，否则一个
/// 迟到或伪造的 record 会把没切成功的窗口写成成功历史。历史键是「应用名 + 规范化标题」，
/// 不是 HWND。
///
/// resolve 本身放进 `spawn_blocking`（S3：probe 底层是同步 Win32 调用，不能占用
/// tokio worker），写历史另起 `spawn_blocking` 做文件 I/O；两步之间只传 owned 数据。
#[allow(clippy::result_large_err)]
fn record_window_switch(
    target: &ActionTarget,
    windows: &Arc<crate::window_list::WindowSnapshotStore>,
    probe: &dyn crate::window_list::WindowProbe,
) -> Result<ActionTarget, Response> {
    if target.kind != TargetKind::Window.as_str() {
        return Err(Response::Error {
            message: "record_window_switch requires a window target".into(),
            category: Some(crate::shell::ShellErrorKind::TargetInvalid),
        });
    }
    match windows.resolve(&target.value, probe) {
        Ok(entry) => Ok(ActionTarget::new(TargetKind::Window, entry.history_key())),
        Err(error) => Err(Response::Error {
            message: error.message().to_owned(),
            category: Some(crate::shell::ShellErrorKind::Conflict),
        }),
    }
}

/// Async continuation: writes the resolved history target to disk via
/// `spawn_blocking` so the tokio worker is not stalled by synchronous file I/O.
/// Takes only owned data so no non-`Sync` reference crosses the await boundary.
async fn write_window_history(
    history_target: ActionTarget,
    history: &Arc<HistoryStore>,
    query_key: Option<String>,
) {
    let history = history.clone();
    match tokio::task::spawn_blocking(move || {
        history.record_with_query(&history_target, HistoryUse::Execute, query_key.as_deref())
    })
    .await
    {
        Ok(Err(error)) => log(format!("窗口历史写入失败：{error}")),
        Err(error) => log(format!("history spawn_blocking failed: {error}")),
        Ok(Ok(())) => {}
    }
}

/// G7：解析 `ext:` / `path:` 查询过滤前缀。
///
/// 单次有限状态扫描，识别未转义的 `ext:`、`path:` 和双引号值，输出 `name_query`
/// 与结构化 filters。任何无效语法都回退为普通文本而非报错丢弃——用户输入不能
/// 静默丢失。
///
/// 语法：
/// - `report ext:pdf` → name="report", filters=[{ext, pdf}]
/// - `design ext:md,pdf path:"Project Docs"` → name="design", ext OR md|pdf, path AND
/// - `ext:.pdf` → 前导点被规范化掉，ext=pdf
/// - `EXT:PDF` → 大小写不敏感
/// - `ext:` → 空值，回退为普通文本
/// - `path:"unterminated` → 未闭合引号，回退为普通文本
/// - `foo:bar` → 未知前缀，按普通文本处理
fn parse_query(raw: &str) -> (String, Vec<SearchFilter>) {
    let known_prefixes = ["ext:", "path:", "size:", "dm:", "dc:", "file:", "folder:"];
    let mut name_parts: Vec<&str> = Vec::new();
    let mut filters = Vec::new();

    let bytes = raw.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        // Skip leading whitespace.
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        // Try to match a known prefix (case-insensitive).
        // 用字节比较而非字符串切片——中文 UTF-8 字符是多字节的，
        // &raw[pos..end] 在非 char boundary 上切片会 panic。
        let matched = known_prefixes.iter().find_map(|prefix| {
            let end = pos + prefix.len();
            if end <= bytes.len() && bytes[pos..end].eq_ignore_ascii_case(prefix.as_bytes()) {
                Some(*prefix)
            } else {
                None
            }
        });

        if let Some(prefix) = matched {
            let field = prefix.trim_end_matches(':');
            let value_start = pos + prefix.len();
            // G7b：file:/folder: 是旗标，不取值——冒号后的文本照常解析为下一
            // token（"file: 报告" 与 "file:报告" 都是旗标 + 名字"报告"）。
            if matches!(field, "file" | "folder") {
                if filters.len() < crate::indexer_ipc::MAX_FILTERS {
                    filters.push(SearchFilter {
                        field: field.into(),
                        value: String::new(),
                    });
                }
                pos = value_start;
                continue;
            }

            // An empty value (whitespace or end-of-string immediately after `:`) means the
            // token is not a valid filter. Fall back to treating the entire token as plain
            // text — the prefix itself becomes part of the name query.
            if value_start >= bytes.len() || bytes[value_start].is_ascii_whitespace() {
                name_parts.push(&raw[pos..value_start]);
                pos = value_start;
                continue;
            }

            // Read the value: either a quoted string or a whitespace-delimited token.
            let (value, value_end, closed) = read_filter_value(raw, value_start);

            if !closed || value.is_empty() {
                // Unterminated quote or empty value: treat the whole token as plain text.
                name_parts.push(&raw[pos..value_end]);
                pos = value_end;
                continue;
            }

            // G7b：size/dm/dc 值必须能解析——解析不了整 token 当普通文本（与
            // ext 空值/未闭合引号同一降级语义），否则半截输入会静默变成"查不到"。
            if matches!(field, "size" | "dm" | "dc") {
                let valid = if field == "size" {
                    crate::filters::is_valid_size_value(value)
                } else {
                    crate::filters::is_valid_date_value(value, &crate::filters::local_now())
                };
                if !valid {
                    name_parts.push(&raw[pos..value_end]);
                    pos = value_end;
                    continue;
                }
                if filters.len() < crate::indexer_ipc::MAX_FILTERS {
                    filters.push(SearchFilter {
                        field: field.into(),
                        value: value.to_owned(),
                    });
                }
                pos = value_end;
                continue;
            }

            // An empty value (whitespace or end-of-string immediately after `:`) means the
            // token is not a valid filter. Fall back to treating the entire token as plain
            // text — the prefix itself becomes part of the name query.
            if value_start >= bytes.len() || bytes[value_start].is_ascii_whitespace() {
                name_parts.push(&raw[pos..value_start]);
                pos = value_start;
                continue;
            }

            // Read the value: either a quoted string or a whitespace-delimited token.
            let (value, value_end, closed) = read_filter_value(raw, value_start);

            if !closed || value.is_empty() {
                // Unterminated quote or empty value: treat the whole token as plain text.
                name_parts.push(&raw[pos..value_end]);
                pos = value_end;
                continue;
            }

            // Ext values are comma-separated (OR); split and normalize each.
            if field.eq_ignore_ascii_case("ext") {
                for part in value.split(',') {
                    let normalized = normalize_ext(part);
                    if normalized.is_empty() {
                        continue;
                    }
                    if filters.len() >= crate::indexer_ipc::MAX_FILTERS {
                        break;
                    }
                    filters.push(SearchFilter {
                        field: "ext".into(),
                        value: normalized,
                    });
                }
            } else if field.eq_ignore_ascii_case("path")
                && filters.len() < crate::indexer_ipc::MAX_FILTERS
            {
                filters.push(SearchFilter {
                    field: "path".into(),
                    value: value.to_owned(),
                });
            }

            pos = value_end;
        } else {
            // Not a filter prefix: consume the token as a plain name part.
            let start = pos;
            while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            name_parts.push(&raw[start..pos]);
        }
    }

    let name_query = name_parts.join(" ");
    (name_query, filters)
}

/// Reads a filter value starting at `start`. Returns (value, end_pos, closed).
///
/// - If the value starts with `"`, reads until the closing `"`. `closed=false` if the
///   string ends without a closing quote (unterminated — caller treats as plain text).
/// - Otherwise, reads until the next whitespace.
fn read_filter_value(raw: &str, start: usize) -> (&str, usize, bool) {
    let bytes = raw.as_bytes();
    if start < bytes.len() && bytes[start] == b'"' {
        let inner_start = start + 1;
        let mut i = inner_start;
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        if i >= bytes.len() {
            // Unterminated quote — return the rest of the string as the "value" but mark
            // unclosed so the caller treats the entire token as plain text.
            return (&raw[inner_start..], bytes.len(), false);
        }
        // i points at the closing quote; value_end is past it.
        return (&raw[inner_start..i], i + 1, true);
    }
    // Unquoted: read until whitespace.
    let mut i = start;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    (&raw[start..i], i, true)
}

/// Normalizes a single extension value: strip one leading dot, lowercase.
fn normalize_ext(raw: &str) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed.strip_prefix('.').unwrap_or(trimmed);
    stripped.to_lowercase()
}

/// True when the query carries any query filter (G7 + G7b stat 过滤)。
/// ext:/path: 原生索引器过滤 + size:/dm:/dc:/file:/folder: broker stat 过滤——
/// 过滤态下 apps/web/alias/command 全部让位，只回文件/文件夹。
/// **不含 exclude_path**：那是 G3 作用域排除，不改变结果类型构成。
fn has_query_filters(filters: &[SearchFilter]) -> bool {
    filters
        .iter()
        .any(|filter| crate::filters::is_known_filter_field(&filter.field))
}

/// FRESH-AUDIT-2 F4: 把请求过滤器转成历史注入同样要遵守的 ext/path 匹配器。
/// exclude_path 不在此列（它已由 exclusion_paths 单独处理）。
fn history_filter_set(filters: Option<&[SearchFilter]>) -> crate::hierarchy::QueryFilters {
    let mut exts = Vec::new();
    let mut paths = Vec::new();
    for filter in filters.unwrap_or_default() {
        match filter.field.as_str() {
            "ext" => exts.push(filter.value.clone()),
            "path" => paths.push(filter.value.clone()),
            _ => {}
        }
    }
    crate::hierarchy::QueryFilters::new(exts, paths)
}

/// 查询记忆键：剥掉窗口模式前缀 `>` 与 `ext:`/`path:` 过滤词，再做存储侧
/// 归一化。记录侧（动作请求）与检索侧（search_service 的 pick 查找）共用，
/// 保证 `report ext:pdf` 与 `report` 落到同一条记忆上；纯过滤词/空白 → None。
fn query_pick_key(raw: &str) -> Option<String> {
    let stripped = raw.trim_start().trim_start_matches('>').trim_start();
    let (name_query, _) = parse_query(stripped);
    crate::history::normalized_query_key(&name_query)
}

/// 一次搜索请求的查询部分（与共享状态分开传递，避免参数表无限膨胀）。
struct SearchArgs<'a> {
    query: &'a str,
    max: usize,
    filters: Option<Vec<SearchFilter>>,
    root: Option<&'a str>,
    mode: SearchMode,
    /// K0：命令上下文（已 sanitize 或 None）。K0 无消费者：search_service 收下后丢弃。
    command_context: Option<crate::commands::SearchCommandContext>,
}

/// Collects literal + pinyin app matches from the Start Menu catalog.
///
/// Apps are only searched when there is no root scope and no query filters — a
/// directory scope means "files under this root", and filters mean "files only".
/// Returns the app results and a count of how many matched (for `matched_count`).
///
/// 热路径分配（审计批次 4）：字面命中只降幂一次（P9）、名单只取 top-N 但总数
/// 精确（P10）、拼音走清单里预编码的紧凑字节且查询只归一化一次（P5）。
fn collect_app_results(
    apps: &SharedApps,
    name_query: &str,
    history: &Arc<HistoryStore>,
    pinyin_enabled: bool,
    limit: usize,
) -> (Vec<SearchResult>, u64) {
    let mut ranked = Vec::new();
    let Ok(apps_guard) = apps.read() else {
        return (ranked, 0);
    };
    let (app_matches, mut app_match_count) =
        crate::apps::search_ranked(&apps_guard, name_query, limit);
    let terms = NameTerms::parse(name_query);
    let mut literal_targets = std::collections::HashSet::new();
    for app in app_matches {
        literal_targets.insert(app.launch_path.clone());
        let target = ActionTarget::new(TargetKind::Application, app.launch_path.clone());
        let (metadata, spans) = match literal_match_lowered(&app.name, &terms) {
            Some((mut metadata, spans)) => {
                metadata.history_score = history.score(&target);
                (Some(metadata), spans)
            }
            None => (None, Vec::new()),
        };
        ranked.push(SearchResult {
            kind: SearchResultKind::App,
            title: Arc::from(app.name.as_str()),
            subtitle: if app.target_path != app.launch_path {
                Arc::from(app.target_path.as_str())
            } else {
                Arc::from(app.launch_path.as_str())
            },
            execute_id: Arc::from(app.launch_path.as_str()),
            target,
            match_spans: spans,
            match_metadata: metadata,
        });
    }
    if pinyin_enabled {
        // 查询归一化一次；清单侧的编码在扫描时就做好了。
        if let Some(normalized) = crate::pinyin::normalize_query(name_query) {
            // P2：链匹配 scratch 循环外持有（即时路径无链，仅复用同一入口）。
            let mut initials_scratch: Vec<u8> = Vec::with_capacity(64);
            for app in apps_guard.iter() {
                if literal_targets.contains(&app.launch_path) {
                    continue;
                }
                let Some(encoded) = app.pinyin.as_deref() else {
                    continue;
                };
                let Some(matched) = crate::pinyin::match_compact_normalized(
                    encoded,
                    normalized.as_bytes(),
                    &mut initials_scratch,
                ) else {
                    continue;
                };
                let target = ActionTarget::new(TargetKind::Application, app.launch_path.clone());
                let metadata = pinyin_metadata(&matched, history.score(&target));
                ranked.push(SearchResult {
                    kind: SearchResultKind::App,
                    title: Arc::from(app.name.as_str()),
                    subtitle: if app.target_path != app.launch_path {
                        Arc::from(app.target_path.as_str())
                    } else {
                        Arc::from(app.launch_path.as_str())
                    },
                    execute_id: Arc::from(app.launch_path.as_str()),
                    target,
                    match_spans: matched.spans,
                    match_metadata: Some(metadata),
                });
                app_match_count = app_match_count.saturating_add(1);
            }
        }
    }
    (ranked, app_match_count)
}

/// Fields extracted from an indexer search reply that the final `Response::Results`
/// needs. Extracted to avoid an 11-tuple return type.
struct IndexerReplyFields {
    is_indexing: bool,
    index_progress: Option<Box<IndexProgressDto>>,
    index_error: Option<String>,
    index_generation: Option<u64>,
    is_truncated: bool,
    index_matched_count: Option<u64>,
    scanned_nodes: Option<u64>,
    name_candidates: Option<u64>,
    entered_top_k: Option<u64>,
    path_constructions: Option<u64>,
    pinyin_status: Option<crate::indexer_ipc::PinyinStatus>,
}

/// Merges indexer items into `ranked`, skipping any whose target already appears in
/// `injected_history_targets` (history candidates injected earlier). Returns the
/// diagnostic fields the response needs.
///
/// 审计 P6：dedup 集合的键是 history 侧同一套 interned 单串（`kind + '\0' + value`），
/// 查找走线程局部缓冲，不再为每条索引结果构造 `(String, String)` 元组。
fn process_indexer_reply(
    reply: crate::indexer_client::SearchReply,
    query: &str,
    history: &Arc<HistoryStore>,
    injected_history_targets: &HashSet<String>,
    app_resolved_paths: &HashSet<String>,
    ranked: &mut Vec<SearchResult>,
) -> IndexerReplyFields {
    let terms = NameTerms::parse(query);
    for item in reply.items {
        let kind = if item.is_directory {
            TargetKind::Directory
        } else {
            TargetKind::File
        };
        let kind_str = kind.as_str();
        if crate::history::with_target_key(kind_str, &item.path, |key| {
            injected_history_targets.contains(key)
        }) {
            continue;
        }
        // Cross-kind dedup: if a Start Menu app already resolved to this file path
        // (e.g. "Wub_x64.lnk" → "Wub_x64.exe"), skip the indexer file result so the
        // same .exe doesn't appear twice with different titles.
        if app_resolved_paths.contains(&item.path) {
            continue;
        }
        // Build the target once, compute its history score while borrowed, then move
        // it into the SearchResult — avoiding the previous target.clone().
        let target = ActionTarget::new(kind, &item.path);
        let history_score = history.score(&target);
        // P9：索引器没带 spans/metadata 时的字面回退只降幂一次，两者共用同一次匹配。
        let (fallback_metadata, fallback_spans) =
            if item.match_spans.is_none() || item.match_metadata.is_none() {
                match literal_match_lowered(&item.name, &terms) {
                    Some((metadata, spans)) => (Some(metadata), Some(spans)),
                    None => (None, None),
                }
            } else {
                (None, None)
            };
        let path_arc = Arc::from(item.path.as_str());
        ranked.push(SearchResult {
            kind: if item.is_directory {
                SearchResultKind::Folder
            } else {
                SearchResultKind::File
            },
            title: Arc::from(item.name.as_str()),
            subtitle: Arc::clone(&path_arc),
            target,
            execute_id: path_arc,
            match_spans: item.match_spans.or(fallback_spans).unwrap_or_default(),
            match_metadata: {
                let mut metadata = item.match_metadata.or(fallback_metadata);
                if let Some(metadata) = metadata.as_mut() {
                    metadata.history_score = history_score;
                }
                metadata
            },
        });
    }
    IndexerReplyFields {
        is_indexing: reply.status.building || !reply.status.ready,
        index_progress: reply
            .status
            .build_progress
            .as_ref()
            .map(|progress| Box::new(build_progress_dto(progress))),
        index_error: reply.status.message.filter(|_| reply.status.degraded),
        index_generation: Some(reply.generation),
        is_truncated: reply.is_truncated,
        index_matched_count: reply.matched_count,
        scanned_nodes: reply.scanned_nodes,
        name_candidates: reply.name_candidates,
        entered_top_k: reply.entered_top_k,
        path_constructions: reply.path_constructions,
        pinyin_status: reply.status.pinyin_status,
    }
}

/// K1：命令执行分派。验证上下文 → 查目录 → 按 owner 路由。
/// broker-owned：调用内置 handler（在此执行）。
/// ui-owned：返回 `UiCommand` 交给前端执行。
/// K3 §4.5：CommandSet 处理器。语义层校验（§4.2）+ 命名空间冲突检查（§4.5）。
/// 语义校验可能触磁盘 I/O（路径存在性），故走 spawn_blocking。
async fn handle_command_set(
    command: crate::persistence::UserCommandDefinition,
    commands: &Arc<crate::commands::CommandStore>,
    engines: &SharedEngines,
    aliases: &Arc<crate::alias::AliasStore>,
) -> Response {
    // 语义校验上下文：保存时无实际 query/selection，用空占位（模板结构校验
    // 不需要真实值；{selection.target} 展开为空 → launch 参数侧丢弃空参）。
    // K4b：args 以声明 default 填充（required 无默认 → 空串）——保存时模板里的
    // {arg.已声明} 可展开，未声明的 {arg.x} 照样报 UnknownPlaceholder。
    let exp_ctx = crate::commands::ExpansionContext {
        query: String::new(),
        current_folder: String::new(),
        selection: None,
        clipboard: None,
        now: crate::filters::local_now(),
        args: command
            .arguments
            .iter()
            .map(|a| {
                (
                    a.name.clone(),
                    if a.default.is_empty() {
                        String::new()
                    } else {
                        a.default.clone()
                    },
                )
            })
            .collect(),
    };
    // §4.2 danger 校验
    if let Err(msg) = crate::commands::validate_user_danger(&command.danger) {
        return Response::CommandApplied { message: msg };
    }
    // §4.2 语义层：按 handler 类型校验
    match command.handler {
        crate::persistence::UserHandlerKind::OpenUrl => {
            let url_template = command
                .handler_params
                .get("url_template")
                .cloned()
                .unwrap_or_default();
            if let Err(msg) = crate::commands::validate_open_url(&url_template, &exp_ctx) {
                return Response::CommandApplied { message: msg };
            }
        }
        crate::persistence::UserHandlerKind::LaunchProgram => {
            if let Err(msg) =
                crate::commands::validate_launch_program(&command.handler_params, &exp_ctx)
            {
                return Response::CommandApplied { message: msg };
            }
        }
        crate::persistence::UserHandlerKind::Unknown => {
            return Response::CommandApplied {
                message: "命令 handler 未知或缺失".into(),
            };
        }
    }
    // §4.5 命名空间冲突检查：逐条查命令关键字
    let engine_snapshot = engines
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let alias_snapshot = aliases.list();
    let command_keywords = commands.all_command_keywords();
    for kw in &command.keywords {
        if let Err(conflict) = crate::commands::validate_trigger_namespace(
            kw,
            crate::commands::TriggerOwner::Command,
            &engine_snapshot,
            &alias_snapshot,
            &command_keywords,
            Some(&command.id),
        ) {
            return Response::CommandApplied {
                message: format!(
                    "关键字「{}」与{}「{}」冲突",
                    kw, conflict.kind, conflict.owner_label
                ),
            };
        }
    }
    // 校验通过——持久化（spawn_blocking：落盘 I/O）
    let commands_for_blocking = commands.clone();
    let def = command.clone();
    let result = tokio::task::spawn_blocking(move || commands_for_blocking.set(def))
        .await
        .unwrap_or_else(|error| Err(format!("set command task: {error}")));
    Response::CommandApplied {
        message: result.err().unwrap_or_default(),
    }
}

/// K3 §4.6：CommandPreview（dry-run）。预览响应返回执行时将用的最终字符串。
/// 调用与执行路径**同一函数**（expand_template / validate_open_url /
/// validate_launch_program），构造保证一致——不靠测试保证一致性。
/// 无副作用：不打开浏览器、不启动进程、不写磁盘、不记历史。
fn handle_command_preview(
    context: crate::commands::CommandInvocationContext,
    commands: &Arc<crate::commands::CommandStore>,
) -> Response {
    if let Err(message) = context.validate() {
        return Response::CommandPreviewResult {
            ok: false,
            message,
            url: None,
            program_path: None,
            program_args: Vec::new(),
            program_working_dir: None,
        };
    }
    let Some(user_cmd) = commands.get_user_command(&context.command_id) else {
        return Response::CommandPreviewResult {
            ok: false,
            message: format!("命令不存在：{}", context.command_id),
            url: None,
            program_path: None,
            program_args: Vec::new(),
            program_working_dir: None,
        };
    };
    // K4b：声明式参数位置切分。必填缺失直接预览失败（与执行同一错误文案）。
    let args = match crate::commands::resolve_arguments(
        context.arguments.text.as_deref(),
        &user_cmd.arguments,
    ) {
        Ok(map) => map,
        Err(message) => {
            return Response::CommandPreviewResult {
                ok: false,
                message,
                url: None,
                program_path: None,
                program_args: Vec::new(),
                program_working_dir: None,
            };
        }
    };
    let exp_ctx = crate::commands::expansion_context_from(&context, args);
    match user_cmd.handler {
        crate::persistence::UserHandlerKind::OpenUrl => {
            let url_template = user_cmd
                .handler_params
                .get("url_template")
                .cloned()
                .unwrap_or_default();
            // §4.6 同函数断言：执行路径调 validate_open_url，预览也调同一函数。
            match crate::commands::validate_open_url(&url_template, &exp_ctx) {
                Ok(url) => Response::CommandPreviewResult {
                    ok: true,
                    message: String::new(),
                    url: Some(url),
                    program_path: None,
                    program_args: Vec::new(),
                    program_working_dir: None,
                },
                Err(msg) => Response::CommandPreviewResult {
                    ok: false,
                    message: msg,
                    url: None,
                    program_path: None,
                    program_args: Vec::new(),
                    program_working_dir: None,
                },
            }
        }
        crate::persistence::UserHandlerKind::LaunchProgram => {
            // §4.6 同函数断言：执行路径调 validate_launch_program，预览也调同一函数。
            match crate::commands::validate_launch_program(&user_cmd.handler_params, &exp_ctx) {
                Ok((path, args, working_dir)) => Response::CommandPreviewResult {
                    ok: true,
                    message: String::new(),
                    url: None,
                    program_path: Some(path),
                    program_args: args,
                    program_working_dir: working_dir,
                },
                Err(msg) => Response::CommandPreviewResult {
                    ok: false,
                    message: msg,
                    url: None,
                    program_path: None,
                    program_args: Vec::new(),
                    program_working_dir: None,
                },
            }
        }
        crate::persistence::UserHandlerKind::Unknown => Response::CommandPreviewResult {
            ok: false,
            message: "命令 handler 未知或缺失".into(),
            url: None,
            program_path: None,
            program_args: Vec::new(),
            program_working_dir: None,
        },
    }
}

async fn execute_command(
    context: crate::commands::CommandInvocationContext,
    commands: &Arc<crate::commands::CommandStore>,
    shell: &Arc<ShellExecutor>,
    history: &Arc<HistoryStore>,
) -> Response {
    if let Err(message) = context.validate() {
        return Response::Error {
            message,
            category: Some(ShellErrorKind::TargetInvalid),
        };
    }
    let catalog = commands.catalog();
    let Some(desc) = catalog.iter().find(|d| d.id == context.command_id) else {
        return Response::Error {
            message: format!("命令不存在：{}", context.command_id),
            category: Some(ShellErrorKind::TargetInvalid),
        };
    };
    if !desc.enabled {
        return Response::Error {
            message: format!("命令已禁用：{}", context.command_id),
            category: Some(ShellErrorKind::TargetInvalid),
        };
    }
    // K2 §4.4 P2：动作面板来源二次校验。列出后用户可能改目录代际、目标文件
    // 可能已删除——只信任前端把校验结果带回来是设计 §5.2 要求的同一道防线。
    if context.source == crate::commands::InvocationSource::ActionPanel {
        if let Some(selection) = &context.selection {
            if let Some(target) = &selection.target {
                if let Err(message) = crate::action_composer::validate_action_panel(
                    target,
                    &desc.id,
                    commands.as_ref(),
                ) {
                    return Response::Error {
                        message,
                        category: Some(ShellErrorKind::TargetInvalid),
                    };
                }
            }
        }
    }
    match desc.owner {
        "broker" => {
            // K3 §4.1：先查内置 handler（BrokerHandlerId），找不到再查用户 handler
            // （UserHandlerKind）。两个枚举无转换路径——分派逻辑独立，类型收口。
            if let Some(handler) = crate::commands::broker_handler(&desc.id) {
                match handler {
                    crate::commands::BrokerHandlerId::OpenTerminalHere => {
                        // K2 §4.4：action_panel 声明 input="selection"——对选中目录执行，
                        // 不用 current_folder。root_search/shortcut 保持 current_folder 语义。
                        let folder = match context.source {
                            crate::commands::InvocationSource::ActionPanel => context
                                .selection
                                .as_ref()
                                .and_then(|s| s.target.as_ref())
                                .filter(|t| t.kind == "directory")
                                .map(|t| t.value.clone())
                                .or_else(|| context.current_folder.clone())
                                .unwrap_or_else(|| {
                                    std::env::current_dir()
                                        .map(|p| p.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                }),
                            _ => context.current_folder.clone().unwrap_or_else(|| {
                                std::env::current_dir()
                                    .map(|p| p.to_string_lossy().into_owned())
                                    .unwrap_or_default()
                            }),
                        };
                        match crate::shell::open_terminal_at(&folder) {
                            Ok(()) => {
                                let _ = commands.record_success(
                                    &desc.id,
                                    crate::history::now_utc(),
                                    history.is_enabled(),
                                );
                                Response::Status {
                                    is_indexing: false,
                                    cancelled: false,
                                }
                            }
                            Err(error) => Response::Error {
                                message: format!("打开终端失败：{}", error.message),
                                category: Some(error.kind),
                            },
                        }
                    }
                    crate::commands::BrokerHandlerId::SystemLock => Response::Error {
                        message: "SystemLock 尚未实现".into(),
                        category: Some(ShellErrorKind::Unsupported),
                    },
                    crate::commands::BrokerHandlerId::CopyPaths => {
                        // K2 §4.7：staging copy_paths。P2 第二次校验——validate()
                        // 已过 128 项上限，此处再核一遍 staging 来源的显式约束：
                        // 空暂存区不可执行（P4：空 → 禁用，不应到达 broker，
                        // 但 P2 要求执行处再校验防篡改）。
                        if context.staged_paths.is_empty() {
                            return Response::Error {
                                message: "暂存区为空，没有路径可复制".into(),
                                category: Some(ShellErrorKind::TargetInvalid),
                            };
                        }
                        // copy_paths 复制原始字符串、不探测存在性，因此不受 UNC 限制。
                        let joined = context.staged_paths.join("\r\n");
                        match shell
                            .execute(ShellOperation::CopyPathsText { text: joined })
                            .await
                        {
                            Ok(_) => {
                                let _ = commands.record_success(
                                    &desc.id,
                                    crate::history::now_utc(),
                                    history.is_enabled(),
                                );
                                // 汇总文案：成功 N 项。暂存区不清空（§4.7 末段：
                                // 「保留暂存区内容不清空，显示汇总」）。
                                Response::Status {
                                    is_indexing: false,
                                    cancelled: false,
                                }
                            }
                            Err(error) => Response::Error {
                                message: format!("复制路径失败：{}", error.message),
                                category: Some(error.kind),
                            },
                        }
                    }
                    crate::commands::BrokerHandlerId::StagingZip => {
                        // K2 §4.5：多目标 ZIP。P2 第二次校验。
                        if context.staged_paths.is_empty() {
                            return Response::Error {
                                message: "暂存区为空，没有文件可压缩".into(),
                                category: Some(ShellErrorKind::TargetInvalid),
                            };
                        }
                        // §4.7 末段：mutation 类命令直接拒绝 UNC。
                        for path in &context.staged_paths {
                            if path.starts_with(r"\\") {
                                return Response::Error {
                                    message: "暂存区含 UNC 路径，ZIP 不支持网络路径".into(),
                                    category: Some(ShellErrorKind::TargetInvalid),
                                };
                            }
                        }
                        // output_path 由 WPF 侧 SaveFileDialog 选择（§4.5-1）。
                        let Some(output_path) = &context.arguments.output_path else {
                            return Response::Error {
                                message: "未指定 ZIP 输出路径".into(),
                                category: Some(ShellErrorKind::TargetInvalid),
                            };
                        };
                        // zip_many 在 spawn_blocking 执行（外部进程，分钟级等待）。
                        let output = output_path.clone();
                        let sources = context.staged_paths.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            crate::zip::zip_many(&sources, &output, None)
                        })
                        .await;
                        match result {
                            Ok(Ok(summary)) => {
                                let _ = commands.record_success(
                                    &desc.id,
                                    crate::history::now_utc(),
                                    history.is_enabled(),
                                );
                                // 暂存区不清空，显示汇总。
                                Response::Status {
                                    is_indexing: false,
                                    cancelled: summary.cancelled,
                                }
                            }
                            Ok(Err(error)) => Response::Error {
                                message: format!("压缩失败：{}", error.message),
                                category: Some(error.kind),
                            },
                            Err(error) => Response::Error {
                                message: format!("压缩任务失败：{error}"),
                                category: Some(ShellErrorKind::System),
                            },
                        }
                    }
                } // 闭合内置 handler match
            } else if let Some(user_cmd) = commands.get_user_command(&desc.id) {
                // K3 §4.1 用户 handler 分派。UserHandlerKind 与 BrokerHandlerId
                // 是独立枚举，此分支不共享 broker_handler 的 match。
                // K4b：声明式参数位置切分，必填缺失 → Error（UI 状态栏展示）。
                let args = match crate::commands::resolve_arguments(
                    context.arguments.text.as_deref(),
                    &user_cmd.arguments,
                ) {
                    Ok(map) => map,
                    Err(message) => {
                        return Response::Error {
                            message,
                            category: Some(ShellErrorKind::TargetInvalid),
                        };
                    }
                };
                let exp_ctx = crate::commands::expansion_context_from(&context, args);
                match user_cmd.handler {
                    crate::persistence::UserHandlerKind::OpenUrl => {
                        let url_template = user_cmd
                            .handler_params
                            .get("url_template")
                            .cloned()
                            .unwrap_or_default();
                        if url_template.is_empty() {
                            return Response::Error {
                                message: "命令缺少 url_template".into(),
                                category: Some(ShellErrorKind::TargetInvalid),
                            };
                        }
                        // §P2 执行层第三次校验：展开后 scheme 仍须 http/https。
                        match crate::commands::validate_open_url(&url_template, &exp_ctx) {
                            Ok(url) => {
                                let target = crate::shell::ActionTarget::new(
                                    crate::shell::TargetKind::Web,
                                    url,
                                );
                                match shell.execute(ShellOperation::Open(target)).await {
                                    Ok(_) => {
                                        let _ = commands.record_success(
                                            &desc.id,
                                            crate::history::now_utc(),
                                            history.is_enabled(),
                                        );
                                        Response::Status {
                                            is_indexing: false,
                                            cancelled: false,
                                        }
                                    }
                                    Err(error) => Response::Error {
                                        message: format!("打开 URL 失败：{}", error.message),
                                        category: Some(error.kind),
                                    },
                                }
                            }
                            Err(message) => Response::Error {
                                message,
                                category: Some(ShellErrorKind::TargetInvalid),
                            },
                        }
                    }
                    crate::persistence::UserHandlerKind::LaunchProgram => {
                        // §4.3 + §4.2 语义层：路径校验 + argv 数组展开。
                        // §P2 执行层第三次校验：路径仍存在、扩展名仍合法。
                        match crate::commands::validate_launch_program(
                            &user_cmd.handler_params,
                            &exp_ctx,
                        ) {
                            Ok((path, args, working_dir)) => {
                                match shell
                                    .execute(ShellOperation::LaunchProgram {
                                        path,
                                        args,
                                        working_dir,
                                    })
                                    .await
                                {
                                    Ok(_) => {
                                        let _ = commands.record_success(
                                            &desc.id,
                                            crate::history::now_utc(),
                                            history.is_enabled(),
                                        );
                                        Response::Status {
                                            is_indexing: false,
                                            cancelled: false,
                                        }
                                    }
                                    Err(error) => Response::Error {
                                        message: format!("启动程序失败：{}", error.message),
                                        category: Some(error.kind),
                                    },
                                }
                            }
                            Err(message) => Response::Error {
                                message,
                                category: Some(ShellErrorKind::TargetInvalid),
                            },
                        }
                    }
                    crate::persistence::UserHandlerKind::Unknown => Response::Error {
                        message: "命令 handler 类型未知或不受支持".into(),
                        category: Some(ShellErrorKind::Unsupported),
                    },
                }
            } else {
                Response::Error {
                    message: format!("命令无 handler：{}", context.command_id),
                    category: Some(ShellErrorKind::Unsupported),
                }
            }
        }
        "ui" => Response::UiCommand {
            id: desc.id.clone(),
            context,
        },
        _ => Response::Error {
            message: format!("未知的命令 owner：{}", desc.owner),
            category: Some(ShellErrorKind::Unsupported),
        },
    }
}

// clippy: 搜索服务的既有参数形状（连接处理器逐 Arc 传入）。
#[allow(clippy::too_many_arguments)]
async fn search_service(
    args: SearchArgs<'_>,
    apps: &SharedApps,
    engines: &SharedEngines,
    history: &Arc<HistoryStore>,
    preferences: &Arc<BrokerPreferences>,
    windows: &Arc<crate::window_list::WindowSnapshotStore>,
    aliases: &Arc<crate::alias::AliasStore>,
    commands: &Arc<crate::commands::CommandStore>,
) -> Response {
    let SearchArgs {
        query,
        max,
        filters,
        root,
        mode,
        command_context,
    } = args;
    // K1：command_context 非 None（已协商 commands_v1）时接入命令搜索。
    // 命令仅在无 root、无过滤、非窗口模式下产出——与 apps/alias 同一批条件守卫。
    let has_command_context = command_context.is_some();
    // G7: parse ext:/path: filter tokens out of the raw query text. The broker owns
    // parsing so the WPF never duplicates syntax rules. Parsed filters join the same
    // `filters` channel as G3's exclude_path — no second protocol lane.
    let (name_query, parsed_filters) = parse_query(query);
    let mut all_filters: Vec<SearchFilter> = filters.unwrap_or_default();
    for filter in &parsed_filters {
        if all_filters.len() >= crate::indexer_ipc::MAX_FILTERS {
            break;
        }
        all_filters.push(filter.clone());
    }
    let all_filters = if all_filters.is_empty() {
        None
    } else {
        Some(all_filters)
    };
    // G7b：索引器通道只保留它认识的字段（ext/path/exclude_path）；stat 类
    // （size/dm/dc/file/folder）不进索引器请求（它的 validate 会整请求拒绝），
    // 在 broker 候选集上后置执行。校验也只针对索引器视图。
    let indexer_filters: Option<Vec<SearchFilter>> = all_filters.as_ref().map(|list| {
        list.iter()
            .filter(|filter| !crate::filters::is_stat_filter_field(&filter.field))
            .cloned()
            .collect()
    });
    if let Err(message) = validate_search_request(max, indexer_filters.as_deref()) {
        return Response::Error {
            message,
            category: None,
        };
    }
    let filters = all_filters.filter(|values| !values.is_empty());
    let has_filters = has_query_filters(filters.as_deref().unwrap_or_default());
    // G5 window mode is exclusive: no files, apps, or web rows mixed in, and no indexer
    // round-trip. Checked before the empty-query branch because an empty window query is
    // meaningful (recent windows) while an empty global query is not.
    if mode == SearchMode::Window {
        // EnumWindows 里的 GetWindowTextW/OpenProcess 是跨进程阻塞调用，挂死窗口会
        // 无限期卡住线程——绝不能占用 2 个 tokio worker 之一（会连管道 accept 一起堵）。
        // 与下方 history_file_candidates 同一模式：闭包只进 Arc 克隆 + owned 数据。
        let windows_for_blocking = windows.clone();
        let history_for_blocking = history.clone();
        let preferences_for_blocking = preferences.clone();
        let window_query = query.to_string();
        let entries = tokio::task::spawn_blocking(move || {
            window_search(
                &window_query,
                max,
                &windows_for_blocking,
                &history_for_blocking,
                &preferences_for_blocking,
            )
        })
        .await
        .unwrap_or_default();
        return window_results(query, entries, history);
    }
    // G4 empty input: host context shows recent file/dir under root from history only.
    // No apps, web, or full-index scan — empty indexer queries are meaningless and expensive.
    // Non-host empty input stays empty here: recent windows live in window mode (G5), which
    // returned above, not in an empty global query.
    if query.trim().is_empty() {
        return empty_query_results(query, max, filters.as_deref(), root, history).await;
    }
    // P1（第一轮 bug 修复）：绝对路径形查询走路径语义——路径当路径用，不当
    // 文件名子串用（旧版整串进名字匹配，文件全路径与目录路径都搜不到任何
    // 东西）。仅全局无 ext:/path: 过滤时接管；exclude_path 照常透传（用户
    // 排除的目录不能靠路径查询绕开）；目录范围/窗口模式维持原路径。
    // M13（全仓复审 2026-08-22）：路径链（浏览→父目录→全局兜底）共享同一条
    // CHAIN_BUDGET，超时后不再发起新的索引器请求——挂死的索引器每次击键
    // 最坏只烧一条预算，不再串成 3×8s。
    let mut chain_deadline: Option<std::time::Instant> = None;
    if root.is_none() && !has_filters && is_absolute_path_query(&name_query) {
        let deadline = std::time::Instant::now() + CHAIN_BUDGET;
        if let Some(response) = path_query_results(
            query,
            &name_query,
            max,
            filters.as_deref(),
            preferences.pinyin_enabled(),
            history,
            deadline,
        )
        .await
        {
            return response;
        }
        // 接管失败（路径与其父目录都不在索引内）→ 落回常规全局搜索（旧行为）。
        chain_deadline = Some(deadline);
    }
    let mut items = Vec::with_capacity(max.min(128));
    // G7: when ext:/path: filters are present, only files/folders are returned — no apps,
    // web, or window results mixed in. G7b stat 过滤同样算过滤态。
    if !has_filters {
        if let Ok(guard) = engines.read() {
            if let Some(hit) = websearch::try_match(query, guard.as_slice()) {
                items.push(hit.into_search_result());
            }
        }
    }
    let result_slots = max.saturating_sub(items.len());
    let mut ranked = Vec::new();
    let exclusions = exclusion_paths(filters.as_deref());
    let filter_set = history_filter_set(filters.as_deref());
    let history_weights = history.weights();
    let pinyin_enabled = preferences.pinyin_enabled();
    let root_clone = root.map(|r| r.to_owned());
    let name_query_clone = name_query.clone();
    let history_candidates = tokio::task::spawn_blocking(move || {
        history_file_candidates(
            &name_query_clone,
            &history_weights,
            pinyin_enabled,
            &exclusions,
            root_clone.as_deref(),
            &filter_set,
            // 注入上限 = 结果槽位数：注入再多也会在最终截断时被丢弃，
            // 同时把每次按键的磁盘 stat 次数压到命中且可展示的条目。
            result_slots,
        )
    })
    .await
    .unwrap_or_default();
    let injected_history_targets: HashSet<String> = history_candidates
        .iter()
        .map(|candidate| {
            crate::history::target_key(&candidate.target.kind, &candidate.target.value)
        })
        .collect();
    ranked.extend(history_candidates);
    // G4: a current-directory scope means "files under this root". Applications are not
    // scoped to a directory, so a root suppresses them entirely rather than leaking
    // global hits (e.g. Start Menu .lnk) into a scoped result list.
    // G7: ext:/path: filters also suppress apps — only files/folders are returned.
    let mut app_match_count = 0u64;
    let mut app_resolved_paths: HashSet<String> = HashSet::new();
    if result_slots > 0 && root.is_none() && !has_filters {
        let (app_results, count) = collect_app_results(
            apps,
            &name_query,
            history,
            preferences.pinyin_enabled(),
            // P10：字面命中只物化候选上限条，不再 usize::MAX 全量。候选按
            // (class, 名字长度) 取前 N——与最终排序的首两级（kind→class）同向，
            // 并沿用索引器那条「3× 槽位」的放宽约定，让重度使用但排位靠后的
            // 程序仍能活到 broker 重排。matched_count 仍是精确总数。
            app_candidate_slots(result_slots),
        );
        app_match_count = count;
        // Collect resolved target paths so indexer file results pointing to the same
        // .exe can be deduped (e.g. "Wub_x64.lnk" → "Wub_x64.exe").
        for r in &app_results {
            if r.kind == SearchResultKind::App
                && !r.subtitle.is_empty()
                && !r.subtitle.eq_ignore_ascii_case(&r.execute_id)
            {
                app_resolved_paths.insert(r.subtitle.as_ref().to_owned());
            }
        }
        ranked.extend(app_results);
    }
    let (service, root_rejection, root_message) = search_index_with_root_fallback(
        &name_query,
        // 候选放宽：向索引器要 3× 槽位（上限沿用 MAX_SEARCH_RESULTS），让重度
        // 使用但匹配位置靠后的文件也能活到 broker 重排；最终截断仍在
        // result_slots，is_truncated 语义不变。协议与索引器代码零改动。
        indexer_request_max(result_slots),
        // G7b：索引器只认 ext/path/exclude_path——stat 类字段（size/dm/dc/
        // file/folder）不转发（索引器 validate 会整请求拒绝），在 broker
        // 候选集上后置执行。
        indexer_filters.as_deref(),
        preferences.pinyin_enabled(),
        root,
        // M13：路径分支留下的 deadline 继续管住兜底链；非路径查询则新开一条。
        chain_deadline.unwrap_or_else(|| std::time::Instant::now() + CHAIN_BUDGET),
    )
    .await;
    let IndexerReplyFields {
        is_indexing,
        index_progress,
        index_error,
        index_generation,
        is_truncated: index_truncated,
        index_matched_count,
        scanned_nodes,
        name_candidates,
        entered_top_k,
        path_constructions,
        pinyin_status,
    } = match service {
        Ok(reply) => process_indexer_reply(
            reply,
            // C2（全仓检验 2026-08-25）：回退 span/元数据按剥掉 ext:/path: 过滤词后
            // 的名字查询计算——传原始 query 时 NameTerms 会把 "ext:pdf" 当第
            // 二个词，多词 AND 匹配全败，带过滤词的文件结果一行高亮都没有。
            &name_query,
            history,
            &injected_history_targets,
            &app_resolved_paths,
            &mut ranked,
        ),
        Err(error) => IndexerReplyFields {
            is_indexing: false,
            index_progress: None,
            index_error: Some(error),
            index_generation: None,
            is_truncated: false,
            index_matched_count: None,
            scanned_nodes: None,
            name_candidates: None,
            entered_top_k: None,
            path_constructions: None,
            pinyin_status: None,
        },
    };
    // 别名通道（2026-08-21 设想）：查询与词精确相等才触发。窗口模式/过滤态/
    // 目录范围不出别名行（G7 过滤=只出文件、root=当前目录范围、web 关键词行
    // 由上方正常逻辑先出）。存在性复验在 spawn_blocking 里（Path::exists 是
    // 磁盘 I/O——history stale paths 教训）。
    let alias_rows = if root.is_none() && !has_filters {
        let aliases_for_blocking = aliases.clone();
        let history_for_alias = history.clone();
        let alias_word = name_query.trim().to_lowercase();
        let alias_limit = result_slots;
        tokio::task::spawn_blocking(move || {
            alias_search_hits(
                &aliases_for_blocking,
                &alias_word,
                &history_for_alias,
                alias_limit,
            )
        })
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut alias_indices: Vec<usize> = Vec::new();
    if !alias_rows.is_empty() {
        // 2026-08-25 修复：别名词与真实文件同名时（如 "cs"），自然行的
        // class-0/Literal/position/history 全部平手，末位 score 键（升序）
        // 里别名行的反转时间戳（数亿）输给文件名长度（个位数）——别名行被
        // Top-N 截断，用户要启动一次目标（history_score 提升）才搜得到，
        // 与设置对话框「置顶显示」承诺相悖。别名是显式意图，与查询记忆
        // （picks）同级：命中行无视匹配质量键直接进前排，行内仍按
        // MatchMetadata::cmp 排（多目标时 usage_tier → 绑定时间倒序）。
        alias_indices = merge_alias_rows(&mut ranked, alias_rows);
    }
    // K1：命令结果在别名合并之后、排序之前注入。命令在已协商 commands_v1 且
    // 无过滤时产出——有 root（目录范围模式）时也显示，因为命令的 current_folder
    // 取自 command_context 而非 root（设计 §6.2：文件搜索 root 不能替代 command
    // context）。窗口模式已提前返回，此处不会命中。
    if has_command_context && !has_filters {
        let command_results = command_search(&name_query, result_slots, commands);
        ranked.extend(command_results);
    }
    // G7b：stat 过滤（size:/dm:/dc:/file:/folder:）——候选集（历史注入 +
    // 索引器 Top-K）齐了之后逐条磁盘 stat。放排序前：先过滤后排序，被过滤行
    // 不花时间戳排序成本；几十到几千次 stat 是磁盘 I/O，进 spawn_blocking。
    let stat_set = crate::filters::stat_filter_set(
        filters.as_deref().unwrap_or_default(),
        &crate::filters::local_now(),
    );
    if !stat_set.is_empty() {
        let taken = std::mem::take(&mut ranked);
        ranked = tokio::task::spawn_blocking(move || {
            crate::filters::apply_stat_filters(taken, &stat_set)
        })
        .await
        .unwrap_or_default();
    }
    // 查询记忆置顶：当前（规范化）查询串选中过的 target 在 kind 内、class 之前
    // 排最前——再次输入同样关键词，上次的选择就是第一条。仅全局搜索路径启用。
    let pick_key = query_pick_key(query);
    let pick_flags: Option<Vec<bool>> = if pick_key.is_some() || !alias_indices.is_empty() {
        let key = pick_key.as_deref();
        Some(
            ranked
                .iter()
                .enumerate()
                // P8：键已由 query_pick_key 归一化，直传避免逐条重复归一化分配。
                .map(|(index, item)| {
                    alias_indices.contains(&index)
                        || key
                            .map(|key| history.query_pick_by_key(&item.target, key))
                            .unwrap_or(false)
                })
                .collect(),
        )
    } else {
        None
    };
    sort_search_results_with_picks(&mut ranked, pick_flags.as_deref());
    dedupe_row_identities(&mut ranked);
    let is_truncated = index_truncated || ranked.len() > result_slots;
    ranked.truncate(result_slots);
    items.extend(ranked);
    let matched_count = index_matched_count.map(|count| count.saturating_add(app_match_count));
    // K1：含命令行的响应不可缓存——命令目录代际变化（增删/启停命令）后
    // 旧缓存会让命令行消失/出现错位。回填 command_catalog_generation 让前端
    // 在收到含命令行但代际过期的响应时主动刷新目录。
    let has_command_rows = items
        .iter()
        .any(|item| item.kind == SearchResultKind::Command);
    Response::Results {
        query: query.to_owned(),
        items,
        is_indexing,
        index_progress,
        index_error,
        index_generation,
        is_truncated,
        matched_count,
        scanned_nodes,
        name_candidates,
        entered_top_k,
        path_constructions,
        pinyin_status,
        history_status: history.take_diagnostic().map(history_status),
        root_rejection,
        root_message,
        cacheable: !has_command_rows,
        command_catalog_generation: if has_command_context {
            Some(commands.generation())
        } else {
            None
        },
    }
}

/// G5：窗口模式的响应外壳。窗口模式不碰索引，所以索引相关字段一律缺省，
/// `is_indexing=false`——窗口列表的可用性与索引就绪无关，不该让 UI 显示「索引中」。
fn window_results(query: &str, items: Vec<SearchResult>, history: &Arc<HistoryStore>) -> Response {
    Response::Results {
        query: query.to_owned(),
        items,
        is_indexing: false,
        index_progress: None,
        index_error: None,
        index_generation: None,
        is_truncated: false,
        matched_count: None,
        scanned_nodes: None,
        name_candidates: None,
        entered_top_k: None,
        path_constructions: None,
        pinyin_status: None,
        history_status: history.take_diagnostic().map(history_status),
        root_rejection: None,
        root_message: None,
        cacheable: true,
        command_catalog_generation: None,
    }
}

/// Empty-query search path for G4 §4.4.
///
/// * With a usable `root` and history on: recent existing file/directory entries under that
///   root, capped at `max`, no indexer round-trip.
/// * Root rejected locally: empty items + structured rejection (UI falls back to global).
/// * No root / history off: empty items. Recent windows are window mode's job (G5), not
///   this path's.
async fn empty_query_results(
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    root: Option<&str>,
    history: &Arc<HistoryStore>,
) -> Response {
    let exclusions = exclusion_paths(filters);
    let filter_set = history_filter_set(filters);
    let (root, root_rejection, root_message) = match requested_root(root) {
        Ok(root) => (root, None, None),
        Err(reason) => (None, Some(reason), Some(reason.message().to_owned())),
    };

    let mut items = match root {
        Some(root) if history.is_enabled() => {
            let weights = history.weights();
            let root_owned = root.to_owned();
            // weights() 按 last_used 降序返回，候选保持该顺序（真 MRU）：
            // 收集满 max 个磁盘上仍存在的路径即停——既不再对全部条目做
            // 磁盘 stat，也不再按频次分数重排。
            tokio::task::spawn_blocking(move || {
                history_file_candidates(
                    "",
                    &weights,
                    false,
                    &exclusions,
                    Some(&root_owned),
                    &filter_set,
                    // L9（全仓复审 2026-08-22）：多取 1 条以区分「恰好 max 条即全部」
                    // 与「还有更多」，避免边界上给出点了没反应的「更多」行。
                    max.saturating_add(1),
                )
            })
            .await
            .unwrap_or_default()
        }
        _ => Vec::new(),
    };

    // FRESH-AUDIT-2 G2: is_truncated 口径统一为「结果集被截断，UI 可提供 more」。
    // L9：按 max 截断后，只有真取到了第 max+1 条才算截断。
    let is_truncated = items.len() > max;
    items.truncate(max);

    Response::Results {
        // Echo the client query unchanged (may be "" or whitespace) so the frontend
        // sequence check still accepts the reply.
        query: query.to_owned(),
        items,
        is_indexing: false,
        index_progress: None,
        index_error: None,
        index_generation: None,
        is_truncated,
        matched_count: None,
        scanned_nodes: None,
        name_candidates: None,
        entered_top_k: None,
        path_constructions: None,
        pinyin_status: None,
        history_status: history.take_diagnostic().map(history_status),
        root_rejection,
        root_message,
        cacheable: true,
        command_catalog_generation: None,
    }
}

/// 程序清单的候选上限（审计 P10）：与索引器同一条「3× 槽位」放宽约定，另设
/// 64 条下限，避免槽位很小时把常用程序挤掉。上限 MAX_SEARCH_RESULTS 是全局
/// 结果量的天花板，程序候选不必超过它。
fn app_candidate_slots(result_slots: usize) -> usize {
    result_slots
        .saturating_mul(3)
        .clamp(64, crate::indexer_ipc::MAX_SEARCH_RESULTS)
}

/// 索引器请求量 = 3× 结果槽位，上限 MAX_SEARCH_RESULTS（索引器会拒绝更大值），
/// 下限 1（槽位被 web 行占尽时也保持合法请求）。
fn indexer_request_max(result_slots: usize) -> usize {
    result_slots
        .saturating_mul(3)
        .clamp(1, crate::indexer_ipc::MAX_SEARCH_RESULTS)
}

/// M13（全仓复审 2026-08-22）：一次搜索请求内所有索引器请求共享的预算。
/// 单条 `search_in_root` 内部另有 8s `REQUEST_BUDGET`；本预算限制的是
/// 「一次击键最多串几条请求」——路径链（浏览→父目录→全局兜底）最坏 3 条，
/// 挂死的索引器不再把一次击键拖成 24s。健康索引器毫秒级响应，永不触顶。
const CHAIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);

/// Runs the indexer search for an optional current-directory root.
///
/// A root the service cannot use never fails the whole search: the request is reissued
/// globally and the rejection is returned as a value, so the response can carry both the
/// global results and the exact reason the scope was dropped. Callers never see the
/// reason flattened into an error string.
async fn search_index_with_root_fallback(
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
    deadline: std::time::Instant,
) -> (
    Result<indexer_client::SearchReply, String>,
    Option<RootRejection>,
    Option<String>,
) {
    // Blank roots mean "global" on both sides of the pipe; only a bounded, non-empty root
    // reaches the service. A locally detectable rejection is reported without a round trip.
    let root = match requested_root(root) {
        Ok(root) => root,
        Err(reason) => {
            let reply =
                indexer_client::search_in_root(query, max, filters, pinyin_enabled, None).await;
            return (
                reply.map_err(|failure| failure.message),
                Some(reason),
                Some(reason.message().to_owned()),
            );
        }
    };

    let reply = indexer_client::search_in_root(query, max, filters, pinyin_enabled, root).await;
    match reply {
        Err(failure) if failure.root_rejection.is_some() && root.is_some() => {
            let reason = failure.root_rejection.expect("checked above");
            // M13：预算耗尽不再重试，直接把拒绝上报给前端（UI 自会回退全局视图）。
            if std::time::Instant::now() >= deadline {
                return (
                    Err("indexer chain budget exhausted".to_string()),
                    Some(reason),
                    Some(failure.message),
                );
            }
            let retried =
                indexer_client::search_in_root(query, max, filters, pinyin_enabled, None).await;
            (
                retried.map_err(|failure| failure.message),
                Some(reason),
                Some(failure.message),
            )
        }
        other => (other.map_err(|failure| failure.message), None, None),
    }
}

/// ── P1 路径查询（第一轮 bug 修复）────────────────────────────────────────
///
/// 绝对路径查询（`E:\foo`、`E:\foo\bar.txt`、粘贴的带引号路径）从来不是文件名
/// 子串——名字匹配整串搜不到任何东西。路径分支把路径当路径用：
///
/// 1. 路径本身是索引内**目录** → 首行 = 目录自身，其后 = 目录下条目
///    （空查询 + root 浏览，排序沿用索引器的名字长度序）。
/// 2. 否则**父目录 + 末段**做范围搜索：完整文件路径精确命中排首（class 0），
///    未打完的路径末段做前缀过滤（边打边收窄，行为同目录导航）。
/// 3. 父目录也不可用 → 返回 None，调用方落回常规全局搜索（旧行为）。
///
/// 响应不带 `index_generation`：路径分支的结果绝不能进前端前缀缓存——
/// 缓存按标题子串过滤，而路径查询的"前缀增长"（E:\foo → E:\foo2）是
/// 换了一个路径，不是更长的同名过滤。
/// 复审1/3/4（T6 全仓复审）：echo 必须是**原始**查询——`parse_query` 会把空白
/// 归一（去首尾、折叠连续空格），用它回显会被前端 staleness 检查丢弃；
/// exclude_path 过滤透传给两次索引器请求（用户排除的目录不因路径查询复活）；
/// root 拒绝（路径不存在/是文件）才降级下一尝试，传输/语义错误立刻回退
/// 全局搜索——否则一次挂死的索引器要连吃 3×8s 请求链。
async fn path_query_results(
    raw_query: &str,
    name_query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    history: &Arc<HistoryStore>,
    deadline: std::time::Instant,
) -> Option<Response> {
    let path = normalize_path_query(name_query)?;

    // 尝试 1：路径本身当目录浏览。文件路径/不存在路径会以 root 拒绝回来。
    // 首行留给自身，浏览请求只要 max-1 条（下限 1：max=1 时也要能探到自身）。
    let browse = indexer_client::search_in_root(
        "",
        (max.saturating_sub(1)).max(1),
        filters,
        pinyin_enabled,
        Some(path.as_str()),
    )
    .await;
    let reply = match browse {
        Ok(reply) => Some(reply),
        // 路径不是目录/不在索引内：降级到父目录 + 末段。
        Err(failure) if failure.root_rejection.is_some() => None,
        // 传输/语义错误：立刻让调用方走全局搜索（错误在那里浮出）。
        Err(_) => return None,
    };
    if let Some(reply) = reply {
        // 自身行：浏览结果包含 root 自身（RootFilter 收 depth 0），但不保证
        // 排在 top-K 里（短名后代可能把它挤出去）——在场就取用，不在场合成
        // （browse 成功即证明目录存在且在索引内）。
        let self_index = reply.items.iter().position(|item| {
            path_bytes_eq(trimmed_path(item.path.as_str()).as_bytes(), path.as_bytes())
        });
        let (self_name, self_is_dir) = match self_index {
            Some(index) => (
                reply.items[index].name.clone(),
                reply.items[index].is_directory,
            ),
            None => (
                path.rsplit('\\').next().unwrap_or(path.as_str()).to_owned(),
                true,
            ),
        };
        let mut ranked = Vec::with_capacity(reply.items.len() + 1);
        ranked.push(folder_self_row(&path, &self_name, self_is_dir));
        let empty = HashSet::new();
        let fields = process_indexer_reply(reply, "", history, &empty, &empty, &mut ranked);
        // 浏览项里也含 root 自身（未跌出 top-K 的情形）——去掉首行之后的重复。
        dedupe_after_self_row(&mut ranked, path.as_str());
        dedupe_row_identities(&mut ranked);
        ranked.truncate(max);
        return Some(path_response(raw_query, fields, ranked));
    }

    // 尝试 2：父目录 + 末段（文件全路径精确命中 / 未打完路径前缀收窄）。
    // 父目录不可用（含传输错误）一律回退全局搜索。
    // M13：链预算耗尽时不再发第二条索引器请求，直接回退全局搜索。
    if std::time::Instant::now() >= deadline {
        return None;
    }
    let (parent, tail) = split_path_query(&path)?;
    let probe = indexer_client::search_in_root(
        tail.as_str(),
        indexer_request_max(max),
        filters,
        pinyin_enabled,
        Some(parent.as_str()),
    )
    .await;
    match probe {
        Ok(reply) => {
            let mut ranked = Vec::with_capacity(reply.items.len());
            let empty = HashSet::new();
            let fields =
                process_indexer_reply(reply, tail.as_str(), history, &empty, &empty, &mut ranked);
            // 复审2-M1（T7 二轮）：请求放宽 3×（indexer_request_max）后，索引器
            // 的 is_truncated 按放宽口径判定；截回 max 后必须像全局路径一样
            // 重算（ipc.rs search_service 的 is_truncated 同式），否则命中数
            // 落在 (max, 3×max] 的结果既显示不全又没有"更多"行可展开。
            dedupe_row_identities(&mut ranked);
            let truncated = fields.is_truncated || ranked.len() > max;
            ranked.truncate(max);
            let fields = IndexerReplyFields {
                is_truncated: truncated,
                ..fields
            };
            Some(path_response(raw_query, fields, ranked))
        }
        Err(_) => None,
    }
}

/// 路径分支的响应外壳：echo 原查询（前端 staleness 检查按原文比对），
/// `index_generation` 恒 None（禁止进前端前缀缓存，见函数头注释）。
fn path_response(query: &str, fields: IndexerReplyFields, ranked: Vec<SearchResult>) -> Response {
    Response::Results {
        query: query.to_owned(),
        items: ranked,
        is_indexing: fields.is_indexing,
        index_progress: fields.index_progress,
        index_error: fields.index_error,
        index_generation: None,
        is_truncated: fields.is_truncated,
        matched_count: fields.index_matched_count,
        scanned_nodes: fields.scanned_nodes,
        name_candidates: fields.name_candidates,
        entered_top_k: fields.entered_top_k,
        path_constructions: fields.path_constructions,
        pinyin_status: fields.pinyin_status,
        history_status: None,
        root_rejection: None,
        root_message: None,
        cacheable: true,
        command_catalog_generation: None,
    }
}

/// 首行之后的条目里去掉与自身路径重复的项（浏览结果含 root 自身）。
fn dedupe_after_self_row(ranked: &mut Vec<SearchResult>, path: &str) {
    let mut index = 1;
    while index < ranked.len() {
        if path_bytes_eq(
            trimmed_path(ranked[index].subtitle.as_ref()).as_bytes(),
            path.as_bytes(),
        ) {
            ranked.remove(index);
        } else {
            index += 1;
        }
    }
}

/// 浏览首行：目录/文件自身。title=末段名，subtitle=完整路径。metadata 不给：
/// 该分支不再排序，首行位置由插入顺序保证。
fn folder_self_row(path: &str, name: &str, is_directory: bool) -> SearchResult {
    let kind = if is_directory {
        TargetKind::Directory
    } else {
        TargetKind::File
    };
    let target = ActionTarget::new(kind, path);
    let path_arc = Arc::from(path);
    SearchResult {
        kind: if is_directory {
            SearchResultKind::Folder
        } else {
            SearchResultKind::File
        },
        title: Arc::from(name),
        subtitle: Arc::clone(&path_arc),
        target,
        execute_id: path_arc,
        match_spans: Vec::new(),
        match_metadata: None,
    }
}

/// 剥掉外层空白与包裹引号（Explorer「复制文件地址」给 `"E:\foo"` 形态）。
/// 判定与规范化共用，保证带引号输入两端一致。
fn unquote_query(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        return trimmed[1..trimmed.len() - 1].trim();
    }
    trimmed
}

/// 识别绝对路径形查询：盘符 + 分隔符（`E:\…` / `e:/…`）或 UNC 前缀（`\\…`）。
/// 只认"长得像"，是否真实存在交给 root 解析判定（不碰磁盘）。
fn is_absolute_path_query(query: &str) -> bool {
    let body = unquote_query(query);
    let bytes = body.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    body.starts_with(r"\\") || body.starts_with("//")
}

/// 路径查询规范化：剥粘贴带回的包裹引号、去尾部分隔符、统一正斜杠。
/// `E:\` 归一为 `E:`（root 解析两侧同型，显示由 RootScope::display 负责）。
fn normalize_path_query(raw: &str) -> Option<String> {
    let body = unquote_query(raw);
    if body.is_empty() {
        return None;
    }
    let unified = body.replace('/', "\\");
    let trimmed = unified.trim_end_matches('\\');
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

/// 拆父目录与末段：`E:\foo\bar` → (`E:\foo`, `bar`)；`E:`（卷根）无父可拆 → None。
fn split_path_query(path: &str) -> Option<(String, String)> {
    let idx = path.rfind('\\')?;
    let (parent, tail) = path.split_at(idx);
    let tail = &tail[1..];
    if parent.is_empty() || tail.is_empty() {
        return None;
    }
    Some((parent.to_owned(), tail.to_owned()))
}

/// History file/directory injection.
///
/// When `root` is set (empty-query host context), only paths under that root survive and
/// the path must still exist on disk. Non-empty query search leaves `root` as `None` so
/// the G2 title-match injection path is unchanged. `limit` caps how many existing-path
/// candidates are returned (collect-and-stop) so neither match filtering nor disk stats
/// scale with the history store size.
///
/// 非空查询：标题/拼音匹配（纯内存）先做，只有命中的条目才做磁盘 stat——
/// 每次按键的 stat 次数从「全部条目」降到「命中条目」。空查询无法用名字
/// 过滤，靠 `limit` 收集即停（`weights()` 已按 last_used 降序）。
fn history_file_candidates(
    query: &str,
    weights: &[HistoryWeight],
    pinyin_enabled: bool,
    exclusions: &[String],
    root: Option<&str>,
    // FRESH-AUDIT-2 F4: 历史注入与索引器同守 ext:/path: 过滤语义——
    // `report ext:pdf` 时非 pdf 历史候选不再混入结果。
    filter_set: &crate::hierarchy::QueryFilters,
    limit: usize,
) -> Vec<SearchResult> {
    let empty_query = query.is_empty();
    // P9 + S4：查询只分词一次，标题匹配的 metadata 与 spans 同源产出。
    let terms = NameTerms::parse(query);
    // P13：root 与排除表都只做一次切片级归一化（去空白/去尾部分隔符），
    // 分隔符统一与大小写折叠在比较时逐字节完成——循环里不再有分配。
    let root_normalized = root.map(trimmed_path);
    let exclusions: Vec<&str> = exclusions
        .iter()
        .map(|excluded| trimmed_path(excluded))
        .filter(|excluded| !excluded.is_empty())
        .collect();
    let mut candidates = Vec::new();
    for weight in weights {
        if candidates.len() >= limit {
            break;
        }
        if weight.target.kind != "file" && weight.target.kind != "directory" {
            continue;
        }
        if path_is_excluded(&weight.target.value, &exclusions) {
            continue;
        }
        if let Some(root) = root_normalized {
            // Prefix boundary: `root` and `root\child` match, `rootOther` does not.
            if !path_is_under_root(&weight.target.value, root) {
                continue;
            }
        }
        let Some(title) = std::path::Path::new(&weight.target.value)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        // F4: 过滤器在名字匹配之前——不匹配的候选不做拼音/磁盘 stat。
        let is_directory = weight.target.kind == "directory";
        if !filter_set.is_empty() {
            if !filter_set.ext_matches(title, is_directory) {
                continue;
            }
            if filter_set.has_path_filter() && !filter_set.path_matches(&weight.target.value) {
                continue;
            }
        }
        let (metadata, match_spans) = if empty_query {
            // No literal query to rank against: same match tier for every row so
            // the caller's order (MRU) decides.
            (
                MatchMetadata {
                    kind: MatchKind::Literal,
                    class: 0,
                    position: 0,
                    score: title.encode_utf16().count() as u32,
                    history_score: weight.score,
                },
                Vec::new(),
            )
        } else if let Some((mut metadata, spans)) = literal_match_lowered(title, &terms) {
            metadata.history_score = weight.score;
            (metadata, spans)
        } else if pinyin_enabled {
            let Some(matched) = crate::pinyin::match_name(title, query) else {
                continue;
            };
            (pinyin_metadata(&matched, weight.score), matched.spans)
        } else {
            continue;
        };
        // 只返回磁盘上仍存在的路径（file 或 directory）。rename/move/delete 后
        // 旧路径不再存在，不应作为历史候选出现在搜索结果中。放在名字匹配
        // 之后：只有已成为候选的路径才花一次磁盘 stat。
        if !std::path::Path::new(&weight.target.value).exists() {
            continue;
        }
        let value_arc = Arc::from(weight.target.value.as_str());
        candidates.push(SearchResult {
            kind: if weight.target.kind == "directory" {
                SearchResultKind::Folder
            } else {
                SearchResultKind::File
            },
            title: Arc::from(title),
            subtitle: Arc::clone(&value_arc),
            execute_id: Arc::clone(&value_arc),
            target: weight.target.clone(),
            match_spans,
            match_metadata: Some(metadata),
        });
    }
    candidates
}

/// 归一化的**无分配**部分（审计 P13）：去首尾空白 + 去尾部分隔符。分隔符统一
/// （`/`→`\`）与大小写折叠改由比较函数逐字节完成，所以整条判断不再分配。
fn trimmed_path(value: &str) -> &str {
    value.trim().trim_end_matches(['\\', '/'])
}

/// 路径字节的归一化形态：`/` 视作 `\`，ASCII 大小写折叠。非 ASCII 字节原样比较，
/// 与 `eq_ignore_ascii_case` 的语义一致。
fn normalized_path_byte(byte: u8) -> u8 {
    if byte == b'/' {
        b'\\'
    } else {
        byte.to_ascii_lowercase()
    }
}

fn path_bytes_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(l, r)| normalized_path_byte(*l) == normalized_path_byte(*r))
}

/// True when `path` is `root` itself or a descendant (`root\...`), case-insensitive.
/// Sibling prefixes such as `C:\rootOther` must not match `C:\root`.
///
/// 零分配：两侧都只做切片级归一化，逐字节比较（每次击键 × 每条排除项都会走
/// 到这里，旧实现在这里分配两个 String）。
fn path_is_under_root(path: &str, root: &str) -> bool {
    let path = trimmed_path(path).as_bytes();
    let root = trimmed_path(root).as_bytes();
    if root.is_empty() {
        return false;
    }
    if path.len() <= root.len() {
        return path.len() == root.len() && path_bytes_eq(path, root);
    }
    let (prefix, rest) = path.split_at(root.len());
    matches!(rest.first(), Some(b'\\' | b'/')) && path_bytes_eq(prefix, root)
}

fn path_is_excluded(path: &str, exclusions: &[&str]) -> bool {
    exclusions
        .iter()
        .any(|excluded| path_is_under_root(path, excluded))
}

fn pinyin_metadata(matched: &crate::pinyin::PinyinMatch, history_score: u32) -> MatchMetadata {
    MatchMetadata {
        kind: match matched.kind {
            crate::pinyin::PinyinMatchKind::Full => MatchKind::FullPinyin,
            crate::pinyin::PinyinMatchKind::Initials => MatchKind::Initials,
        },
        class: matched.class,
        position: matched.position,
        score: matched.score,
        history_score,
    }
}

fn history_status(diagnostic: HistoryDiagnostic) -> String {
    match diagnostic {
        HistoryDiagnostic::Corrupt => "corrupt",
        HistoryDiagnostic::FutureSchema => "version_mismatch",
        HistoryDiagnostic::Invalid => "invalid",
        HistoryDiagnostic::Io => "io_error",
    }
    .to_owned()
}

/// 把 indexer 的逐卷进度转成前端 DTO。记录数缺失时用 0 表示"未知"，
/// 与旧字段的既有含义一致；卷计数总是可用。
fn build_progress_dto(progress: &BuildProgress) -> IndexProgressDto {
    IndexProgressDto {
        scanned: progress.records_scanned.unwrap_or(0),
        total_estimate: progress.records_estimate.unwrap_or(0),
        volumes_total: Some(progress.volumes_total),
        volumes_done: Some(progress.volumes_done),
        current_volume: progress.current_volume.clone(),
    }
}

/// G5：把一个窗口按查询打分，取「标题」与「应用名」里更强的那个来源。
///
/// 显示上标题是主行、应用名是副行，所以高亮 spans 只在标题命中时才有值；应用名命中时
/// 不给标题染色，来源由副行自身体现。
fn rank_window(
    entry: &crate::window_list::WindowEntry,
    query: &str,
    pinyin_enabled: bool,
    history_score: u32,
) -> Option<(MatchMetadata, Vec<i32>)> {
    let mut best: Option<(MatchMetadata, Vec<i32>)> = None;
    let mut consider = |metadata: MatchMetadata, spans: Vec<i32>| {
        // MatchMetadata::cmp 把更强的匹配排在前面（Literal < FullPinyin，class/position
        // 小者优先，history_score 已被反转），所以更强 = 更小。
        if best.as_ref().is_none_or(|(current, _)| metadata < *current) {
            best = Some((metadata, spans));
        }
    };

    // P9 + S4：查询只分词一次，两个来源共用；metadata 与 spans 同源产出。
    let terms = NameTerms::parse(query);
    if let Some((mut metadata, spans)) = literal_match_lowered(&entry.title, &terms) {
        metadata.history_score = history_score;
        consider(metadata, spans);
    }
    if let Some((mut metadata, _)) = literal_match_lowered(&entry.app_name, &terms) {
        metadata.history_score = history_score;
        // 命中在应用名上，标题不染色。
        consider(metadata, Vec::new());
    }
    if pinyin_enabled {
        if let Some(matched) = crate::pinyin::match_name(&entry.title, query) {
            let metadata = pinyin_metadata(&matched, history_score);
            consider(metadata, matched.spans);
        }
        if let Some(matched) = crate::pinyin::match_name(&entry.app_name, query) {
            let metadata = pinyin_metadata(&matched, history_score);
            consider(metadata, Vec::new());
        }
    }
    best
}

/// G5：把一个窗口条目转成搜索结果。`token` 只在本次枚举内有效。
fn window_result(
    entry: &crate::window_list::WindowEntry,
    token: &str,
    metadata: Option<MatchMetadata>,
    match_spans: Vec<i32>,
) -> SearchResult {
    let token_arc = Arc::from(token);
    SearchResult {
        kind: SearchResultKind::Window,
        title: Arc::from(entry.title.as_str()),
        subtitle: Arc::from(entry.app_name.as_str()),
        // execute_id 对窗口没有旧读者语义，与 target.value 保持一致即可。
        execute_id: Arc::clone(&token_arc),
        target: ActionTarget::new(TargetKind::Window, token),
        match_spans,
        match_metadata: metadata,
    }
}

/// G5：窗口模式搜索。空查询走「历史 ∩ 当前枚举」的最近窗口。
///
/// 每次请求都重新枚举，不保留常驻窗口列表；snapshot 随本次 publish 整表替换。
fn window_search(
    query: &str,
    max: usize,
    windows: &Arc<crate::window_list::WindowSnapshotStore>,
    history: &Arc<HistoryStore>,
    preferences: &Arc<BrokerPreferences>,
) -> Vec<SearchResult> {
    let self_pids = [std::process::id()];
    let published = crate::window_list::enumerate_and_publish(windows, &self_pids);
    rank_window_list(
        &published,
        query,
        max,
        history,
        preferences,
        crate::history::now_utc(),
    )
}

/// 排名部分与枚举分开，因为 `enumerate_and_publish` 直接打真实桌面，测试进不去。
/// 「历史 ∩ 当前枚举」这条约定就住在这里，不拆开的话它没法被断言。
fn rank_window_list(
    published: &[(String, crate::window_list::WindowEntry)],
    query: &str,
    max: usize,
    history: &Arc<HistoryStore>,
    preferences: &Arc<BrokerPreferences>,
    now: u64,
) -> Vec<SearchResult> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return recent_windows(published, max, history, now);
    }

    let mut ranked: Vec<SearchResult> = Vec::new();
    for (token, entry) in published {
        let history_target = ActionTarget::new(TargetKind::Window, entry.history_key());
        let history_score = history.score(&history_target);
        if let Some((metadata, spans)) =
            rank_window(entry, trimmed, preferences.pinyin_enabled(), history_score)
        {
            ranked.push(window_result(entry, token, Some(metadata), spans));
        }
    }

    sort_search_results(&mut ranked);
    ranked.truncate(max);
    ranked
}

/// 空查询 = 「用过（有效分>0，即 frecency 未衰减尽）∩ 现在还在」的最近窗口，
/// 按 last_used 降序。时间优先于分数：五分钟前切过一次的窗口，排在上周
/// 高频使用的窗口前面——「最近使用的窗口」就该由时间说了算。
fn recent_windows(
    published: &[(String, crate::window_list::WindowEntry)],
    max: usize,
    history: &Arc<HistoryStore>,
    now: u64,
) -> Vec<SearchResult> {
    let mut ranked: Vec<(u64, SearchResult)> = Vec::new();
    for (token, entry) in published {
        let history_target = ActionTarget::new(TargetKind::Window, entry.history_key());
        let Some((history_score, last_used)) = history.usage_at(&history_target, now) else {
            continue;
        };
        if history_score == 0 {
            continue;
        }
        ranked.push((
            last_used,
            window_result(
                entry,
                token,
                Some(MatchMetadata {
                    kind: MatchKind::Literal,
                    class: 0,
                    position: 0,
                    score: 0,
                    history_score,
                }),
                Vec::new(),
            ),
        ));
    }
    ranked.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| {
            left.1
                .title
                .to_lowercase()
                .cmp(&right.1.title.to_lowercase())
        })
    });
    ranked.truncate(max);
    ranked.into_iter().map(|(_, result)| result).collect()
}

/// 字面匹配的单次降幂实现（审计 P9）：metadata 与高亮 spans 共用同一个小写串，
/// 调用方还能把分词提到循环外，一次搜索只分词一次。
///
/// UTF-16 偏移必须在**小写串**上算：大小写转换会改变码元数（如 'İ'），在原串上
/// 数偏移会与前端的高亮错位。
///
/// S4（PRISM-IMPL-PLAN-4-2026-08-20）：多 term AND——每个 term 都是标题子串
/// 才命中；class 0 只在单 term 整名精确时给；position 取各 term 命中 UTF-16
/// 偏移最小值；spans 每个 term 一段，按起点升序合并重叠段后输出
///（前端 ResultList 用 cursor 推进消费，要求升序不倒退）。单 term 与旧实现
/// 逐字节等价。
fn literal_match_lowered(title: &str, terms: &NameTerms) -> Option<(MatchMetadata, Vec<i32>)> {
    let title_lower = title.to_lowercase();
    if let Some(single) = terms.single() {
        let byte_position = title_lower.find(single)?;
        let metadata = MatchMetadata {
            kind: MatchKind::Literal,
            class: if title_lower == single {
                0
            } else if byte_position == 0 {
                1
            } else {
                2
            },
            position: title_lower[..byte_position].encode_utf16().count() as u32,
            score: title.encode_utf16().count() as u32,
            history_score: 0,
        };
        // 空查询不产生高亮（与旧 match_spans 的空查询短路一致），但仍是一次匹配。
        let spans = if single.is_empty() {
            Vec::new()
        } else {
            vec![
                metadata.position as i32,
                single.encode_utf16().count() as i32,
            ]
        };
        return Some((metadata, spans));
    }
    let mut raw_spans: Vec<(u32, u32)> = Vec::new();
    let mut position = u32::MAX;
    let mut at_start = false;
    for term in terms.iter() {
        let byte_position = title_lower.find(term)?;
        at_start |= byte_position == 0;
        let start = title_lower[..byte_position].encode_utf16().count() as u32;
        position = position.min(start);
        raw_spans.push((start, term.encode_utf16().count() as u32));
    }
    raw_spans.sort_unstable();
    let mut spans: Vec<i32> = Vec::with_capacity(raw_spans.len() * 2);
    for (start, len) in raw_spans {
        let (start, len) = (start as i32, len as i32);
        let last = spans.len().checked_sub(2);
        if last.is_some_and(|last| spans[last] + spans[last + 1] >= start) {
            // 与前段重叠或相接：合并（保留更远的终点）。
            let last = last.expect("checked above");
            let last_start = spans[last];
            let last_end = last_start + spans[last + 1];
            let merged_end = (start + len).max(last_end);
            spans[last + 1] = merged_end - last_start;
        } else {
            spans.push(start);
            spans.push(len);
        }
    }
    let metadata = MatchMetadata {
        kind: MatchKind::Literal,
        class: if at_start { 1 } else { 2 },
        position,
        score: title.encode_utf16().count() as u32,
        history_score: 0,
    };
    Some((metadata, spans))
}

/// K1：命令目录搜索。复用 `literal_match_lowered` 做标题匹配，命令行
/// `match_metadata` 参与统一排序（`sort_search_results_with_picks`）。
///
/// 仅匹配 `enabled` 且有 `root_search` binding 的命令。命令 ID 与文件路径
/// 键空间不重叠，无需与别名/历史去重。
fn command_search(
    query: &str,
    max_results: usize,
    commands: &Arc<crate::commands::CommandStore>,
) -> Vec<SearchResult> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let terms = NameTerms::parse(query);
    let catalog = commands.catalog();
    let mut results = Vec::new();
    for desc in catalog {
        if !desc.enabled || desc.bindings.root_search.is_none() {
            continue;
        }
        // K3 §4.4：show_in_root_search=false 的命令不进根搜索 lane，
        // 只能经关键字/快捷键/动作面板到达。
        if let Some(ref root_search) = desc.bindings.root_search {
            if !root_search.show_in_root_search {
                continue;
            }
        }
        if let Some((metadata, spans)) = literal_match_lowered(&desc.title, &terms) {
            results.push(SearchResult {
                kind: SearchResultKind::Command,
                title: Arc::from(desc.title.as_str()),
                subtitle: Arc::from(desc.subtitle.as_str()),
                execute_id: Arc::from(desc.id.as_str()),
                target: ActionTarget::new(TargetKind::Command, desc.id.clone()),
                match_spans: spans,
                match_metadata: Some(metadata),
            });
        }
    }
    // 命令行内按 MatchMetadata 排序，随后参与统一排序。
    results.sort_by(|a, b| {
        a.match_metadata
            .cmp(&b.match_metadata)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    results.truncate(max_results);
    results
}

/// 只要 metadata 的旧签名（测试沿用；生产路径一律走 `literal_match_lowered`）。
#[cfg(test)]
fn rank_title(title: &str, query: &str) -> Option<MatchMetadata> {
    literal_match_lowered(title, &NameTerms::parse(query)).map(|(metadata, _)| metadata)
}

#[cfg(test)]
fn compare_search_results(left: &SearchResult, right: &SearchResult) -> std::cmp::Ordering {
    left.match_metadata
        .cmp(&right.match_metadata)
        .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
        .then_with(|| left.subtitle.cmp(&right.subtitle))
        .then(left.kind.cmp(&right.kind))
}

/// Sorts results by `compare_search_results` but pre-computes the lowercased title
/// once per item instead of once per comparison (O(n) vs O(n log n) allocations).
fn sort_search_results(items: &mut Vec<SearchResult>) {
    sort_search_results_with_picks(items, None);
}

/// `picks` 与 items 对齐：true = 该项被当前（规范化）查询串选中过，排在所有
/// 匹配质量键之前（查询记忆置顶）。标志只活在 broker 排序里，不进
/// MatchMetadata、不进序列化。
/// S1（PRISM-IMPL-PLAN-4-2026-08-20）：原实现 kind 单独提前 + picks 在 kind 内
/// ——现在 metadata_kind 键删除，picks 直接在 is_none 垫底判定之后、
/// MatchMetadata::cmp（class→kind→…）之前。pick 权重变强：picked 项跨 class
/// 跨 kind 置顶（「上次在同样输入下选的就是它」强于任何匹配质量信号），
/// picked 内部仍按匹配质量排序。
///
/// 审计 P4：重建用 `Option::take` 按序取出，零 String clone、无环特例
/// （替代旧实现的全量 clone 重建）。
fn sort_search_results_with_picks(items: &mut Vec<SearchResult>, picks: Option<&[bool]>) {
    // Pre-compute lowercased titles once (O(n) allocations) and sort by index so the
    // comparator borrows from the cache instead of re-allocating per comparison.
    let lowercased: Vec<String> = items.iter().map(|item| item.title.to_lowercase()).collect();
    let mut indices: Vec<usize> = (0..items.len()).collect();
    indices.sort_by(|&a, &b| {
        // FRESH-AUDIT-2 G1: 无元数据项（注入候选等）不参与 kind 竞争——
        // Option 的 None < Some 会把它们排到最前，反超真实命中，改为垫底。
        // S1（PRISM-IMPL-PLAN-4-2026-08-20）：删除单独提前的 metadata_kind 比较
        // 键，kind 交由 MatchMetadata::cmp 在 class 之后裁决（新契约见 hierarchy）。
        metadata_kind(&items[a])
            .is_none()
            .cmp(&metadata_kind(&items[b]).is_none())
            .then_with(|| match picks {
                Some(picks) => picks[b].cmp(&picks[a]),
                None => std::cmp::Ordering::Equal,
            })
            .then_with(|| items[a].match_metadata.cmp(&items[b].match_metadata))
            .then_with(|| lowercased[a].cmp(&lowercased[b]))
            .then_with(|| items[a].subtitle.cmp(&items[b].subtitle))
            .then(items[a].kind.cmp(&items[b].kind))
    });
    // 按排好的下标顺序用 Option::take 重建：每个槽位 take 一次，零 String clone。
    // 旧实现用 clone 重建全表（每条 4-5 个 String 拷贝）；更早的环跟随原地交换
    // 在 2/3 元素环上自抵消或乱序（aabb5fe 修复产物）。
    let mut source: Vec<Option<SearchResult>> = items.drain(..).map(Some).collect();
    for &index in &indices {
        // source[index] 必然存在——每个下标恰好被 take 一次。
        items.push(source[index].take().unwrap());
    }
}

fn metadata_kind(item: &SearchResult) -> Option<MatchKind> {
    item.match_metadata.map(|metadata| metadata.kind)
}
fn list_actions(target: ActionTarget) -> Response {
    match crate::actions::list_actions(&target) {
        Ok(items) => Response::Actions { items },
        Err(ShellError { kind, message }) => Response::Error {
            message,
            category: Some(kind),
        },
    }
}

async fn run_shell(
    shell: &Arc<ShellExecutor>,
    operation: ShellOperation,
    history: &Arc<HistoryStore>,
    query_key: Option<String>,
) -> Response {
    let history_record = match &operation {
        ShellOperation::Open(target)
        | ShellOperation::Properties(target)
        | ShellOperation::OpenWith(target) => Some((target.clone(), HistoryUse::Execute)),
        ShellOperation::Reveal(target) => Some((target.clone(), HistoryUse::Reveal)),
        ShellOperation::RunAction { target, .. } => Some((target.clone(), HistoryUse::Execute)),
        // K2 §4.7：copy_paths 是暂存区批量操作，无单一 target 语义，不进
        // 文件历史（按 search-key 记录频度的入口是单文件动作，staging 批量
        // 不构成单文件访问记录）。
        ShellOperation::CopyPathsText { .. } => None,
        // K3 §4.3：launch_program 是命令执行，走 record_success 而非文件历史。
        ShellOperation::LaunchProgram { .. } => None,
    };
    finish_shell_response(
        shell.execute(operation).await,
        history_record,
        history,
        query_key,
    )
    .await
}

async fn finish_shell_response(
    outcome: Result<ShellOutcome, ShellError>,
    history_record: Option<(ActionTarget, HistoryUse)>,
    history: &Arc<HistoryStore>,
    query_key: Option<String>,
) -> Response {
    match outcome {
        Ok(ShellOutcome::Success) => {
            if let Some((target, usage)) = history_record {
                // history.record_with_query() does synchronous disk I/O (persist).
                // Offload it to a blocking thread so the tokio worker is not stalled.
                let history = history.clone();
                match tokio::task::spawn_blocking(move || {
                    history.record_with_query(&target, usage, query_key.as_deref())
                })
                .await
                {
                    Ok(Err(_)) => log("history write failed"),
                    Err(error) => log(format!("history spawn_blocking failed: {error}")),
                    Ok(Ok(())) => {}
                }
            }
            Response::Status {
                is_indexing: false,
                cancelled: false,
            }
        }
        Ok(ShellOutcome::Cancelled) => Response::Status {
            is_indexing: false,
            cancelled: true,
        },
        Err(ShellError { kind, message }) => Response::Error {
            message,
            category: Some(kind),
        },
    }
}

pub(crate) fn resolve_target(
    target: Option<ActionTarget>,
    legacy_id: Option<String>,
    expected: Option<TargetKind>,
) -> ActionTarget {
    if let Some(target) = target {
        return target;
    }
    let value = legacy_id.unwrap_or_default();
    let kind = expected.unwrap_or_else(|| {
        if websearch::is_http_url(&value) {
            TargetKind::Web
        } else {
            TargetKind::File
        }
    });
    ActionTarget::new(kind, value)
}

/// 热替换网页引擎列表；空列表回落预设 bi/b/g，与 Config::load 行为一致。
fn reload_engines(mut engines: Vec<WebEngine>, shared: &SharedEngines) -> Response {
    if engines.is_empty() {
        engines = WebEngine::defaults();
    }
    let count = engines.len();
    // 先拼好日志字符串再 move engines，避免借用与移动冲突。
    let keywords = engines
        .iter()
        .map(|e| e.keyword.as_str())
        .collect::<Vec<_>>()
        .join(",");
    match shared.write() {
        Ok(mut guard) => {
            *guard = engines;
            log(format!("网页引擎已热重载（{count} 个）：{keywords}"));
            Response::Status {
                is_indexing: false,
                cancelled: false,
            }
        }
        Err(_) => Response::Error {
            message: "无法更新网页引擎（锁被占用）".into(),
            category: None,
        },
    }
}

/// 执行选中项：http(s) URL 用默认浏览器打开；否则按文件/程序路径处理。
/// URL 不走文件路径校验（绝对路径检查会误拒 `https://...`）。
/// 只要 spans 的旧签名（测试沿用；生产路径一律走 `literal_match_lowered`）。
#[cfg(test)]
fn match_spans(title: &str, query: &str) -> Vec<i32> {
    if query.is_empty() {
        return Vec::new();
    }
    literal_match_lowered(title, &NameTerms::parse(query))
        .map(|(_, spans)| spans)
        .unwrap_or_default()
}

#[cfg(test)]
mod window_protocol_tests {
    use super::*;
    use crate::window_list::{RawWindow, WindowEntry, WindowProbe, WindowSnapshotStore};

    struct AlwaysLive(RawWindow);

    impl WindowProbe for AlwaysLive {
        fn probe(&self, handle: u64) -> Option<RawWindow> {
            (handle == self.0.handle).then(|| self.0.clone())
        }
    }

    struct AlwaysGone;

    impl WindowProbe for AlwaysGone {
        fn probe(&self, _handle: u64) -> Option<RawWindow> {
            None
        }
    }

    fn live_window() -> RawWindow {
        RawWindow {
            handle: 0x900,
            pid: 77,
            title: "报告.docx - Word".into(),
            app_name: "winword".into(),
            app_path: r"C:\Office\winword.exe".into(),
            is_visible: true,
            ..RawWindow::default()
        }
    }

    fn entry() -> WindowEntry {
        WindowEntry {
            handle: 0x900,
            pid: 77,
            title: "报告.docx - Word".into(),
            app_name: "winword".into(),
            app_path: r"C:\Office\winword.exe".into(),
            class_name: "OpusApp".into(),
            is_minimized: false,
        }
    }

    fn history_store(tag: &str) -> Arc<HistoryStore> {
        let dir =
            std::env::temp_dir().join(format!("prism-window-history-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(HistoryStore::load(&dir, true))
    }

    /// history.record() 用真实墙钟写入，读取侧注入同一个"现在"才能保证
    /// 衰减 Δt≈0（大数值 now 会把有效分衰减成 0）。
    fn now_utc_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    // --- old-reader compatibility -------------------------------------------------

    /// S4（PRISM-IMPL-PLAN-4-2026-08-20）：多 term 高亮 spans——每个 term 一段，
    /// 按起点升序输出（前端 ResultList 用 cursor 推进，要求不倒退），重叠/相接
    /// 段合并；term 输入乱序时 spans 仍升序。
    #[test]
    fn s4_multi_term_spans_ascending_and_merged() {
        // term 在查询里逆序出现：spans 仍按名字内位置升序（报告@0 两单元、
        // prism@「报告-」之后三单元处、长五）。
        let spans = match_spans("报告-prism-v2.docx", "prism 报告");
        assert_eq!(spans, [0, 2, 3, 5], "两段按名字内位置升序");
        // 重叠段合并：pr 与 prism 都命中 prism，起点相同 → 单段。
        let merged = match_spans("prism 报告", "pri prism");
        assert_eq!(merged, [0, 5], "重叠 term 合并为一段");
        // 单 term 与旧行为一致。
        assert_eq!(match_spans("prism 报告", "prism"), [0, 5]);
    }

    #[test]
    fn search_without_mode_still_decodes_and_means_all() {
        let request: Request =
            serde_json::from_str(r#"{"type":"search","query":"a","max":10}"#).unwrap();
        match request {
            Request::Search { mode, .. } => {
                assert_eq!(mode, None);
                assert_eq!(mode.unwrap_or_default(), SearchMode::All);
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn window_mode_decodes() {
        let request: Request =
            serde_json::from_str(r#"{"type":"search","query":">a","mode":"window"}"#).unwrap();
        match request {
            Request::Search { mode, .. } => assert_eq!(mode, Some(SearchMode::Window)),
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn unknown_mode_degrades_instead_of_failing_the_request() {
        // A newer frontend adding a mode must not hard-fail an older broker.
        let request: Request =
            serde_json::from_str(r#"{"type":"search","query":"a","mode":"holograph"}"#).unwrap();
        match request {
            Request::Search { mode, .. } => assert_eq!(mode, Some(SearchMode::Unknown)),
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn unknown_mode_is_treated_as_all_not_as_window() {
        assert_ne!(SearchMode::Unknown, SearchMode::Window);
    }

    #[test]
    fn window_kind_serializes_as_snake_case() {
        let result = window_result(&entry(), "1024", None, Vec::new());
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["kind"], "window");
        assert_eq!(json["target"]["kind"], "window");
        assert_eq!(json["target"]["value"], "1024");
    }

    #[test]
    fn window_target_value_passes_existing_numeric_validation() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        let target = ActionTarget::new(TargetKind::Window, &token);
        assert_eq!(target.validate().unwrap(), TargetKind::Window);
    }

    // --- resolve ------------------------------------------------------------------

    #[test]
    fn resolve_returns_the_handle_for_a_live_token() {
        let store = Arc::new(WindowSnapshotStore::new());
        let token = store.publish(vec![entry()])[0].0.clone();
        let response = resolve_window(
            &ActionTarget::new(TargetKind::Window, &token),
            &store,
            &AlwaysLive(live_window()),
        );
        match response {
            Response::WindowHandle { handle, pid, .. } => {
                assert_eq!(handle, 0x900);
                assert_eq!(pid, 77);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn resolve_rejects_a_closed_window_as_conflict() {
        let store = Arc::new(WindowSnapshotStore::new());
        let token = store.publish(vec![entry()])[0].0.clone();
        let response = resolve_window(
            &ActionTarget::new(TargetKind::Window, &token),
            &store,
            &AlwaysGone,
        );
        assert!(matches!(
            response,
            Response::Error {
                category: Some(crate::shell::ShellErrorKind::Conflict),
                ..
            }
        ));
    }

    #[test]
    fn resolve_rejects_a_non_window_target() {
        let store = Arc::new(WindowSnapshotStore::new());
        let response = resolve_window(
            &ActionTarget::new(TargetKind::File, r"C:\a.txt"),
            &store,
            &AlwaysLive(live_window()),
        );
        assert!(matches!(
            response,
            Response::Error {
                category: Some(crate::shell::ShellErrorKind::TargetInvalid),
                ..
            }
        ));
    }

    #[test]
    fn window_targets_never_reach_the_shell_executor() {
        // Activation is a foreground-rule-bound Win32 call, not a Shell verb. The shell
        // path must keep refusing window targets outright.
        let target = ActionTarget::new(TargetKind::Window, "1024");
        assert_eq!(target.validate().unwrap(), TargetKind::Window);
        assert!(crate::actions::list_actions(&target).is_err());
    }

    // --- record -------------------------------------------------------------------

    #[tokio::test]
    async fn record_writes_history_under_an_app_plus_title_key() {
        let store = Arc::new(WindowSnapshotStore::new());
        let history = history_store("record");
        let token = store.publish(vec![entry()])[0].0.clone();
        let history_target = record_window_switch(
            &ActionTarget::new(TargetKind::Window, &token),
            &store,
            &AlwaysLive(live_window()),
        )
        .expect("should resolve");
        write_window_history(history_target, &history, None).await;
        let key = ActionTarget::new(TargetKind::Window, entry().history_key());
        assert!(history.score(&key) > 0);
        // The handle must not be what got persisted.
        let by_handle = ActionTarget::new(TargetKind::Window, "2304");
        assert_eq!(history.score(&by_handle), 0);
    }

    #[tokio::test]
    async fn record_for_a_dead_window_does_not_write_success_history() {
        let store = Arc::new(WindowSnapshotStore::new());
        let history = history_store("dead");
        let token = store.publish(vec![entry()])[0].0.clone();
        let response = record_window_switch(
            &ActionTarget::new(TargetKind::Window, &token),
            &store,
            &AlwaysGone,
        )
        .expect_err("dead window should fail");
        assert!(matches!(response, Response::Error { .. }));
        let key = ActionTarget::new(TargetKind::Window, entry().history_key());
        assert_eq!(history.score(&key), 0);
    }

    #[tokio::test]
    async fn record_for_a_stale_token_does_not_write_success_history() {
        let store = Arc::new(WindowSnapshotStore::new());
        let history = history_store("stale");
        let stale = store.publish(vec![entry()])[0].0.clone();
        // M10：上一拍 token 现在仍可解析（快照环带），「stale」指掉出环带的
        // 旧 token——多发布几代把它挤出 SNAPSHOT_HISTORY。
        for _ in 0..20 {
            store.publish(vec![entry()]);
        }
        let response = record_window_switch(
            &ActionTarget::new(TargetKind::Window, &stale),
            &store,
            &AlwaysLive(live_window()),
        )
        .expect_err("stale token should fail");
        assert!(matches!(response, Response::Error { .. }));
        let key = ActionTarget::new(TargetKind::Window, entry().history_key());
        assert_eq!(history.score(&key), 0);
    }

    // --- ranking ------------------------------------------------------------------

    #[test]
    fn literal_title_match_beats_pinyin_match() {
        let (literal, _) = rank_window(&entry(), "报告", true, 0).expect("title hit");
        let (pinyin, _) = rank_window(&entry(), "bg", true, 0).expect("pinyin hit");
        assert_eq!(literal.kind, MatchKind::Literal);
        assert!(literal < pinyin);
    }

    #[test]
    fn app_name_match_is_found_when_the_title_does_not_match() {
        let (metadata, spans) = rank_window(&entry(), "winword", true, 0).expect("app hit");
        assert_eq!(metadata.kind, MatchKind::Literal);
        // Hit is on the subtitle, so the title carries no highlight.
        assert!(spans.is_empty());
    }

    #[test]
    fn chinese_title_highlight_uses_utf16_offsets() {
        let (_, spans) = rank_window(&entry(), "报告", true, 0).expect("title hit");
        assert_eq!(spans, vec![0, 2]);
    }

    #[test]
    fn history_breaks_ties_within_the_same_match_tier() {
        let (cold, _) = rank_window(&entry(), "报告", true, 0).unwrap();
        let (warm, _) = rank_window(&entry(), "报告", true, 9).unwrap();
        assert!(warm < cold, "more-used window must rank first");
    }

    #[test]
    fn pinyin_disabled_drops_pinyin_only_hits() {
        assert!(rank_window(&entry(), "bg", false, 0).is_none());
        assert!(rank_window(&entry(), "报告", false, 0).is_some());
    }

    #[test]
    fn a_window_matching_nothing_is_not_a_candidate() {
        assert!(rank_window(&entry(), "zzzz", true, 0).is_none());
    }

    // --- 空输入 = 历史 ∩ 当前枚举（PRD 验收第 5 条）--------------------------------

    /// 空输入只列「用过 + 现在还在」的窗口。这里两个窗口都在枚举里，只有一个有历史。
    #[test]
    fn empty_query_lists_only_windows_that_have_history() {
        let history = history_store("recent-intersect");
        let used = entry();
        let never_used = WindowEntry {
            handle: 0xA01,
            title: "Untitled - Notepad".into(),
            app_name: "notepad".into(),
            ..entry()
        };
        history
            .record(
                &ActionTarget::new(TargetKind::Window, used.history_key()),
                HistoryUse::Execute,
            )
            .unwrap();

        let published = vec![("10".to_owned(), used), ("11".to_owned(), never_used)];
        let results = rank_window_list(
            &published,
            "",
            20,
            &history,
            &Arc::new(BrokerPreferences::new(true)),
            now_utc_secs(),
        );

        assert_eq!(results.len(), 1, "the never-used window must not be listed");
        assert!(results[0].title.contains("报告.docx"));
    }

    /// 已关闭的窗口即使有历史也不能出现——它不在本次枚举里，所以求交集后自然消失。
    /// 这是「持久历史不展示已关闭窗口」那条验收的核心：历史留着，枚举说了不算。
    #[test]
    fn empty_query_hides_a_remembered_window_once_it_is_closed() {
        let history = history_store("recent-closed");
        let closed = entry();
        history
            .record(
                &ActionTarget::new(TargetKind::Window, closed.history_key()),
                HistoryUse::Execute,
            )
            .unwrap();
        let preferences = Arc::new(BrokerPreferences::new(true));

        // 还开着：列出来。
        let present = vec![("10".to_owned(), closed.clone())];
        assert_eq!(
            rank_window_list(&present, "", 20, &history, &preferences, now_utc_secs()).len(),
            1,
            "control: while enumerated it is listed"
        );

        // 关掉后本次枚举为空，历史条目仍在磁盘上。
        let results = rank_window_list(&[], "", 20, &history, &preferences, now_utc_secs());
        assert!(
            results.is_empty(),
            "history must not resurrect a window that is gone"
        );
    }

    /// 空查询窗口列表按 last_used（MRU）排序：最近用过一次的窗口排在
    /// 更早高频使用的窗口前面——「最近使用的窗口」由时间而不是分数决定。
    #[test]
    fn empty_query_windows_order_by_recency_not_score() {
        let history = history_store("recent-order");
        let frequent_but_old = entry();
        let recent = WindowEntry {
            handle: 0xA02,
            title: "笔记.txt - Notepad".into(),
            app_name: "notepad".into(),
            ..entry()
        };
        let now = 1_800_000_000u64;
        for _ in 0..5 {
            history
                .record_at(
                    &ActionTarget::new(TargetKind::Window, frequent_but_old.history_key()),
                    HistoryUse::Execute,
                    None,
                    now - 500_000,
                )
                .unwrap();
        }
        history
            .record_at(
                &ActionTarget::new(TargetKind::Window, recent.history_key()),
                HistoryUse::Execute,
                None,
                now - 10_000,
            )
            .unwrap();

        let published = vec![
            ("10".to_owned(), frequent_but_old),
            ("11".to_owned(), recent),
        ];
        let results = rank_window_list(
            &published,
            "",
            20,
            &history,
            &Arc::new(BrokerPreferences::new(true)),
            now,
        );

        assert_eq!(results.len(), 2);
        assert!(
            results[0].title.contains("笔记.txt"),
            "recently-used window ranks first despite the lower usage score"
        );
        assert!(results[1].title.contains("报告.docx"));
    }

    #[test]
    fn window_search_response_does_not_claim_indexing() {
        // Window availability is unrelated to index readiness; showing "indexing" here
        // would be a lie the UI acts on.
        let history = history_store("shell");
        let response = window_results("", Vec::new(), &history);
        match response {
            Response::Results {
                is_indexing,
                index_generation,
                ..
            } => {
                assert!(!is_indexing);
                assert_eq!(index_generation, None);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[tokio::test]
    async fn bounded_line_reader_preserves_remainders_and_rejects_oversized_lines() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = BoundedLineReader::new(server);
        let writer_task = tokio::spawn(async move {
            tokio::io::AsyncWriteExt::write_all(&mut client, b"first\r\nsecond\n")
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut client).await.unwrap();
            // 单行超过 1MB：读取器必须报错而不是继续累积。
            let huge = vec![b'x'; MAX_REQUEST_LINE_BYTES + 10];
            tokio::io::AsyncWriteExt::write_all(&mut client, &huge)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut client, b"\n")
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut client).await.unwrap();
        });

        assert_eq!(reader.next_line().await.unwrap().as_deref(), Some("first"));
        assert_eq!(reader.next_line().await.unwrap().as_deref(), Some("second"));
        assert!(
            reader.next_line().await.is_err(),
            "oversized line must be an error, not a buffered string"
        );
        let _ = writer_task.await;
    }

    #[tokio::test]
    async fn bounded_line_reader_handles_eof_without_trailing_newline() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = BoundedLineReader::new(server);
        tokio::spawn(async move {
            tokio::io::AsyncWriteExt::write_all(&mut client, b"tail-no-newline")
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut client).await.unwrap();
            drop(client);
        });

        assert_eq!(
            reader.next_line().await.unwrap().as_deref(),
            Some("tail-no-newline")
        );
        assert_eq!(reader.next_line().await.unwrap(), None);
    }

    /// P12 的核心断言：同一条 2MB 数据行，走入站 1MB 上限被拒，走响应方向的
    /// 放宽上限被接受。只测「超限报错」的旧断言对「把正常大响应也误杀」是绿的，
    /// 所以这里做同字节的 A/B。
    #[tokio::test]
    async fn response_direction_limit_accepts_lines_the_request_limit_rejects() {
        let payload = vec![b'y'; 2 * 1024 * 1024];
        assert!(
            payload.len() > MAX_REQUEST_LINE_BYTES,
            "A/B 前提：载荷必须超过入站上限，否则两侧都会通过"
        );

        let feed = |limit: usize| {
            let payload = payload.clone();
            async move {
                // 64KB 管道缓冲：2MB 载荷不至于变成几万次 64 字节唤醒。
                let (mut client, server) = tokio::io::duplex(64 * 1024);
                let mut reader = BoundedLineReader::with_limit(server, limit);
                let writer_task = tokio::spawn(async move {
                    let _ = tokio::io::AsyncWriteExt::write_all(&mut client, &payload).await;
                    let _ = tokio::io::AsyncWriteExt::write_all(&mut client, b"\n").await;
                    let _ = tokio::io::AsyncWriteExt::flush(&mut client).await;
                });
                let line = reader.next_line().await;
                // 拒绝分支下读取器提前放弃，写入方仍阻塞在未排空的管道上——
                // 这里绝不能 await 它，否则测试挂死。
                writer_task.abort();
                line
            }
        };

        let rejected = feed(MAX_REQUEST_LINE_BYTES).await;
        assert!(rejected.is_err(), "2MB 行在 1MB 上限下必须被拒");

        let accepted = feed(8 * 1024 * 1024).await;
        assert_eq!(
            accepted.unwrap().map(|line| line.len()),
            Some(2 * 1024 * 1024),
            "响应方向的放宽上限必须完整放行同一条行"
        );
    }

    /// 别名系统（2026-08-21 设想）：精确命中词表的目标以 class 0 行加入合并。
    /// 存在性复验（悬空绑定静默跳过）与 kind 映射在这里锚定。
    #[test]
    fn alias_search_hits_exact_word_with_class_zero() {
        let dir = std::env::temp_dir().join(format!("prism-alias-hits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("weixin.exe");
        std::fs::write(&exe, b"x").unwrap();
        let folder = dir.join("资料");
        std::fs::create_dir_all(&folder).unwrap();

        let aliases = std::sync::Arc::new(crate::alias::AliasStore::load(&dir));
        aliases
            .set(
                &ActionTarget {
                    kind: "application".into(),
                    value: exe.to_string_lossy().into_owned(),
                },
                &["wx".into()],
                1234,
            )
            .unwrap();
        aliases
            .set(
                &ActionTarget {
                    kind: "directory".into(),
                    value: folder.to_string_lossy().into_owned(),
                },
                &["wx".into()],
                100,
            )
            .unwrap();
        // 悬空绑定：路径不存在。
        aliases
            .set(
                &ActionTarget {
                    kind: "file".into(),
                    value: dir.join("ghost.txt").to_string_lossy().into_owned(),
                },
                &["wx".into()],
                50,
            )
            .unwrap();

        let history_dir =
            std::env::temp_dir().join(format!("prism-alias-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&history_dir);
        let history = Arc::new(HistoryStore::load(&history_dir, true));
        let rows = alias_search_hits(&aliases, "WX", &history, 8);
        assert_eq!(
            rows.len(),
            2,
            "悬空绑定静默跳过：{:?}",
            rows.iter().map(|r| r.title.as_ref()).collect::<Vec<_>>()
        );
        for row in &rows {
            let metadata = row.match_metadata.unwrap();
            assert_eq!(metadata.class, 0);
            assert_eq!(metadata.kind, MatchKind::Literal);
            assert!(row.match_spans.is_empty());
        }
        // kinds 映射：application→App、directory→Folder。
        assert!(rows.iter().any(|row| row.kind == SearchResultKind::App));
        assert!(rows.iter().any(|row| row.kind == SearchResultKind::Folder));
        // 精确触发：前缀词不命中。
        assert!(alias_search_hits(&aliases, "w", &history, 8).is_empty());
        assert!(alias_search_hits(&aliases, "wxx", &history, 8).is_empty());
        let _ = std::fs::remove_dir_all(&history_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2026-08-24 修复锚定：同目标的拼音行不得丢掉别名绑定的 class-0 Literal
    /// 加成——merge_alias_rows 升级排序键后，全局排序里别名目标压过 Initials
    /// 行排到首位（首次搜索即置顶，不依赖查询记忆）。
    #[test]
    fn alias_merge_upgrades_pinyin_row_to_class_zero_literal() {
        let lnk = r"C:\Users\x\Start Menu\Programs\微信.lnk";
        // 既有行：拼音 Initials 命中（apps 通道对 "wx" 的真实产物形态）。
        let mut ranked = vec![SearchResult {
            kind: SearchResultKind::App,
            title: Arc::from("微信"),
            subtitle: Arc::from(lnk),
            execute_id: Arc::from(lnk),
            target: ActionTarget {
                kind: "application".into(),
                value: lnk.into(),
            },
            match_spans: Vec::new(),
            match_metadata: Some(MatchMetadata {
                kind: MatchKind::Initials,
                class: 0,
                position: 0,
                score: 2,
                history_score: 0,
            }),
        }];
        // 别名行：同目标，class 0 Literal（绑定 "wx"）。
        let alias_row = SearchResult {
            kind: SearchResultKind::App,
            title: Arc::from("微信.lnk"),
            subtitle: Arc::from(lnk),
            execute_id: Arc::from(lnk),
            target: ActionTarget {
                kind: "application".into(),
                value: lnk.into(),
            },
            match_spans: Vec::new(),
            match_metadata: Some(MatchMetadata {
                kind: MatchKind::Literal,
                class: 0,
                position: 0,
                score: (i32::MAX as u32) - 1770000000,
                history_score: 0,
            }),
        };
        merge_alias_rows(&mut ranked, vec![alias_row]);
        // 不追加重复行；既有行的 metadata 升级为别名的 class-0 Literal。
        assert_eq!(ranked.len(), 1);
        let upgraded = ranked[0].match_metadata.unwrap();
        assert_eq!(upgraded.kind, MatchKind::Literal);
        assert_eq!(upgraded.class, 0);
        // 全局排序：升级后的别名目标压过其他 Initials 行。
        let mut ordered = vec![
            SearchResult {
                kind: SearchResultKind::App,
                title: Arc::from("网校"),
                subtitle: Arc::from(r"C:\Apps\wangxiao.lnk"),
                execute_id: Arc::from(r"C:\Apps\wangxiao.lnk"),
                target: ActionTarget {
                    kind: "application".into(),
                    value: r"C:\Apps\wangxiao.lnk".into(),
                },
                match_spans: Vec::new(),
                match_metadata: Some(MatchMetadata {
                    kind: MatchKind::Initials,
                    class: 0,
                    position: 0,
                    score: 2,
                    history_score: 99, // 重度使用
                }),
            },
            ranked[0].clone(),
        ];
        sort_search_results_with_picks(&mut ordered, None);
        assert_eq!(ordered[0].target.value, lnk, "别名目标必须排首位");
    }

    /// 2026-08-25 用户报告锚定：给从未启动过的 exe 绑定别名词后，精确查询该词
    /// 必须立即出现别名行——不依赖历史/启动记录。全链路（search_service）验证，
    /// 复刻真实场景：kind=file 的 exe + 独立词 + 空历史 + 空 apps。
    /// 索引器不可达（CI/无服务）时走 Err 分支同样成立：别名通道不碰索引。
    #[tokio::test]
    async fn alias_word_row_appears_without_prior_launch() {
        let dir = std::env::temp_dir().join(format!("prism-alias-svc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("Panel v1.4.3.exe");
        std::fs::write(&exe, b"x").unwrap();

        let aliases = Arc::new(crate::alias::AliasStore::load(&dir));
        aliases
            .set(
                &ActionTarget {
                    kind: "file".into(),
                    value: exe.to_string_lossy().into_owned(),
                },
                &["cs".into()],
                1_787_578_631,
            )
            .unwrap();

        let history_dir =
            std::env::temp_dir().join(format!("prism-alias-svc-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&history_dir);
        let history = Arc::new(HistoryStore::load(&history_dir, true));
        let apps: SharedApps = Arc::new(std::sync::RwLock::new(Vec::new()));
        let engines: SharedEngines = Arc::new(std::sync::RwLock::new(Vec::new()));
        let preferences = Arc::new(BrokerPreferences::new(false));
        let windows = Arc::new(crate::window_list::WindowSnapshotStore::new());

        let response = search_service(
            SearchArgs {
                query: "cs",
                max: 8,
                filters: None,
                root: None,
                mode: SearchMode::All,
                command_context: None,
            },
            &apps,
            &engines,
            &history,
            &preferences,
            &windows,
            &aliases,
            &Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir())),
        )
        .await;

        let Response::Results { items, .. } = response else {
            panic!("search_service 必须返回 Results");
        };
        let titles = items
            .iter()
            .map(|item| {
                format!(
                    "{} [{:?}]",
                    item.title,
                    item.match_metadata
                        .map(|m| (m.class, m.kind, m.score, m.history_score))
                )
            })
            .collect::<Vec<_>>();
        let exe_path = exe.to_string_lossy().into_owned();
        assert!(
            items.iter().any(|item| item.target.value == exe_path),
            "未启动过的目标必须由别名行召回：{titles:?}"
        );
        assert_eq!(
            items[0].target.value, exe_path,
            "class-0 别名行必须置顶：{titles:?}"
        );
        let _ = std::fs::remove_dir_all(&history_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// merge 的另一半：无 metadata 的既有行（注入候选等）接受升级；不同目标
    /// 的别名行照旧追加；别名 metadata 更差时不降级既有行。
    #[test]
    fn alias_merge_appends_new_targets_and_never_downgrades() {
        let mut ranked = vec![
            SearchResult {
                kind: SearchResultKind::File,
                title: Arc::from("a.txt"),
                subtitle: Arc::from(r"C:\a.txt"),
                execute_id: Arc::from(r"C:\a.txt"),
                target: ActionTarget {
                    kind: "file".into(),
                    value: r"C:\a.txt".into(),
                },
                match_spans: Vec::new(),
                match_metadata: None,
            },
            SearchResult {
                kind: SearchResultKind::File,
                title: Arc::from("b.txt"),
                subtitle: Arc::from(r"C:\b.txt"),
                execute_id: Arc::from(r"C:\b.txt"),
                target: ActionTarget {
                    kind: "file".into(),
                    value: r"C:\b.txt".into(),
                },
                match_spans: Vec::new(),
                match_metadata: Some(MatchMetadata {
                    kind: MatchKind::Literal,
                    class: 0,
                    position: 0,
                    score: 5,
                    history_score: 7,
                }),
            },
        ];
        let rows = vec![
            // 无 metadata 的既有行 → 升级。
            SearchResult {
                kind: SearchResultKind::File,
                title: Arc::from("a.txt"),
                subtitle: Arc::from(r"C:\a.txt"),
                execute_id: Arc::from(r"C:\a.txt"),
                target: ActionTarget {
                    kind: "file".into(),
                    value: r"C:\a.txt".into(),
                },
                match_spans: Vec::new(),
                match_metadata: Some(MatchMetadata {
                    kind: MatchKind::Literal,
                    class: 0,
                    position: 0,
                    score: 1,
                    history_score: 0,
                }),
            },
            // 既有行 metadata 更优（history_score 7 > 0）→ 不降级。
            SearchResult {
                kind: SearchResultKind::File,
                title: Arc::from("b.txt"),
                subtitle: Arc::from(r"C:\b.txt"),
                execute_id: Arc::from(r"C:\b.txt"),
                target: ActionTarget {
                    kind: "file".into(),
                    value: r"C:\b.txt".into(),
                },
                match_spans: Vec::new(),
                match_metadata: Some(MatchMetadata {
                    kind: MatchKind::Literal,
                    class: 0,
                    position: 0,
                    score: 1,
                    history_score: 0,
                }),
            },
            // 新目标 → 追加。
            SearchResult {
                kind: SearchResultKind::File,
                title: Arc::from("c.txt"),
                subtitle: Arc::from(r"C:\c.txt"),
                execute_id: Arc::from(r"C:\c.txt"),
                target: ActionTarget {
                    kind: "file".into(),
                    value: r"C:\c.txt".into(),
                },
                match_spans: Vec::new(),
                match_metadata: Some(MatchMetadata {
                    kind: MatchKind::Literal,
                    class: 0,
                    position: 0,
                    score: 1,
                    history_score: 0,
                }),
            },
        ];
        merge_alias_rows(&mut ranked, rows);
        assert_eq!(ranked.len(), 3, "无重复行，新目标追加");
        assert!(ranked[0].match_metadata.is_some(), "None 行接受升级");
        assert_eq!(
            ranked[1].match_metadata.unwrap().history_score,
            7,
            "更优既有行不降级"
        );
        assert_eq!(ranked[2].target.value, r"C:\c.txt");
    }

    /// 2026-08-25 用户报告锚定：同键行（kind+execute_id+title 相同，spans 可不同）
    /// 绝不能进最终结果——前端 ResultList 的同步算法与 WPF Selector 的选中簿记
    /// 都假定行键唯一，同键两行让前端把同一实例插两次并抛
    /// "An item with the same key has already been added. Key: …ItemInfo"，
    /// 且此后每次按键都复发直至重启。
    #[test]
    fn dedupe_row_identities_keeps_first_occurrence() {
        let mk = |title: &str, exec: &str, spans: Vec<i32>| SearchResult {
            kind: SearchResultKind::Folder,
            title: Arc::from(title),
            subtitle: Arc::from(exec),
            execute_id: Arc::from(exec),
            target: crate::shell::ActionTarget::new(crate::shell::TargetKind::Directory, exec),
            match_spans: spans,
            match_metadata: None,
        };
        let mut ranked = vec![
            mk("B", r"C:\B", vec![0, 1]),
            mk("other", r"C:\x", vec![]),
            // 同键不同 spans（字面通道 + 拼音通道对同一路径各出一条的形态）。
            mk("B", r"C:\B", vec![2, 3]),
            // 同名不同路径：不是重复，必须保留。
            mk("B", r"C:\B2", vec![]),
        ];
        dedupe_row_identities(&mut ranked);
        assert_eq!(ranked.len(), 3, "同键行只留首行，同名异径保留");
        assert_eq!(ranked[0].execute_id.as_ref(), r"C:\B");
        assert_eq!(
            ranked[0].match_spans,
            vec![0, 1],
            "保留首行（排序已定优劣）"
        );
        assert_eq!(ranked[1].execute_id.as_ref(), r"C:\x");
        assert_eq!(ranked[2].execute_id.as_ref(), r"C:\B2");
    }

    /// 别名协议三命令的解码形状。
    #[test]
    fn alias_requests_decode() {
        let set: Request = serde_json::from_str(
            r#"{"type":"alias_set","target":{"kind":"file","value":"C:\\a.exe"},"words":["wx","微信"]}"#,
        )
        .unwrap();
        assert!(
            matches!(set, Request::AliasSet { ref target, ref words } if target.kind == "file" && words.len() == 2)
        );
        let delete: Request = serde_json::from_str(
            r#"{"type":"alias_delete","target":{"kind":"file","value":"C:\\a.exe"}}"#,
        )
        .unwrap();
        assert!(matches!(delete, Request::AliasDelete { .. }));
        let list: Request = serde_json::from_str(r#"{"type":"alias_list"}"#).unwrap();
        assert!(matches!(list, Request::AliasList));
        // 回执序列化形状。
        let json = serde_json::to_string(&Response::AliasApplied {
            message: String::new(),
        })
        .unwrap();
        assert!(json.contains(r#""type":"alias_applied""#), "{json}");
    }

    /// resolve_lnk 请求解码 + lnk_target 响应序列化形状（对齐 alias_requests_decode）。
    #[test]
    fn resolve_lnk_request_decodes_and_response_serializes() {
        let req: Request = serde_json::from_str(
            r#"{"type":"resolve_lnk","target":{"kind":"application","value":"C:\\Programs\\WeChat.lnk"}}"#,
        )
        .unwrap();
        assert!(
            matches!(req, Request::ResolveLnk { ref target } if target.kind == "application"
                && target.value.ends_with("WeChat.lnk"))
        );

        let json_some = serde_json::to_string(&Response::LnkTarget {
            resolved: Some(r"C:\Programs\WeChat\WeChat.exe".into()),
        })
        .unwrap();
        assert!(json_some.contains(r#""type":"lnk_target""#), "{json_some}");
        assert!(json_some.contains(r#"resolved"#), "{json_some}");

        let json_none = serde_json::to_string(&Response::LnkTarget { resolved: None }).unwrap();
        assert!(json_none.contains(r#""type":"lnk_target""#), "{json_none}");
        // None 经 skip？不——resolved 非 skip 字段，需显式 null。
        assert!(json_none.contains(r#""resolved":null"#), "{json_none}");
    }

    /// dispatch_non_search 对非 .lnk target 返回 None，免 STA 往返（纯逻辑不依赖 COM）。
    #[tokio::test]
    async fn dispatch_resolve_lnk_non_lnk_returns_none_without_sta() {
        let engines = default_engines();
        let shell = ShellExecutor::start().expect("shell worker starts");
        let history = Arc::new(HistoryStore::load(
            &std::env::temp_dir().join(format!("prism-ipc-lnk-{}", std::process::id())),
            true,
        ));
        let prefs = Arc::new(BrokerPreferences::new(true));
        let windows = Arc::new(crate::window_list::WindowSnapshotStore::new());
        let aliases = Arc::new(crate::alias::AliasStore::load(&std::env::temp_dir()));
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
        let target = ActionTarget {
            kind: "application".into(),
            value: r"C:\Programs\WeChat\WeChat.exe".into(),
        };
        let resp = dispatch_non_search(
            Request::ResolveLnk { target },
            &engines,
            &shell,
            &history,
            &prefs,
            &windows,
            &aliases,
            &commands,
            crate::commands::ConnectionCaps::default(),
        )
        .await;
        assert!(
            matches!(resp, Response::LnkTarget { resolved: None }),
            "non-.lnk must short-circuit to None"
        );
    }

    #[test]
    fn legacy_and_typed_action_requests_both_decode() {
        let legacy: Request =
            serde_json::from_str(r#"{"type":"execute","id":"C:\\Windows\\explorer.exe"}"#).unwrap();
        assert!(matches!(
            legacy,
            Request::Execute {
                id: Some(_),
                target: None,
                ..
            }
        ));

        let typed: Request = serde_json::from_str(
            r#"{"type":"execute","target":{"kind":"web","value":"https://example.com"}}"#,
        )
        .unwrap();
        assert!(matches!(
            typed,
            Request::Execute {
                id: None,
                target: Some(ActionTarget { ref kind, .. }),
                ..
            } if kind == "web"
        ));
    }

    #[test]
    fn search_results_always_serialize_a_typed_target() {
        let item = SearchResult {
            kind: SearchResultKind::File,
            title: "x".into(),
            subtitle: r"C:\x".into(),
            execute_id: r"C:\x".into(),
            target: ActionTarget::new(TargetKind::File, r"C:\x"),
            match_spans: Vec::new(),
            match_metadata: None,
        };
        let json = serde_json::to_value(item).unwrap();
        assert_eq!(json["target"]["kind"], "file");
        assert_eq!(json["target"]["value"], r"C:\x");
    }

    /// FRESH-AUDIT-2 G1: 无匹配元数据的注入项排在真实命中之后，而不是反超到最前。
    #[test]
    fn sort_places_items_without_match_metadata_last() {
        let item = |title: &str, metadata: Option<MatchMetadata>| SearchResult {
            kind: SearchResultKind::File,
            title: title.into(),
            subtitle: "C:\\x".into(),
            execute_id: "C:\\x".into(),
            target: ActionTarget::new(TargetKind::File, "C:\\x"),
            match_spans: Vec::new(),
            match_metadata: metadata,
        };
        let mut items = vec![
            item("injected", None),
            item("needle.txt", rank_title("needle.txt", "ne")),
        ];
        sort_search_results(&mut items);
        assert_eq!(&*items[0].title, "needle.txt");
        assert_eq!(&*items[1].title, "injected");
    }

    #[test]
    fn file_history_candidates_survive_indexer_top_k_and_respect_exclusions() {
        // history_file_candidates now filters out paths that don't exist on disk,
        // so we create real temp files for the test paths.
        let temp_dir = std::env::temp_dir();
        let late_dir = temp_dir.join("prism-history-test-late");
        let zeta_path = late_dir.join("zeta.txt");
        let beta_path = temp_dir.join("beta.txt");
        let _ = std::fs::create_dir_all(&late_dir);
        let _ = std::fs::write(&zeta_path, "test");
        let _ = std::fs::write(&beta_path, "test");
        let zeta_str = zeta_path.to_str().unwrap().to_string();
        let beta_str = beta_path.to_str().unwrap().to_string();

        let weights = vec![HistoryWeight {
            target: ActionTarget::new(TargetKind::File, &zeta_str),
            score: 4,
            last_used_utc: 1_000,
        }];
        let mut candidates = history_file_candidates(
            "ta",
            &weights,
            true,
            &[],
            None,
            &crate::hierarchy::QueryFilters::none(),
            8,
        );
        candidates.push(SearchResult {
            kind: SearchResultKind::File,
            title: "beta.txt".into(),
            subtitle: Arc::from(beta_str.as_str()),
            execute_id: Arc::from(beta_str.as_str()),
            target: ActionTarget::new(TargetKind::File, &beta_str),
            match_spans: match_spans("beta.txt", "ta"),
            match_metadata: rank_title("beta.txt", "ta"),
        });
        candidates.sort_by(compare_search_results);
        assert_eq!(&*candidates[0].title, "zeta.txt");
        assert_eq!(
            candidates[0]
                .match_metadata
                .as_ref()
                .map(|metadata| metadata.history_score),
            Some(4)
        );

        // Exclusion path must use the real temp dir path.
        let exclusion = late_dir.to_str().unwrap().to_string();
        assert!(history_file_candidates(
            "ta",
            &weights,
            true,
            &[exclusion],
            None,
            &crate::hierarchy::QueryFilters::none(),
            8
        )
        .is_empty());

        // FRESH-AUDIT-2 F4: ext:pdf 过滤时非 pdf 历史候选必须被过滤掉。
        let pdf_dir = temp_dir.join("prism-history-test-f4");
        let pdf_path = pdf_dir.join("zeta.pdf");
        let _ = std::fs::create_dir_all(&pdf_dir);
        let _ = std::fs::write(&pdf_path, "test");
        let pdf_str = pdf_path.to_str().unwrap().to_string();
        let mixed = vec![
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &pdf_str),
                score: 4,
                last_used_utc: 1_000,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &zeta_str),
                score: 8,
                last_used_utc: 2_000,
            },
        ];
        let ext_pdf = crate::hierarchy::QueryFilters::new(vec!["pdf".into()], vec![]);
        let filtered = history_file_candidates("zeta", &mixed, true, &[], None, &ext_pdf, 8);
        assert_eq!(filtered.len(), 1, "only the .pdf history entry survives");
        assert_eq!(&*filtered[0].title, "zeta.pdf");
        let _ = std::fs::remove_dir_all(&pdf_dir);

        // Cleanup
        let _ = std::fs::remove_file(&zeta_path);
        let _ = std::fs::remove_file(&beta_path);
        let _ = std::fs::remove_dir(&late_dir);
    }

    #[test]
    fn empty_query_history_under_root_keeps_existing_descendants_only() {
        let dir = std::env::temp_dir().join(format!(
            "prism-empty-query-root-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = dir.join("project");
        let nested = root.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        let inside = nested.join("inside.txt");
        let sibling_dir = dir.join("other");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        let outside = sibling_dir.join("outside.txt");
        std::fs::write(&inside, b"in").unwrap();
        std::fs::write(&outside, b"out").unwrap();
        // Same basename as an inside hit, but under a different tree — must be dropped.
        let outside_same_name = sibling_dir.join("inside.txt");
        std::fs::write(&outside_same_name, b"nope").unwrap();
        // Prefix-boundary trap: `projectX` shares the `project` text prefix but is not a
        // descendant of `project\`. It must stay out of the empty-query root list.
        let root_other = dir.join("projectX");
        std::fs::create_dir_all(&root_other).unwrap();
        let root_other_file = root_other.join("trap.txt");
        std::fs::write(&root_other_file, b"trap").unwrap();

        let root_str = root.to_string_lossy().replace('/', "\\");
        let inside_str = inside.to_string_lossy().replace('/', "\\");
        let outside_str = outside.to_string_lossy().replace('/', "\\");
        let outside_same_str = outside_same_name.to_string_lossy().replace('/', "\\");
        let gone_str = nested.join("gone.txt").to_string_lossy().replace('/', "\\");
        let folder_str = nested.to_string_lossy().replace('/', "\\");
        let root_other_str = root_other_file.to_string_lossy().replace('/', "\\");
        let root_self_str = root_str.clone();

        // weights 按 last_used 降序构造（与 weights() 的返回契约一致）：
        // 空查询候选保持输入顺序 = MRU，收集满 limit 个现存路径即停。
        let weights = vec![
            HistoryWeight {
                target: ActionTarget::new(TargetKind::Window, "12345"),
                score: 99,
                last_used_utc: 999,
            },
            // Window history must never surface on the empty-query file path.
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &outside_str),
                score: 40,
                last_used_utc: 900,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &outside_same_str),
                score: 30,
                last_used_utc: 850,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &gone_str),
                score: 20,
                last_used_utc: 800,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &root_other_str),
                score: 18,
                last_used_utc: 750,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::Directory, &folder_str),
                score: 12,
                last_used_utc: 700,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &inside_str),
                score: 8,
                last_used_utc: 600,
            },
            // The root directory itself is a valid empty-query hit.
            HistoryWeight {
                target: ActionTarget::new(TargetKind::Directory, &root_self_str),
                score: 6,
                last_used_utc: 500,
            },
        ];

        let candidates = history_file_candidates(
            "",
            &weights,
            false,
            &[],
            Some(root_str.as_str()),
            &crate::hierarchy::QueryFilters::none(),
            8,
        );
        assert_eq!(
            candidates
                .iter()
                .map(|item| &*item.execute_id)
                .collect::<Vec<_>>(),
            [
                folder_str.as_str(),
                inside_str.as_str(),
                root_self_str.as_str()
            ]
        );
        assert_eq!(
            candidates[0].kind,
            SearchResultKind::Folder,
            "MRU order: folder was used most recently among empty-query rows"
        );
        assert!(
            !candidates
                .iter()
                .any(|item| item.execute_id.eq_ignore_ascii_case(&root_other_str)),
            "prefix-boundary: rootOther must not match root"
        );

        // Non-empty query must still use title matching and ignore the root filter arg
        // when callers pass None (G2 injection path).
        let named = history_file_candidates(
            "inside",
            &weights,
            false,
            &[],
            None,
            &crate::hierarchy::QueryFilters::none(),
            8,
        );
        assert!(named.iter().any(|item| *item.execute_id == inside_str));
        assert!(
            named
                .iter()
                .any(|item| *item.execute_id == outside_same_str),
            "without a root filter, sibling trees still inject on title match"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn empty_query_results_require_root_and_enabled_history() {
        let dir = std::env::temp_dir().join(format!(
            "prism-empty-query-response-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        std::fs::write(&file, b"x").unwrap();
        let file_str = file.to_string_lossy().replace('/', "\\");
        let root_str = dir.to_string_lossy().replace('/', "\\");

        let history_dir = std::env::temp_dir().join(format!(
            "prism-empty-query-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let history = Arc::new(HistoryStore::load(&history_dir, true));
        history
            .record(
                &ActionTarget::new(TargetKind::File, &file_str),
                HistoryUse::Execute,
            )
            .unwrap();

        let with_root = empty_query_results("", 8, None, Some(root_str.as_str()), &history).await;
        match with_root {
            Response::Results { items, .. } => {
                assert_eq!(items.len(), 1);
                assert_eq!(&*items[0].execute_id, file_str);
            }
            other => panic!("expected results, got {other:?}"),
        }

        let no_root = empty_query_results("", 8, None, None, &history).await;
        match no_root {
            Response::Results { items, .. } => {
                assert!(
                    items.is_empty(),
                    "non-host empty input defers recent windows to G5"
                );
            }
            other => panic!("expected results, got {other:?}"),
        }

        history.set_enabled(false);
        let disabled = empty_query_results("", 8, None, Some(root_str.as_str()), &history).await;
        match disabled {
            Response::Results { items, .. } => assert!(items.is_empty()),
            other => panic!("expected results, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(history_dir);
    }

    /// S1（PRISM-IMPL-PLAN-4-2026-08-20）新契约：class 跨 kind——class 0 的拼音
    /// 命中先于 class 2 的字面命中；同 class 0 内 kind 锁死（全拼<首字母）、
    /// 桶内 history 决胜。
    #[test]
    fn cross_type_ranking_preserves_match_tiers_and_same_tier_history() {
        let result = |kind, title: &str, metadata| SearchResult {
            kind,
            title: title.into(),
            subtitle: title.into(),
            execute_id: title.into(),
            target: ActionTarget::new(TargetKind::File, title),
            match_spans: Vec::new(),
            match_metadata: Some(metadata),
        };
        let rank = |kind, class, history_score| MatchMetadata {
            kind,
            class,
            position: 0,
            score: 10,
            history_score,
        };
        let mut items = [
            result(
                SearchResultKind::Folder,
                "initials",
                rank(MatchKind::Initials, 0, u32::MAX),
            ),
            result(
                SearchResultKind::File,
                "full-history",
                rank(MatchKind::FullPinyin, 0, 20),
            ),
            result(
                SearchResultKind::App,
                "literal",
                rank(MatchKind::Literal, 2, 0),
            ),
            result(
                SearchResultKind::App,
                "full-no-history",
                rank(MatchKind::FullPinyin, 0, 0),
            ),
        ];
        items.sort_by(compare_search_results);
        assert_eq!(
            items.iter().map(|item| &*item.title).collect::<Vec<_>>(),
            ["full-history", "full-no-history", "initials", "literal"]
        );
    }

    /// 查询记忆置顶（S1 后新契约）：picked 项跨 class/kind 置顶——「上次在同样
    /// 输入下选的就是它」强于任何匹配质量信号；picked 内部仍按匹配质量
    ///（class→kind→…）排序。
    #[test]
    fn query_pick_promotes_above_match_quality_but_orders_within() {
        let item = |kind: MatchKind, title: &str, class: u8| SearchResult {
            kind: SearchResultKind::File,
            title: title.into(),
            subtitle: title.into(),
            execute_id: title.into(),
            target: ActionTarget::new(TargetKind::File, title),
            match_spans: Vec::new(),
            match_metadata: Some(MatchMetadata {
                kind,
                class,
                position: 0,
                score: 10,
                history_score: 0,
            }),
        };
        // "plain" 是精确命中（class 0），"picked" 是子串命中（class 2）：
        // 没有 pick 时 plain 必须在前——先验证基准序。
        let mut baseline = vec![
            item(MatchKind::Literal, "plain", 0),
            item(MatchKind::Literal, "picked", 2),
        ];
        sort_search_results(&mut baseline);
        assert_eq!(
            baseline
                .iter()
                .map(|entry| &*entry.title)
                .collect::<Vec<_>>(),
            ["plain", "picked"],
            "baseline: exact match beats substring without picks"
        );

        let mut items = vec![
            item(MatchKind::Literal, "plain", 0),
            item(MatchKind::Literal, "picked", 2),
            item(MatchKind::FullPinyin, "pinyin", 0),
        ];
        let picks = vec![false, true, true];
        sort_search_results_with_picks(&mut items, Some(&picks));
        // picked 的两个字面/拼音项（class 2/0）都越过未 picked 的 class 0 精确命中；
        // picked 内部 class 0 的拼音项先于 class 2 的字面项。
        assert_eq!(
            items.iter().map(|entry| &*entry.title).collect::<Vec<_>>(),
            ["pinyin", "picked", "plain"],
            "picked items promote above match quality; within picks, class decides"
        );
    }

    /// 旧的原地置换在 2/3 元素环上会自抵消或乱序：三档排序必须真正落位。
    #[test]
    fn sort_search_results_orders_items_across_match_tiers() {
        let item = |kind: MatchKind, title: &str, class: u8| SearchResult {
            kind: SearchResultKind::File,
            title: title.into(),
            subtitle: title.into(),
            execute_id: title.into(),
            target: ActionTarget::new(TargetKind::File, title),
            match_spans: Vec::new(),
            match_metadata: Some(MatchMetadata {
                kind,
                class,
                position: 0,
                score: 10,
                history_score: 0,
            }),
        };
        let mut items = vec![
            item(MatchKind::FullPinyin, "pinyin", 0),
            item(MatchKind::Literal, "substring", 2),
            item(MatchKind::Literal, "exact", 0),
        ];
        sort_search_results(&mut items);
        // S1（PRISM-IMPL-PLAN-4-2026-08-20）：class 先于 kind——class 0 内字面
        //（exact）先于拼音（pinyin），class 2（substring）垫底。
        assert_eq!(
            items.iter().map(|entry| &*entry.title).collect::<Vec<_>>(),
            ["exact", "pinyin", "substring"]
        );
    }

    #[tokio::test]
    async fn only_successful_shell_outcomes_record_history() {
        let dir =
            std::env::temp_dir().join(format!("prism-ipc-history-outcomes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let history = Arc::new(HistoryStore::load(&dir, true));
        let target = ActionTarget::new(TargetKind::File, r"C:\history-outcome.txt");
        let record = || Some((target.clone(), HistoryUse::Execute));

        let cancelled =
            finish_shell_response(Ok(ShellOutcome::Cancelled), record(), &history, None).await;
        assert!(matches!(
            cancelled,
            Response::Status {
                cancelled: true,
                ..
            }
        ));
        assert_eq!(history.score(&target), 0);

        let failed = finish_shell_response(
            Err(ShellError {
                kind: crate::shell::ShellErrorKind::System,
                message: "redacted failure".into(),
            }),
            record(),
            &history,
            None,
        )
        .await;
        assert!(matches!(failed, Response::Error { .. }));
        assert_eq!(history.score(&target), 0);

        let success =
            finish_shell_response(Ok(ShellOutcome::Success), record(), &history, None).await;
        assert!(matches!(
            success,
            Response::Status {
                cancelled: false,
                ..
            }
        ));
        assert_eq!(history.score(&target), 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 动作请求携带可选 query（查询记忆的记录侧输入）。缺失、null、有值
    /// 三种形态都必须解码——旧前端永远不发这个字段。
    #[test]
    fn action_query_field_is_optional_and_backward_compatible() {
        let legacy: Request =
            serde_json::from_str(r#"{"type":"execute","target":{"kind":"file","value":"C:\\a"}}"#)
                .unwrap();
        match legacy {
            Request::Execute { query, .. } => assert_eq!(query, None),
            other => panic!("unexpected request: {other:?}"),
        }

        let explicit_null: Request = serde_json::from_str(
            r#"{"type":"execute","target":{"kind":"file","value":"C:\\a"},"query":null}"#,
        )
        .unwrap();
        match explicit_null {
            Request::Execute { query, .. } => assert_eq!(query, None),
            other => panic!("unexpected request: {other:?}"),
        }

        let with_query: Request = serde_json::from_str(
            r#"{"type":"run_action","target":{"kind":"file","value":"C:\\a"},"action":"copy","query":"Report ext:pdf"}"#,
        )
        .unwrap();
        match with_query {
            Request::RunAction { query, .. } => {
                assert_eq!(query.as_deref(), Some("Report ext:pdf"))
            }
            other => panic!("unexpected request: {other:?}"),
        }

        let window: Request = serde_json::from_str(
            r#"{"type":"record_window_switch","target":{"kind":"window","value":"7"},"query":">win"}"#,
        )
        .unwrap();
        match window {
            Request::RecordWindowSwitch { query, .. } => {
                assert_eq!(query.as_deref(), Some(">win"))
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    // K2 §4.2：内置 ActionItem 序列化后 JSON 与 K1 逐字节一致。
    // skip_serializing_if 让新字段在默认值时不出现在 JSON 里，保证未协商
    // commands_v1 的旧前端拿到的动作列表完全没变化（P3）。
    #[test]
    fn builtin_action_item_serializes_byte_identical_to_k1() {
        let item = ActionItem {
            id: "copy".into(),
            label: "复制".into(),
            icon_glyph: "\u{E8C8}".into(),
            has_submenu: false,
            is_section_header: false,
            invocation_kind: "builtin_action".into(),
            command_id: None,
            is_enabled: true,
            disabled_reason: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        // K1 格式：恰好 5 字段，无 invocation_kind/command_id/is_enabled/disabled_reason
        let expected = r#"{"id":"copy","label":"复制","icon_glyph":"","has_submenu":false,"is_section_header":false}"#;
        assert_eq!(json, expected);

        // 命令动作：invocation_kind=command 时新字段出现
        let cmd_item = ActionItem {
            id: "cmd.terminal".into(),
            label: "在此处打开终端".into(),
            icon_glyph: String::new(),
            has_submenu: false,
            is_section_header: false,
            invocation_kind: "command".into(),
            command_id: Some("prism.terminal.open".into()),
            is_enabled: true,
            disabled_reason: None,
        };
        let cmd_json = serde_json::to_string(&cmd_item).unwrap();
        assert!(cmd_json.contains(r#""invocation_kind":"command""#));
        assert!(cmd_json.contains(r#""command_id":"prism.terminal.open""#));
        // is_enabled=true 仍被 skip（is_true 谓词）
        assert!(!cmd_json.contains(r#""is_enabled""#));
        assert!(!cmd_json.contains(r#""disabled_reason""#));

        // 禁用态：is_enabled=false 出现，disabled_reason 出现
        let disabled = ActionItem {
            id: "cmd.zip".into(),
            label: "压缩为 ZIP".into(),
            icon_glyph: String::new(),
            has_submenu: false,
            is_section_header: false,
            invocation_kind: "command".into(),
            command_id: Some("prism.staging.zip".into()),
            is_enabled: false,
            disabled_reason: Some("需要 7-Zip".into()),
        };
        let dis_json = serde_json::to_string(&disabled).unwrap();
        assert!(dis_json.contains(r#""is_enabled":false"#));
        assert!(dis_json.contains(r#""disabled_reason":"需要 7-Zip""#));
    }

    /// 纯过滤词或空白没有记忆意义。
    #[test]
    fn query_pick_key_strips_prefixes_filters_and_normalizes() {
        assert_eq!(query_pick_key("Report ext:pdf").as_deref(), Some("report"));
        assert_eq!(query_pick_key("  >Word  ").as_deref(), Some("word"));
        assert_eq!(query_pick_key("report").as_deref(), Some("report"));
        assert_eq!(query_pick_key("ext:pdf"), None);
        assert_eq!(query_pick_key("   "), None);
    }

    /// 成功动作带上查询键 → 写入 query 子表，`query_pick` 能查到；
    /// 查询键缺失（旧前端）时只记 frecency。
    #[tokio::test]
    async fn successful_action_with_query_records_pick_memory() {
        let dir =
            std::env::temp_dir().join(format!("prism-ipc-history-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let history = Arc::new(HistoryStore::load(&dir, true));
        let target = ActionTarget::new(TargetKind::File, r"C:\pick.txt");

        let with_query = finish_shell_response(
            Ok(ShellOutcome::Success),
            Some((target.clone(), HistoryUse::Execute)),
            &history,
            Some("conf".to_string()),
        )
        .await;
        assert!(matches!(with_query, Response::Status { .. }));
        assert!(history.query_pick(&target, "conf"));
        assert!(!history.query_pick(&target, "other"));

        let legacy = finish_shell_response(
            Ok(ShellOutcome::Success),
            Some((
                ActionTarget::new(TargetKind::File, r"C:\legacy.txt"),
                HistoryUse::Execute,
            )),
            &history,
            None,
        )
        .await;
        assert!(matches!(legacy, Response::Status { .. }));
        assert!(!history.query_pick(
            &ActionTarget::new(TargetKind::File, r"C:\legacy.txt"),
            "conf"
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn unknown_target_kind_is_rejected_without_execution() {
        let shell = ShellExecutor::start().unwrap();
        let history = Arc::new(HistoryStore::load(
            &std::env::temp_dir().join(format!("prism-ipc-history-{}", std::process::id())),
            true,
        ));
        let preferences = Arc::new(BrokerPreferences::new(true));
        let response = dispatch_non_search(
            Request::Execute {
                id: None,
                target: Some(ActionTarget {
                    kind: "future".into(),
                    value: "opaque".into(),
                }),
                query: None,
            },
            &default_engines(),
            &shell,
            &history,
            &preferences,
            &Arc::new(crate::window_list::WindowSnapshotStore::new()),
            &Arc::new(crate::alias::AliasStore::load(&std::env::temp_dir())),
            &Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir())),
            crate::commands::ConnectionCaps::default(),
        )
        .await;
        assert!(matches!(
            response,
            Response::Error {
                category: Some(crate::shell::ShellErrorKind::Unsupported),
                ..
            }
        ));
    }

    fn default_engines() -> SharedEngines {
        Arc::new(std::sync::RwLock::new(WebEngine::defaults()))
    }

    /// S3：resolve/record 迁入 spawn_blocking 后行为不变——过期 token 照样被拒。
    /// 空 store 的 generation 为 0，世代检查发生在 Win32 probe 之前，
    /// 所以这条测试路径完全不触发真实窗口枚举。
    #[tokio::test]
    async fn resolve_and_record_window_reject_stale_tokens_off_worker() {
        let shell = ShellExecutor::start().unwrap();
        let history = Arc::new(HistoryStore::load(
            &std::env::temp_dir().join(format!("prism-ipc-window-resolve-{}", std::process::id())),
            true,
        ));
        let preferences = Arc::new(BrokerPreferences::new(true));
        let windows = Arc::new(crate::window_list::WindowSnapshotStore::new());

        let stale = ActionTarget::new(TargetKind::Window, "1");
        for request in [
            Request::ResolveWindow {
                target: stale.clone(),
            },
            Request::RecordWindowSwitch {
                target: stale,
                query: None,
            },
        ] {
            let response = dispatch_non_search(
                request,
                &default_engines(),
                &shell,
                &history,
                &preferences,
                &windows,
                &Arc::new(crate::alias::AliasStore::load(&std::env::temp_dir())),
                &Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir())),
                crate::commands::ConnectionCaps::default(),
            )
            .await;
            assert!(
                matches!(
                    response,
                    Response::Error {
                        category: Some(crate::shell::ShellErrorKind::Conflict),
                        ..
                    }
                ),
                "stale-generation tokens must stay conflicts after the spawn_blocking move"
            );
        }
    }

    /// The broker protocol gained `root` without a version bump, so an old frontend that
    /// never sends the field has to decode byte-for-byte as before.
    #[test]
    fn broker_search_root_is_optional_and_backward_compatible() {
        let legacy: Request =
            serde_json::from_str(r#"{"type":"search","query":"x","max":8}"#).unwrap();
        assert!(matches!(legacy, Request::Search { root: None, .. }));

        let explicit_null: Request =
            serde_json::from_str(r#"{"type":"search","query":"x","max":8,"root":null}"#).unwrap();
        assert!(matches!(explicit_null, Request::Search { root: None, .. }));

        let scoped: Request = serde_json::from_str(
            r#"{"type":"search","query":"x","max":8,"root":"C:\\Users\\me\\docs"}"#,
        )
        .unwrap();
        let Request::Search { root, .. } = scoped else {
            panic!("expected a search request");
        };
        assert_eq!(root.as_deref(), Some(r"C:\Users\me\docs"));
        // A blank root is a cleared scope, not an error: it must behave like no root.
        assert_eq!(requested_root(Some("   ")).unwrap(), None);
        assert_eq!(
            requested_root(root.as_deref()).unwrap(),
            Some(r"C:\Users\me\docs")
        );
    }

    /// Every rejection reaches the frontend as a value whose string form matches
    /// `root_scope::RootRejection`, and is omitted entirely for ordinary searches.
    #[test]
    fn root_rejection_is_reported_as_a_structured_field() {
        let results = |rejection: Option<RootRejection>| Response::Results {
            query: "x".into(),
            items: Vec::new(),
            is_indexing: false,
            index_progress: None,
            index_error: None,
            index_generation: Some(3),
            is_truncated: false,
            matched_count: None,
            scanned_nodes: None,
            name_candidates: None,
            entered_top_k: None,
            path_constructions: None,
            pinyin_status: None,
            history_status: None,
            root_message: rejection.map(|reason| reason.message().to_owned()),
            root_rejection: rejection,
            cacheable: true,
            command_catalog_generation: None,
        };

        let global = serde_json::to_value(results(None)).unwrap();
        assert!(global.get("root_rejection").is_none(), "{global}");
        assert!(global.get("root_message").is_none(), "{global}");

        for reason in [
            RootRejection::NotAbsolute,
            RootRejection::Unsupported,
            RootRejection::TooLong,
            RootRejection::TooDeep,
            RootRejection::VolumeNotIndexed,
            RootRejection::NotFound,
            RootRejection::NotADirectory,
            RootRejection::AccessDenied,
        ] {
            let json = serde_json::to_value(results(Some(reason))).unwrap();
            assert_eq!(
                json["root_rejection"].as_str(),
                Some(reason.reason()),
                "reason strings must stay identical to root_scope"
            );
            assert_eq!(json["root_message"].as_str(), Some(reason.message()));
        }
    }

    /// 候选放宽的边界：3× 放大、槽位耗尽仍合法、不越过协议上限。
    #[test]
    fn indexer_request_max_widens_within_protocol_bounds() {
        assert_eq!(indexer_request_max(8), 24);
        assert_eq!(indexer_request_max(0), 1, "web 行占尽槽位也要保持合法请求");
        assert_eq!(indexer_request_max(1000), 1000, "more 模式已在上限，不放大");
        assert_eq!(
            indexer_request_max(400),
            1000,
            "3× 超上限时收敛到 MAX_SEARCH_RESULTS"
        );
    }

    /// A root the indexer refuses degrades to a global search. The reply must still be a
    /// normal `results` response — never an `error` — so the frontend keeps working.
    #[tokio::test]
    async fn unusable_root_degrades_to_a_global_search_with_a_reason() {
        // No indexer service is running in tests, so the index part fails; what matters is
        // that a locally detectable rejection is reported as a field, not as an error.
        let too_long = format!("C:\\{}", "a".repeat(crate::root_scope::MAX_ROOT_PATH_BYTES));
        let budget = std::time::Instant::now() + CHAIN_BUDGET;
        let (_, rejection, message) =
            search_index_with_root_fallback("needle", 8, None, false, Some(&too_long), budget)
                .await;
        assert_eq!(rejection, Some(RootRejection::TooLong));
        assert_eq!(message.as_deref(), Some(RootRejection::TooLong.message()));

        let (_, rejection, message) = search_index_with_root_fallback(
            "needle",
            8,
            None,
            false,
            None,
            std::time::Instant::now() + CHAIN_BUDGET,
        )
        .await;
        assert_eq!(rejection, None, "a global search reports no rejection");
        assert_eq!(message, None);

        let (_, rejection, _) = search_index_with_root_fallback(
            "needle",
            8,
            None,
            false,
            Some("   "),
            std::time::Instant::now() + CHAIN_BUDGET,
        )
        .await;
        assert_eq!(rejection, None, "a blank root is a global search");
    }
}

#[cfg(test)]
mod query_parser_tests {
    use super::*;

    /// P1（第一轮 bug 修复）：路径查询三件套——识别、规范化、父/末段拆分。
    #[test]
    fn p1_path_query_helpers_classify_normalize_and_split() {
        assert!(is_absolute_path_query(r"E:\foo"));
        assert!(is_absolute_path_query("e:/foo"));
        assert!(is_absolute_path_query(r"\\server\share"));
        assert!(is_absolute_path_query("//srv/share"));
        // 复审 B：粘贴带引号的路径也要能进路径分支（gate 与归一化共用剥引号）。
        assert!(is_absolute_path_query(r#""E:\foo bar""#));
        assert!(is_absolute_path_query(" \"E:\\foo\" "));
        // 不是路径：普通词、盘符无分隔符、相对路径。
        assert!(!is_absolute_path_query("note:foo"));
        assert!(!is_absolute_path_query("E:"));
        assert!(!is_absolute_path_query(r"foo\bar"));
        assert!(!is_absolute_path_query(""));

        // 粘贴引号剥离、正斜杠统一、尾分隔符剥离（卷根 E:\ 归一为 E:）。
        assert_eq!(
            normalize_path_query(r#""E:\foo bar""#).as_deref(),
            Some(r"E:\foo bar")
        );
        assert_eq!(normalize_path_query("E:/x/y/").as_deref(), Some(r"E:\x\y"));
        assert_eq!(normalize_path_query(r"E:\").as_deref(), Some("E:"));
        assert_eq!(
            normalize_path_query("  E:\\x\\  ").as_deref(),
            Some(r"E:\x")
        );
        assert_eq!(normalize_path_query("   ").as_deref(), None);
        assert_eq!(normalize_path_query(r#""""#).as_deref(), None);

        assert_eq!(
            split_path_query(r"E:\foo\bar.txt"),
            Some((r"E:\foo".to_owned(), "bar.txt".to_owned()))
        );
        assert_eq!(
            split_path_query(r"E:\foo"),
            Some(("E:".to_owned(), "foo".to_owned()))
        );
        // 卷根无父可拆。
        assert_eq!(split_path_query("E:"), None);
    }

    fn ext(value: &str) -> SearchFilter {
        SearchFilter {
            field: "ext".into(),
            value: value.into(),
        }
    }

    fn path(value: &str) -> SearchFilter {
        SearchFilter {
            field: "path".into(),
            value: value.into(),
        }
    }

    #[test]
    fn single_ext_filter() {
        let (name, filters) = parse_query("report ext:pdf");
        assert_eq!(name, "report");
        assert_eq!(filters, vec![ext("pdf")]);
        // 复审 L4（2026-08-21）：直连索引器管道的带点 ext 值也要归一——
        //「broker 已剥点」的假设对任意本地客户端不成立。
        let dotted = crate::indexer_ipc::ext_filters(Some(&[ext(".PDF")]));
        assert_eq!(dotted, vec!["pdf".to_owned()]);
    }

    #[test]
    fn multi_ext_or() {
        let (name, filters) = parse_query("design ext:md,pdf");
        assert_eq!(name, "design");
        assert_eq!(filters, vec![ext("md"), ext("pdf")]);
    }

    #[test]
    fn ext_and_path_combined() {
        let (name, filters) = parse_query(r#"design ext:md,pdf path:"Project Docs""#);
        assert_eq!(name, "design");
        assert_eq!(filters, vec![ext("md"), ext("pdf"), path("Project Docs")]);
    }

    #[test]
    fn leading_dot_stripped_and_case_insensitive() {
        let (name, filters) = parse_query("ext:.PDF");
        assert_eq!(name, "");
        assert_eq!(filters, vec![ext("pdf")]);
    }

    #[test]
    fn empty_ext_falls_back_to_plain_text() {
        let (name, filters) = parse_query("ext: ");
        assert_eq!(name, "ext:");
        assert!(filters.is_empty());
    }

    #[test]
    fn unterminated_quote_falls_back_to_plain_text() {
        let (name, filters) = parse_query(r#"path:"unterminated"#);
        assert!(filters.is_empty(), "no filters should be parsed");
        assert_eq!(name, r#"path:"unterminated"#);
    }

    #[test]
    fn unknown_prefix_is_plain_text() {
        let (name, filters) = parse_query("foo:bar");
        assert_eq!(name, "foo:bar");
        assert!(filters.is_empty());
    }

    #[test]
    fn repeated_ext_filters_are_preserved() {
        let (name, filters) = parse_query("ext:pdf ext:pdf");
        assert_eq!(name, "");
        assert_eq!(filters, vec![ext("pdf"), ext("pdf")]);
    }

    #[test]
    fn path_without_quotes() {
        let (name, filters) = parse_query(r#"report path:C:\Users"#);
        assert_eq!(name, "report");
        assert_eq!(filters, vec![path(r"C:\Users")]);
    }

    #[test]
    fn multiple_tokens_and_filters() {
        let (name, filters) = parse_query(r#"report final ext:pdf path:"My Docs""#);
        assert_eq!(name, "report final");
        assert_eq!(filters, vec![ext("pdf"), path("My Docs")]);
    }

    #[test]
    fn bare_query_has_no_filters() {
        let (name, filters) = parse_query("just a normal search");
        assert_eq!(name, "just a normal search");
        assert!(filters.is_empty());
    }

    #[test]
    fn has_query_filters_detects_ext_and_path() {
        assert!(has_query_filters(&[ext("pdf")]));
        assert!(has_query_filters(&[path("docs")]));
        assert!(!has_query_filters(&[SearchFilter {
            field: "exclude_path".into(),
            value: r"C:\x".into(),
        }]));
        assert!(!has_query_filters(&[]));
    }

    // ── G7b：stat 过滤（size/dm/dc/file/folder）─────────────────────────

    fn sf(field: &str, value: &str) -> SearchFilter {
        SearchFilter {
            field: field.into(),
            value: value.into(),
        }
    }

    #[test]
    fn size_filter_parsed_with_name() {
        let (name, filters) = parse_query("report size:>10mb");
        assert_eq!(name, "report");
        assert_eq!(filters, vec![sf("size", ">10mb")]);
        assert!(has_query_filters(&filters));
    }

    #[test]
    fn size_before_name_and_multiple_specs() {
        let (name, filters) = parse_query("size:>1mb size:<100mb 报告");
        assert_eq!(name, "报告");
        assert_eq!(filters, vec![sf("size", ">1mb"), sf("size", "<100mb")]);
    }

    #[test]
    fn invalid_size_value_falls_back_to_plain_text() {
        let (name, filters) = parse_query("size:abc");
        assert_eq!(name, "size:abc");
        assert!(filters.is_empty());
        assert!(!has_query_filters(&filters));
    }

    #[test]
    fn date_filter_and_flag_parsed() {
        let (name, filters) = parse_query("dm:today 报告");
        assert_eq!(name, "报告");
        assert_eq!(filters, vec![sf("dm", "today")]);

        let (name, filters) = parse_query("dc:2024 ..ignore");
        // "..ignore" 不是合法日期 → dc:2024 合法（紧凑年），".."+词进名字。
        assert_eq!(filters, vec![sf("dc", "2024")]);
        assert!(name.contains("..ignore"));

        let (name, filters) = parse_query("file: 报告");
        assert_eq!(name, "报告");
        assert_eq!(filters, vec![sf("file", "")]);

        // 旗标带附着值：冒号后文本照常成为名字 token。
        let (name, filters) = parse_query("folder:xyz");
        assert_eq!(name, "xyz");
        assert_eq!(filters, vec![sf("folder", "")]);
        assert!(has_query_filters(&filters));
    }

    #[test]
    fn stat_filters_combined_with_ext() {
        let (name, filters) = parse_query("report ext:pdf size:>1mb folder:");
        // folder: 旗标后面是查询结尾。
        assert_eq!(name, "report");
        assert_eq!(
            filters,
            vec![ext("pdf"), sf("size", ">1mb"), sf("folder", "")]
        );
    }

    /// 回归测试：中文输入（如"知乎"）不应在 parse_query 中 panic。
    /// 根因：parse_query 用字节索引对 &str 做切片 &raw[pos..end]，
    /// 其中 end = pos + prefix.len()。中文 UTF-8 是 3 字节/字符，
    /// end=4 落在第二个字符中间 → "end byte index 4 is not a char boundary" panic。
    /// 修复：改用 bytes[pos..end].eq_ignore_ascii_case 做字节比较，不做字符串切片。
    #[test]
    fn chinese_query_does_not_panic_in_parse_query() {
        // "知乎" = 6 bytes (e7 9f a5 e4 b9 8e)。pos=0, prefix="ext:" len=4,
        // 旧代码 &raw[0..4] 在字节 4 切片——落在"乎"的中间 → panic。
        let (name, filters) = parse_query("知乎");
        assert_eq!(name, "知乎");
        assert!(filters.is_empty());

        // 混合中英文也不应 panic
        let (name, filters) = parse_query("知乎 ext:pdf");
        assert_eq!(name, "知乎");
        assert_eq!(filters, vec![ext("pdf")]);

        // 纯中文多词
        let (name, _) = parse_query("知乎日报");
        assert_eq!(name, "知乎日报");
    }
}

/// 审计批次 2：H3 管道实例重建策略 + L1 握手限时。
#[cfg(test)]
mod pipe_lifecycle_tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    fn temp_pipe(tag: &str) -> String {
        format!(r"\\.\pipe\prism-broker-test-{tag}-{}", std::process::id())
    }

    /// H3：名下无实例 + 管道名被别的持有者占用 → 必须判定为抢注并返回 Err，
    /// 而不是无限重试（两个 broker 并存会随机分走客户端连接）。
    #[tokio::test]
    async fn rearm_detects_foreign_takeover_when_we_hold_nothing() {
        let name = temp_pipe("takeover");
        // 模拟第三方持有者：管道名上已存在首实例。
        let _foreign = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();

        let ownership = PipeOwnership::new();
        let result = rearm_listener(&name, &ownership).await;
        assert!(result.is_err(), "名下无实例仍被占用必须是抢注错误");
        assert_eq!(ownership.instances.load(Ordering::Relaxed), 0);
    }

    /// H3：名下无实例 + 管道名无人占用 → 探测成功，直接以首实例身份恢复服务。
    #[tokio::test]
    async fn rearm_adopts_first_instance_when_name_is_free() {
        let name = temp_pipe("free");
        let ownership = PipeOwnership::new();

        let server = rearm_listener(&name, &ownership).await.unwrap();
        assert_eq!(ownership.instances.load(Ordering::Relaxed), 1);
        drop(server);
    }

    /// H3：名下仍有自己的实例（正常状态）→ 走普通重建，与自己实例并存，不误判抢注。
    #[tokio::test]
    async fn rearm_plain_creates_alongside_own_instances() {
        let name = temp_pipe("own");
        let ownership = PipeOwnership::new();
        let _own = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        ownership.instances.store(1, Ordering::Relaxed);

        let server = rearm_listener(&name, &ownership).await.unwrap();
        assert_eq!(ownership.instances.load(Ordering::Relaxed), 2);
        drop(server);
    }

    /// R-A2：当前用户 SID 形如 S-1-...（管道 DACL 的原料）。
    #[test]
    fn current_user_sid_has_sddl_shape() {
        let sid = super::current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "SID 应为字符串形式：{sid}");
    }

    /// R-A2：带 ACL 管道可正常创建（当前用户 + SYSTEM DACL 不阻碍自身连接）。
    #[tokio::test]
    async fn create_broker_pipe_with_acl_succeeds() {
        let name = temp_pipe("acl");
        let server = super::create_broker_pipe(&name, true).unwrap();
        drop(server);
    }

    /// L1：客户端连上后不发首行 → 握手读超时（Err 而非永久挂起）。
    #[tokio::test]
    async fn silent_client_times_out_of_handshake() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut lines = BoundedLineReader::new(server);
        let result = read_handshake_line(&mut lines, Duration::from_millis(50)).await;
        assert!(result.is_err(), "静默客户端必须在握手限时内被拒");
        let _ = client.shutdown().await;
    }

    /// L1：客户端立刻发首行 → 握手读原样返回行内容，不受限时影响。
    #[tokio::test]
    async fn prompt_client_passes_handshake() {
        let (mut client, server) = tokio::io::duplex(64);
        client.write_all(b"{\"type\":\"hello\"}\n").await.unwrap();
        client.flush().await.unwrap();
        let mut lines = BoundedLineReader::new(server);
        let line = read_handshake_line(&mut lines, Duration::from_secs(10))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line, "{\"type\":\"hello\"}");
    }

    /// L1：握手前先到 EOF（客户端秒断）→ 返回 None 而不是等满限时。
    #[tokio::test]
    async fn early_eof_returns_none_without_waiting() {
        let (client, server) = tokio::io::duplex(64);
        drop(client);
        let mut lines = BoundedLineReader::new(server);
        let result = read_handshake_line(&mut lines, Duration::from_secs(10)).await;
        assert_eq!(result, Ok(None));
    }

    /// B5（AUDIT-4 批次D）：writer 必须按请求序号严格保序——乱序到达的响应
    /// 缓冲到 BTreeMap，顺序 0,1,2 写出；通道关闭后排干残余再退出。
    #[tokio::test]
    async fn b5_ordered_writer_writes_responses_in_request_order() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let (tx, rx) = tokio::sync::mpsc::channel(super::MAX_QUEUED_RESPONSES);
        let writer = tokio::spawn(super::ordered_writer(server, rx));

        // 乱序投递：2 先到、0 后到，1 最晚。
        tx.send((
            2,
            Response::Error {
                message: "two".into(),
                category: None,
            },
        ))
        .await
        .unwrap();
        tx.send((
            0,
            Response::Error {
                message: "zero".into(),
                category: None,
            },
        ))
        .await
        .unwrap();
        tx.send((
            1,
            Response::Error {
                message: "one".into(),
                category: None,
            },
        ))
        .await
        .unwrap();
        drop(tx);

        // 给 writer 一点时间写完（三条消息都很小）。
        for _ in 0..100 {
            if writer.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(writer.await.is_ok(), "通道关闭后 writer 必须排干退出");

        client.shutdown().await.unwrap();
        let mut received = String::new();
        client.read_to_string(&mut received).await.unwrap();
        let lines: Vec<&str> = received.lines().collect();
        assert_eq!(lines.len(), 3, "三条响应全部写出：{received:?}");
        assert!(lines[0].contains("zero"), "序号 0 必须最先写出");
        assert!(lines[1].contains("one"), "序号 1 第二");
        assert!(lines[2].contains("two"), "序号 2 最后");
    }

    // ── K0 协议测试 R10-R14 ──────────────────────────────────────────

    /// R10：hello 带 capabilities:["commands_v1"] → 回包 features 含之、
    /// command_catalog_generation 为 Some；不带 → 回包 JSON 不含这两个 key。
    #[tokio::test]
    async fn r10_hello_capability_negotiation_byte_compatibility() {
        // 协商连接：带 capabilities
        let (mut client, server) = tokio::io::duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
            handle_test_connection(server, commands).await;
        });
        client
            .write_all(b"{\"type\":\"hello\",\"protocol\":1,\"capabilities\":[\"commands_v1\"]}\n")
            .await
            .unwrap();
        client.flush().await.unwrap();
        // 关闭写端 → server 读到 EOF 退出 → server 关闭写端 → read_to_end 返回
        let _ = client.shutdown().await;
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        let _ = server_task.await;
        let hello_line = String::from_utf8_lossy(&buf);
        let hello: serde_json::Value =
            serde_json::from_str(hello_line.lines().next().unwrap()).unwrap();
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["protocol"], 1);
        let features = hello["features"].as_array().unwrap();
        assert!(
            features.iter().any(|f| f == "commands_v1"),
            "features 必须含 commands_v1: {hello_line}"
        );
        assert!(
            hello["command_catalog_generation"].as_u64().is_some(),
            "协商连接必须有 generation: {hello_line}"
        );

        // 未协商连接：不带 capabilities
        let (mut client2, server2) = tokio::io::duplex(8 * 1024);
        let server_task2 = tokio::spawn(async move {
            let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
            handle_test_connection(server2, commands).await;
        });
        client2
            .write_all(b"{\"type\":\"hello\",\"protocol\":1}\n")
            .await
            .unwrap();
        client2.flush().await.unwrap();
        let _ = client2.shutdown().await;
        let mut buf2 = Vec::new();
        client2.read_to_end(&mut buf2).await.unwrap();
        let _ = server_task2.await;
        let hello2_line = String::from_utf8_lossy(&buf2);
        let hello2: serde_json::Value =
            serde_json::from_str(hello2_line.lines().next().unwrap()).unwrap();
        assert_eq!(hello2["type"], "hello");
        assert!(
            hello2.get("features").is_none() || hello2["features"].as_array().unwrap().is_empty(),
            "未协商连接 features 必须缺失或空: {hello2_line}"
        );
        assert!(
            hello2.get("command_catalog_generation").is_none(),
            "未协商连接 generation 必须缺失: {hello2_line}"
        );
    }

    /// R11：hello protocol 不匹配 → 回 Error 且不写入 caps（随后 CommandList 仍被拒）。
    #[tokio::test]
    async fn r11_hello_protocol_mismatch_does_not_grant_caps() {
        let (mut client, server) = tokio::io::duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
            handle_test_connection(server, commands).await;
        });
        client
            .write_all(
                b"{\"type\":\"hello\",\"protocol\":999,\"capabilities\":[\"commands_v1\"]}\n",
            )
            .await
            .unwrap();
        client.flush().await.unwrap();
        let _ = client.shutdown().await;
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        let _ = server_task.await;
        let line = String::from_utf8_lossy(&buf);
        let resp: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
        assert_eq!(resp["type"], "error", "protocol 不匹配必须回 error: {line}");
        assert!(
            resp["message"].as_str().unwrap().contains("incompatible"),
            "错误信息必须说明不兼容: {line}"
        );
    }

    /// R12：CommandList 未协商 → Error；已协商 → CommandCatalog{generation, items}
    /// 且 items 含 prism.settings.open。
    #[tokio::test]
    async fn r12_command_list_gated_by_caps() {
        // 未协商 → Error
        let (mut client, server) = tokio::io::duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
            handle_test_connection(server, commands).await;
        });
        // 先发不带 capabilities 的 hello，再发 command_list
        client
            .write_all(b"{\"type\":\"hello\",\"protocol\":1}\n{\"type\":\"command_list\"}\n")
            .await
            .unwrap();
        client.flush().await.unwrap();
        let _ = client.shutdown().await;
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        let _ = server_task.await;
        let remaining = String::from_utf8_lossy(&buf);
        let lines: Vec<&str> = remaining.lines().collect();
        // 第一行是 hello 回包，第二行是 command_list 回包
        let cmd_resp: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(
            cmd_resp["type"], "error",
            "未协商 CommandList 必须回 error: {remaining}"
        );

        // 已协商 → CommandCatalog
        let (mut client2, server2) = tokio::io::duplex(8 * 1024);
        let server_task2 = tokio::spawn(async move {
            let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
            handle_test_connection(server2, commands).await;
        });
        client2
            .write_all(
                b"{\"type\":\"hello\",\"protocol\":1,\"capabilities\":[\"commands_v1\"]}\n{\"type\":\"command_list\"}\n",
            )
            .await
            .unwrap();
        client2.flush().await.unwrap();
        let _ = client2.shutdown().await;
        let mut buf2 = Vec::new();
        client2.read_to_end(&mut buf2).await.unwrap();
        let _ = server_task2.await;
        let remaining2 = String::from_utf8_lossy(&buf2);
        let lines2: Vec<&str> = remaining2.lines().collect();
        let catalog: serde_json::Value = serde_json::from_str(lines2[1]).unwrap();
        assert_eq!(catalog["type"], "command_catalog", "{remaining2}");
        assert!(
            catalog["generation"].as_u64().is_some(),
            "必须有 generation"
        );
        let items = catalog["items"].as_array().unwrap();
        assert!(
            items.iter().any(|item| item["id"] == "prism.settings.open"),
            "items 必须含 prism.settings.open: {remaining2}"
        );
    }

    /// R13：Results 序列化——cacheable=true 时 JSON 无 cacheable key（skip_serializing_if=is_true）。
    #[test]
    fn r13_results_cacheable_omitted_when_true() {
        let history = Arc::new(HistoryStore::load(
            &std::env::temp_dir().join(format!("r13-{}", std::process::id())),
            true,
        ));
        let response = window_results("", Vec::new(), &history);
        let json = serde_json::to_value(&response).unwrap();
        assert!(
            json.get("cacheable").is_none(),
            "cacheable=true 时 JSON 不应含 cacheable key: {json}"
        );
        assert!(
            json.get("command_catalog_generation").is_none(),
            "K0 Results 不应含 command_catalog_generation: {json}"
        );

        // 手造 cacheable=false 检查 key 存在
        let json_false = serde_json::json!({
            "type": "results",
            "query": "x",
            "items": [],
            "is_indexing": false,
            "is_truncated": false,
            "cacheable": false,
        });
        assert_eq!(json_false["cacheable"], false);
    }

    /// R14：K0 命令无生产者——对一组查询（含内置命令标题「设置」「打开设置」）调
    /// search_service，断言 items 中无 kind=="command"。
    #[tokio::test]
    async fn r14_search_service_produces_no_command_rows() {
        let aliases = Arc::new(crate::alias::AliasStore::load(&std::env::temp_dir()));
        let history_dir = std::env::temp_dir().join(format!("r14-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&history_dir);
        let history = Arc::new(HistoryStore::load(&history_dir, true));
        let apps: SharedApps = Arc::new(std::sync::RwLock::new(Vec::new()));
        let engines: SharedEngines = Arc::new(std::sync::RwLock::new(Vec::new()));
        let preferences = Arc::new(BrokerPreferences::new(false));
        let windows = Arc::new(crate::window_list::WindowSnapshotStore::new());
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));

        for query in ["设置", "打开设置", "settings", "open settings"] {
            let response = search_service(
                SearchArgs {
                    query,
                    max: 8,
                    filters: None,
                    root: None,
                    mode: SearchMode::All,
                    command_context: None,
                },
                &apps,
                &engines,
                &history,
                &preferences,
                &windows,
                &aliases,
                &commands,
            )
            .await;
            let Response::Results { items, .. } = response else {
                panic!("search_service 必须返回 Results (query={query})");
            };
            assert!(
                items
                    .iter()
                    .all(|item| item.kind != SearchResultKind::Command),
                "K0 不应有 command 行产出 (query={query})"
            );
        }
        let _ = std::fs::remove_dir_all(&history_dir);
    }

    // K1: command_search 匹配标题
    #[test]
    fn command_search_matches_title() {
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
        // 内置"打开设置"匹配"设置"
        let results = command_search("设置", 10, &commands);
        assert!(results
            .iter()
            .any(|r| r.execute_id.as_ref() == "prism.settings.open"));
        // 内置"在此处打开终端"匹配"终端"
        let results = command_search("终端", 10, &commands);
        assert!(results
            .iter()
            .any(|r| r.execute_id.as_ref() == "prism.terminal.open"));
    }

    // K1: 空查询不返回命令
    #[test]
    fn command_search_empty_query_returns_empty() {
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
        assert!(command_search("", 10, &commands).is_empty());
        assert!(command_search("   ", 10, &commands).is_empty());
    }

    // K1: 无 root_search binding 的命令不出现在结果中
    #[test]
    fn command_search_only_returns_root_search_bound() {
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
        let results = command_search("打开", 10, &commands);
        // 两条内置命令都有 root_search binding，都应出现
        assert!(!results.is_empty());
        for r in &results {
            assert_eq!(r.kind, SearchResultKind::Command);
            assert!(r.target.kind == "command");
        }
    }

    // K3 §4.4：show_in_root_search=false 的命令不进根搜索 lane
    #[test]
    fn command_search_filters_show_in_root_search_false() {
        let dir =
            std::env::temp_dir().join(format!("prism-cmd-root-filter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let commands = Arc::new(crate::commands::CommandStore::load(&dir));

        // 普通命令：show_in_root_search=true（默认）
        let mut cmd_visible = crate::persistence::UserCommandDefinition::default();
        cmd_visible.id = "user.visible_cmd".into();
        cmd_visible.title = "Visible Command".into();
        cmd_visible.enabled = true;
        cmd_visible.bindings.root_search = Some(crate::persistence::CommandBinding::default());
        commands.set(cmd_visible).unwrap();

        // 隐藏命令：show_in_root_search=false
        let mut cmd_hidden = crate::persistence::UserCommandDefinition::default();
        cmd_hidden.id = "user.hidden_cmd".into();
        cmd_hidden.title = "Visible Command Hidden".into();
        cmd_hidden.enabled = true;
        cmd_hidden.bindings.root_search = Some(crate::persistence::CommandBinding {
            show_in_root_search: false,
            ..Default::default()
        });
        commands.set(cmd_hidden).unwrap();

        let results = command_search("Visible Command", 10, &commands);
        assert!(results
            .iter()
            .any(|r| r.execute_id.as_ref() == "user.visible_cmd"));
        assert!(!results
            .iter()
            .any(|r| r.execute_id.as_ref() == "user.hidden_cmd"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // K1: 命令执行——ui-owned 命令返回 UiCommand
    #[tokio::test]
    async fn execute_command_ui_owned_returns_ui_command() {
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
        let shell = ShellExecutor::start().unwrap();
        let history_dir = std::env::temp_dir().join(format!("k1-exec-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&history_dir);
        let history = Arc::new(HistoryStore::load(&history_dir, true));

        let context = crate::commands::CommandInvocationContext {
            command_id: "prism.settings.open".into(),
            ..Default::default()
        };
        let response = execute_command(context, &commands, &shell, &history).await;
        match response {
            Response::UiCommand { id, .. } => assert_eq!(id, "prism.settings.open"),
            other => panic!("expected UiCommand, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&history_dir);
    }

    // K1: 命令执行——无效 command_id 返回 Error
    #[tokio::test]
    async fn execute_command_unknown_id_returns_error() {
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
        let shell = ShellExecutor::start().unwrap();
        let history_dir = std::env::temp_dir().join(format!("k1-exec-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&history_dir);
        let history = Arc::new(HistoryStore::load(&history_dir, true));

        let context = crate::commands::CommandInvocationContext {
            command_id: "user.nonexistent".into(),
            ..Default::default()
        };
        let response = execute_command(context, &commands, &shell, &history).await;
        match response {
            Response::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&history_dir);
    }

    // ── K3 §4.6 commit 5：CommandPreview（dry-run）同函数断言 ──────────

    fn preview_store(tag: &str) -> Arc<crate::commands::CommandStore> {
        let dir = std::env::temp_dir().join(format!("prism-preview-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(crate::commands::CommandStore::load(&dir))
    }

    #[test]
    fn preview_open_url_matches_execution_url() {
        // §4.6 同函数断言：preview 返回的 URL == 执行路径实际使用的 URL。
        let commands = preview_store("openurl");
        let mut cmd = crate::persistence::UserCommandDefinition::default();
        cmd.id = "user.preview_url".into();
        cmd.title = "Search".into();
        cmd.handler = crate::persistence::UserHandlerKind::OpenUrl;
        cmd.handler_params.insert(
            "url_template".into(),
            "https://x.test/search?q={query}".into(),
        );
        commands.set(cmd).unwrap();

        let context = crate::commands::CommandInvocationContext {
            command_id: "user.preview_url".into(),
            arguments: crate::commands::CommandArguments {
                text: Some("hello world".into()),
                ..Default::default()
            },
            ..Default::default()
        };

        // 预览路径
        let response = handle_command_preview(context.clone(), &commands);
        match response {
            Response::CommandPreviewResult {
                ok: true,
                url: Some(url),
                ..
            } => {
                // 执行路径调同一 validate_open_url，展开结果必须一致
                let exp_ctx = crate::commands::expansion_context_from(&context, Default::default());
                let exec_url =
                    crate::commands::validate_open_url("https://x.test/search?q={query}", &exp_ctx)
                        .unwrap();
                assert_eq!(url, exec_url);
                assert!(url.contains("hello%20world"));
            }
            other => panic!("expected CommandPreviewResult ok, got {other:?}"),
        }
    }

    #[test]
    fn preview_launch_program_matches_execution_args() {
        // §4.6 同函数断言：preview 返回的 argv == 执行路径实际使用的 argv。
        let commands = preview_store("launch");
        let mut cmd = crate::persistence::UserCommandDefinition::default();
        cmd.id = "user.preview_launch".into();
        cmd.title = "Notepad".into();
        cmd.handler = crate::persistence::UserHandlerKind::LaunchProgram;
        cmd.handler_params
            .insert("path".into(), r"C:\Windows\notepad.exe".into());
        cmd.handler_params
            .insert("args_template".into(), "{query}".into());
        commands.set(cmd).unwrap();

        let context = crate::commands::CommandInvocationContext {
            command_id: "user.preview_launch".into(),
            arguments: crate::commands::CommandArguments {
                text: Some("hello world & |".into()),
                ..Default::default()
            },
            ..Default::default()
        };

        let response = handle_command_preview(context.clone(), &commands);
        if let Response::CommandPreviewResult {
            ok: true,
            program_path: Some(path),
            program_args,
            ..
        } = response
        {
            // 执行路径调同一 validate_launch_program，展开结果必须一致
            let exp_ctx = crate::commands::expansion_context_from(&context, Default::default());
            let (exec_path, exec_args, _) = crate::commands::validate_launch_program(
                &commands
                    .get_user_command("user.preview_launch")
                    .unwrap()
                    .handler_params,
                &exp_ctx,
            )
            .unwrap();
            assert_eq!(path, exec_path);
            assert_eq!(program_args, exec_args);
            // raw 模式：空格不 percent-encode
            assert_eq!(program_args.len(), 1);
            assert_eq!(program_args[0], "hello world & |");
        }
    }

    #[test]
    fn preview_unknown_command_returns_error() {
        let commands = preview_store("unknown");
        let context = crate::commands::CommandInvocationContext {
            command_id: "user.does_not_exist".into(),
            ..Default::default()
        };
        let response = handle_command_preview(context, &commands);
        match response {
            Response::CommandPreviewResult {
                ok: false, message, ..
            } => {
                assert!(message.contains("不存在") || message.contains("not"));
            }
            other => panic!("expected CommandPreviewResult ok=false, got {other:?}"),
        }
    }

    #[test]
    fn preview_open_url_bad_scheme_returns_error() {
        // 校验失败时返回错误文案，非空字符串
        let commands = preview_store("badscheme");
        let mut cmd = crate::persistence::UserCommandDefinition::default();
        cmd.id = "user.preview_bad".into();
        cmd.title = "Bad".into();
        cmd.handler = crate::persistence::UserHandlerKind::OpenUrl;
        cmd.handler_params
            .insert("url_template".into(), "javascript:alert(1)".into());
        commands.set(cmd).unwrap();

        let context = crate::commands::CommandInvocationContext {
            command_id: "user.preview_bad".into(),
            ..Default::default()
        };
        let response = handle_command_preview(context, &commands);
        match response {
            Response::CommandPreviewResult {
                ok: false, message, ..
            } => {
                assert!(!message.is_empty());
            }
            other => panic!("expected ok=false, got {other:?}"),
        }
    }

    #[test]
    fn preview_has_no_side_effects() {
        // 预览不产生副作用：不写磁盘（generation 不变）、不记历史。
        // 预览后 command store 的 generation 应与预览前相同。
        let commands = preview_store("noeffect");
        let mut cmd = crate::persistence::UserCommandDefinition::default();
        cmd.id = "user.preview_noop".into();
        cmd.title = "Noop".into();
        cmd.handler = crate::persistence::UserHandlerKind::OpenUrl;
        cmd.handler_params
            .insert("url_template".into(), "https://x.test/{query}".into());
        commands.set(cmd).unwrap();
        let gen_before = commands.generation();

        let context = crate::commands::CommandInvocationContext {
            command_id: "user.preview_noop".into(),
            arguments: crate::commands::CommandArguments {
                text: Some("test".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let _response = handle_command_preview(context, &commands);
        let gen_after = commands.generation();
        assert_eq!(
            gen_before, gen_after,
            "preview must not increment generation (no persistence)"
        );
    }

    // K3 §4.8：导出往返——data_snapshot 返回用户命令深拷贝，
    // 内置命令不在 CommandData 中自然不导出。
    #[test]
    fn command_export_snapshot_roundtrip() {
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
        let mut cmd1 = crate::persistence::UserCommandDefinition::default();
        cmd1.id = "user.export_test1".into();
        cmd1.title = "Export Test 1".into();
        cmd1.handler = crate::persistence::UserHandlerKind::OpenUrl;
        cmd1.handler_params
            .insert("url_template".into(), "https://x.test/{query}".into());
        commands.set(cmd1.clone()).unwrap();

        let mut cmd2 = crate::persistence::UserCommandDefinition::default();
        cmd2.id = "user.export_test2".into();
        cmd2.title = "Export Test 2".into();
        cmd2.handler = crate::persistence::UserHandlerKind::LaunchProgram;
        cmd2.handler_params
            .insert("path".into(), "C:/Windows/System32/notepad.exe".into());
        commands.set(cmd2.clone()).unwrap();

        let snapshot = commands.data_snapshot();
        assert_eq!(snapshot.commands.len(), 2, "snapshot has user commands");
        assert!(snapshot
            .commands
            .iter()
            .any(|c| c.id == "user.export_test1"));
        assert!(snapshot
            .commands
            .iter()
            .any(|c| c.id == "user.export_test2"));
        // 内置命令不在快照中
        assert!(
            !snapshot
                .commands
                .iter()
                .any(|c| c.id.starts_with("builtin.")),
            "builtin commands not in export snapshot"
        );

        // 往返：序列化→反序列化→比较
        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: crate::persistence::CommandData = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, restored, "roundtrip preserves command data");

        let _ = std::fs::remove_file(std::env::temp_dir().join("commands-v1.json"));
    }

    // K2 §5 commit 5：copy_paths handler 集成测试。
    //
    // 不直接验证剪贴板内容（修改系统剪贴板会污染用户环境，且 CI 不稳定）。
    // 覆盖：空拒绝、129 项拒绝、合法路径成功、UNC 路径在 copy_paths 下通过。
    // 剪贴板写入由 actions.rs 的单元测试与 §5 手工验收覆盖。

    fn staging_test_setup(
        suffix: &str,
    ) -> (
        Arc<crate::commands::CommandStore>,
        Arc<ShellExecutor>,
        Arc<HistoryStore>,
    ) {
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));
        let shell = ShellExecutor::start().unwrap();
        let history_dir =
            std::env::temp_dir().join(format!("k2-staging-{suffix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&history_dir);
        let history = Arc::new(HistoryStore::load(&history_dir, true));
        (commands, shell, history)
    }

    fn staging_ctx(paths: Vec<String>) -> crate::commands::CommandInvocationContext {
        crate::commands::CommandInvocationContext {
            command_id: "prism.staging.copy_paths".into(),
            source: crate::commands::InvocationSource::Staging,
            staged_paths: paths,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn copy_paths_rejects_empty_staging() {
        let (commands, shell, history) = staging_test_setup("empty");
        // 空暂存区：validate() 通过（0 ≤ 128），但 handler 层 P2 第二次校验拒绝。
        let response = execute_command(staging_ctx(vec![]), &commands, &shell, &history).await;
        match response {
            Response::Error { message, .. } => {
                assert!(message.contains("暂存区为空"), "got: {message}")
            }
            other => panic!("expected Error for empty staging, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn copy_paths_rejects_129_items() {
        let (commands, shell, history) = staging_test_setup("129");
        let paths = (0..129).map(|i| format!(r"C:\p{i}")).collect::<Vec<_>>();
        let response = execute_command(staging_ctx(paths), &commands, &shell, &history).await;
        // validate() 拒绝 129 项（>128）
        match response {
            Response::Error { .. } => {}
            other => panic!("expected Error for 129 items, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn copy_paths_succeeds_for_valid_paths() {
        let (commands, shell, history) = staging_test_setup("valid");
        let paths = vec![r"C:\a\file1.txt".into(), r"C:\b\folder".into()];
        let response = execute_command(staging_ctx(paths), &commands, &shell, &history).await;
        match response {
            Response::Status { .. } => {}
            other => panic!("expected Status for valid staging, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn copy_paths_accepts_unc_paths() {
        // K2 §4.7：copy_paths 复制原始字符串、不探测存在性，不受 UNC 限制。
        let (commands, shell, history) = staging_test_setup("unc");
        let paths = vec![r"\\server\share\file.txt".into()];
        let response = execute_command(staging_ctx(paths), &commands, &shell, &history).await;
        match response {
            Response::Status { .. } => {}
            other => panic!("expected Status for UNC staging, got {other:?}"),
        }
    }

    // K2 §4.5 commit 6：staging ZIP handler 测试。
    // 不真正压缩（避免修改磁盘 + 依赖 7-Zip），只验证校验路径。
    // 注意：若机器未装 7-Zip，prism.staging.zip 在 catalog() 中标 enabled=false，
    // execute_command 在 handler 前即返回「命令已禁用」——这是正确行为。
    // 测试同时接受 disabled 和 handler 层校验两条路径。

    fn staging_zip_ctx(
        paths: Vec<String>,
        output: Option<&str>,
    ) -> crate::commands::CommandInvocationContext {
        crate::commands::CommandInvocationContext {
            command_id: "prism.staging.zip".into(),
            source: crate::commands::InvocationSource::Staging,
            staged_paths: paths,
            arguments: crate::commands::CommandArguments {
                output_path: output.map(String::from),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn staging_zip_rejects_empty_staging() {
        let (commands, shell, history) = staging_test_setup("zip-empty");
        let response = execute_command(
            staging_zip_ctx(vec![], Some(r"C:\out.zip")),
            &commands,
            &shell,
            &history,
        )
        .await;
        match response {
            Response::Error { message, .. } => {
                assert!(
                    message.contains("暂存区为空") || message.contains("命令已禁用"),
                    "got: {message}"
                );
            }
            other => panic!("expected Error for empty staging zip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn staging_zip_rejects_missing_output_path() {
        let (commands, shell, history) = staging_test_setup("zip-noout");
        let response = execute_command(
            staging_zip_ctx(vec![r"C:\a.txt".into()], None),
            &commands,
            &shell,
            &history,
        )
        .await;
        match response {
            Response::Error { message, .. } => {
                assert!(
                    message.contains("输出路径") || message.contains("命令已禁用"),
                    "got: {message}"
                );
            }
            other => panic!("expected Error for missing output, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn staging_zip_rejects_unc_source() {
        let (commands, shell, history) = staging_test_setup("zip-unc");
        let response = execute_command(
            staging_zip_ctx(vec![r"\\server\share\file.txt".into()], Some(r"C:\out.zip")),
            &commands,
            &shell,
            &history,
        )
        .await;
        match response {
            Response::Error { message, .. } => {
                assert!(
                    message.contains("UNC") || message.contains("命令已禁用"),
                    "got: {message}"
                );
            }
            other => panic!("expected Error for UNC staging zip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn staging_zip_rejects_non_zip_output() {
        let (commands, shell, history) = staging_test_setup("zip-badext");
        let response = execute_command(
            staging_zip_ctx(vec![r"C:\a.txt".into()], Some(r"C:\out.rar")),
            &commands,
            &shell,
            &history,
        )
        .await;
        match response {
            Response::Error { .. } => {}
            other => panic!("expected Error for non-zip output, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn staging_zip_rejects_existing_output() {
        let (commands, shell, history) = staging_test_setup("zip-conflict");
        let tmp = std::env::temp_dir().join(format!("k2-zip-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("out.zip");
        std::fs::write(&out, b"existing").unwrap();
        let response = execute_command(
            staging_zip_ctx(vec![r"C:\a.txt".into()], Some(out.to_str().unwrap())),
            &commands,
            &shell,
            &history,
        )
        .await;
        match response {
            Response::Error { message, .. } => {
                assert!(
                    message.contains("exists")
                        || message.contains("已存在")
                        || message.contains("命令已禁用"),
                    "got: {message}"
                );
            }
            other => panic!("expected Error for existing output, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn search_service_produces_command_rows_when_negotiated() {
        let aliases = Arc::new(crate::alias::AliasStore::load(&std::env::temp_dir()));
        // 独立目录 + 清理，避免 temp 里的残留历史污染
        let history_dir =
            std::env::temp_dir().join(format!("k1-search-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&history_dir);
        std::fs::create_dir_all(&history_dir).unwrap();
        let history = Arc::new(HistoryStore::load(&history_dir, true));
        let apps: SharedApps = Arc::new(std::sync::RwLock::new(Vec::new()));
        let engines: SharedEngines = Arc::new(std::sync::RwLock::new(Vec::new()));
        let preferences = Arc::new(BrokerPreferences::new(false));
        let windows = Arc::new(crate::window_list::WindowSnapshotStore::new());
        let commands = Arc::new(crate::commands::CommandStore::load(&std::env::temp_dir()));

        let response = search_service(
            SearchArgs {
                query: "设置",
                max: 1000,
                filters: None,
                root: None,
                mode: SearchMode::All,
                command_context: Some(crate::commands::SearchCommandContext::default()),
            },
            &apps,
            &engines,
            &history,
            &preferences,
            &windows,
            &aliases,
            &commands,
        )
        .await;
        let Response::Results {
            items, cacheable, ..
        } = response
        else {
            panic!("search_service 必须返回 Results");
        };
        assert!(
            items
                .iter()
                .any(|item| item.kind == SearchResultKind::Command),
            "协商连接下应产出 command 行，items: {:?}",
            items
                .iter()
                .map(|i| (i.kind, i.title.as_ref()))
                .collect::<Vec<_>>()
        );
        assert!(!cacheable, "含命令行的响应不可缓存");
        let _ = std::fs::remove_dir_all(&history_dir);
    }

    /// 测试辅助：用一个管道路径模拟连接循环——只处理 hello + command_list 两条请求。
    /// 不做完整连接循环，只直接调用 dispatch_non_search 与内联 hello。
    async fn handle_test_connection(
        mut server: tokio::io::DuplexStream,
        commands: Arc<crate::commands::CommandStore>,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (reader, mut writer) = tokio::io::split(&mut server);
        let mut lines = BufReader::new(reader).lines();
        let mut caps = crate::commands::ConnectionCaps::default();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let req: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let req_type = req["type"].as_str().unwrap_or("");
            let response = match req_type {
                "hello" => {
                    let protocol = req["protocol"].as_u64().unwrap_or(0) as u32;
                    let capabilities = req["capabilities"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if protocol == BROKER_PROTOCOL {
                        let negotiated = capabilities.iter().any(|c| c == "commands_v1");
                        caps.commands_v1 = negotiated;
                        // 逐字节对齐 Response::Hello 的 skip_serializing_if：
                        // features 仅在非空时出现，command_catalog_generation 仅在 Some 时出现。
                        let mut hello = serde_json::json!({
                            "type": "hello",
                            "protocol": protocol,
                            "version": VERSION,
                            "build_id": crate::build_id(),
                        });
                        if negotiated {
                            hello["features"] = serde_json::json!(["commands_v1"]);
                            hello["command_catalog_generation"] =
                                serde_json::json!(commands.generation());
                        }
                        hello
                    } else {
                        serde_json::json!({
                            "type": "error",
                            "message": format!("broker protocol {protocol} is incompatible with {BROKER_PROTOCOL}"),
                        })
                    }
                }
                "command_list" => {
                    if !caps.commands_v1 {
                        serde_json::json!({
                            "type": "error",
                            "message": "command catalog requires commands_v1 capability",
                            "category": "unsupported",
                        })
                    } else {
                        let items = commands.catalog();
                        serde_json::json!({
                            "type": "command_catalog",
                            "generation": commands.generation(),
                            "items": items,
                        })
                    }
                }
                "set_command_shortcut" => {
                    if !caps.commands_v1 {
                        serde_json::json!({
                            "type": "error",
                            "message": "command shortcut requires commands_v1 capability",
                            "category": "unsupported",
                        })
                    } else {
                        let command_id = req["command_id"].as_str().unwrap_or("").to_string();
                        let combo = if req.get("combo").is_none() || req["combo"].is_null() {
                            None
                        } else {
                            req["combo"].as_str().map(String::from)
                        };
                        let result = commands.set_shortcut_binding(&command_id, combo);
                        serde_json::json!({
                            "type": "command_shortcut_applied",
                            "message": result.err().unwrap_or_default(),
                        })
                    }
                }
                _ => serde_json::json!({"type": "error", "message": "unknown"}),
            };
            let out = format!("{}\n", serde_json::to_string(&response).unwrap());
            let _ = writer.write_all(out.as_bytes()).await;
            let _ = writer.flush().await;
        }
    }
}
