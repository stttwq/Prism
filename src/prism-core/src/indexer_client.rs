//! Broker → indexer client.
//!
//! Originally this opened a *fresh* named-pipe connection for every search
//! request (connect → hello → status → search → close).  Under high-frequency
//! typing the 2-worker-thread indexer runtime could not re-arm the pipe
//! listener fast enough, producing ``ERROR_PIPE_BUSY`` and 2-second timeouts
//! (measured 2026-08-07: 517/800 searches failed during a USN flood).
//!
//! The persistent connection below keeps a single long-lived pipe to the
//! indexer, lazily connecting on first use and reconnecting transparently
//! after a broken pipe.  All public entry points (``search_with_options``,
//! ``search_in_root``) reuse the same connection, so a search is now a single
//! round-trip (``status`` → ``search``) on a warm connection instead of a
//! three-step handshake on a cold one.
//!
//! Concurrency (audit batch 2, conservative M2): the singleton lock is only
//! ever acquired with ``try_lock``.  When a search is already in flight, the
//! next request does **not** queue behind it (the old 10 s queue manifested as
//! a frontend-wide freeze during slow indexer periods) — it degrades to a
//! one-off short connection for that single request.  The indexer service
//! arms 4 pipe listeners, so concurrent instances are natively supported; a
//! real connection pool is deferred to audit batch 9.
//!
//! The short-lived ``search_pipe`` helper is retained for tests that exercise
//! the wire protocol against a temporary pipe name.

use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::sync::Mutex;

use crate::indexer_ipc::{
    IndexerItem, IndexerRequest, IndexerResponse, IndexerStatus, SearchFilter,
};
use crate::root_scope::RootRejection;
use crate::{INDEXER_PIPE_NAME, INDEXER_PROTOCOL};

/// A failed indexer search.
///
/// `root_rejection` is only set when the service refused the *root*, never for transport
/// or protocol problems. Callers use it to fall back to a global search and tell the user
/// which scope was dropped, instead of showing an opaque error string.
///
/// `semantic` marks failures that came back as an `IndexerResponse::Error` — the
/// service answered; the request itself was refused (query too long, max out of
/// range, …). Retrying or reconnecting cannot change the answer, so the persistent
/// path must NOT drop the connection for these (复审 M 2026-08-21：语义错误
/// 曾被当传输错误处理，>4KB 查询每击键断连重连一次，持久连接机制失效)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFailure {
    pub message: String,
    pub root_rejection: Option<RootRejection>,
    semantic: bool,
}

impl SearchFailure {
    fn root(reason: RootRejection, message: String) -> Self {
        Self {
            message,
            root_rejection: Some(reason),
            semantic: false,
        }
    }

    /// The service answered with an Error response: a refusal, not a transport fault.
    fn semantic(message: String) -> Self {
        Self {
            message,
            root_rejection: None,
            semantic: true,
        }
    }
}

impl From<String> for SearchFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            root_rejection: None,
            semantic: false,
        }
    }
}

impl From<&str> for SearchFailure {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

impl std::fmt::Display for SearchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug)]
pub struct SearchReply {
    pub status: IndexerStatus,
    pub generation: u64,
    pub items: Vec<IndexerItem>,
    pub is_truncated: bool,
    pub matched_count: Option<u64>,
    pub scanned_nodes: Option<u64>,
    pub name_candidates: Option<u64>,
    pub entered_top_k: Option<u64>,
    pub path_constructions: Option<u64>,
}

// ---------------------------------------------------------------------------
// Persistent connection — the long-lived pipe to the indexer service.
// ---------------------------------------------------------------------------

/// 响应方向的单行上限（审计 P12）：与入站请求的 1MB 上限**不同**——一次
/// `max=1000` 的搜索返回上千条长路径，合法响应本来就会超过 1MB，复用 1MB 会
/// 把正常搜索误杀。8MB 足够容纳协议上限的结果集，同时仍拦住失控/恶意的无尽行。
const MAX_INDEXER_LINE_BYTES: usize = 8 * 1024 * 1024;

/// A long-lived connection to the indexer with its own reader/writer halves.
///
/// The connection is established lazily on first use and reused for every
/// subsequent search.  If the pipe breaks (indexer restart, OS error, etc.)
/// the holder is dropped and the next caller creates a fresh connection.
struct PersistentConnection {
    writer: tokio::io::WriteHalf<NamedPipeClient>,
    lines: crate::ipc::BoundedLineReader<tokio::io::ReadHalf<NamedPipeClient>>,
}

impl PersistentConnection {
    async fn connect(pipe_name: &str) -> Result<Self, String> {
        let pipe = connect_pipe(pipe_name).await?;
        let (reader, writer) = tokio::io::split(pipe);
        let lines = crate::ipc::BoundedLineReader::with_limit(reader, MAX_INDEXER_LINE_BYTES);
        Ok(Self { writer, lines })
    }

    /// Send a request and read one response line.  Named-pipe I/O is
    /// request/response paired, so callers must ensure they hold the
    /// connection lock for the full exchange.
    async fn exchange(&mut self, request: &IndexerRequest) -> Result<IndexerResponse, String> {
        let mut bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        self.writer
            .write_all(&bytes)
            .await
            .map_err(|error| error.to_string())?;
        self.writer
            .flush()
            .await
            .map_err(|error| error.to_string())?;

        let line = self
            .lines
            .next_line()
            .await?
            .ok_or("indexer service closed the connection")?;
        serde_json::from_str(&line).map_err(|error| format!("invalid indexer response: {error}"))
    }
}

/// Process-wide singleton connection guarded by a tokio Mutex.
static INDEXER_CONNECTION: OnceLock<Mutex<Option<PersistentConnection>>> = OnceLock::new();

fn connection_lock() -> &'static Mutex<Option<PersistentConnection>> {
    INDEXER_CONNECTION.get_or_init(|| Mutex::new(None))
}

/// Total budget for one search sequence (connect + handshake + search + retry).
/// A live-but-unresponsive indexer service must never pin a request forever.
/// Generous against normal exchanges (milliseconds) plus the indexer's
/// worst-case write-lock stall during name-pool compaction (seconds).
const REQUEST_BUDGET: Duration = Duration::from_secs(8);

/// Connect to the indexer pipe, retrying a few times to ride out the
/// microsecond gap where the server is re-arming a listener slot.
async fn connect_pipe(pipe_name: &str) -> Result<NamedPipeClient, String> {
    let mut last_error = None;
    for _ in 0..5 {
        match ClientOptions::new().open(pipe_name) {
            Ok(pipe) => return Ok(pipe),
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    Err(format!(
        "indexer service is unavailable: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "unknown connection error".into())
    ))
}

/// Handshake: exchange Hello messages to verify protocol compatibility.
async fn handshake(conn: &mut PersistentConnection) -> Result<(), String> {
    match conn
        .exchange(&IndexerRequest::Hello {
            protocol: INDEXER_PROTOCOL,
        })
        .await?
    {
        IndexerResponse::Hello { protocol } if protocol == INDEXER_PROTOCOL => Ok(()),
        IndexerResponse::Error { message } => Err(message),
        _ => Err("indexer service returned an invalid hello response".into()),
    }
}

/// Run a search, preferring the warm persistent connection.
///
/// The singleton lock is taken with `try_lock` only (audit batch 2, M2
/// conservative): if a search is already in flight, this request degrades to
/// a one-off short connection instead of queueing — the former 10 s queue
/// turned any slow indexer period into a frontend-wide freeze.  Both paths are
/// time-bounded by `REQUEST_BUDGET`; a hung indexer degrades this one request
/// into an error, never blocks subsequent searches.
async fn search_via_persistent(
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    match connection_lock().try_lock() {
        Ok(conn) => search_on_persistent(conn, query, max, filters, pinyin_enabled, root).await,
        Err(_contention) => {
            search_one_off(INDEXER_PIPE_NAME, query, max, filters, pinyin_enabled, root).await
        }
    }
}

/// The lock-held path: lazy connect + handshake + (status → search) with one
/// transport-level reconnect retry.
async fn search_on_persistent(
    mut conn: tokio::sync::MutexGuard<'static, Option<PersistentConnection>>,
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    let request = async {
        // Lazy connect + handshake on first use, or after a prior drop.
        let needs_reconnect = conn.is_none();
        if needs_reconnect {
            let mut new_conn = PersistentConnection::connect(INDEXER_PIPE_NAME).await?;
            handshake(&mut new_conn).await?;
            *conn = Some(new_conn);
        }

        // First attempt on the (possibly fresh) connection.
        match search_on_connection(
            conn.as_mut().unwrap(),
            query,
            max,
            filters,
            pinyin_enabled,
            root,
        )
        .await
        {
            Ok(reply) => Ok(reply),
            // Transport-level failure: drop the connection, reconnect, retry once.
            // 语义错误（semantic / root_rejection）不在其列——服务已明确拒绝
            // 该请求，重连重试只会得到同一答案并白白丢弃健康连接。
            Err(failure)
                if failure.root_rejection.is_none() && !failure.semantic =>
            {
                *conn = None; // drop the broken connection
                let mut new_conn = PersistentConnection::connect(INDEXER_PIPE_NAME).await?;
                handshake(&mut new_conn).await?;
                let reply =
                    search_on_connection(&mut new_conn, query, max, filters, pinyin_enabled, root)
                        .await?;
                *conn = Some(new_conn);
                Ok(reply)
            }
            // Semantic responses (root rejection / Error) propagate as-is.
            Err(failure) => Err(failure),
        }
    };

    match tokio::time::timeout(REQUEST_BUDGET, request).await {
        Ok(result) => result,
        Err(_elapsed) => {
            // Cancelled mid-exchange: the connection may sit on a half-read
            // response line — it must never be reused.
            *conn = None;
            Err(SearchFailure::from("indexer request timed out".to_string()))
        }
    }
}

/// AUDIT-2026-08-18 R-A7: 一次性降级连接的并发闸。
/// 此前锁竞争降级路径无全局上限——每个并发搜索各开一条到 indexer 的短连接，
/// 突发竞争时可同时打出几十条连接（每条占服务端任务 + 行缓冲）。许可数 2：
/// 正常情况下该路径本身就是罕见的竞争兜底，排队等待即可。
/// 复审 M（2026-08-21 全仓重审）：排队等闸计入 REQUEST_BUDGET——acquire 原先
/// 在 timeout 之外，索引器慢期排队任务的实际等待无界（8s×任务数/2）。
/// 超时放弃时 RAII 许可随 future 一起 drop，无泄漏。
static ONE_OFF_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// R-A7 测试观测用：当前持有一次性连接闸的在途请求数。与闸许可数一致，
/// 单独设原子是因为 server 侧观测会受客户端断开后的 EOF 滞留干扰而虚高。
static ONE_OFF_INFLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 峰值观测（R-A7 测试断言用）：峰值 ≤ 闸许可数即并发未超标。
static ONE_OFF_PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The contention path: a one-off connection (connect → hello → status →
/// search → drop) that never touches the process-wide singleton.  The pipe
/// name is a parameter so tests can exercise it against a temporary pipe.
async fn search_one_off(
    pipe_name: &str,
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    // 闸排队与实际请求同在 REQUEST_BUDGET 内（见 ONE_OFF_GATE 注释）。
    let attempt = async {
        let _permit = ONE_OFF_GATE
            .acquire()
            .await
            .map_err(|_| SearchFailure::from("one-off gate closed".to_string()))?;
        // 拿到许可后才计在途：排队等闸的请求不计，峰值才等于真实并发连接数。
        let _inflight = Inflight::track();
        let mut conn = PersistentConnection::connect(pipe_name).await?;
        handshake(&mut conn).await?;
        search_on_connection(&mut conn, query, max, filters, pinyin_enabled, root).await
    };
    tokio::time::timeout(REQUEST_BUDGET, attempt)
        .await
        .map_err(|_| SearchFailure::from("indexer request timed out".to_string()))?
}

/// 在途计数 RAII：构造 +1，drop -1，供闸测试观测峰值。
struct Inflight {
    _private: (),
}

impl Inflight {
    fn track() -> Self {
        use std::sync::atomic::Ordering;
        let current = ONE_OFF_INFLIGHT.fetch_add(1, Ordering::AcqRel) + 1;
        ONE_OFF_PEAK.fetch_max(current, Ordering::AcqRel);
        Self { _private: () }
    }
}

impl Drop for Inflight {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        ONE_OFF_INFLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Run a full search sequence (status → search) on an established connection.
async fn search_on_connection(
    conn: &mut PersistentConnection,
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    let status = match conn
        .exchange(&IndexerRequest::Status)
        .await
        .map_err(SearchFailure::from)?
    {
        IndexerResponse::Status(status) => status,
        IndexerResponse::Error { message } => return Err(SearchFailure::semantic(message)),
        _ => return Err("indexer service returned an invalid status response".into()),
    };
    let mut status = status;
    // Only a completely unready index short-circuits. `ready && building` — a first build
    // that has published some volumes — must still issue the Search so the volumes that
    // are already indexed return results.
    if !status.ready {
        return Ok(SearchReply {
            generation: status.generation,
            status,
            items: Vec::new(),
            is_truncated: false,
            matched_count: Some(0),
            scanned_nodes: Some(0),
            name_candidates: Some(0),
            entered_top_k: Some(0),
            path_constructions: Some(0),
        });
    }

    match conn
        .exchange(&IndexerRequest::Search {
            query: query.to_owned(),
            max,
            filters: filters.map(ToOwned::to_owned),
            pinyin_enabled: Some(pinyin_enabled),
            root: root.map(ToOwned::to_owned),
        })
        .await
        .map_err(SearchFailure::from)?
    {
        IndexerResponse::Results {
            generation,
            items,
            is_truncated,
            matched_count,
            scanned_nodes,
            name_candidates,
            entered_top_k,
            path_constructions,
            pinyin_status,
        } => {
            if pinyin_status.is_some() {
                status.pinyin_status = pinyin_status;
            }
            Ok(SearchReply {
                status,
                generation,
                items,
                is_truncated,
                matched_count,
                scanned_nodes,
                name_candidates,
                entered_top_k,
                path_constructions,
            })
        }
        IndexerResponse::Error { message } => Err(SearchFailure::semantic(message)),
        IndexerResponse::RootUnavailable { reason, message } => {
            // The reason travels as a value, not as prose: the broker turns it back into a
            // structured field so the UI can explain the fallback instead of guessing.
            Err(SearchFailure::root(reason, message))
        }
        _ => Err("indexer service returned an invalid search response".into()),
    }
}

// ---------------------------------------------------------------------------
// Public API — unchanged signatures, now backed by the persistent connection.
// ---------------------------------------------------------------------------

pub async fn search(query: &str, max: usize) -> Result<SearchReply, String> {
    search_with_options(query, max, None, false).await
}

pub async fn search_with_filters(
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
) -> Result<SearchReply, String> {
    search_with_options(query, max, filters, false).await
}

pub async fn search_with_options(
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
) -> Result<SearchReply, String> {
    search_via_persistent(query, max, filters, pinyin_enabled, None)
        .await
        .map_err(|failure| failure.message)
}

/// Search restricted to a current-directory root. `None` is an ordinary global search;
/// a root the service cannot use comes back as [`SearchFailure::root_rejection`] so the
/// caller can retry globally and report the exact degradation reason.
pub async fn search_in_root(
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    search_via_persistent(query, max, filters, pinyin_enabled, root).await
}

/// Apply the pinyin enabled preference to the indexer service via an explicit
/// management command (audit M4+M6). Replaces the old empty-query side-channel
/// that abused the Search request's `pinyin_enabled` field. Uses the same warm
/// persistent connection as searches, bounded by `REQUEST_BUDGET`. Errors are
/// returned (not swallowed) so the broker can log them.
pub async fn set_pinyin_enabled(enabled: bool) -> Result<(), String> {
    let conn = connection_lock().try_lock();
    match conn {
        Ok(mut conn) => {
            let request = async {
                let needs_reconnect = conn.is_none();
                if needs_reconnect {
                    let mut new_conn = PersistentConnection::connect(INDEXER_PIPE_NAME).await?;
                    handshake(&mut new_conn).await?;
                    *conn = Some(new_conn);
                }
                let response = conn
                    .as_mut()
                    .unwrap()
                    .exchange(&IndexerRequest::SetPinyinEnabled { enabled })
                    .await?;
                match response {
                    IndexerResponse::Status(_) => Ok(()),
                    IndexerResponse::Error { message } => Err(message),
                    _ => Err("unexpected response to SetPinyinEnabled".into()),
                }
            };
            match tokio::time::timeout(REQUEST_BUDGET, request).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    *conn = None;
                    Err("indexer SetPinyinEnabled request timed out".into())
                }
            }
        }
        Err(_contention) => {
            // A search is in flight on the persistent connection. Fall back to a
            // one-off connection so the preference is applied immediately rather
            // than queued behind an in-flight search.
            set_pinyin_enabled_one_off(enabled).await
        }
    }
}

async fn set_pinyin_enabled_one_off(enabled: bool) -> Result<(), String> {
    let request = async {
        let mut conn = PersistentConnection::connect(INDEXER_PIPE_NAME).await?;
        handshake(&mut conn).await?;
        let response = conn
            .exchange(&IndexerRequest::SetPinyinEnabled { enabled })
            .await?;
        match response {
            IndexerResponse::Status(_) => Ok(()),
            IndexerResponse::Error { message } => Err(message),
            _ => Err("unexpected response to SetPinyinEnabled".into()),
        }
    };
    match tokio::time::timeout(REQUEST_BUDGET, request).await {
        Ok(result) => result,
        Err(_elapsed) => Err("indexer SetPinyinEnabled request timed out".into()),
    }
}

// ---------------------------------------------------------------------------
// Short-lived client — retained for tests that exercise the wire protocol
// against a temporary pipe name (the persistent singleton is process-wide
// and always targets the real INDEXER_PIPE_NAME).
// ---------------------------------------------------------------------------

/// 2-second timeout wrapping the whole short-lived request.
#[cfg(test)]
async fn search_pipe(
    pipe_name: &str,
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    tokio::time::timeout(
        Duration::from_secs(2),
        search_pipe_inner(pipe_name, query, max, filters, pinyin_enabled, root),
    )
    .await
    .map_err(|_| SearchFailure::from("indexer service request timed out"))?
}

#[cfg(test)]
async fn search_pipe_inner(
    pipe_name: &str,
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    let pipe = connect_pipe(pipe_name).await?;
    let (reader, mut writer) = tokio::io::split(pipe);
    let mut lines = crate::ipc::BoundedLineReader::with_limit(reader, MAX_INDEXER_LINE_BYTES);

    write_request(
        &mut writer,
        &IndexerRequest::Hello {
            protocol: INDEXER_PROTOCOL,
        },
    )
    .await?;
    match read_response(&mut lines).await? {
        IndexerResponse::Hello { protocol } if protocol == INDEXER_PROTOCOL => {}
        IndexerResponse::Error { message } => return Err(SearchFailure::semantic(message)),
        _ => return Err("indexer service returned an invalid hello response".into()),
    }

    write_request(&mut writer, &IndexerRequest::Status).await?;
    let mut status = match read_response(&mut lines).await? {
        IndexerResponse::Status(status) => status,
        IndexerResponse::Error { message } => return Err(SearchFailure::semantic(message)),
        _ => return Err("indexer service returned an invalid status response".into()),
    };
    // Only a completely unready index short-circuits. `ready && building` — a first build
    // that has published some volumes — must still issue the Search so the volumes that
    // are already indexed return results.
    if !status.ready {
        return Ok(SearchReply {
            generation: status.generation,
            status,
            items: Vec::new(),
            is_truncated: false,
            matched_count: Some(0),
            scanned_nodes: Some(0),
            name_candidates: Some(0),
            entered_top_k: Some(0),
            path_constructions: Some(0),
        });
    }

    write_request(
        &mut writer,
        &IndexerRequest::Search {
            query: query.to_owned(),
            max,
            filters: filters.map(ToOwned::to_owned),
            pinyin_enabled: Some(pinyin_enabled),
            root: root.map(ToOwned::to_owned),
        },
    )
    .await?;
    match read_response(&mut lines).await? {
        IndexerResponse::Results {
            generation,
            items,
            is_truncated,
            matched_count,
            scanned_nodes,
            name_candidates,
            entered_top_k,
            path_constructions,
            pinyin_status,
        } => {
            if pinyin_status.is_some() {
                status.pinyin_status = pinyin_status;
            }
            Ok(SearchReply {
                status,
                generation,
                items,
                is_truncated,
                matched_count,
                scanned_nodes,
                name_candidates,
                entered_top_k,
                path_constructions,
            })
        }
        IndexerResponse::Error { message } => Err(SearchFailure::semantic(message)),
        IndexerResponse::RootUnavailable { reason, message } => {
            // The reason travels as a value, not as prose: the broker turns it back into a
            // structured field so the UI can explain the fallback instead of guessing.
            Err(SearchFailure::root(reason, message))
        }
        _ => Err("indexer service returned an invalid search response".into()),
    }
}

#[cfg(test)]
async fn write_request<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    request: &IndexerRequest,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| error.to_string())?;
    writer.flush().await.map_err(|error| error.to_string())
}

#[cfg(test)]
async fn read_response<R: tokio::io::AsyncRead + Unpin>(
    lines: &mut crate::ipc::BoundedLineReader<R>,
) -> Result<IndexerResponse, String> {
    let line = lines
        .next_line()
        .await?
        .ok_or("indexer service closed the connection")?;
    serde_json::from_str(&line).map_err(|error| format!("invalid indexer response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::{IndexState, VolumeId, VolumeIndex};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use crate::indexer_runtime::{handle_connection, ServiceState};
    use tokio::net::windows::named_pipe::ServerOptions;

    #[tokio::test]
    async fn broker_client_performs_hello_status_and_search() {
        let pipe_name = format!(r"\\.\pipe\prism-indexer-test-{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&pipe_name)
            .unwrap();
        let state = ServiceState::new();
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "test".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            9,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "needle.txt", false).unwrap();
        state.publish(IndexState {
            volumes: vec![volume],
            generation: 4,
            events_since_checkpoint: 0,
        });
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            handle_connection(server, state).await.unwrap();
        });

        let reply = search_pipe(&pipe_name, "needle", 10, None, false, None)
            .await
            .unwrap();
        assert!(reply.status.ready);
        assert_eq!(reply.items.len(), 1);
        assert_eq!(reply.items[0].path, r"C:\needle.txt");
        drop(reply);
        server_task.await.unwrap();
    }

    /// 审计批次 2 M2（保守版）：锁竞争降级路径的一次性短连接必须能独立完成
    /// 整条 hello → status → search 序列，不依赖全局单例连接。
    #[tokio::test]
    async fn one_off_connection_completes_a_full_search() {
        let pipe_name = format!(r"\\.\pipe\prism-indexer-oneoff-{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&pipe_name)
            .unwrap();
        let state = ServiceState::new();
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "test".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            9,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "needle.txt", false).unwrap();
        state.publish(IndexState {
            volumes: vec![volume],
            generation: 4,
            events_since_checkpoint: 0,
        });
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            handle_connection(server, state).await.unwrap();
        });

        let reply = search_one_off(&pipe_name, "needle", 10, None, false, None)
            .await
            .unwrap();
        assert!(reply.status.ready);
        assert_eq!(reply.items.len(), 1);
        assert_eq!(reply.items[0].path, r"C:\needle.txt");
        drop(reply);
        server_task.await.unwrap();
    }

    /// AUDIT-2026-08-18 R-A7: 并发 20 个一次性降级搜索，峰值同时打开的
    /// 连接数不得超过 ONE_OFF_GATE 的许可数（2）。
    #[tokio::test]
    async fn one_off_connections_are_capped_by_the_gate() {
        let pipe_name = format!(r"\\.\pipe\prism-indexer-gate-{}", std::process::id());
        let state = ServiceState::new();
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "test".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            9,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "needle.txt", false).unwrap();
        state.publish(IndexState {
            volumes: vec![volume],
            generation: 4,
            events_since_checkpoint: 0,
        });

        let concurrency = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_pipe_name = pipe_name.clone();
        // 4 个常臂 listener（对齐生产 PIPE_LISTENERS 语义）：accept 后立刻重臂，
        // 消除单 listener 重臂间隙在满套件并发负载下造成的客户端连接失败。
        let mut listeners = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let pipe_name = server_pipe_name.clone();
            let state = state.clone();
            let concurrency = concurrency.clone();
            let peak = peak.clone();
            listeners.spawn(async move {
                loop {
                    let server = ServerOptions::new()
                        .reject_remote_clients(true)
                        .create(&pipe_name)
                        .unwrap();
                    server.connect().await.unwrap();
                    let state = state.clone();
                    let concurrency = concurrency.clone();
                    let peak = peak.clone();
                    tokio::spawn(async move {
                        let current = concurrency.fetch_add(1, Ordering::AcqRel) + 1;
                        peak.fetch_max(current, Ordering::AcqRel);
                        let _ = handle_connection(server, state).await;
                        concurrency.fetch_sub(1, Ordering::AcqRel);
                    });
                }
            });
        }

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..20 {
            let pipe_name = pipe_name.clone();
            tasks.spawn(async move {
                search_one_off(&pipe_name, "needle", 10, None, false, None).await
            });
        }
        let mut failures = Vec::new();
        while let Some(result) = tasks.join_next().await {
            if result.unwrap().is_err() {
                failures.push("one-off search failed");
            }
        }
        listeners.abort_all();
        assert!(failures.is_empty(), "all 20 one-off searches must succeed");
        // 峰值观测在 search_one_off 内部（ONE_OFF_PEAK）：server 侧计数会因客户端
        // 断开后的 EOF 滞留虚高，不能作为闸断言依据。
        let observed_peak = ONE_OFF_PEAK.load(Ordering::Acquire);
        assert!(
            observed_peak <= 2,
            "peak concurrent one-off searches {observed_peak} must stay within the gate"
        );
    }

    /// M7：入站行长上限。超过 1MB 的行必须让连接断开，
    /// 而不是被无限缓冲撑爆服务内存。
    #[tokio::test]
    async fn oversized_request_line_disconnects_the_client() {
        let pipe_name = format!(r"\\.\pipe\prism-indexer-oversize-{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&pipe_name)
            .unwrap();
        let state = ServiceState::new();
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            assert!(handle_connection(server, state).await.is_err());
        });

        let pipe = ClientOptions::new().open(&pipe_name).unwrap();
        let (reader, mut writer) = tokio::io::split(pipe);
        let mut lines = crate::ipc::BoundedLineReader::with_limit(reader, MAX_INDEXER_LINE_BYTES);

        let hello = serde_json::to_string(&IndexerRequest::Hello {
            protocol: crate::INDEXER_PROTOCOL,
        })
        .unwrap();
        writer
            .write_all(format!("{hello}\n").as_bytes())
            .await
            .unwrap();
        writer.flush().await.unwrap();
        read_response(&mut lines).await.unwrap();

        let huge = vec![b'a'; 2 * 1024 * 1024];
        // 服务端在累积超过 1MB 时即断开：客户端后续写入可能得到 BrokenPipe，忽略。
        let _ = writer.write_all(&huge).await;
        let _ = writer.write_all(b"\n").await;
        let _ = writer.flush().await;

        // 服务端断开：读到 EOF 或 IO 错误，绝不能收到超长行的"响应"。
        let closed = lines.next_line().await;
        assert!(matches!(closed, Ok(None)) || closed.is_err());
        server_task.await.unwrap();
    }

    /// A partially built index answers searches instead of being short-circuited to an
    /// empty reply. Before per-volume publishing this state could not occur, so the
    /// `!status.ready` early return covered every build; now it must not swallow the
    /// volumes that are already live.
    #[tokio::test]
    async fn partially_built_index_still_answers_searches() {
        let pipe_name = format!(r"\\.\pipe\prism-indexer-partial-{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&pipe_name)
            .unwrap();
        let state = ServiceState::new();
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "system".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            9,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "needle.txt", false).unwrap();
        // Only the first of several volumes has landed: ready and building are both true.
        state.merge_and_publish(volume);
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            handle_connection(server, state).await.unwrap();
        });

        let reply = search_pipe(&pipe_name, "needle", 10, None, false, None)
            .await
            .unwrap();
        assert!(reply.status.ready, "a merged volume makes the index ready");
        assert!(
            reply.status.building,
            "the remaining volumes are still building"
        );
        assert_eq!(
            reply.items.len(),
            1,
            "results from published volumes must survive the not-ready early return"
        );
        assert!(
            !reply.is_truncated,
            "an incomplete index is not the same as a max-truncated result set"
        );
        drop(reply);
        server_task.await.unwrap();
    }

    /// The service refusal must reach the caller as a value. A root that is not in the
    /// index is a *degradation*, so the broker has to be able to retry globally and name
    /// the reason; a formatted error string would make that impossible.
    #[tokio::test]
    async fn unusable_root_reaches_the_caller_as_a_structured_rejection() {
        let pipe_name = format!(r"\\.\pipe\prism-indexer-root-{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&pipe_name)
            .unwrap();
        let state = ServiceState::new();
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "test".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            9,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "needle.txt", false).unwrap();
        state.publish(IndexState {
            volumes: vec![volume],
            generation: 4,
            events_since_checkpoint: 0,
        });
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            handle_connection(server, state).await.unwrap();
        });

        let failure = search_pipe(&pipe_name, "needle", 10, None, false, Some(r"C:\missing"))
            .await
            .unwrap_err();
        assert_eq!(
            failure.root_rejection,
            Some(crate::root_scope::RootRejection::NotFound)
        );
        assert!(!failure.message.is_empty());
        server_task.await.unwrap();
    }

    #[test]
    fn transport_failures_never_look_like_root_rejections() {
        let failure = SearchFailure::from("indexer service is unavailable".to_string());
        assert_eq!(failure.root_rejection, None);
    }
}
