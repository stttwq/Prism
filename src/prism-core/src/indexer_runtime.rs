//! Long-running indexer service state, IPC, checkpointing, and rebuild coordination.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, Notify};

use crate::hierarchy::{ApplyOutcome, IndexState, VolumeIndex};
use crate::index_cache;
use crate::indexer_ipc::{
    validate_search_request, BuildProgress, IndexerItem, IndexerRequest, IndexerResponse,
    IndexerStatus, SearchFilter,
};
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

/// First-build progress counters (R4).
///
/// Sampling has to stay off the hot path: the enumeration loop accumulates once per
/// `FSCTL_ENUM_USN_DATA` batch rather than once per record, and readers only touch these
/// on a `status` request. `active` distinguishes "no build running" (report nothing) from
/// "build running, zero volumes done so far".
#[derive(Default)]
struct BuildProgressCounters {
    active: AtomicBool,
    volumes_total: AtomicU64,
    volumes_done: AtomicU64,
    records_scanned: AtomicU64,
    records_estimate: AtomicU64,
    current_volume: RwLock<Option<String>>,
}

impl BuildProgressCounters {
    fn begin(&self, volumes_total: usize, records_estimate: Option<u64>) {
        self.volumes_total
            .store(volumes_total as u64, Ordering::Release);
        self.volumes_done.store(0, Ordering::Release);
        self.records_scanned.store(0, Ordering::Release);
        self.records_estimate
            .store(records_estimate.unwrap_or(0), Ordering::Release);
        if let Ok(mut guard) = self.current_volume.write() {
            *guard = None;
        }
        self.active.store(true, Ordering::Release);
    }

    fn begin_volume(&self, mount_path: &str) {
        if let Ok(mut guard) = self.current_volume.write() {
            *guard = Some(mount_path.to_owned());
        }
    }

    fn records_scanned(&self) -> u64 {
        self.records_scanned.load(Ordering::Relaxed)
    }

    fn set_records_scanned(&self, count: u64) {
        self.records_scanned.store(count, Ordering::Relaxed);
    }

    fn volume_done(&self) {
        self.volumes_done.fetch_add(1, Ordering::AcqRel);
    }

    /// Ends the reporting window. Called on every terminal path, so a failed build stops
    /// advertising stale progress just like a successful one.
    fn finish(&self) {
        self.active.store(false, Ordering::Release);
        if let Ok(mut guard) = self.current_volume.write() {
            *guard = None;
        }
    }

    fn snapshot(&self) -> Option<BuildProgress> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        let records_scanned = self.records_scanned.load(Ordering::Relaxed);
        let records_estimate = self.records_estimate.load(Ordering::Acquire);
        Some(BuildProgress {
            volumes_total: self.volumes_total.load(Ordering::Acquire) as usize,
            volumes_done: self.volumes_done.load(Ordering::Acquire) as usize,
            current_volume: self
                .current_volume
                .read()
                .ok()
                .and_then(|guard| guard.clone()),
            records_scanned: (records_scanned > 0).then_some(records_scanned),
            records_estimate: (records_estimate > 0).then_some(records_estimate),
        })
    }
}

pub struct ServiceState {
    index: RwLock<Option<IndexState>>,
    building: AtomicBool,
    degraded: AtomicBool,
    message: RwLock<Option<String>>,
    generation_notify: Notify,
    progress: BuildProgressCounters,
    /// R2 persistence gate. Cleared while a first build is in flight so that no exit
    /// path can write a partial index that would later look like a complete cache.
    first_build_complete: AtomicBool,
}

impl ServiceState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            index: RwLock::new(None),
            building: AtomicBool::new(true),
            degraded: AtomicBool::new(false),
            message: RwLock::new(None),
            generation_notify: Notify::new(),
            progress: BuildProgressCounters::default(),
            first_build_complete: AtomicBool::new(false),
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
            build_progress: self.progress.snapshot(),
        }
    }

    /// Merges one freshly built volume into the live index and publishes it.
    ///
    /// Unlike [`Self::publish`], this leaves `building` set: the result is a searchable
    /// but incomplete index (`ready && building`). Each merge bumps the generation so
    /// front-end caches keyed on it fall out of date on their own.
    pub(crate) fn merge_and_publish(&self, volume: VolumeIndex) {
        if let Ok(mut guard) = self.index.write() {
            let index = guard.get_or_insert_with(IndexState::default);
            // A rebuilt volume replaces its earlier copy rather than duplicating it.
            if let Some(slot) = index
                .volumes
                .iter_mut()
                .find(|existing| existing.volume_id == volume.volume_id)
            {
                *slot = volume;
            } else {
                index.volumes.push(volume);
            }
            index.generation = index.generation.saturating_add(1);
        }
        self.degraded.store(false, Ordering::Release);
        if let Ok(mut message) = self.message.write() {
            *message = None;
        }
        self.generation_notify.notify_waiters();
    }

    /// Marks the end of a per-volume first build.
    ///
    /// Deliberately does *not* replace the live index the way [`Self::publish`] does:
    /// watchers for already-published volumes have been applying USN events since their
    /// volume landed, and overwriting the index with the build-time snapshot would drop
    /// them. Only the status flags and the R2 gate change here.
    fn finish_first_build(&self) {
        self.building.store(false, Ordering::Release);
        self.first_build_complete.store(true, Ordering::Release);
        self.progress.finish();
        self.generation_notify.notify_waiters();
    }

    fn first_build_is_complete(&self) -> bool {
        self.first_build_complete.load(Ordering::Acquire)
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
        // A whole-index publish is the terminal state of both cache hits and rebuilds,
        // so it opens the R2 persistence gate and clears any first-build progress.
        self.first_build_complete.store(true, Ordering::Release);
        self.progress.finish();
        self.generation_notify.notify_waiters();
    }

    fn set_error(&self, error: String) {
        self.degraded.store(true, Ordering::Release);
        self.building.store(false, Ordering::Release);
        if let Ok(mut message) = self.message.write() {
            *message = Some(error);
        }
    }

    fn search(
        &self,
        query: &str,
        max: usize,
        filters: Option<&[SearchFilter]>,
    ) -> Result<IndexerResponse, String> {
        validate_search_request(max, filters)?;
        let guard = self.index.read().map_err(|_| "index lock is poisoned")?;
        let state = guard.as_ref().ok_or("file index is not ready")?;
        let generation = state.generation;
        let outcome = state.search(query, max);
        let items = outcome
            .items
            .into_iter()
            .map(|hit| IndexerItem {
                name: hit.name,
                path: hit.path,
                is_directory: hit.is_directory,
                match_metadata: Some(hit.match_metadata),
            })
            .collect();
        Ok(IndexerResponse::Results {
            generation,
            items,
            is_truncated: outcome.is_truncated,
            matched_count: Some(outcome.matched_count),
            scanned_nodes: Some(outcome.scanned_nodes),
            name_candidates: Some(outcome.name_candidates),
            entered_top_k: Some(outcome.entered_top_k),
            path_constructions: Some(outcome.path_constructions),
        })
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

    let epoch = Arc::new(AtomicU64::new(1));
    let (rebuild_tx, mut rebuild_rx) = mpsc::unbounded_channel::<String>();

    let initial = acquire_initial_index(&state, &data_dir, &stop, &epoch, &rebuild_tx).await;

    let initial = match initial {
        Ok(initial) => initial,
        Err(error) => {
            state.set_error(error.clone());
            state.progress.finish();
            pipe_task.abort();
            return Err(error);
        }
    };

    if initial.interrupted {
        // Stop arrived mid first build. The R2 gate keeps the partial index off disk.
        epoch.fetch_add(1, Ordering::AcqRel);
        pipe_task.abort();
        return Ok(());
    }

    if !initial.watchers_started {
        start_watchers(
            state.clone(),
            initial.descriptors.clone(),
            stop.clone(),
            epoch.clone(),
            rebuild_tx.clone(),
        );
    }

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

/// Outcome of getting the service to a serving state at startup.
struct InitialIndex {
    descriptors: Vec<VolumeDescriptor>,
    /// True when the per-volume first build already started a watcher for each volume as
    /// it was published, so `run` must not start a second set.
    watchers_started: bool,
    /// True when shutdown was requested before the first build finished.
    interrupted: bool,
}

/// Brings the index up, publishing whatever is usable as early as possible.
///
/// Cache hit: unchanged from before — validate, publish the whole index once.
/// Cache miss: build volume by volume, publishing and starting a watcher after each, so
/// the system volume becomes searchable without waiting for the rest (R1).
async fn acquire_initial_index(
    state: &Arc<ServiceState>,
    data_dir: &std::path::Path,
    stop: &Arc<Shutdown>,
    epoch: &Arc<AtomicU64>,
    rebuild_tx: &mpsc::UnboundedSender<String>,
) -> Result<InitialIndex, String> {
    let cached = tokio::task::spawn_blocking({
        let data_dir = data_dir.to_path_buf();
        move || load_cached(&data_dir)
    })
    .await
    .map_err(|error| format!("initial index task: {error}"))?;

    let (descriptors, records_estimate) = match cached {
        CachedLoad::Hit { index, descriptors } => {
            state.publish(index);
            return Ok(InitialIndex {
                descriptors,
                watchers_started: false,
                interrupted: false,
            });
        }
        CachedLoad::Miss {
            descriptors,
            records_estimate,
        } => (descriptors, records_estimate),
    };

    if descriptors.is_empty() {
        return Err("no local fixed NTFS volumes were found".into());
    }

    state.progress.begin(descriptors.len(), records_estimate);

    for descriptor in &descriptors {
        if stop.is_requested() {
            log("first build stopped before completion; no v5 cache was written");
            return Ok(InitialIndex {
                descriptors,
                watchers_started: true,
                interrupted: true,
            });
        }

        state.progress.begin_volume(&descriptor.mount_path);
        let built = tokio::task::spawn_blocking({
            let descriptor = descriptor.clone();
            let state = state.clone();
            let stop = stop.clone();
            move || build_volume_reporting(&descriptor, &state, &stop)
        })
        .await
        .map_err(|error| format!("volume build task: {error}"))?;

        let volume = built?;
        if stop.is_requested() {
            log("first build stopped before publishing the completed volume; no v5 cache was written");
            return Ok(InitialIndex {
                descriptors,
                watchers_started: true,
                interrupted: true,
            });
        }
        state.merge_and_publish(volume);
        state.progress.volume_done();
        // The volume carries a USN checkpoint captured before enumeration, so a watcher
        // started now resumes from it without missing changes made during the build.
        start_watchers(
            state.clone(),
            vec![descriptor.clone()],
            stop.clone(),
            epoch.clone(),
            rebuild_tx.clone(),
        );
        log(format!("first build published {}", descriptor.mount_path));
    }

    persist_first_build_with(state, |snapshot| index_cache::save(snapshot, data_dir))?;
    Ok(InitialIndex {
        descriptors,
        watchers_started: true,
        interrupted: false,
    })
}

fn persist_first_build_with<F>(state: &ServiceState, save: F) -> Result<(), String>
where
    F: FnOnce(&IndexState) -> Result<(), String>,
{
    let snapshot = {
        let guard = state.index.read().map_err(|_| "index lock is poisoned")?;
        guard.as_ref().cloned().ok_or("index is not ready")?
    };
    save(&snapshot)?;
    // `building=false` is externally observable. Publish it only after the durable cache
    // exists, otherwise clients can observe a completed build that is not restart-safe.
    state.finish_first_build();
    Ok(())
}

enum CachedLoad {
    Hit {
        index: IndexState,
        descriptors: Vec<VolumeDescriptor>,
    },
    Miss {
        descriptors: Vec<VolumeDescriptor>,
        /// Record count from the rejected cache, if any, used only as a progress
        /// denominator. A first install has none and reports volume counts alone.
        records_estimate: Option<u64>,
    },
}

/// Discovers fixed NTFS volumes with the system volume first (R3).
fn discover_ordered_volumes() -> Result<Vec<VolumeDescriptor>, String> {
    let descriptors = ntfs::discover_volumes()?;
    Ok(ntfs::order_volumes(
        descriptors,
        ntfs::system_drive_letter(),
    ))
}

fn load_cached(data_dir: &std::path::Path) -> CachedLoad {
    let descriptors = match discover_ordered_volumes() {
        Ok(descriptors) => descriptors,
        Err(error) => {
            log(format!("volume discovery failed: {error}"));
            Vec::new()
        }
    };
    // The cache is read once here: on a hit it becomes the live index, and on a stale
    // miss its record count is the only available estimate for progress reporting.
    if let Ok(mut index) = index_cache::load(data_dir) {
        if validate_checkpoints(&mut index, &descriptors).is_ok() {
            index.events_since_checkpoint = 0;
            return CachedLoad::Hit { index, descriptors };
        }
        log("v5 cache checkpoint is stale; rebuilding while old state remains unpublished");
        let records = index
            .volumes
            .iter()
            .map(|volume| volume.nodes.len() as u64)
            .sum::<u64>();
        return CachedLoad::Miss {
            descriptors,
            records_estimate: (records > 0).then_some(records),
        };
    }
    CachedLoad::Miss {
        descriptors,
        records_estimate: None,
    }
}

/// Builds one volume, feeding enumerated record counts into the progress counters, and
/// retries once on failure the way the whole-index build path does.
fn build_volume_reporting(
    descriptor: &VolumeDescriptor,
    state: &ServiceState,
    stop: &Shutdown,
) -> Result<VolumeIndex, String> {
    build_volume_reporting_with(
        state,
        stop,
        &descriptor.mount_path,
        |should_cancel, report| ntfs::build_volume_with_progress(descriptor, should_cancel, report),
    )
}

fn build_volume_reporting_with(
    state: &ServiceState,
    stop: &Shutdown,
    description: &str,
    mut build: impl FnMut(&dyn Fn() -> bool, &mut dyn FnMut(u64)) -> Result<VolumeIndex, String>,
) -> Result<VolumeIndex, String> {
    let records_before_attempt = state.progress.records_scanned();
    let should_cancel = || stop.is_requested();
    let attempt_records = Cell::new(0u64);
    let mut report = |count: u64| {
        attempt_records.set(attempt_records.get().saturating_add(count));
        state
            .progress
            .set_records_scanned(records_before_attempt.saturating_add(attempt_records.get()));
    };
    match build(&should_cancel, &mut report) {
        Ok(volume) => Ok(volume),
        Err(first) => {
            if stop.is_requested() {
                return Err(first);
            }
            log(format!("MFT build retry for {description}: {first}"));
            state.progress.set_records_scanned(records_before_attempt);
            attempt_records.set(0);
            build(&should_cancel, &mut report)
        }
    }
}

fn build_all() -> Result<(IndexState, Vec<VolumeDescriptor>), String> {
    let descriptors = discover_ordered_volumes()?;
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
        start_watcher(
            state.clone(),
            descriptor,
            stop.clone(),
            epoch.clone(),
            rebuild_tx.clone(),
            watcher_epoch,
        );
    }
}

/// Starts the USN watcher for a single volume.
///
/// Used by the first-build loop so a volume enters live monitoring the moment it is
/// published. `build_volume` captures its USN checkpoint *before* enumerating and replays
/// up to the post-enumeration cursor, so the watcher resuming from `volume.next_usn` cannot
/// miss changes made during enumeration or in the gap before it starts (R1).
fn start_watcher(
    state: Arc<ServiceState>,
    descriptor: VolumeDescriptor,
    stop: Arc<Shutdown>,
    epoch: Arc<AtomicU64>,
    rebuild_tx: mpsc::UnboundedSender<String>,
    watcher_epoch: u64,
) {
    tokio::task::spawn_blocking(move || {
        if let Err(error) = watch_volume(&state, &descriptor, &stop, &epoch, watcher_epoch) {
            if !stop.is_requested() && epoch.load(Ordering::Acquire) == watcher_epoch {
                let _ = rebuild_tx.send(format!("{}: {error}", descriptor.mount_path));
            }
        }
    });
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
    // R2 gate: a first build in flight means the live index covers only some volumes.
    // Writing it now would produce a file that later looks like a complete cache, so
    // every exit path — including SCM Stop — skips the write and forces a full rebuild.
    if !state.first_build_is_complete() {
        log("skipping v5 cache write: the first build has not completed");
        return Ok(());
    }
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
            Ok(IndexerRequest::Search {
                query,
                max,
                filters,
            }) => match state.search(&query, max, filters.as_deref()) {
                Ok(response) => response,
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
    use crate::hierarchy::VolumeId;
    use crate::indexer_ipc::{MAX_FILTERS, MAX_FILTER_VALUE_BYTES, MAX_SEARCH_RESULTS};

    fn test_volume(guid: &str, mount_path: &str, name: &str) -> VolumeIndex {
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: guid.into(),
                serial: 1,
            },
            mount_path.into(),
            7,
            9,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, name, false).unwrap();
        volume
    }

    fn descriptor(letter: char) -> VolumeDescriptor {
        VolumeDescriptor {
            drive_letter: letter,
            id: VolumeId {
                guid: format!("guid-{letter}"),
                serial: u32::from(letter as u8),
            },
            mount_path: format!("{letter}:\\"),
        }
    }

    #[test]
    fn first_merge_makes_the_index_searchable_while_still_building() {
        let state = ServiceState::new();
        assert!(!state.status().ready);

        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));

        let status = state.status();
        // The new `ready && building` combination: usable, still filling in.
        assert!(status.ready, "one merged volume must be searchable");
        assert!(status.building, "the first build is still in progress");
        assert_eq!(status.volumes, 1);
        assert_eq!(status.generation, 1);
        assert!(state
            .search("needle", 8, None)
            .is_ok_and(|response| matches!(response, IndexerResponse::Results { ref items, .. } if items.len() == 1)));
    }

    #[test]
    fn each_merge_accumulates_volumes_and_advances_generation() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "alpha.txt"));
        state.merge_and_publish(test_volume("v2", "D:\\", "beta.txt"));

        let status = state.status();
        assert_eq!(status.volumes, 2, "merges accumulate rather than replace");
        assert_eq!(
            status.generation, 2,
            "every merge advances the generation so consumer caches invalidate"
        );

        // Re-merging the same identity replaces that volume instead of duplicating it.
        state.merge_and_publish(test_volume("v1", "C:\\", "alpha.txt"));
        let status = state.status();
        assert_eq!(status.volumes, 2);
        assert_eq!(status.generation, 3);
    }

    #[tokio::test]
    async fn merge_wakes_generation_waiters() {
        let state = ServiceState::new();
        let waiter = tokio::spawn({
            let state = state.clone();
            async move { state.wait_generation(0, 1_000).await }
        });
        tokio::task::yield_now().await;
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));
        assert_eq!(waiter.await.unwrap(), 1);
    }

    #[test]
    fn partial_index_is_never_written_to_disk() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));

        let dir = std::env::temp_dir().join(format!("prism-g9-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Mid-first-build checkpoint: succeeds as a no-op, writes nothing.
        checkpoint(&state, &dir).unwrap();
        assert!(
            !index_cache::cache_path(&dir).exists(),
            "a partial first build must not leave a cache file behind"
        );

        state.finish_first_build();
        checkpoint(&state, &dir).unwrap();
        assert!(
            index_cache::cache_path(&dir).exists(),
            "the cache is written once the first build completes"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_build_reports_complete_only_after_cache_save_succeeds() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));

        persist_first_build_with(&state, |snapshot| {
            assert_eq!(snapshot.volumes.len(), 1);
            assert!(
                state.status().building,
                "save runs before completion is visible"
            );
            assert!(!state.first_build_is_complete());
            Ok(())
        })
        .unwrap();

        assert!(!state.status().building);
        assert!(state.first_build_is_complete());
    }

    #[test]
    fn failed_first_build_cache_save_does_not_publish_completion() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));

        let error = persist_first_build_with(&state, |_| Err("cache save failed".into()))
            .expect_err("save failure must propagate");

        assert_eq!(error, "cache save failed");
        assert!(state.status().building);
        assert!(!state.first_build_is_complete());
    }

    #[test]
    fn finishing_the_first_build_keeps_watcher_applied_events() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));
        let generation_after_merge = state.status().generation;

        state.finish_first_build();

        let status = state.status();
        assert!(status.ready);
        assert!(!status.building, "the first build is over");
        assert_eq!(
            status.volumes, 1,
            "finishing must not replace the live index that watchers have been updating"
        );
        assert_eq!(
            status.generation, generation_after_merge,
            "finishing only flips flags; it publishes no new index"
        );
        assert!(status.build_progress.is_none());
    }

    #[test]
    fn progress_is_reported_only_while_a_build_runs() {
        let state = ServiceState::new();
        assert!(
            state.status().build_progress.is_none(),
            "no build running means no progress field at all"
        );

        state.progress.begin(3, Some(500_000));
        state.progress.begin_volume("C:\\");
        state.progress.set_records_scanned(22_388);

        let progress = state.status().build_progress.expect("build is active");
        assert_eq!(progress.volumes_total, 3);
        assert_eq!(progress.volumes_done, 0);
        assert_eq!(progress.current_volume.as_deref(), Some("C:\\"));
        assert_eq!(progress.records_scanned, Some(22_388));
        assert_eq!(progress.records_estimate, Some(500_000));

        state.progress.volume_done();
        assert_eq!(
            state.status().build_progress.unwrap().volumes_done,
            1,
            "counts advance per volume"
        );

        state.progress.finish();
        assert!(state.status().build_progress.is_none());
    }

    #[test]
    fn a_first_install_reports_volume_counts_without_an_estimate() {
        let state = ServiceState::new();
        // No previous cache exists, so there is nothing to estimate a record total from.
        state.progress.begin(2, None);

        let progress = state.status().build_progress.unwrap();
        assert_eq!(progress.volumes_total, 2);
        assert_eq!(
            progress.records_estimate, None,
            "a first install shows N/M volumes rather than a fake percentage"
        );
        assert_eq!(progress.records_scanned, None);
    }

    #[test]
    fn failed_build_attempt_does_not_inflate_progress_on_retry() {
        let state = ServiceState::new();
        state.progress.begin(1, Some(100));
        let stop = Shutdown::new();
        let attempts = Cell::new(0usize);

        let volume =
            build_volume_reporting_with(&state, &stop, "C:\\", |_should_cancel, report| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 1 {
                    report(80);
                    Err("synthetic first-attempt failure".into())
                } else {
                    report(25);
                    Ok(test_volume("v1", "C:\\", "needle.txt"))
                }
            })
            .unwrap();

        assert_eq!(volume.mount_path, "C:\\");
        assert_eq!(attempts.get(), 2);
        assert_eq!(
            state.status().build_progress.unwrap().records_scanned,
            Some(25),
            "the failed attempt must be removed before retry progress is published"
        );
    }

    #[test]
    fn cancelled_build_is_not_retried() {
        let state = ServiceState::new();
        state.progress.begin(1, None);
        let stop = Shutdown::new();
        let stop_during_build = stop.clone();
        let attempts = Cell::new(0usize);

        let result = build_volume_reporting_with(&state, &stop, "C:\\", |should_cancel, report| {
            attempts.set(attempts.get() + 1);
            report(10);
            stop_during_build.request();
            assert!(should_cancel());
            Err("volume build cancelled".into())
        });

        assert_eq!(result.unwrap_err(), "volume build cancelled");
        assert_eq!(attempts.get(), 1, "cancellation must not trigger a retry");
    }

    #[test]
    fn the_system_volume_is_ordered_first_regardless_of_letter() {
        // Deliberately out of alphabetical order, with the system volume last.
        let descriptors = vec![descriptor('D'), descriptor('E'), descriptor('C')];
        let ordered = ntfs::order_volumes(descriptors, Some('C'));
        let letters: Vec<char> = ordered.iter().map(|d| d.drive_letter).collect();
        assert_eq!(
            letters,
            vec!['C', 'D', 'E'],
            "system volume first, remaining volumes keep their relative order"
        );

        // A non-alphabetically-first system drive still wins.
        let descriptors = vec![descriptor('C'), descriptor('D'), descriptor('E')];
        let ordered = ntfs::order_volumes(descriptors, Some('E'));
        let letters: Vec<char> = ordered.iter().map(|d| d.drive_letter).collect();
        assert_eq!(
            letters,
            vec!['E', 'C', 'D'],
            "ordering must not depend on the letters happening to sort correctly"
        );
    }

    #[test]
    fn volume_order_is_unchanged_when_the_system_drive_is_unknown() {
        let descriptors = vec![descriptor('D'), descriptor('C')];
        let ordered = ntfs::order_volumes(descriptors, None);
        let letters: Vec<char> = ordered.iter().map(|d| d.drive_letter).collect();
        assert_eq!(letters, vec!['D', 'C'], "stable and predictable for reruns");
    }

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

    #[test]
    fn search_request_limits_are_enforced() {
        assert!(validate_search_request(0, None).is_err());
        assert!(validate_search_request(MAX_SEARCH_RESULTS + 1, None).is_err());
        assert!(validate_search_request(8, Some(&[])).is_ok());
        let too_many = vec![
            SearchFilter {
                field: "ext".into(),
                value: "txt".into(),
            };
            MAX_FILTERS + 1
        ];
        assert!(validate_search_request(8, Some(&too_many)).is_err());
        let long_value = SearchFilter {
            field: "path".into(),
            value: "x".repeat(MAX_FILTER_VALUE_BYTES + 1),
        };
        assert!(validate_search_request(8, Some(&[long_value])).is_err());
    }
}
