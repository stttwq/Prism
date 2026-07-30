//! Short-lived client used by the user-session broker to query the indexer service.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

use crate::indexer_ipc::{
    IndexerItem, IndexerRequest, IndexerResponse, IndexerStatus, SearchFilter,
};
use crate::{INDEXER_PIPE_NAME, INDEXER_PROTOCOL};

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
    search_with_filters(query, max, None).await
}

pub async fn search_with_filters(
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
) -> Result<SearchReply, String> {
    search_pipe(INDEXER_PIPE_NAME, query, max, filters).await
}

async fn search_pipe(
    pipe_name: &str,
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
) -> Result<SearchReply, String> {
    tokio::time::timeout(
        Duration::from_secs(2),
        search_pipe_inner(pipe_name, query, max, filters),
    )
    .await
    .map_err(|_| "indexer service request timed out".to_string())?
}

async fn search_pipe_inner(
    pipe_name: &str,
    query: &str,
    max: usize,
    filters: Option<&[SearchFilter]>,
) -> Result<SearchReply, String> {
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
        IndexerResponse::Error { message } => return Err(message),
        _ => return Err("indexer service returned an invalid hello response".into()),
    }

    write_request(&mut writer, &IndexerRequest::Status).await?;
    let status = match read_response(&mut lines).await? {
        IndexerResponse::Status(status) => status,
        IndexerResponse::Error { message } => return Err(message),
        _ => return Err("indexer service returned an invalid status response".into()),
    };
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
        } => Ok(SearchReply {
            status,
            generation,
            items,
            is_truncated,
            matched_count,
            scanned_nodes,
            name_candidates,
            entered_top_k,
            path_constructions,
        }),
        IndexerResponse::Error { message } => Err(message),
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

        let reply = search_pipe(&pipe_name, "needle", 10, None).await.unwrap();
        assert!(reply.status.ready);
        assert_eq!(reply.items.len(), 1);
        assert_eq!(reply.items[0].path, r"C:\needle.txt");
        drop(reply);
        server_task.await.unwrap();
    }
}
