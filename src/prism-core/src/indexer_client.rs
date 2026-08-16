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
//! The short-lived ``search_pipe`` helper is retained for tests that exercise
//! the wire protocol against a temporary pipe name.

use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFailure {
    pub message: String,
    pub root_rejection: Option<RootRejection>,
}

impl SearchFailure {
    fn root(reason: RootRejection, message: String) -> Self {
        Self {
            message,
            root_rejection: Some(reason),
        }
    }
}

impl From<String> for SearchFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            root_rejection: None,
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

/// A long-lived connection to the indexer with its own reader/writer halves.
///
/// The connection is established lazily on first use and reused for every
/// subsequent search.  If the pipe breaks (indexer restart, OS error, etc.)
/// the holder is dropped and the next caller creates a fresh connection.
struct PersistentConnection {
    writer: tokio::io::WriteHalf<NamedPipeClient>,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::io::ReadHalf<NamedPipeClient>>>,
}

impl PersistentConnection {
    async fn connect(pipe_name: &str) -> Result<Self, String> {
        let pipe = connect_pipe(pipe_name).await?;
        let (reader, writer) = tokio::io::split(pipe);
        let lines = BufReader::new(reader).lines();
        Ok(Self { writer, lines })
    }

    /// Send a request and read one response line.  Named-pipe I/O is
    /// request/response paired, so callers must ensure they hold the
    /// connection lock for the full exchange.
    async fn exchange(&mut self, request: &IndexerRequest) -> Result<IndexerResponse, String> {
        let mut bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.map_err(|error| error.to_string())?;
        self.writer.flush().await.map_err(|error| error.to_string())?;

        let line = self
            .lines
            .next_line()
            .await
            .map_err(|error| error.to_string())?
            .ok_or("indexer service closed the connection")?;
        serde_json::from_str(&line).map_err(|error| format!("invalid indexer response: {error}"))
    }
}

/// Process-wide singleton connection guarded by a tokio Mutex.
static INDEXER_CONNECTION: OnceLock<Mutex<Option<PersistentConnection>>> = OnceLock::new();

fn connection_lock() -> &'static Mutex<Option<PersistentConnection>> {
    INDEXER_CONNECTION.get_or_init(|| Mutex::new(None))
}

/// Ceiling on waiting for the process-wide connection mutex: each in-flight
/// exchange is capped by `REQUEST_BUDGET`, so exceeding this means the queue
/// is wedged — fail fast instead of piling more waiters on.
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// Total budget for one search sequence (connect + handshake + search + retry).
/// A live-but-unresponsive indexer service must never pin the connection
/// mutex forever. Generous against normal exchanges (milliseconds) plus the
/// indexer's worst-case write-lock stall during name-pool compaction (seconds).
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
    match conn.exchange(&IndexerRequest::Hello { protocol: INDEXER_PROTOCOL }).await? {
        IndexerResponse::Hello { protocol } if protocol == INDEXER_PROTOCOL => Ok(()),
        IndexerResponse::Error { message } => Err(message),
        _ => Err("indexer service returned an invalid hello response".into()),
    }
}

/// Ensure the singleton connection exists and is healthy, then run `search`.
///
/// If the connection is missing or the exchange fails at the transport level,
/// a fresh connection is created and the search is retried once.
///
/// Both the lock wait and the whole in-lock sequence are time-bounded: a hung
/// indexer must degrade this one request into an error, never block every
/// subsequent search behind the process-wide mutex.
async fn search_via_persistent(
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    let guard = connection_lock();
    let mut conn = tokio::time::timeout(LOCK_WAIT, guard.lock())
        .await
        .map_err(|_| SearchFailure::from("indexer connection is busy".to_string()))?;

    let request = async {
        // Lazy connect + handshake on first use, or after a prior drop.
        let needs_reconnect = conn.is_none();
        if needs_reconnect {
            let mut new_conn = PersistentConnection::connect(INDEXER_PIPE_NAME).await?;
            handshake(&mut new_conn).await?;
            *conn = Some(new_conn);
        }

        // First attempt on the (possibly fresh) connection.
        match search_on_connection(conn.as_mut().unwrap(), query, max, filters, pinyin_enabled, root).await {
            Ok(reply) => Ok(reply),
            // Transport-level failure: drop the connection, reconnect, retry once.
            Err(failure) if failure.root_rejection.is_none() => {
                *conn = None; // drop the broken connection
                let mut new_conn = PersistentConnection::connect(INDEXER_PIPE_NAME).await?;
                handshake(&mut new_conn).await?;
                let reply = search_on_connection(&mut new_conn, query, max, filters, pinyin_enabled, root).await?;
                *conn = Some(new_conn);
                Ok(reply)
            }
            // Root rejection is a semantic response, not a transport error — propagate as-is.
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

/// Run a full search sequence (status → search) on an established connection.
async fn search_on_connection(
    conn: &mut PersistentConnection,
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    let status = match conn.exchange(&IndexerRequest::Status).await.map_err(SearchFailure::from)? {
        IndexerResponse::Status(status) => status,
        IndexerResponse::Error { message } => return Err(message.into()),
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
        IndexerResponse::Error { message } => Err(message.into()),
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
    let mut lines = BufReader::new(reader).lines();

    write_request(
        &mut writer,
        &IndexerRequest::Hello {
            protocol: INDEXER_PROTOCOL,
        },
    )
    .await?;
    match read_response(&mut lines).await? {
        IndexerResponse::Hello { protocol } if protocol == INDEXER_PROTOCOL => {}
        IndexerResponse::Error { message } => return Err(message.into()),
        _ => return Err("indexer service returned an invalid hello response".into()),
    }

    write_request(&mut writer, &IndexerRequest::Status).await?;
    let mut status = match read_response(&mut lines).await? {
        IndexerResponse::Status(status) => status,
        IndexerResponse::Error { message } => return Err(message.into()),
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
        IndexerResponse::Error { message } => Err(message.into()),
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
async fn read_response<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
) -> Result<IndexerResponse, String> {
    let line = lines
        .next_line()
        .await
        .map_err(|error| error.to_string())?
        .ok_or("indexer service closed the connection")?;
    serde_json::from_str(&line).map_err(|error| format!("invalid indexer response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::{IndexState, VolumeId, VolumeIndex};
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
        let mut lines = tokio::io::BufReader::new(reader).lines();

        let hello = serde_json::to_string(&IndexerRequest::Hello {
            protocol: crate::INDEXER_PROTOCOL,
        })
        .unwrap();
        writer.write_all(format!("{hello}\n").as_bytes()).await.unwrap();
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
