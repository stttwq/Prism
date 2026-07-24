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
use crate::index::SharedIndex;
use crate::websearch::{self, WebEngine};
use crate::{log, VERSION};

/// 共享引擎列表（可热重载）。
/// 读路径：search 持读锁；写路径：reload_engines 换整表。
pub type SharedEngines = Arc<std::sync::RwLock<Vec<WebEngine>>>;

/// 前端发来的请求消息。`type` 字段区分类型（snake_case）。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// 连接自检：期望回 pong。
    Ping,
    /// 即时搜索。
    Search {
        query: String,
        #[serde(default = "default_max")]
        max: usize,
    },
    /// 执行选中项（打开文件 / 启动程序 / 打开网址）。
    Execute { id: String },
    /// 打开文件所在文件夹并选中。
    Reveal { id: String },
    /// 请求某文件的动作列表（→ 键动作面板）。
    Actions { id: String },
    /// 执行动作面板里的某个动作。
    RunAction { id: String, action: String },
    /// 设置页保存后热重载网页引擎列表（步骤 8）。
    ReloadEngines {
        engines: Vec<WebEngine>,
    },
}

fn default_max() -> usize {
    100
}
/// 后端回给前端的响应消息。
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// ping 的回应，附带后端版本供前端自检。
    Pong { version: String },
    /// 搜索结果列表（"展示更多"行由前端追加）。
    /// `is_indexing=true` 表示索引尚未就绪，items 可能为空。
    Results {
        query: String,
        items: Vec<SearchResult>,
        #[serde(default)]
        is_indexing: bool,
    },
    /// 动作面板列表。
    Actions { items: Vec<ActionItem> },
    /// 后端状态推送（如索引进行中）。
    Status { is_indexing: bool },
    /// 出错时回传，前端在列表区以单行提示展示。
    Error { message: String },
}

/// 单条搜索结果，字段对应 frontend-spec.md 第 2 节 `SearchResult` record。
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    /// "app" | "file" | "folder" | "web"（"more" 行由前端生成）。
    pub kind: String,
    pub title: String,
    pub subtitle: String,
    /// 回传后端用于 execute/reveal/actions 的标识。
    pub execute_id: String,
    /// 标题中要染蓝的区间，扁平数组 [start,len,start,len,...]。
    pub match_spans: Vec<i32>,
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
    index: SharedIndex,
    apps: SharedApps,
    engines: SharedEngines,
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

        let index = index.clone();
        let apps = apps.clone();
        let engines = engines.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(connected, index, apps, engines).await {
                log(format!("连接处理结束：{e}"));
            }
        });
    }
}

/// 单个连接的收发循环：逐行读入 JSON 请求，分发后逐行写回 JSON 响应。
async fn handle_connection(
    pipe: NamedPipeServer,
    index: SharedIndex,
    apps: SharedApps,
    engines: SharedEngines,
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
            Ok(req) => dispatch(req, &index, &apps, &engines),
            Err(e) => Response::Error {
                message: format!("无法解析请求：{e}"),
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
/// 请求分发。search 为纯读；execute/reveal 会调系统打开文件/URL（有副作用）。
pub fn dispatch(
    req: Request,
    index: &SharedIndex,
    apps: &SharedApps,
    engines: &SharedEngines,
) -> Response {
    match req {
        Request::Ping => Response::Pong {
            version: VERSION.to_string(),
        },
        Request::Search { query, max } => search(&query, max, index, apps, engines),
        Request::Execute { id } => execute_id(&id),
        Request::Reveal { id } => reveal_path(&id),
        Request::Actions { id } => list_actions(&id),
        Request::RunAction { id, action } => run_action(&id, &action),
        Request::ReloadEngines { engines: list } => reload_engines(list, engines),
    }
}

fn list_actions(id: &str) -> Response {
    match crate::actions::list_actions(id) {
        Ok(items) => Response::Actions { items },
        Err(message) => Response::Error { message },
    }
}

fn run_action(id: &str, action: &str) -> Response {
    match crate::actions::run_action(id, action) {
        Ok(()) => Response::Status { is_indexing: false },
        Err(message) => Response::Error { message },
    }
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
            Response::Status { is_indexing: false }
        }
        Err(_) => Response::Error {
            message: "无法更新网页引擎（锁被占用）".into(),
        },
    }
}

/// 执行选中项：http(s) URL 用默认浏览器打开；否则按文件/程序路径处理。
/// URL 不走文件路径校验（绝对路径检查会误拒 `https://...`）。
fn execute_id(id: &str) -> Response {
    if websearch::is_http_url(id) {
        return open_url(id.trim());
    }
    execute_path(id)
}

fn open_url(url: &str) -> Response {
    // 基础防护：空、NUL、非 http(s) 已在 is_http_url 过滤；此处再挡控制字符。
    if url.is_empty() || url.contains('\0') || url.chars().any(|c| c.is_control()) {
        return Response::Error {
            message: "网址无效".into(),
        };
    }
    match open_with_shell(url) {
        Ok(()) => {
            log(format!("打开网址：{url}"));
            Response::Status { is_indexing: false }
        }
        Err(e) => Response::Error {
            message: format!("无法打开网址：{e}"),
        },
    }
}

/// 用系统默认程序打开文件/文件夹。`id` 为完整路径（search 的 execute_id）。
fn execute_path(path: &str) -> Response {
    if let Err(e) = validate_path(path) {
        return Response::Error { message: e };
    }
    match open_with_shell(path) {
        Ok(()) => {
            log(format!("打开：{path}"));
            Response::Status { is_indexing: false }
        }
        Err(e) => Response::Error {
            message: format!("无法打开：{e}"),
        },
    }
}

/// 在资源管理器中打开所在文件夹并选中该文件。
fn reveal_path(path: &str) -> Response {
    if let Err(e) = validate_path(path) {
        return Response::Error { message: e };
    }
    match reveal_in_explorer(path) {
        Ok(()) => {
            log(format!("定位：{path}"));
            Response::Status { is_indexing: false }
        }
        Err(e) => Response::Error {
            message: format!("无法定位：{e}"),
        },
    }
}

/// 拒绝空路径、NUL、相对路径与可疑前缀，降低"任意 ShellExecute"风险。
/// 正常搜索结果的 execute_id 都是绝对路径；恶意/损坏客户端会被挡下。
/// 注意：http(s) URL 不经过此函数（见 `execute_id`）。
fn validate_path(path: &str) -> Result<(), String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("路径为空".into());
    }
    if path.contains('\0') {
        return Err("路径含非法字符".into());
    }
    let p = std::path::Path::new(path);
    if !p.is_absolute() {
        return Err("拒绝相对路径".into());
    }
    Ok(())
}

/// ShellExecuteW "open"：支持中文路径与 URL，走系统关联 / 默认浏览器。
#[cfg(windows)]
fn open_with_shell(path: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "open\0".encode_utf16().collect();

    // 返回值 > 32 表示成功（ShellExecute 历史约定）。
    let rc = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (rc.0 as isize) > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecute 失败，代码 {}", rc.0 as isize))
    }
}

#[cfg(not(windows))]
fn open_with_shell(path: &str) -> Result<(), String> {
    Err(format!("非 Windows 平台无法打开：{path}"))
}

/// explorer /select,"path" —— 打开所在文件夹并选中该文件；支持中文与空格路径。
#[cfg(windows)]
fn reveal_in_explorer(path: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    // 规范化为 Windows 反斜杠，explorer /select 对正斜杠偶发失效。
    let normalized = path.replace('/', "\\");
    // 整段作为单个 raw 参数，避免 CreateProcess 二次转义破坏引号。
    let arg = format!("/select,\"{normalized}\"");

    std::process::Command::new("explorer")
        .raw_arg(arg)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn reveal_in_explorer(path: &str) -> Result<(), String> {
    Err(format!("非 Windows 平台无法定位：{path}"))
}

/// 执行搜索：网页匹配（若有）置顶 → 程序 → 文件/文件夹。
///
/// 排序选择：关键词命中时 web 结果排在最前（用户明确输入了引擎前缀，意图就是搜网页），
/// 其后程序 > 文件（design.md）。索引未就绪时 is_indexing=true。
fn search(
    query: &str,
    max: usize,
    index: &SharedIndex,
    apps: &SharedApps,
    engines: &SharedEngines,
) -> Response {
    let max = max.max(1);
    let mut items: Vec<SearchResult> = Vec::with_capacity(max.min(128));

    // 0) 网页快捷搜索：关键词命中则插入一条 kind=web，置顶。
    // 持读锁拷一份列表引用期间的快照，避免 search 与 reload 互相阻塞过久。
    if let Ok(guard) = engines.read() {
        if let Some(hit) = websearch::try_match(query, guard.as_slice()) {
            items.push(hit.into_search_result());
        }
    }

    // 1) 程序清单（通常秒级就绪；未扫完时为空，不阻塞文件搜索）。
    if items.len() < max {
        if let Ok(apps_guard) = apps.read() {
            // 程序最多占结果的前半，至少留几条给文件；小 max 时程序可占满。
            let app_budget = if max <= 5 { max } else { (max / 2).max(5) };
            let remain_for_apps = max.saturating_sub(items.len()).min(app_budget);
            for a in crate::apps::search(&apps_guard, query, remain_for_apps) {
                if items.len() >= max {
                    break;
                }
                items.push(SearchResult {
                    kind: "app".into(),
                    title: a.name.clone(),
                    // 副标题：目标路径；解析失败时回退到 .lnk 路径。
                    subtitle: if a.target_path != a.launch_path {
                        a.target_path.clone()
                    } else {
                        a.launch_path.clone()
                    },
                    execute_id: a.launch_path.clone(),
                    match_spans: match_spans(&a.name, query),
                });
            }
        }
    }

    // 2) 文件索引。
    let (file_items, is_indexing) = match index.read() {
        Ok(guard) => match guard.as_ref() {
            Some(idx) => {
                let remain = max.saturating_sub(items.len());
                let files = idx
                    .search(query, remain)
                    .iter()
                    .map(|e| {
                        let name = idx.entry_name(e);
                        let path = idx.entry_path(e); // v3: 由 dir+name 拼出
                        SearchResult {
                            kind: if e.kind == 1 {
                                "folder".into()
                            } else {
                                "file".into()
                            },
                            title: name.to_string(),
                            subtitle: path.clone(),
                            execute_id: path,
                            match_spans: match_spans(name, query),
                        }
                    })
                    .collect::<Vec<_>>();
                (files, false)
            }
            None => (Vec::new(), true),
        },
        Err(_) => (Vec::new(), true),
    };
    items.extend(file_items);

    Response::Results {
        query: query.to_string(),
        items,
        is_indexing,
    }
}

/// 计算标题中匹配区间（不区分大小写），返回扁平数组 [start,len,...]，
/// start/len 以 UTF-16 码元计（与前端 C# string 索引一致）。
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
mod tests {
    use super::*;
    use crate::apps::AppEntry;
    use std::sync::{Arc, RwLock};

    fn empty_index() -> SharedIndex {
        Arc::new(RwLock::new(None))
    }

    fn empty_apps() -> SharedApps {
        Arc::new(RwLock::new(Vec::new()))
    }

    fn ready_index() -> SharedIndex {
        Arc::new(RwLock::new(Some(crate::index::FileIndex::default())))
    }

    fn default_engines() -> SharedEngines {
        Arc::new(RwLock::new(WebEngine::defaults()))
    }

    fn engines_of(list: Vec<WebEngine>) -> SharedEngines {
        Arc::new(RwLock::new(list))
    }

    fn sample_apps() -> SharedApps {
        Arc::new(RwLock::new(vec![AppEntry {
            name: "微信".into(),
            name_lower: "微信".into(),
            launch_path: r"C:\Users\x\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\微信.lnk"
                .into(),
            target_path: r"C:\Program Files\Tencent\WeChat\WeChat.exe".into(),
        }]))
    }

    fn parse(line: &str) -> Request {
        serde_json::from_str(line).expect("请求应能解析")
    }

    fn to_json(resp: &Response) -> serde_json::Value {
        serde_json::to_value(resp).expect("响应应能序列化")
    }

    #[test]
    fn ping_returns_pong_with_version() {
        let resp = dispatch(
            parse(r#"{"type":"ping"}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        assert_eq!(v["type"], "pong");
        assert_eq!(v["version"], VERSION);
    }

    #[test]
    fn search_echoes_query_with_empty_items() {
        let resp = dispatch(
            parse(r#"{"type":"search","query":"xyznope","max":100}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        assert_eq!(v["type"], "results");
        assert_eq!(v["query"], "xyznope");
        assert_eq!(v["items"].as_array().unwrap().len(), 0);
        assert_eq!(v["is_indexing"], true, "索引未就绪应标记 is_indexing");
    }

    #[test]
    fn search_ready_index_is_not_indexing() {
        let resp = dispatch(
            parse(r#"{"type":"search","query":"anything","max":10}"#),
            &ready_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        assert_eq!(v["type"], "results");
        assert_eq!(v["is_indexing"], false);
        assert_eq!(v["items"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn search_apps_come_first_with_kind_app() {
        let resp = dispatch(
            parse(r#"{"type":"search","query":"微信","max":10}"#),
            &ready_index(),
            &sample_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        assert_eq!(v["type"], "results");
        assert_eq!(v["is_indexing"], false);
        let items = v["items"].as_array().unwrap();
        assert!(!items.is_empty());
        assert_eq!(items[0]["kind"], "app");
        assert_eq!(items[0]["title"], "微信");
        assert!(items[0]["execute_id"]
            .as_str()
            .unwrap_or("")
            .ends_with(".lnk"));
    }

    #[test]
    fn search_web_keyword_comes_first() {
        let resp = dispatch(
            parse(r#"{"type":"search","query":"g 天气","max":10}"#),
            &ready_index(),
            &sample_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        let items = v["items"].as_array().unwrap();
        assert!(!items.is_empty());
        assert_eq!(items[0]["kind"], "web");
        assert!(items[0]["title"].as_str().unwrap_or("").contains("Google"));
        let url = items[0]["execute_id"].as_str().unwrap_or("");
        assert!(url.starts_with("https://www.google.com/search?q="));
        assert!(url.contains("%E5%A4%A9%E6%B0%94"));
    }

    #[test]
    fn search_bi_not_baidu() {
        let resp = dispatch(
            parse(r#"{"type":"search","query":"bi foo","max":5}"#),
            &ready_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        let items = v["items"].as_array().unwrap();
        assert_eq!(items[0]["kind"], "web");
        assert!(items[0]["title"].as_str().unwrap_or("").contains("Bing"));
        assert!(items[0]["execute_id"]
            .as_str()
            .unwrap_or("")
            .contains("bing.com"));
    }

    #[test]
    fn search_custom_engine_from_shared_list() {
        let engines = engines_of(vec![WebEngine {
            keyword: "gh".into(),
            name: "GitHub".into(),
            url_template: "https://github.com/search?q={q}".into(),
        }]);
        let resp = dispatch(
            parse(r#"{"type":"search","query":"gh prism","max":5}"#),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        let v = to_json(&resp);
        let items = v["items"].as_array().unwrap();
        assert_eq!(items[0]["kind"], "web");
        assert_eq!(
            items[0]["execute_id"],
            "https://github.com/search?q=prism"
        );
        // 预设 g 不在自定义列表中，不应命中。
        let resp2 = dispatch(
            parse(r#"{"type":"search","query":"g 天气","max":5}"#),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        let items2 = to_json(&resp2)["items"].as_array().unwrap().clone();
        assert!(
            items2.iter().all(|i| i["kind"] != "web"),
            "仅有自定义引擎时 g 不应出 web 结果"
        );
    }

    #[test]
    fn reload_engines_replaces_list_and_affects_search() {
        let engines = default_engines();
        // 先确认预设 g 可用。
        let before = dispatch(
            parse(r#"{"type":"search","query":"g 天气","max":5}"#),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        assert_eq!(to_json(&before)["items"][0]["kind"], "web");

        // 热重载为仅 GitHub。
        let resp = dispatch(
            parse(
                r#"{"type":"reload_engines","engines":[{"keyword":"gh","name":"GitHub","url_template":"https://github.com/search?q={q}"}]}"#,
            ),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        assert_eq!(to_json(&resp)["type"], "status");

        let after_g = dispatch(
            parse(r#"{"type":"search","query":"g 天气","max":5}"#),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        let items_g = to_json(&after_g)["items"].as_array().unwrap().clone();
        assert!(
            items_g.iter().all(|i| i["kind"] != "web"),
            "reload 后 g 不应再命中"
        );

        let after_gh = dispatch(
            parse(r#"{"type":"search","query":"gh prism","max":5}"#),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        assert_eq!(to_json(&after_gh)["items"][0]["kind"], "web");
        assert!(to_json(&after_gh)["items"][0]["execute_id"]
            .as_str()
            .unwrap_or("")
            .contains("github.com"));
    }

    #[test]
    fn reload_engines_empty_falls_back_to_defaults() {
        let engines = engines_of(vec![WebEngine {
            keyword: "only".into(),
            name: "Only".into(),
            url_template: "https://example.com?q={q}".into(),
        }]);
        let resp = dispatch(
            parse(r#"{"type":"reload_engines","engines":[]}"#),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        assert_eq!(to_json(&resp)["type"], "status");
        // 空列表回落 bi/b/g，g 应再次可用。
        let search = dispatch(
            parse(r#"{"type":"search","query":"g hi","max":3}"#),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        assert_eq!(to_json(&search)["items"][0]["kind"], "web");
        assert!(to_json(&search)["items"][0]["title"]
            .as_str()
            .unwrap_or("")
            .contains("Google"));
    }

    #[test]
    fn reload_engines_accepts_pascal_case_fields() {
        // 前端 PipeClient 发 Keyword/Name/UrlTemplate。
        let engines = default_engines();
        let resp = dispatch(
            parse(
                r#"{"type":"reload_engines","engines":[{"Keyword":"gh","Name":"GitHub","UrlTemplate":"https://github.com/search?q={q}"}]}"#,
            ),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        assert_eq!(to_json(&resp)["type"], "status");
        let search = dispatch(
            parse(r#"{"type":"search","query":"gh x","max":3}"#),
            &ready_index(),
            &empty_apps(),
            &engines,
        );
        assert_eq!(to_json(&search)["items"][0]["kind"], "web");
    }

    #[test]
    fn search_max_defaults_when_absent() {
        let req = parse(r#"{"type":"search","query":"a"}"#);
        assert!(
            matches!(req, Request::Search { max, .. } if max == 100),
            "max 缺省时应默认 100"
        );
    }

    #[test]
    fn unknown_message_is_error_not_panic() {
        let resp = match serde_json::from_str::<Request>(r#"{"type":"bogus"}"#) {
            Ok(req) => dispatch(req, &empty_index(), &empty_apps(), &default_engines()),
            Err(e) => Response::Error {
                message: format!("无法解析请求：{e}"),
            },
        };
        assert_eq!(to_json(&resp)["type"], "error");
    }

    #[test]
    fn execute_missing_path_returns_error() {
        let resp = dispatch(
            parse(r#"{"type":"execute","id":"Z:\\prism-no-such-file-xyz.dat"}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        assert_eq!(v["type"], "error");
        assert!(v["message"].as_str().unwrap_or("").contains("无法打开"));
    }

    #[test]
    fn execute_rejects_relative_path() {
        let resp = dispatch(
            parse(r#"{"type":"execute","id":"not\\absolute.txt"}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        assert_eq!(v["type"], "error");
        assert!(v["message"].as_str().unwrap_or("").contains("相对路径"));
    }

    #[test]
    fn execute_rejects_empty_path() {
        let resp = dispatch(
            parse(r#"{"type":"execute","id":""}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        assert_eq!(to_json(&resp)["type"], "error");
    }

    #[test]
    fn execute_https_url_not_rejected_as_relative_path() {
        // 不走 validate_path；在无 GUI/沙箱环境 ShellExecute 可能失败，
        // 但错误信息绝不能是「拒绝相对路径」。
        let resp = dispatch(
            parse(r#"{"type":"execute","id":"https://www.google.com/search?q=test"}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        let t = v["type"].as_str().unwrap_or("");
        assert!(t == "status" || t == "error", "unexpected type {t}");
        if t == "error" {
            let msg = v["message"].as_str().unwrap_or("");
            assert!(
                !msg.contains("相对路径"),
                "https URL 不应被路径校验拒绝：{msg}"
            );
            assert!(
                msg.contains("网址") || msg.contains("ShellExecute") || msg.contains("无法打开"),
                "错误应来自打开 URL 流程：{msg}"
            );
        }
    }

    #[test]
    fn reveal_missing_path_still_spawns_or_errors() {
        let resp = dispatch(
            parse(r#"{"type":"reveal","id":"Z:\\prism-no-such-file-xyz.dat"}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        let t = v["type"].as_str().unwrap_or("");
        assert!(t == "status" || t == "error", "unexpected type {t}");
    }

    #[test]
    fn actions_returns_basics_for_absolute_path() {
        let resp = dispatch(
            parse(r#"{"type":"actions","id":"C:\\Windows\\explorer.exe"}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        assert_eq!(v["type"], "actions");
        let items = v["items"].as_array().unwrap();
        assert!(items.iter().any(|i| i["id"] == "open_folder"));
        assert!(items.iter().any(|i| i["id"] == "copy"));
        assert!(items.iter().any(|i| i["id"] == "cut"));
        assert!(items.iter().any(|i| i["id"] == "copy_path"));
        assert_eq!(items.len(), 4, "第一版仅四条基础动作");
    }

    #[test]
    fn actions_rejects_relative_path() {
        let resp = dispatch(
            parse(r#"{"type":"actions","id":"relative.txt"}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        assert_eq!(to_json(&resp)["type"], "error");
    }

    #[test]
    fn run_action_unknown_is_error() {
        let resp = dispatch(
            parse(r#"{"type":"run_action","id":"C:\\Windows\\explorer.exe","action":"nope"}"#),
            &empty_index(),
            &empty_apps(),
            &default_engines(),
        );
        let v = to_json(&resp);
        assert_eq!(v["type"], "error");
        assert!(v["message"].as_str().unwrap_or("").contains("未知动作"));
    }
}
