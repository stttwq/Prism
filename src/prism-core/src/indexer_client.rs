//! Short-lived client used by the user-session broker to query the indexer service.

use std::time::Duration;

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
    search_pipe(INDEXER_PIPE_NAME, query, max, filters, pinyin_enabled, None)
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
    search_pipe(INDEXER_PIPE_NAME, query, max, filters, pinyin_enabled, root).await
}

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

async fn search_pipe_inner(
    pipe_name: &str,
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
    pinyin_enabled: bool,
    root: Option<&str>,
) -> Result<SearchReply, SearchFailure> {
    let pipe = connect(pipe_name).await?;
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

async fn connect(pipe_name: &str) -> Result<NamedPipeClient, String> {
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
