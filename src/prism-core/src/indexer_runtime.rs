//! Long-running indexer service state, IPC, checkpointing, and rebuild coordination.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, Notify};

use crate::hierarchy::{ApplyOutcome, IndexState};
use crate::index_cache;
use crate::indexer_ipc::{IndexerItem, IndexerRequest, IndexerResponse, IndexerStatus};
use crate::ntfs::{self, VolumeDescriptor};
use crate::{log, INDEXER_PIPE_NAME, INDEXER_PROTOCOL};

pub struct Shutdown {
    requested: AtomicBool,
    notify: Notify,
}

impl Shutdown {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            requested: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_requested() {
                return;
            }
            notified.await;
        }
    }
}

pub struct ServiceState {
    index: RwLock<Option<IndexState>>,
    building: AtomicBool,
    degraded: AtomicBool,
    message: RwLock<Option<String>>,
    generation_notify: Notify,
}

impl ServiceState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            index: RwLock::new(None),
            building: AtomicBool::new(true),
            degraded: AtomicBool::new(false),
            message: RwLock::new(None),
            generation_notify: Notify::new(),
        })
    }

    pub fn status(&self) -> IndexerStatus {
        let index = self.index.read().ok();
        let state = index.as_deref().and_then(Option::as_ref);
        IndexerStatus {
            ready: state.is_some(),
            building: self.building.load(Ordering::Acquire),
            degraded: self.degraded.load(Ordering::Acquire),
            generation: state.map(|value| value.generation).unwrap_or(0),
            volumes: state.map(|value| value.volumes.len()).unwrap_or(0),
            memory_bytes: state.map(IndexState::memory_bytes).unwrap_or(0),
            message: self.message.read().ok().and_then(|value| value.clone()),
        }
    }

    pub(crate) fn publish(&self, mut state: IndexState) {
        let previous_generation = self
            .index
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|value| value.generation))
            .unwrap_or(0);
        state.generation = previous_generation.saturating_add(1).max(state.generation);
        if let Ok(mut guard) = self.index.write() {
            *guard = Some(state);
        }
        self.building.store(false, Ordering::Release);
        self.degraded.store(false, Ordering::Release);
        if let Ok(mut message) = self.message.write() {
            *message = None;
        }
        self.generation_notify.notify_waiters();
    }

    fn set_error(&self, error: String) {
        self.degraded.store(true, Ordering::Release);
        self.building.store(false, Ordering::Release);
        if let Ok(mut message) = self.message.write() {
            *message = Some(error);
        }
    }

    fn search(&self, query: &str, max: usize) -> Result<(u64, Vec<IndexerItem>), String> {
        let guard = self.index.read().map_err(|_| "index lock is poisoned")?;
        let state = guard.as_ref().ok_or("file index is not ready")?;
        let generation = state.generation;
        let items = state
            .search(query, max.min(1000))
            .into_iter()
            .map(|hit| IndexerItem {
                name: hit.name,
                path: hit.path,
                is_directory: hit.is_directory,
            })
            .collect();
        Ok((generation, items))
    }

    async fn wait_generation(&self, after: u64, timeout_ms: u64) -> u64 {
        let timeout = Duration::from_millis(timeout_ms.min(30_000));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.generation_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let generation = self.status().generation;
            if generation > after {
                return generation;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.status().generation;
            }
        }
    }
}

pub async fn run(stop: Arc<Shutdown>) -> Result<(), String> {
    let state = ServiceState::new();
    let first_pipe = create_pipe(true).map_err(|error| format!("create indexer pipe: {error}"))?;
    let mut pipe_task = tokio::spawn(serve(state.clone(), first_pipe));
    let data_dir = index_cache::machine_data_dir();

    let initial = tokio::task::spawn_blocking({
        let data_dir = data_dir.clone();
        move || load_or_build(&data_dir)
    })
    .await
    .map_err(|error| format!("initial index task: {error}"))?;

    let descriptors = match initial {
        Ok((index, descriptors)) => {
            state.publish(index);
            descriptors
        }
        Err(error) => {
            state.set_error(error.clone());
            pipe_task.abort();
            return Err(error);
        }
    };

    let epoch = Arc::new(AtomicU64::new(1));
    let (rebuild_tx, mut rebuild_rx) = mpsc::unbounded_channel::<String>();
    start_watchers(
        state.clone(),
        descriptors.clone(),
        stop.clone(),
        epoch.clone(),
        rebuild_tx.clone(),
    );

    let mut last_checkpoint = Instant::now();
    let mut maintenance = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            result = &mut pipe_task => {
                return match result {
                    Ok(Ok(())) => Err("indexer pipe server stopped unexpectedly".into()),
                    Ok(Err(error)) => Err(format!("indexer pipe server: {error}")),
                    Err(error) => Err(format!("indexer pipe task: {error}")),
                };
            }
            reason = rebuild_rx.recv() => {
                let Some(reason) = reason else { break };
                epoch.fetch_add(1, Ordering::AcqRel);
                state.building.store(true, Ordering::Release);
                log(format!("serialized index rebuild requested: {reason}"));
                while rebuild_rx.try_recv().is_ok() {}
                let rebuilt = tokio::task::spawn_blocking(build_all).await
                    .map_err(|error| format!("rebuild task: {error}"))?;
                match rebuilt {
                    Ok((index, descriptors)) => {
                        index_cache::save(&index, &data_dir)?;
                        state.publish(index);
                        start_watchers(state.clone(), descriptors, stop.clone(), epoch.clone(), rebuild_tx.clone());
                        last_checkpoint = Instant::now();
                    }
                    Err(error) => state.set_error(error),
                }
            }
            _ = maintenance.tick() => {
                let checkpoint_due = state.index.read().ok().and_then(|guard| {
                    guard.as_ref().map(|index| index.events_since_checkpoint >= 100_000)
                }).unwrap_or(false) || last_checkpoint.elapsed() >= Duration::from_secs(60 * 60);
                if checkpoint_due {
                    checkpoint(&state, &data_dir)?;
                    last_checkpoint = Instant::now();
                }
            }
        }
    }

    epoch.fetch_add(1, Ordering::AcqRel);
    checkpoint(&state, &data_dir)?;
    pipe_task.abort();
    Ok(())
}

fn load_or_build(
    data_dir: &std::path::Path,
) -> Result<(IndexState, Vec<VolumeDescriptor>), String> {
    let descriptors = ntfs::discover_volumes()?;
    if let Ok(mut index) = index_cache::load(data_dir) {
        if validate_checkpoints(&mut index, &descriptors).is_ok() {
            index.events_since_checkpoint = 0;
            return Ok((index, descriptors));
        }
        log("v5 cache checkpoint is stale; rebuilding while old state remains unpublished");
    }
    let (index, descriptors) = build_all()?;
    index_cache::save(&index, data_dir)?;
    Ok((index, descriptors))
}

fn build_all() -> Result<(IndexState, Vec<VolumeDescriptor>), String> {
    let descriptors = ntfs::discover_volumes()?;
    if descriptors.is_empty() {
        return Err("no local fixed NTFS volumes were found".into());
    }
    let mut volumes = Vec::with_capacity(descriptors.len());
    for descriptor in &descriptors {
        let volume = match ntfs::build_volume(descriptor) {
            Ok(volume) => volume,
            Err(first) => {
                log(format!(
                    "MFT build retry for {}: {first}",
                    descriptor.mount_path
                ));
                ntfs::build_volume(descriptor)?
            }
        };
        volumes.push(volume);
    }
    Ok((
        IndexState {
            volumes,
            generation: 1,
            events_since_checkpoint: 0,
        },
        descriptors,
    ))
}

fn validate_checkpoints(
    index: &mut IndexState,
    descriptors: &[VolumeDescriptor],
) -> Result<(), String> {
    if index.volumes.len() != descriptors.len() {
        return Err("fixed NTFS volume set changed".into());
    }
    for volume in &mut index.volumes {
        let descriptor = descriptors
            .iter()
            .find(|candidate| candidate.id == volume.volume_id)
            .ok_or("cached volume identity is no longer mounted")?;
        let handle = ntfs::open_volume(descriptor, false)?;
        let journal = ntfs::query_journal(&handle)?;
        if journal.journal_id != volume.journal_id || volume.next_usn < journal.first_usn {
            return Err("cached USN checkpoint is no longer replayable".into());
        }
        volume.mount_path.clone_from(&descriptor.mount_path);
    }
    Ok(())
}

fn start_watchers(
    state: Arc<ServiceState>,
    descriptors: Vec<VolumeDescriptor>,
    stop: Arc<Shutdown>,
    epoch: Arc<AtomicU64>,
    rebuild_tx: mpsc::UnboundedSender<String>,
) {
    let watcher_epoch = epoch.load(Ordering::Acquire);
    for descriptor in descriptors {
        let state = state.clone();
        let stop = stop.clone();
        let epoch = epoch.clone();
        let rebuild_tx = rebuild_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = watch_volume(&state, &descriptor, &stop, &epoch, watcher_epoch) {
                if !stop.is_requested() && epoch.load(Ordering::Acquire) == watcher_epoch {
                    let _ = rebuild_tx.send(format!("{}: {error}", descriptor.mount_path));
                }
            }
        });
    }
}

fn watch_volume(
    service: &ServiceState,
    descriptor: &VolumeDescriptor,
    stop: &Shutdown,
    epoch: &AtomicU64,
    watcher_epoch: u64,
) -> Result<(), String> {
    let handle = ntfs::open_volume(descriptor, false)?;
    while !stop.is_requested() && epoch.load(Ordering::Acquire) == watcher_epoch {
        let (journal_id, start_usn) = {
            let guard = service.index.read().map_err(|_| "index lock is poisoned")?;
            let volume = guard
                .as_ref()
                .and_then(|index| {
                    index
                        .volumes
                        .iter()
                        .find(|volume| volume.volume_id == descriptor.id)
                })
                .ok_or("volume disappeared from live index")?;
            (volume.journal_id, volume.next_usn)
        };
        let journal = ntfs::query_journal(&handle)?;
        if journal.journal_id != journal_id || start_usn < journal.first_usn {
            return Err("USN checkpoint expired or journal id changed".into());
        }
        let (next_usn, records) = ntfs::read_changes(&handle, journal_id, start_usn, true)?;
        if stop.is_requested() || epoch.load(Ordering::Acquire) != watcher_epoch {
            return Ok(());
        }
        if next_usn < start_usn {
            return Err("USN cursor moved backwards".into());
        }
        if records.is_empty() && next_usn == start_usn {
            continue;
        }
        let changed = records.len() as u64;
        let rebuild_required = {
            let mut guard = service
                .index
                .write()
                .map_err(|_| "index lock is poisoned")?;
            if stop.is_requested() || epoch.load(Ordering::Acquire) != watcher_epoch {
                return Ok(());
            }
            let index = guard.as_mut().ok_or("index is not ready")?;
            let volume = index
                .volumes
                .iter_mut()
                .find(|volume| volume.volume_id == descriptor.id)
                .ok_or("volume disappeared from live index")?;
            let rebuild_required =
                ntfs::apply_records(volume, &records, next_usn)? == ApplyOutcome::RebuildRequired;
            let events_before = index.events_since_checkpoint;
            index.events_since_checkpoint = index.events_since_checkpoint.saturating_add(changed);
            if changed > 0 && events_before / 10_000 != index.events_since_checkpoint / 10_000 {
                for volume in &mut index.volumes {
                    if volume.compact_names_if_needed()? {
                        log(format!("compacted name pool for {}", volume.mount_path));
                    }
                }
            }
            if changed > 0 {
                index.generation = index.generation.saturating_add(1);
            }
            rebuild_required
        };
        if changed > 0 {
            service.generation_notify.notify_waiters();
        }
        if rebuild_required {
            return Err("excluded-directory boundary changed".into());
        }
    }
    Ok(())
}

fn checkpoint(state: &ServiceState, data_dir: &std::path::Path) -> Result<(), String> {
    let snapshot = {
        let guard = state.index.read().map_err(|_| "index lock is poisoned")?;
        guard.as_ref().cloned().ok_or("index is not ready")?
    };
    index_cache::save(&snapshot, data_dir)?;
    if let Ok(mut guard) = state.index.write() {
        if let Some(index) = guard.as_mut() {
            index.events_since_checkpoint = index
                .events_since_checkpoint
                .saturating_sub(snapshot.events_since_checkpoint);
        }
    }
    Ok(())
}

async fn serve(state: Arc<ServiceState>, mut server: NamedPipeServer) -> Result<(), String> {
    loop {
        server.connect().await.map_err(|error| error.to_string())?;
        let connected = server;
        server = create_pipe(false).map_err(|error| error.to_string())?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(connected, state).await {
                log(format!("indexer IPC client disconnected: {error}"));
            }
        });
    }
}

fn create_pipe(first: bool) -> std::io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);

    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, BOOL, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    // The pipe handle must be duplex for request/response traffic. Protocol decoding
    // remains read-only, while PIPE_REJECT_REMOTE_CLIENTS keeps authenticated remote
    // users out. AU also covers local launch contexts that do not carry INTERACTIVE.
    let sddl: Vec<u16> = std::ffi::OsStr::new("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)")
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
        options.create_with_security_attributes_raw(
            INDEXER_PIPE_NAME,
            (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
        )
    };
    unsafe {
        let _ = LocalFree(HLOCAL(descriptor.0));
    }
    result
}

pub(crate) async fn handle_connection(
    pipe: NamedPipeServer,
    state: Arc<ServiceState>,
) -> Result<(), String> {
    let (reader, mut writer) = tokio::io::split(pipe);
    let mut lines = BufReader::new(reader).lines();
    let hello = lines
        .next_line()
        .await
        .map_err(|error| error.to_string())?
        .ok_or("client closed before hello")?;
    match serde_json::from_str::<IndexerRequest>(&hello) {
        Ok(IndexerRequest::Hello { protocol }) if protocol == INDEXER_PROTOCOL => {
            write_response(&mut writer, &IndexerResponse::Hello { protocol }).await?;
        }
        _ => {
            write_response(
                &mut writer,
                &IndexerResponse::Error {
                    message: "incompatible or missing hello".into(),
                },
            )
            .await?;
            return Ok(());
        }
    }
    while let Some(line) = lines.next_line().await.map_err(|error| error.to_string())? {
        let response = match serde_json::from_str::<IndexerRequest>(&line) {
            Ok(IndexerRequest::Status) => IndexerResponse::Status(state.status()),
            Ok(IndexerRequest::Search { query, max }) => match state.search(&query, max) {
                Ok((generation, items)) => IndexerResponse::Results { generation, items },
                Err(message) => IndexerResponse::Error { message },
            },
            Ok(IndexerRequest::WaitGeneration { after, timeout_ms }) => {
                IndexerResponse::Generation {
                    generation: state.wait_generation(after, timeout_ms).await,
                }
            }
            Ok(IndexerRequest::Hello { .. }) | Err(_) => IndexerResponse::Error {
                message: "unknown or invalid read-only command".into(),
            },
        };
        write_response(&mut writer, &response).await?;
    }
    Ok(())
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &IndexerResponse,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(response).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| error.to_string())?;
    writer.flush().await.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generation_wait_observes_publish() {
        let state = ServiceState::new();
        let waiter = tokio::spawn({
            let state = state.clone();
            async move { state.wait_generation(0, 1_000).await }
        });
        tokio::task::yield_now().await;
        state.publish(IndexState::default());
        assert_eq!(waiter.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn shutdown_request_before_wait_is_observed() {
        let shutdown = Shutdown::new();
        shutdown.request();
        tokio::time::timeout(Duration::from_millis(50), shutdown.cancelled())
            .await
            .expect("pre-existing shutdown request should not be lost");
    }

    #[tokio::test]
    async fn shutdown_request_wakes_waiter() {
        let shutdown = Shutdown::new();
        let waiter = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { shutdown.cancelled().await }
        });
        tokio::task::yield_now().await;
        shutdown.request();
        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("shutdown request should wake the runtime")
            .unwrap();
    }
}
