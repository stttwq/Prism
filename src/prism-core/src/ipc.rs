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
use crate::indexer_ipc::{exclusion_paths, validate_search_request, BuildProgress, SearchFilter};
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
}

impl BrokerPreferences {
    pub fn new(pinyin_enabled: bool) -> Self {
        Self {
            pinyin_enabled: AtomicBool::new(pinyin_enabled),
        }
    }

    fn pinyin_enabled(&self) -> bool {
        self.pinyin_enabled.load(Ordering::Acquire)
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
    },
    /// ping 的回应，附带后端版本供前端自检。
    Pong {
        version: String,
    },
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
    },
    /// 动作面板列表。
    Actions {
        items: Vec<ActionItem>,
    },
    /// 后端状态推送（如索引进行中）。
    Status {
        is_indexing: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        cancelled: bool,
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
        tokio::spawn(async move {
            if let Err(e) =
                handle_connection(connected, apps, engines, shell, history, preferences).await
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
            }) => {
                search_service(
                    &query,
                    max,
                    filters,
                    &apps,
                    &engines,
                    &history,
                    &preferences,
                )
                .await
            }
            Ok(req) => dispatch_non_search(req, &engines, &shell, &history, &preferences).await,
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
) -> Response {
    match req {
        Request::Hello { protocol } if protocol == BROKER_PROTOCOL => Response::Hello {
            protocol,
            version: VERSION.to_string(),
        },
        Request::Hello { protocol } => Response::Error {
            message: format!("broker protocol {protocol} is incompatible with {BROKER_PROTOCOL}"),
            category: None,
        },
        Request::Ping => Response::Pong {
            version: VERSION.to_string(),
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
        Request::RunAction { id, target, action } => {
            run_shell(
                shell,
                ShellOperation::RunAction {
                    target: resolve_target(target, id, Some(TargetKind::File)),
                    action,
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
        Request::Search { .. } => Response::Error {
            message: "search must be dispatched asynchronously".into(),
            category: None,
        },
    }
}

async fn search_service(
    query: &str,
    max: usize,
    filters: Option<Vec<SearchFilter>>,
    apps: &SharedApps,
    engines: &SharedEngines,
    history: &Arc<HistoryStore>,
    preferences: &Arc<BrokerPreferences>,
) -> Response {
    if let Err(message) = validate_search_request(max, filters.as_deref()) {
        return Response::Error {
            message,
            category: None,
        };
    }
    let filters = filters.filter(|values| !values.is_empty());
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
    if result_slots > 0 {
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
    let service = indexer_client::search_with_options(
        query,
        result_slots.max(1),
        filters.as_deref(),
        preferences.pinyin_enabled(),
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
    }
}

fn history_file_candidates(
    query: &str,
    weights: &[HistoryWeight],
    pinyin_enabled: bool,
    exclusions: &[String],
) -> Vec<SearchResult> {
    let mut candidates = Vec::new();
    for weight in weights {
        if weight.target.kind != "file" && weight.target.kind != "directory" {
            continue;
        }
        if path_is_excluded(&weight.target.value, exclusions) {
            continue;
        }
        let Some(title) = std::path::Path::new(&weight.target.value)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        let (metadata, match_spans) = if let Some(mut metadata) = rank_title(title, query) {
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

fn path_is_excluded(path: &str, exclusions: &[String]) -> bool {
    let normalized = path.replace('/', "\\");
    exclusions.iter().any(|excluded| {
        normalized.eq_ignore_ascii_case(excluded)
            || normalized.get(excluded.len()..).is_some_and(|suffix| {
                suffix.starts_with('\\')
                    && normalized
                        .get(..excluded.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(excluded))
            })
    })
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
        let mut candidates = history_file_candidates("ta", &weights, true, &[]);
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

        assert!(history_file_candidates("ta", &weights, true, &[r"C:\late".into()]).is_empty());
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
}
