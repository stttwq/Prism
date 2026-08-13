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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    },
    /// 打开文件所在文件夹并选中。
    Reveal {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        target: Option<ActionTarget>,
    },
    /// 请求某文件的动作列表（→ 键动作面板）。
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
    ResolveWindow { target: ActionTarget },
    /// G5：WPF 激活成功后回报，broker 据此写窗口历史。失败路径不发本消息。
    RecordWindowSwitch { target: ActionTarget },
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
/// 管道服务主循环：创建管道实例 → 等待前端连接 → 交给连接处理器 →
/// 立刻建下一个实例等待重连。前端崩溃/重启不影响后端。
pub async fn serve(
    pipe_name: &str,
    apps: SharedApps,
    engines: SharedEngines,
    shell: Arc<ShellExecutor>,
    history: Arc<HistoryStore>,
    preferences: Arc<BrokerPreferences>,
) -> std::io::Result<()> {
    // first_pipe_instance 默认 true，确保本进程是该管道名的首个持有者。
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(pipe_name)?;

    // One snapshot store for the whole broker: window tokens must stay comparable across
    // reconnects, and it holds only the latest enumeration.
    let windows = Arc::new(crate::window_list::WindowSnapshotStore::new());

    loop {
        // 等待一个客户端连上当前实例。
        server.connect().await?;
        // 立刻为下一个客户端准备好新实例，再处理当前连接。
        let connected = server;
        server = ServerOptions::new().create(pipe_name)?;

        let apps = apps.clone();
        let engines = engines.clone();
        let shell = shell.clone();
        let history = history.clone();
        let preferences = preferences.clone();
        let windows = windows.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(
                connected,
                apps,
                engines,
                shell,
                history,
                preferences,
                windows,
            )
            .await
            {
                log(format!("连接处理结束：{e}"));
            }
        });
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
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
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

        let mut buf = serde_json::to_vec(&response).unwrap_or_else(|e| {
            format!("{{\"type\":\"error\",\"message\":\"序列化失败:{e}\"}}").into_bytes()
        });
        buf.push(b'\n');
        writer.write_all(&buf).await?;
        writer.flush().await?;
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
        Request::Execute { id, target } => {
            run_shell(
                shell,
                ShellOperation::Open(resolve_target(target, id, None)),
                history,
            )
            .await
        }
        Request::Reveal { id, target } => {
            run_shell(
                shell,
                ShellOperation::Reveal(resolve_target(target, id, Some(TargetKind::File))),
                history,
            )
            .await
        }
        Request::Actions { id, target } => {
            list_actions(resolve_target(target, id, Some(TargetKind::File)))
        }
        Request::RunAction { id, target, action, args } => {
            run_shell(
                shell,
                ShellOperation::RunAction {
                    target: resolve_target(target, id, Some(TargetKind::File)),
                    action,
                    args: args.unwrap_or_default(),
                    zip_program: preferences.zip_program(),
                },
                history,
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
            // An empty read-only search applies the capability choice without scanning:
            // disable releases the mmap/overlay, while enable loads the optional sidecar.
            let _ = indexer_client::search_with_options("", 1, None, pinyin_enabled).await;
            Response::Status {
                is_indexing: false,
                cancelled: false,
            }
        }
        Request::ClearHistory => match history.clear() {
            Ok(()) => Response::Status {
                is_indexing: false,
                cancelled: false,
            },
            Err(message) => Response::Error {
                message,
                category: None,
            },
        },
        Request::ResolveWindow { target } => {
            resolve_window(&target, windows, &crate::window_list::SystemWindowProbe)
        }
        Request::RecordWindowSwitch { target } => {
            record_window_switch(&target, windows, &crate::window_list::SystemWindowProbe, history)
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
fn record_window_switch(
    target: &ActionTarget,
    windows: &Arc<crate::window_list::WindowSnapshotStore>,
    probe: &dyn crate::window_list::WindowProbe,
    history: &Arc<HistoryStore>,
) -> Response {
    if target.kind != TargetKind::Window.as_str() {
        return Response::Error {
            message: "record_window_switch requires a window target".into(),
            category: Some(crate::shell::ShellErrorKind::TargetInvalid),
        };
    }
    match windows.resolve(&target.value, probe) {
        Ok(entry) => {
            let history_target = ActionTarget::new(TargetKind::Window, entry.history_key());
            if let Err(error) = history.record(&history_target, HistoryUse::Execute) {
                log(format!("窗口历史写入失败：{error}"));
            }
            Response::Status {
                is_indexing: false,
                cancelled: false,
            }
        }
        Err(error) => Response::Error {
            message: error.message().to_owned(),
            category: Some(crate::shell::ShellErrorKind::Conflict),
        },
    }
}

/// 一次搜索请求的查询部分（与共享状态分开传递，避免参数表无限膨胀）。
struct SearchArgs<'a> {
    query: &'a str,
    max: usize,
    filters: Option<Vec<SearchFilter>>,
    root: Option<&'a str>,
    mode: SearchMode,
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
    if let Err(message) = validate_search_request(max, filters.as_deref()) {
        return Response::Error {
            message,
            category: None,
        };
    }
    let filters = filters.filter(|values| !values.is_empty());
    // G5 window mode is exclusive: no files, apps, or web rows mixed in, and no indexer
    // round-trip. Checked before the empty-query branch because an empty window query is
    // meaningful (recent windows) while an empty global query is not.
    if mode == SearchMode::Window {
        return window_results(
            query,
            window_search(query, max, windows, history, preferences),
            history,
        );
    }
    // G4 empty input: host context shows recent file/dir under root from history only.
    // No apps, web, or full-index scan — empty indexer queries are meaningless and expensive.
    // Non-host empty input stays empty here: recent windows live in window mode (G5), which
    // returned above, not in an empty global query.
    if query.trim().is_empty() {
        return empty_query_results(query, max, filters.as_deref(), root, history);
    }
    let mut items = Vec::with_capacity(max.min(128));
    if let Ok(guard) = engines.read() {
        if let Some(hit) = websearch::try_match(query, guard.as_slice()) {
            items.push(hit.into_search_result());
        }
    }
    let result_slots = max.saturating_sub(items.len());
    let mut ranked = Vec::new();
    let exclusions = exclusion_paths(filters.as_deref());
    let history_weights = history.weights();
    let history_candidates = history_file_candidates(
        query,
        &history_weights,
        preferences.pinyin_enabled(),
        &exclusions,
        root,
    );
    let injected_history_targets: HashSet<_> = history_candidates
        .iter()
        .map(|candidate| {
            (
                candidate.target.kind.clone(),
                candidate.target.value.clone(),
            )
        })
        .collect();
    ranked.extend(history_candidates);
    let mut app_match_count = 0u64;
    // G4: a current-directory scope means "files under this root". Applications are not
    // scoped to a directory, so a root suppresses them entirely rather than leaking
    // global hits (e.g. Start Menu .lnk) into a scoped result list.
    if result_slots > 0 && root.is_none() {
        if let Ok(apps_guard) = apps.read() {
            let app_matches = crate::apps::search(&apps_guard, query, usize::MAX);
            app_match_count = app_matches.len() as u64;
            let mut literal_targets = std::collections::HashSet::new();
            for app in app_matches {
                literal_targets.insert(app.launch_path.clone());
                let target = ActionTarget::new(TargetKind::Application, app.launch_path.clone());
                let mut metadata = rank_title(&app.name, query);
                if let Some(metadata) = metadata.as_mut() {
                    metadata.history_score = history.score(&target);
                }
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
                    match_spans: match_spans(&app.name, query),
                    match_metadata: metadata,
                });
            }
            if preferences.pinyin_enabled() {
                for app in apps_guard.iter() {
                    if literal_targets.contains(&app.launch_path) {
                        continue;
                    }
                    let Some(matched) = crate::pinyin::match_name(&app.name, query) else {
                        continue;
                    };
                    let target =
                        ActionTarget::new(TargetKind::Application, app.launch_path.clone());
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
                        target: target.clone(),
                        match_spans: matched.spans,
                        match_metadata: Some(metadata),
                    });
                    app_match_count = app_match_count.saturating_add(1);
                }
            }
        }
    }
    let (service, root_rejection, root_message) = search_index_with_root_fallback(
        query,
        result_slots.max(1),
        filters.as_deref(),
        preferences.pinyin_enabled(),
        root,
    )
    .await;
    let (
        is_indexing,
        index_progress,
        index_error,
        index_generation,
        index_truncated,
        index_matched_count,
        scanned_nodes,
        name_candidates,
        entered_top_k,
        path_constructions,
        pinyin_status,
    ) = match service {
        Ok(reply) => {
            for item in reply.items {
                let target = ActionTarget::new(
                    if item.is_directory {
                        TargetKind::Directory
                    } else {
                        TargetKind::File
                    },
                    item.path.clone(),
                );
                if injected_history_targets.contains(&(target.kind.clone(), target.value.clone())) {
                    continue;
                }
                ranked.push(SearchResult {
                    kind: if item.is_directory {
                        SearchResultKind::Folder
                    } else {
                        SearchResultKind::File
                    },
                    title: item.name.clone(),
                    subtitle: item.path.clone(),
                    target: target.clone(),
                    execute_id: item.path,
                    match_spans: item
                        .match_spans
                        .unwrap_or_else(|| match_spans(&item.name, query)),
                    match_metadata: {
                        let mut metadata = item
                            .match_metadata
                            .or_else(|| rank_title(&item.name, query));
                        if let Some(metadata) = metadata.as_mut() {
                            metadata.history_score = history.score(&target);
                        }
                        metadata
                    },
                });
            }
            (
                reply.status.building || !reply.status.ready,
                reply
                    .status
                    .build_progress
                    .as_ref()
                    .map(|progress| Box::new(build_progress_dto(progress))),
                reply.status.message.filter(|_| reply.status.degraded),
                Some(reply.generation),
                reply.is_truncated,
                reply.matched_count,
                reply.scanned_nodes,
                reply.name_candidates,
                reply.entered_top_k,
                reply.path_constructions,
                reply.status.pinyin_status,
            )
        }
        Err(error) => (
            false,
            None,
            Some(error),
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
    };
    ranked.sort_by(compare_search_results);
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
fn empty_query_results(
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
            let mut items =
                history_file_candidates("", &history.weights(), false, &exclusions, Some(root));
            // Rank by history first; compare_search_results already prefers higher
            // history_score within the same match tier, which empty-query items share.
            items.sort_by(compare_search_results);
            items.truncate(max);
            items
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
/// the G2 title-match injection path is unchanged.
fn history_file_candidates(
    query: &str,
    weights: &[HistoryWeight],
    pinyin_enabled: bool,
    exclusions: &[String],
    root: Option<&str>,
) -> Vec<SearchResult> {
    let empty_query = query.is_empty();
    let root_normalized = root.map(normalize_path_prefix);
    let mut candidates = Vec::new();
    for weight in weights {
        if weight.target.kind != "file" && weight.target.kind != "directory" {
            continue;
        }
        if path_is_excluded(&weight.target.value, exclusions) {
            continue;
        }
        if let Some(root) = root_normalized.as_deref() {
            // Prefix boundary: `root` and `root\child` match, `rootOther` does not.
            if !path_is_under_root(&weight.target.value, root) {
                continue;
            }
        }
        // Empty-query recent list only shows paths that still exist (file or directory).
        if empty_query && !std::path::Path::new(&weight.target.value).exists() {
            continue;
        }
        let Some(title) = std::path::Path::new(&weight.target.value)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        let (metadata, match_spans) = if empty_query {
            // No literal query to rank against: same match tier for every row so history
            // score alone decides order (see MatchMetadata::cmp).
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
        } else if let Some(mut metadata) = rank_title(title, query) {
            metadata.history_score = weight.score;
            (metadata, match_spans(title, query))
        } else if pinyin_enabled {
            let Some(matched) = crate::pinyin::match_name(title, query) else {
                continue;
            };
            (pinyin_metadata(&matched, weight.score), matched.spans)
        } else {
            continue;
        };
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

fn normalize_path_prefix(value: &str) -> String {
    value
        .trim()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_owned()
}

/// True when `path` is `root` itself or a descendant (`root\...`), case-insensitive.
/// Sibling prefixes such as `C:\rootOther` must not match `C:\root`.
fn path_is_under_root(path: &str, root: &str) -> bool {
    let normalized = normalize_path_prefix(path);
    let root = normalize_path_prefix(root);
    if root.is_empty() {
        return false;
    }
    normalized.eq_ignore_ascii_case(&root)
        || normalized.get(root.len()..).is_some_and(|suffix| {
            suffix.starts_with('\\')
                && normalized
                    .get(..root.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&root))
        })
}

fn path_is_excluded(path: &str, exclusions: &[String]) -> bool {
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

    if let Some(mut metadata) = rank_title(&entry.title, query) {
        metadata.history_score = history_score;
        consider(metadata, match_spans(&entry.title, query));
    }
    if let Some(mut metadata) = rank_title(&entry.app_name, query) {
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
    rank_window_list(&published, query, max, history, preferences)
}

/// 排名部分与枚举分开，因为 `enumerate_and_publish` 直接打真实桌面，测试进不去。
/// 「历史 ∩ 当前枚举」这条约定就住在这里，不拆开的话它没法被断言。
fn rank_window_list(
    published: &[(String, crate::window_list::WindowEntry)],
    query: &str,
    max: usize,
    history: &Arc<HistoryStore>,
    preferences: &Arc<BrokerPreferences>,
) -> Vec<SearchResult> {
    let trimmed = query.trim();
    let mut ranked: Vec<SearchResult> = Vec::new();

    for (token, entry) in published {
        let history_target = ActionTarget::new(TargetKind::Window, entry.history_key());
        let history_score = history.score(&history_target);
        if trimmed.is_empty() {
            // 空输入只显示「用过 + 现在还在」的窗口，已关闭的自然不在枚举里。
            if history_score == 0 {
                continue;
            }
            ranked.push(window_result(
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
            ));
            continue;
        }
        if let Some((metadata, spans)) =
            rank_window(entry, trimmed, preferences.pinyin_enabled(), history_score)
        {
            ranked.push(window_result(entry, token, Some(metadata), spans));
        }
    }

    ranked.sort_by(compare_search_results);
    ranked.truncate(max);
    ranked
}

fn rank_title(title: &str, query: &str) -> Option<MatchMetadata> {
    let title_lower = title.to_lowercase();
    let query_lower = query.to_lowercase();
    let byte_position = title_lower.find(&query_lower)?;
    Some(MatchMetadata {
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
    })
}

fn compare_search_results(left: &SearchResult, right: &SearchResult) -> std::cmp::Ordering {
    left.match_metadata
        .cmp(&right.match_metadata)
        .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
        .then_with(|| left.subtitle.cmp(&right.subtitle))
        .then(left.kind.cmp(&right.kind))
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
) -> Response {
    let history_record = match &operation {
        ShellOperation::Open(target)
        | ShellOperation::Properties(target)
        | ShellOperation::OpenWith(target) => Some((target.clone(), HistoryUse::Execute)),
        ShellOperation::Reveal(target) => Some((target.clone(), HistoryUse::Reveal)),
        ShellOperation::RunAction { target, .. } => Some((target.clone(), HistoryUse::Execute)),
    };
    finish_shell_response(shell.execute(operation).await, history_record, history)
}

fn finish_shell_response(
    outcome: Result<ShellOutcome, ShellError>,
    history_record: Option<(ActionTarget, HistoryUse)>,
    history: &HistoryStore,
) -> Response {
    match outcome {
        Ok(ShellOutcome::Success) => {
            if let Some((target, usage)) = history_record {
                if history.record(&target, usage).is_err() {
                    log("history write failed");
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
fn match_spans(title: &str, query: &str) -> Vec<i32> {
    if query.is_empty() {
        return Vec::new();
    }
    let title_lower = title.to_lowercase();
    let query_lower = query.to_lowercase();
    let Some(byte_pos) = title_lower.find(&query_lower) else {
        return Vec::new();
    };
    // 字节偏移 → UTF-16 码元偏移。在小写串上计算，避免大小写改变码元数导致错位。
    let start_u16 = title_lower[..byte_pos].encode_utf16().count();
    let len_u16 = query_lower.encode_utf16().count();
    vec![start_u16 as i32, len_u16 as i32]
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
        let dir = std::env::temp_dir()
            .join(format!("prism-window-history-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(HistoryStore::load(&dir, true))
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

    #[test]
    fn record_writes_history_under_an_app_plus_title_key() {
        let store = Arc::new(WindowSnapshotStore::new());
        let history = history_store("record");
        let token = store.publish(vec![entry()])[0].0.clone();
        let response = record_window_switch(
            &ActionTarget::new(TargetKind::Window, &token),
            &store,
            &AlwaysLive(live_window()),
            &history,
        );
        assert!(matches!(response, Response::Status { .. }));

        let key = ActionTarget::new(TargetKind::Window, entry().history_key());
        assert!(history.score(&key) > 0);
        // The handle must not be what got persisted.
        let by_handle = ActionTarget::new(TargetKind::Window, "2304");
        assert_eq!(history.score(&by_handle), 0);
    }

    #[test]
    fn record_for_a_dead_window_does_not_write_success_history() {
        let store = Arc::new(WindowSnapshotStore::new());
        let history = history_store("dead");
        let token = store.publish(vec![entry()])[0].0.clone();
        let response = record_window_switch(
            &ActionTarget::new(TargetKind::Window, &token),
            &store,
            &AlwaysGone,
            &history,
        );
        assert!(matches!(response, Response::Error { .. }));
        let key = ActionTarget::new(TargetKind::Window, entry().history_key());
        assert_eq!(history.score(&key), 0);
    }

    #[test]
    fn record_for_a_stale_token_does_not_write_success_history() {
        let store = Arc::new(WindowSnapshotStore::new());
        let history = history_store("stale");
        let stale = store.publish(vec![entry()])[0].0.clone();
        store.publish(vec![entry()]);
        let response = record_window_switch(
            &ActionTarget::new(TargetKind::Window, &stale),
            &store,
            &AlwaysLive(live_window()),
            &history,
        );
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
            rank_window_list(&present, "", 20, &history, &preferences).len(),
            1,
            "control: while enumerated it is listed"
        );

        // 关掉后本次枚举为空，历史条目仍在磁盘上。
        let results = rank_window_list(&[], "", 20, &history, &preferences);
        assert!(
            results.is_empty(),
            "history must not resurrect a window that is gone"
        );
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

    #[test]
    fn legacy_and_typed_action_requests_both_decode() {
        let legacy: Request =
            serde_json::from_str(r#"{"type":"execute","id":"C:\\Windows\\explorer.exe"}"#).unwrap();
        assert!(matches!(
            legacy,
            Request::Execute {
                id: Some(_),
                target: None
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
                target: Some(ActionTarget { ref kind, .. })
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
        let weights = vec![HistoryWeight {
            target: ActionTarget::new(TargetKind::File, r"C:\late\zeta.txt"),
            score: 4,
        }];
        let mut candidates = history_file_candidates("ta", &weights, true, &[], None);
        candidates.push(SearchResult {
            kind: SearchResultKind::File,
            title: "beta.txt".into(),
            subtitle: r"C:\beta.txt".into(),
            execute_id: r"C:\beta.txt".into(),
            target: ActionTarget::new(TargetKind::File, r"C:\beta.txt"),
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

        assert!(
            history_file_candidates("ta", &weights, true, &[r"C:\late".into()], None).is_empty()
        );
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

        let weights = vec![
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &outside_str),
                score: 40,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &outside_same_str),
                score: 30,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &gone_str),
                score: 20,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &root_other_str),
                score: 18,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::File, &inside_str),
                score: 8,
            },
            HistoryWeight {
                target: ActionTarget::new(TargetKind::Directory, &folder_str),
                score: 12,
            },
            // The root directory itself is a valid empty-query hit.
            HistoryWeight {
                target: ActionTarget::new(TargetKind::Directory, &root_self_str),
                score: 6,
            },
            // Window history must never surface on the empty-query file path.
            HistoryWeight {
                target: ActionTarget::new(TargetKind::Window, "12345"),
                score: 99,
            },
        ];

        let mut candidates =
            history_file_candidates("", &weights, false, &[], Some(root_str.as_str()));
        candidates.sort_by(compare_search_results);
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
            "higher history score wins among empty-query rows"
        );
        assert!(
            !candidates
                .iter()
                .any(|item| item.execute_id.eq_ignore_ascii_case(&root_other_str)),
            "prefix-boundary: rootOther must not match root"
        );

        // Non-empty query must still use title matching and ignore the root filter arg
        // when callers pass None (G2 injection path).
        let named = history_file_candidates("inside", &weights, false, &[], None);
        assert!(named.iter().any(|item| item.execute_id == inside_str));
        assert!(
            named.iter().any(|item| item.execute_id == outside_same_str),
            "without a root filter, sibling trees still inject on title match"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_query_results_require_root_and_enabled_history() {
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

        let with_root = empty_query_results("", 8, None, Some(root_str.as_str()), &history);
        match with_root {
            Response::Results { items, .. } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].execute_id, file_str);
            }
            other => panic!("expected results, got {other:?}"),
        }

        let no_root = empty_query_results("", 8, None, None, &history);
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
        let disabled = empty_query_results("", 8, None, Some(root_str.as_str()), &history);
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

    #[test]
    fn only_successful_shell_outcomes_record_history() {
        let dir =
            std::env::temp_dir().join(format!("prism-ipc-history-outcomes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let history = HistoryStore::load(&dir, true);
        let target = ActionTarget::new(TargetKind::File, r"C:\history-outcome.txt");
        let record = || Some((target.clone(), HistoryUse::Execute));

        let cancelled = finish_shell_response(Ok(ShellOutcome::Cancelled), record(), &history);
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
        );
        assert!(matches!(failed, Response::Error { .. }));
        assert_eq!(history.score(&target), 0);

        let success = finish_shell_response(Ok(ShellOutcome::Success), record(), &history);
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
