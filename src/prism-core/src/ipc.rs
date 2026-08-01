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

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::apps::SharedApps;
use crate::hierarchy::MatchMetadata;
use crate::indexer_client;
use crate::indexer_ipc::{validate_search_request, BuildProgress, SearchFilter};
use crate::shell::{
    ActionTarget, ShellError, ShellExecutor, ShellOperation, ShellOutcome, TargetKind,
};
use crate::websearch::{self, WebEngine};
use crate::{log, VERSION};

pub const BROKER_PROTOCOL: u32 = 1;

/// 共享引擎列表（可热重载）。
/// 读路径：search 持读锁；写路径：reload_engines 换整表。
pub type SharedEngines = Arc<std::sync::RwLock<Vec<WebEngine>>>;

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
        tokio::spawn(async move {
            if let Err(e) = handle_connection(connected, apps, engines, shell).await {
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
            }) => search_service(&query, max, filters, &apps, &engines).await,
            Ok(req) => dispatch_non_search(req, &engines, &shell).await,
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
            )
            .await
        }
        Request::Reveal { id, target } => {
            run_shell(
                shell,
                ShellOperation::Reveal(resolve_target(target, id, Some(TargetKind::File))),
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
            )
            .await
        }
        Request::ReloadEngines { engines: list } => reload_engines(list, engines),
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
    let mut app_match_count = 0u64;
    if result_slots > 0 {
        if let Ok(apps_guard) = apps.read() {
            let app_matches = crate::apps::search(&apps_guard, query, usize::MAX);
            app_match_count = app_matches.len() as u64;
            for app in app_matches {
                ranked.push(SearchResult {
                    kind: SearchResultKind::App,
                    title: app.name.clone(),
                    subtitle: if app.target_path != app.launch_path {
                        app.target_path.clone()
                    } else {
                        app.launch_path.clone()
                    },
                    execute_id: app.launch_path.clone(),
                    target: ActionTarget::new(TargetKind::Application, app.launch_path.clone()),
                    match_spans: match_spans(&app.name, query),
                    match_metadata: rank_title(&app.name, query),
                });
            }
        }
    }
    let service =
        indexer_client::search_with_filters(query, result_slots.max(1), filters.as_deref()).await;
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
    ) = match service {
        Ok(reply) => {
            for item in reply.items {
                ranked.push(SearchResult {
                    kind: if item.is_directory {
                        SearchResultKind::Folder
                    } else {
                        SearchResultKind::File
                    },
                    title: item.name.clone(),
                    subtitle: item.path.clone(),
                    target: ActionTarget::new(
                        if item.is_directory {
                            TargetKind::Directory
                        } else {
                            TargetKind::File
                        },
                        item.path.clone(),
                    ),
                    execute_id: item.path,
                    match_spans: match_spans(&item.name, query),
                    match_metadata: item
                        .match_metadata
                        .or_else(|| rank_title(&item.name, query)),
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
    }
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
        class: if title_lower == query_lower {
            0
        } else if byte_position == 0 {
            1
        } else {
            2
        },
        position: title_lower[..byte_position].encode_utf16().count() as u32,
        score: title.encode_utf16().count() as u32,
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

async fn run_shell(shell: &Arc<ShellExecutor>, operation: ShellOperation) -> Response {
    match shell.execute(operation).await {
        Ok(ShellOutcome::Success) => Response::Status {
            is_indexing: false,
            cancelled: false,
        },
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

    #[tokio::test]
    async fn unknown_target_kind_is_rejected_without_execution() {
        let shell = ShellExecutor::start().unwrap();
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
