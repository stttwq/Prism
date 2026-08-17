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

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::apps::SharedApps;
use crate::hierarchy::{MatchKind, MatchMetadata};
use crate::history::{HistoryDiagnostic, HistoryStore, HistoryUse, HistoryWeight};
use crate::indexer_client;
use crate::indexer_ipc::{
    exclusion_paths, requested_root, validate_search_request, BuildProgress, SearchFilter,
};
use crate::root_scope::RootRejection;
use crate::shell::{
    ActionTarget, ShellError, ShellExecutor, ShellOperation, ShellOutcome, TargetKind,
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

    fn pinyin_enabled(&self) -> bool {
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
/// 后端回给前端的响应消息。
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol: u32,
        version: String,
        /// 构建指纹（`0.1.0+release.<mtime>`），用于确认跑的是刚编出来的那份。
        /// 纯新增字段，旧前端忽略即可。
        build_id: String,
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
    /// 出错时回传，前端在列表区以单行提示展示。
    Error {
        message: String,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    /// "app" | "file" | "folder" | "web"（"more" 行由前端生成）。
    pub kind: SearchResultKind,
    pub title: String,
    pub subtitle: String,
    /// 回传后端用于 execute/reveal/actions 的标识。
    pub execute_id: String,
    /// Typed execution contract. `execute_id` remains for old readers only.
    pub target: ActionTarget,
    /// 标题中要染蓝的区间，扁平数组 [start,len,start,len,...]。
    pub match_spans: Vec<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_metadata: Option<MatchMetadata>,
}

/// 动作面板单项，字段对应 frontend-spec.md 第 2 节 `ActionItem` record。
#[derive(Debug, Clone, Serialize)]
pub struct ActionItem {
    /// "open_folder"|"copy"|"cut"|"copy_path"|"shell:<n>" 等。
    pub id: String,
    pub label: String,
    /// Segoe Fluent Icons 字形码，无图标传 ""。
    pub icon_glyph: String,
    pub has_submenu: bool,
    pub is_section_header: bool,
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
}

/// 管道服务主循环：创建管道实例 → 等待前端连接 → 交给连接处理器 →
/// 立刻建下一个实例等待重连。前端崩溃/重启不影响后端。
///
/// 实例重建失败不再让进程退出（审计 H3：此前任何一次 `create` 失败都会令
/// `serve` 上抛 → main 退出，活动连接全断），改为退避重试；仅当确认管道名被
/// 第三方进程抢注（名下已无任何实例仍被占用）时才视为致命。
pub async fn serve(
    pipe_name: &str,
    apps: SharedApps,
    engines: SharedEngines,
    shell: Arc<ShellExecutor>,
    history: Arc<HistoryStore>,
    preferences: Arc<BrokerPreferences>,
) -> std::io::Result<()> {
    // 首个实例带 first_pipe_instance(true)：创建失败说明管道名已被另一个 broker
    // 持有（真双实例），唯一正确动作是退出并让 main 上报。
    let first = ServerOptions::new()
        .first_pipe_instance(true)
        .create(pipe_name)?;

    // One snapshot store for the whole broker: window tokens must stay comparable across
    // reconnects, and it holds only the latest enumeration.
    let shared = Arc::new(BrokerShared {
        apps,
        engines,
        shell,
        history,
        preferences,
        windows: Arc::new(crate::window_list::WindowSnapshotStore::new()),
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
        match ServerOptions::new().create(pipe_name) {
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
    loop {
        if let Err(error) = server.connect().await {
            // armed 实例被本错误路径丢弃，计数必须同步减掉，否则抢注探测永远看不到 0。
            ownership.instances.fetch_sub(1, Ordering::Relaxed);
            return Err(error);
        }
        let connected = server;
        // 实例从 armed 转为 connected，仍归本进程持有，计数不变；
        // 连接任务结束时由其减 1。
        server = rearm_listener(&pipe_name, &ownership).await?;

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
            } = &*shared;
            let result = handle_connection(
                connected,
                apps.clone(),
                engines.clone(),
                shell.clone(),
                history.clone(),
                preferences.clone(),
                windows.clone(),
            )
            .await;
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
                return match ServerOptions::new()
                    .first_pipe_instance(true)
                    .create(pipe_name)
                {
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
        match ServerOptions::new().create(pipe_name) {
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
async fn handle_connection(
    pipe: NamedPipeServer,
    apps: SharedApps,
    engines: SharedEngines,
    shell: Arc<ShellExecutor>,
    history: Arc<HistoryStore>,
    preferences: Arc<BrokerPreferences>,
    windows: Arc<crate::window_list::WindowSnapshotStore>,
) -> std::io::Result<()> {
    log("前端已连接");
    let (reader, mut writer) = tokio::io::split(pipe);
    let mut lines = BoundedLineReader::new(reader);
    // 审计 P15：响应序列化缓冲在整条连接生命期内复用，每次响应不再新建 Vec。
    // 偶发的超大响应（"more" 模式 300 项）不长期占着容量：写完超过 `RESPONSE_BUFFER_KEEP`
    // 就缩回去。
    let mut out: Vec<u8> = Vec::with_capacity(8 * 1024);
    const RESPONSE_BUFFER_KEEP: usize = 256 * 1024;

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
                let response = Response::Error {
                    message,
                    category: None,
                };
                out.clear();
                if serde_json::to_writer(&mut out, &response).is_ok() {
                    out.push(b'\n');
                    let _ = writer.write_all(&out).await;
                    let _ = writer.flush().await;
                }
                log("入站请求行异常，断开连接");
                return Ok(());
            }
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Request>(line) {
            Ok(Request::Search {
                query,
                max,
                filters,
                root,
                mode,
            }) => {
                search_service(
                    SearchArgs {
                        query: &query,
                        max,
                        filters,
                        root: root.as_deref(),
                        mode: mode.unwrap_or_default(),
                    },
                    &apps,
                    &engines,
                    &history,
                    &preferences,
                    &windows,
                )
                .await
            }
            Ok(req) => {
                dispatch_non_search(req, &engines, &shell, &history, &preferences, &windows).await
            }
            Err(e) => Response::Error {
                message: format!("无法解析请求：{e}"),
                category: None,
            },
        };

        out.clear();
        if let Err(error) = serde_json::to_writer(&mut out, &response) {
            // 序列化中途失败：丢掉半截字节，回一条自造的错误行（与旧行为一致）。
            out.clear();
            out.extend_from_slice(
                format!("{{\"type\":\"error\",\"message\":\"序列化失败:{error}\"}}").as_bytes(),
            );
        }
        out.push(b'\n');
        writer.write_all(&out).await?;
        writer.flush().await?;
        if out.capacity() > RESPONSE_BUFFER_KEEP {
            out = Vec::with_capacity(8 * 1024);
        }
    }

    log("前端断开连接");
    Ok(())
}

async fn dispatch_non_search(
    req: Request,
    engines: &SharedEngines,
    shell: &Arc<ShellExecutor>,
    history: &Arc<HistoryStore>,
    preferences: &Arc<BrokerPreferences>,
    windows: &Arc<crate::window_list::WindowSnapshotStore>,
) -> Response {
    match req {
        Request::Hello { protocol } if protocol == BROKER_PROTOCOL => Response::Hello {
            protocol,
            version: VERSION.to_string(),
            build_id: crate::build_id(),
        },
        Request::Hello { protocol } => Response::Error {
            message: format!("broker protocol {protocol} is incompatible with {BROKER_PROTOCOL}"),
            category: None,
        },
        Request::Ping => Response::Pong {
            version: VERSION.to_string(),
            build_id: crate::build_id(),
        },
        Request::Execute { id, target, query } => {
            run_shell(
                shell,
                ShellOperation::Open(resolve_target(target, id, None)),
                history,
                query.as_deref().and_then(query_pick_key),
            )
            .await
        }
        Request::Reveal { id, target, query } => {
            run_shell(
                shell,
                ShellOperation::Reveal(resolve_target(target, id, Some(TargetKind::File))),
                history,
                query.as_deref().and_then(query_pick_key),
            )
            .await
        }
        Request::Actions { id, target } => {
            list_actions(resolve_target(target, id, Some(TargetKind::File)))
        }
        Request::RunAction {
            id,
            target,
            action,
            args,
            query,
        } => {
            run_shell(
                shell,
                ShellOperation::RunAction {
                    target: resolve_target(target, id, Some(TargetKind::File)),
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
            // history.clear() does synchronous file removal. Offload it to a blocking
            // thread so the tokio worker is not stalled.
            let history = history.clone();
            match tokio::task::spawn_blocking(move || history.clear()).await {
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
    }
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
    let known_prefixes = ["ext:", "path:"];
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

/// True when `filters` contains at least one `ext` or `path` entry (G7).
fn has_query_filters(filters: &[SearchFilter]) -> bool {
    filters
        .iter()
        .any(|f| f.field == "ext" || f.field == "path")
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
    let query_lower = name_query.to_lowercase();
    let mut literal_targets = std::collections::HashSet::new();
    for app in app_matches {
        literal_targets.insert(app.launch_path.clone());
        let target = ActionTarget::new(TargetKind::Application, app.launch_path.clone());
        let (metadata, spans) = match literal_match_lowered(&app.name, &query_lower) {
            Some((mut metadata, spans)) => {
                metadata.history_score = history.score(&target);
                (Some(metadata), spans)
            }
            None => (None, Vec::new()),
        };
        ranked.push(SearchResult {
            kind: SearchResultKind::App,
            title: app.name.clone(),
            subtitle: if app.target_path != app.launch_path {
                app.target_path.clone()
            } else {
                app.launch_path.clone()
            },
            execute_id: app.launch_path.clone(),
            target,
            match_spans: spans,
            match_metadata: metadata,
        });
    }
    if pinyin_enabled {
        // 查询归一化一次；清单侧的编码在扫描时就做好了。
        if let Some(normalized) = crate::pinyin::normalize_query(name_query) {
            for app in apps_guard.iter() {
                if literal_targets.contains(&app.launch_path) {
                    continue;
                }
                let Some(encoded) = app.pinyin.as_deref() else {
                    continue;
                };
                let Some(matched) =
                    crate::pinyin::match_compact_normalized(encoded, normalized.as_bytes())
                else {
                    continue;
                };
                let target = ActionTarget::new(TargetKind::Application, app.launch_path.clone());
                let metadata = pinyin_metadata(&matched, history.score(&target));
                ranked.push(SearchResult {
                    kind: SearchResultKind::App,
                    title: app.name.clone(),
                    subtitle: if app.target_path != app.launch_path {
                        app.target_path.clone()
                    } else {
                        app.launch_path.clone()
                    },
                    execute_id: app.launch_path.clone(),
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
    let query_lower = query.to_lowercase();
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
                match literal_match_lowered(&item.name, &query_lower) {
                    Some((metadata, spans)) => (Some(metadata), Some(spans)),
                    None => (None, None),
                }
            } else {
                (None, None)
            };
        ranked.push(SearchResult {
            kind: if item.is_directory {
                SearchResultKind::Folder
            } else {
                SearchResultKind::File
            },
            title: item.name.clone(),
            subtitle: item.path.clone(),
            target,
            execute_id: item.path,
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

async fn search_service(
    args: SearchArgs<'_>,
    apps: &SharedApps,
    engines: &SharedEngines,
    history: &Arc<HistoryStore>,
    preferences: &Arc<BrokerPreferences>,
    windows: &Arc<crate::window_list::WindowSnapshotStore>,
) -> Response {
    let SearchArgs {
        query,
        max,
        filters,
        root,
        mode,
    } = args;
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
    if let Err(message) = validate_search_request(max, all_filters.as_deref()) {
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
    let mut items = Vec::with_capacity(max.min(128));
    // G7: when ext:/path: filters are present, only files/folders are returned — no apps,
    // web, or window results mixed in.
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
                app_resolved_paths.insert(r.subtitle.clone());
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
        filters.as_deref(),
        preferences.pinyin_enabled(),
        root,
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
            query,
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
    // 查询记忆置顶：当前（规范化）查询串选中过的 target 在 kind 内、class 之前
    // 排最前——再次输入同样关键词，上次的选择就是第一条。仅全局搜索路径启用。
    let pick_key = query_pick_key(query);
    let pick_flags: Option<Vec<bool>> = pick_key.as_ref().map(|key| {
        ranked
            .iter()
            // P8：键已由 query_pick_key 归一化，直传避免逐条重复归一化分配。
            .map(|item| history.query_pick_by_key(&item.target, key))
            .collect()
    });
    sort_search_results_with_picks(&mut ranked, pick_flags.as_deref());
    let is_truncated = index_truncated || ranked.len() > result_slots;
    ranked.truncate(result_slots);
    items.extend(ranked);
    let matched_count = index_matched_count.map(|count| count.saturating_add(app_match_count));
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
    let (root, root_rejection, root_message) = match requested_root(root) {
        Ok(root) => (root, None, None),
        Err(reason) => (None, Some(reason), Some(reason.message().to_owned())),
    };

    let items = match root {
        Some(root) if history.is_enabled() => {
            let weights = history.weights();
            let root_owned = root.to_owned();
            // weights() 按 last_used 降序返回，候选保持该顺序（真 MRU）：
            // 收集满 max 个磁盘上仍存在的路径即停——既不再对全部条目做
            // 磁盘 stat，也不再按频次分数重排。
            tokio::task::spawn_blocking(move || {
                history_file_candidates("", &weights, false, &exclusions, Some(&root_owned), max)
            })
            .await
            .unwrap_or_default()
        }
        _ => Vec::new(),
    };

    Response::Results {
        // Echo the client query unchanged (may be "" or whitespace) so the frontend
        // sequence check still accepts the reply.
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
        root_rejection,
        root_message,
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
    limit: usize,
) -> Vec<SearchResult> {
    let empty_query = query.is_empty();
    // P9：查询只降幂一次，标题匹配的 metadata 与 spans 同源产出。
    let query_lower = query.to_lowercase();
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
        } else if let Some((mut metadata, spans)) = literal_match_lowered(title, &query_lower) {
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
        candidates.push(SearchResult {
            kind: if weight.target.kind == "directory" {
                SearchResultKind::Folder
            } else {
                SearchResultKind::File
            },
            title: title.to_owned(),
            subtitle: weight.target.value.clone(),
            execute_id: weight.target.value.clone(),
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

    // P9：查询只降幂一次，两个来源共用；metadata 与 spans 同源产出。
    let query_lower = query.to_lowercase();
    if let Some((mut metadata, spans)) = literal_match_lowered(&entry.title, &query_lower) {
        metadata.history_score = history_score;
        consider(metadata, spans);
    }
    if let Some((mut metadata, _)) = literal_match_lowered(&entry.app_name, &query_lower) {
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
    SearchResult {
        kind: SearchResultKind::Window,
        title: entry.title.clone(),
        subtitle: entry.app_name.clone(),
        // execute_id 对窗口没有旧读者语义，与 target.value 保持一致即可。
        execute_id: token.to_owned(),
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
/// 调用方还能把 `query_lower` 提到循环外，一次搜索只降幂一次查询。
///
/// UTF-16 偏移必须在**小写串**上算：大小写转换会改变码元数（如 'İ'），在原串上
/// 数偏移会与前端的高亮错位。
fn literal_match_lowered(title: &str, query_lower: &str) -> Option<(MatchMetadata, Vec<i32>)> {
    let title_lower = title.to_lowercase();
    let byte_position = title_lower.find(query_lower)?;
    let metadata = MatchMetadata {
        kind: MatchKind::Literal,
        class: if title_lower == query_lower {
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
    let spans = if query_lower.is_empty() {
        Vec::new()
    } else {
        vec![
            metadata.position as i32,
            query_lower.encode_utf16().count() as i32,
        ]
    };
    Some((metadata, spans))
}

/// 只要 metadata 的旧签名（测试沿用；生产路径一律走 `literal_match_lowered`）。
#[cfg(test)]
fn rank_title(title: &str, query: &str) -> Option<MatchMetadata> {
    literal_match_lowered(title, &query.to_lowercase()).map(|(metadata, _)| metadata)
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

/// `picks` 与 items 对齐：true = 该项被当前（规范化）查询串选中过，在
/// kind 层级内、class 之前排最前（查询记忆置顶）。标志只活在 broker 排序里，
/// 不进 MatchMetadata、不进序列化。kind 仍最先比较：拼音命中的 picked 项
/// 不能越过字面命中的未 picked 项。
///
/// 审计 P4：重建用 `Option::take` 按序取出，零 String clone、无环特例
/// （替代旧实现的全量 clone 重建）。
fn sort_search_results_with_picks(items: &mut Vec<SearchResult>, picks: Option<&[bool]>) {
    // Pre-compute lowercased titles once (O(n) allocations) and sort by index so the
    // comparator borrows from the cache instead of re-allocating per comparison.
    let lowercased: Vec<String> = items.iter().map(|item| item.title.to_lowercase()).collect();
    let mut indices: Vec<usize> = (0..items.len()).collect();
    indices.sort_by(|&a, &b| {
        metadata_kind(&items[a])
            .cmp(&metadata_kind(&items[b]))
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

fn resolve_target(
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
    literal_match_lowered(title, &query.to_lowercase())
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
        store.publish(vec![entry()]);
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
        let mut candidates = history_file_candidates("ta", &weights, true, &[], None, 8);
        candidates.push(SearchResult {
            kind: SearchResultKind::File,
            title: "beta.txt".into(),
            subtitle: beta_str.clone(),
            execute_id: beta_str.clone(),
            target: ActionTarget::new(TargetKind::File, &beta_str),
            match_spans: match_spans("beta.txt", "ta"),
            match_metadata: rank_title("beta.txt", "ta"),
        });
        candidates.sort_by(compare_search_results);
        assert_eq!(candidates[0].title, "zeta.txt");
        assert_eq!(
            candidates[0]
                .match_metadata
                .as_ref()
                .map(|metadata| metadata.history_score),
            Some(4)
        );

        // Exclusion path must use the real temp dir path.
        let exclusion = late_dir.to_str().unwrap().to_string();
        assert!(history_file_candidates("ta", &weights, true, &[exclusion], None, 8).is_empty());

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

        let candidates =
            history_file_candidates("", &weights, false, &[], Some(root_str.as_str()), 8);
        assert_eq!(
            candidates
                .iter()
                .map(|item| item.execute_id.as_str())
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
        let named = history_file_candidates("inside", &weights, false, &[], None, 8);
        assert!(named.iter().any(|item| item.execute_id == inside_str));
        assert!(
            named.iter().any(|item| item.execute_id == outside_same_str),
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
                assert_eq!(items[0].execute_id, file_str);
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
            items
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            ["literal", "full-history", "full-no-history", "initials"]
        );
    }

    /// 查询记忆置顶：picked 项在同 kind 内排最前（越过 class），但不能越过
    /// kind 层级（字面命中的未 picked 项仍先于拼音命中的 picked 项）。
    #[test]
    fn query_pick_promotes_within_kind_but_not_across() {
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
                .map(|entry| entry.title.as_str())
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
        assert_eq!(
            items
                .iter()
                .map(|entry| entry.title.as_str())
                .collect::<Vec<_>>(),
            ["picked", "plain", "pinyin"],
            "picked literal ranks first; picked pinyin must not cross the kind tier"
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
        assert_eq!(
            items
                .iter()
                .map(|entry| entry.title.as_str())
                .collect::<Vec<_>>(),
            ["exact", "substring", "pinyin"]
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

    /// 查询记忆键归一化：剥 `>` 前缀与过滤词，大小写/空白归一；
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
        let (_, rejection, message) =
            search_index_with_root_fallback("needle", 8, None, false, Some(&too_long)).await;
        assert_eq!(rejection, Some(RootRejection::TooLong));
        assert_eq!(message.as_deref(), Some(RootRejection::TooLong.message()));

        let (_, rejection, message) =
            search_index_with_root_fallback("needle", 8, None, false, None).await;
        assert_eq!(rejection, None, "a global search reports no rejection");
        assert_eq!(message, None);

        let (_, rejection, _) =
            search_index_with_root_fallback("needle", 8, None, false, Some("   ")).await;
        assert_eq!(rejection, None, "a blank root is a global search");
    }
}

#[cfg(test)]
mod query_parser_tests {
    use super::*;

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
}
