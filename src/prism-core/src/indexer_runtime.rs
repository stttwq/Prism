//! Long-running indexer service state, IPC, checkpointing, and rebuild coordination.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, Notify};

use crate::hierarchy::{ApplyOutcome, IndexState, VolumeIndex};
use crate::index_cache;
use crate::indexer_ipc::{
    requested_root, validate_search_request, BuildProgress, IndexerItem, IndexerRequest,
    IndexerResponse, IndexerStatus, PinyinStatus, SearchFilter,
};
use crate::ntfs::{self, VolumeDescriptor};
use crate::pinyin_sidecar::{LoadErrorKind, PinyinSidecar};
use crate::root_scope::RootScope;
use crate::{log, INDEXER_PIPE_NAME, INDEXER_PROTOCOL};
use crate::logging;

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
    pinyin: RwLock<Option<PinyinSidecar>>,
    pinyin_status: RwLock<PinyinStatus>,
    pinyin_data_dir: RwLock<Option<PathBuf>>,
    pinyin_needs_rebuild: AtomicBool,
    pinyin_enabled: AtomicBool,
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
            pinyin: RwLock::new(None),
            pinyin_status: RwLock::new(PinyinStatus::Building),
            pinyin_data_dir: RwLock::new(None),
            pinyin_needs_rebuild: AtomicBool::new(false),
            pinyin_enabled: AtomicBool::new(true),
        })
    }

    fn set_pinyin_data_dir(&self, data_dir: &Path) {
        if let Ok(mut slot) = self.pinyin_data_dir.write() {
            *slot = Some(data_dir.to_path_buf());
        }
    }

    fn pinyin_status(&self) -> PinyinStatus {
        self.pinyin_status
            .read()
            .map(|status| *status)
            .unwrap_or(PinyinStatus::Corrupt)
    }

    fn set_pinyin_status(&self, status: PinyinStatus) {
        if let Ok(mut current) = self.pinyin_status.write() {
            *current = status;
        }
    }

    fn release_pinyin(&self) {
        self.pinyin_enabled.store(false, Ordering::Release);
        if let Ok(mut sidecar) = self.pinyin.write() {
            *sidecar = None;
        }
        self.set_pinyin_status(PinyinStatus::Disabled);
    }

    fn begin_pinyin_rebuild(&self) {
        if let Ok(mut sidecar) = self.pinyin.write() {
            *sidecar = None;
        }
        self.set_pinyin_status(PinyinStatus::Building);
    }

    fn load_pinyin(&self, index: &IndexState) {
        if !self.pinyin_enabled.load(Ordering::Acquire) {
            self.release_pinyin();
            return;
        }
        if self.pinyin.read().is_ok_and(|sidecar| sidecar.is_some()) {
            self.set_pinyin_status(PinyinStatus::Ready);
            return;
        }
        let data_dir = self
            .pinyin_data_dir
            .read()
            .ok()
            .and_then(|value| value.clone());
        let Some(data_dir) = data_dir else {
            self.set_pinyin_status(PinyinStatus::Missing);
            return;
        };
        match PinyinSidecar::load(&data_dir, index) {
            Ok(sidecar) => {
                if !self.pinyin_enabled.load(Ordering::Acquire) {
                    self.release_pinyin();
                    return;
                }
                if let Ok(mut current) = self.pinyin.write() {
                    *current = Some(sidecar);
                }
                self.set_pinyin_status(PinyinStatus::Ready);
            }
            Err(error) => {
                if !self.pinyin_enabled.load(Ordering::Acquire) {
                    self.release_pinyin();
                    return;
                }
                self.set_pinyin_status(load_error_status(error.kind));
                self.pinyin_needs_rebuild.store(true, Ordering::Release);
            }
        }
    }

    fn ensure_pinyin_loaded(&self, index: &IndexState) {
        if self.pinyin.read().is_ok_and(|sidecar| sidecar.is_some()) {
            self.set_pinyin_status(PinyinStatus::Ready);
            return;
        }
        if self.pinyin_status() == PinyinStatus::Building
            || self.pinyin_needs_rebuild.load(Ordering::Acquire)
        {
            return;
        }
        self.load_pinyin(index);
    }

    fn rebuild_pinyin(&self, index: &IndexState, data_dir: &Path) {
        if !self.pinyin_enabled.load(Ordering::Acquire) {
            self.release_pinyin();
            return;
        }
        self.begin_pinyin_rebuild();
        match PinyinSidecar::build(index).and_then(|sidecar| {
            sidecar.save(data_dir)?;
            PinyinSidecar::load(data_dir, index).map_err(|error| error.message)
        }) {
            Ok(sidecar) => {
                if !self.pinyin_enabled.load(Ordering::Acquire) {
                    self.release_pinyin();
                    return;
                }
                if let Ok(mut current) = self.pinyin.write() {
                    *current = Some(sidecar);
                }
                self.set_pinyin_status(PinyinStatus::Ready);
                self.pinyin_needs_rebuild.store(false, Ordering::Release);
                log("pinyin sidecar ready");
            }
            Err(_) => {
                if !self.pinyin_enabled.load(Ordering::Acquire) {
                    self.release_pinyin();
                    return;
                }
                if let Ok(mut current) = self.pinyin.write() {
                    *current = None;
                }
                self.set_pinyin_status(PinyinStatus::Corrupt);
                self.pinyin_needs_rebuild.store(true, Ordering::Release);
                log("pinyin sidecar rebuild failed");
            }
        }
    }

    fn load_pinyin_from_live(&self) {
        if let Ok(index) = self.index.read() {
            if let Some(index) = index.as_ref() {
                self.load_pinyin(index);
            }
        }
    }

    fn rebuild_pinyin_from_live(&self) {
        let data_dir = self
            .pinyin_data_dir
            .read()
            .ok()
            .and_then(|value| value.clone());
        let Some(data_dir) = data_dir else {
            return;
        };
        if let Ok(index) = self.index.read() {
            if let Some(index) = index.as_ref() {
                self.rebuild_pinyin(index, &data_dir);
            }
        }
    }

    fn apply_pinyin_records(&self, volume: usize, records: &[ntfs::UsnRecord]) {
        if !self.pinyin_enabled.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut sidecar_slot) = self.pinyin.write() else {
            self.pinyin_needs_rebuild.store(true, Ordering::Release);
            return;
        };
        let Some(sidecar) = sidecar_slot.as_mut() else {
            self.pinyin_needs_rebuild.store(true, Ordering::Release);
            return;
        };
        let mut invalidate = false;
        for record in records {
            let Ok((record_number, _)) = VolumeIndex::split_frn(record.frn) else {
                invalidate = true;
                break;
            };
            let name = if record.reason & ntfs::USN_REASON_FILE_DELETE != 0 {
                None
            } else if record.reason
                & (ntfs::USN_REASON_FILE_CREATE | ntfs::USN_REASON_RENAME_NEW_NAME)
                != 0
            {
                Some(record.name.as_str())
            } else {
                continue;
            };
            match sidecar.apply_delta(volume, record_number, name) {
                Ok(true) | Err(_) => {
                    invalidate = true;
                    break;
                }
                Ok(false) => {}
            }
        }
        if invalidate {
            *sidecar_slot = None;
            drop(sidecar_slot);
            self.set_pinyin_status(PinyinStatus::Building);
            self.pinyin_needs_rebuild.store(true, Ordering::Release);
        }
    }

    /// Just the generation number, without `status()`'s memory accounting.
    ///
    /// `status()` walks the index and the pinyin sidecar to total `memory_bytes`,
    /// which is far more work than a readiness poll needs. Still takes the read
    /// lock, so callers on the async runtime must go through `spawn_blocking`.
    pub fn generation(&self) -> u64 {
        self.index
            .read()
            .ok()
            .as_deref()
            .and_then(Option::as_ref)
            .map(|state| state.generation)
            .unwrap_or(0)
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
            memory_bytes: state.map(IndexState::memory_bytes).unwrap_or(0)
                + self
                    .pinyin
                    .read()
                    .ok()
                    .and_then(|sidecar| sidecar.as_ref().map(PinyinSidecar::resident_bytes))
                    .unwrap_or(0),
            message: self.message.read().ok().and_then(|value| value.clone()),
            build_progress: self.progress.snapshot(),
            pinyin_status: Some(self.pinyin_status()),
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

    /// 降级恢复：瞬时故障（如 checkpoint 写盘失败）事后自愈时清除提示，
    /// 前端的「文件索引不可用」随下一次成功 checkpoint 自动消失。
    fn clear_error(&self) {
        self.degraded.store(false, Ordering::Release);
        if let Ok(mut message) = self.message.write() {
            *message = None;
        }
    }

    fn search(
        &self,
        query: &str,
        max: usize,
        filters: Option<&[SearchFilter]>,
        pinyin_enabled: bool,
        root: Option<&str>,
    ) -> Result<IndexerResponse, String> {
        validate_search_request(max, filters)?;
        let root = match requested_root(root) {
            Ok(root) => root,
            Err(rejection) => {
                return Ok(IndexerResponse::RootUnavailable {
                    reason: rejection,
                    message: rejection.message().to_owned(),
                })
            }
        };
        let guard = self.index.read().map_err(|_| "index lock is poisoned")?;
        let state = guard.as_ref().ok_or("file index is not ready")?;
        self.pinyin_enabled.store(pinyin_enabled, Ordering::Release);
        if pinyin_enabled {
            self.ensure_pinyin_loaded(state);
        } else {
            self.release_pinyin();
        }
        let root_bound = match root {
            Some(root) => match RootScope::resolve(state, root) {
                Ok(scope) => Some(scope.bound()),
                Err(rejection) => {
                    // The caller falls back to a global search; the reason stays explicit.
                    return Ok(IndexerResponse::RootUnavailable {
                        reason: rejection,
                        message: rejection.message().to_owned(),
                    });
                }
            },
            None => None,
        };
        let generation = state.generation;
        let exts = crate::indexer_ipc::ext_filters(filters);
        let paths = crate::indexer_ipc::path_filters(filters);
        let has_query_filters = !exts.is_empty() || !paths.is_empty();
        // G7: an empty name query normally means "no search", but when ext:/path:
        // filters are present the user wants all files matching the filters — the
        // empty name matches every candidate in match_metadata, so we must not
        // short-circuit here.
        if query.is_empty() && !has_query_filters {
            return Ok(IndexerResponse::Results {
                generation,
                items: Vec::new(),
                is_truncated: false,
                matched_count: Some(0),
                scanned_nodes: Some(0),
                name_candidates: Some(0),
                entered_top_k: Some(0),
                path_constructions: Some(0),
                pinyin_status: Some(self.pinyin_status()),
            });
        }
        let exclusions = crate::indexer_ipc::exclusion_paths(filters);
        let query_filters = crate::hierarchy::QueryFilters::new(
            crate::indexer_ipc::ext_filters(filters),
            crate::indexer_ipc::path_filters(filters),
        );
        let outcome = state.search_in_root_filtered(
            query,
            max,
            &exclusions,
            root_bound,
            &query_filters,
        );
        let mut items: Vec<_> = outcome
            .items
            .into_iter()
            .map(|hit| IndexerItem {
                name: hit.name,
                path: hit.path,
                is_directory: hit.is_directory,
                match_metadata: Some(hit.match_metadata),
                match_spans: None,
            })
            .collect();
        let literal_count = outcome.matched_count;
        let mut matched_count = literal_count;
        let mut path_constructions = outcome.path_constructions;
        if pinyin_enabled && literal_count < max as u64 {
            if let Ok(sidecar) = self.pinyin.read() {
                if let Some(sidecar) = sidecar.as_ref() {
                    let pinyin = sidecar.search_in_root(
                        state,
                        query,
                        max.saturating_sub(items.len()),
                        &exclusions,
                        root_bound,
                        &query_filters,
                    );
                    matched_count = matched_count.saturating_add(pinyin.matched_count);
                    path_constructions =
                        path_constructions.saturating_add(pinyin.path_constructions);
                    items.extend(pinyin.items.into_iter().map(|hit| IndexerItem {
                        name: hit.name,
                        path: hit.path,
                        is_directory: hit.is_directory,
                        match_metadata: Some(hit.match_metadata),
                        match_spans: Some(hit.match_spans),
                    }));
                }
            }
        }
        items.sort_by(|left, right| {
            left.match_metadata
                .cmp(&right.match_metadata)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.path.cmp(&right.path))
                .then(left.is_directory.cmp(&right.is_directory))
        });
        items.truncate(max);
        Ok(IndexerResponse::Results {
            generation,
            items,
            is_truncated: outcome.is_truncated || matched_count > max as u64,
            matched_count: Some(matched_count),
            scanned_nodes: Some(outcome.scanned_nodes),
            name_candidates: Some(outcome.name_candidates),
            entered_top_k: Some(outcome.entered_top_k),
            path_constructions: Some(path_constructions),
            pinyin_status: Some(self.pinyin_status()),
        })
    }

    /// Wait until the index generation passes `after`, or the timeout expires.
    ///
    /// Reads the generation off the runtime via `spawn_blocking`: it needs
    /// `index.read()`, and during a USN flood the watcher holds `index.write()`
    /// in tight batches. Blocking here used to stall a worker thread on every
    /// loop iteration, which with 2 workers starved the pipe accept loop and
    /// surfaced to clients as ERROR_PIPE_BUSY.
    async fn wait_generation(self: &Arc<Self>, after: u64, timeout_ms: u64) -> u64 {
        let timeout = Duration::from_millis(timeout_ms.min(30_000));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.generation_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let generation = self.generation_off_runtime().await;
            if generation > after {
                return generation;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.generation_off_runtime().await;
            }
        }
    }

    /// `generation()` on the blocking pool. Falls back to 0 only if the task
    /// itself fails, which is indistinguishable from "no index yet" to callers.
    async fn generation_off_runtime(self: &Arc<Self>) -> u64 {
        let state = Arc::clone(self);
        tokio::task::spawn_blocking(move || state.generation())
            .await
            .unwrap_or(0)
    }
}

fn load_error_status(kind: LoadErrorKind) -> PinyinStatus {
    match kind {
        LoadErrorKind::Missing | LoadErrorKind::Io => PinyinStatus::Missing,
        LoadErrorKind::Corrupt => PinyinStatus::Corrupt,
        LoadErrorKind::VersionMismatch => PinyinStatus::VersionMismatch,
        LoadErrorKind::IndexMismatch => PinyinStatus::IndexMismatch,
    }
}

pub async fn run(stop: Arc<Shutdown>) -> Result<(), String> {
    let state = ServiceState::new();
    let first_pipe = create_pipe(true).map_err(|error| format!("create indexer pipe: {error}"))?;
    let mut pipe_task = tokio::spawn(serve(state.clone(), first_pipe));
    let data_dir = index_cache::machine_data_dir();
    state.set_pinyin_data_dir(&data_dir);

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
                let error = match result {
                    Ok(Ok(())) => "indexer pipe server stopped unexpectedly".to_string(),
                    Ok(Err(error)) => format!("indexer pipe server: {error}"),
                    Err(error) => format!("indexer pipe task: {error}"),
                };
                logging::event_detail("error", "pipe_server_exit", &error, None, None);
                return Err(error);
            }
            reason = rebuild_rx.recv() => {
                let Some(reason) = reason else { break };
                epoch.fetch_add(1, Ordering::AcqRel);
                state.building.store(true, Ordering::Release);
                logging::event_detail("info", "rebuild_requested", &reason, None, None);
                log(format!("serialized index rebuild requested: {reason}"));
                while rebuild_rx.try_recv().is_ok() {}
                let rebuilt = tokio::task::spawn_blocking(build_all).await
                    .map_err(|error| format!("rebuild task: {error}"))?;
                match rebuilt {
                    Ok((index, descriptors)) => {
                        // 缓存写失败按可降级故障处理：新索引已在内存，先发布继续服务；
                        // 旧缓存 + USN 前滚保证重启安全。退出会丢弃热索引并触发
                        // SCM 重启循环，代价远大于一次降级提示。
                        let save_error = index_cache::save(&index, &data_dir).err();
                        if let Some(error) = &save_error {
                            logging::event_detail("error", "rebuild_save_failed", error, None, None);
                        }
                        state.set_pinyin_status(PinyinStatus::Building);
                        state.publish(index);
                        if let Some(error) = save_error {
                            state.set_error(format!("index cache save failed: {error}"));
                        }
                        if let Err(error) = rebuild_pinyin_from_live(state.clone()).await {
                            logging::event_detail("error", "rebuild_pinyin_failed", &error, None, None);
                            state.set_error(format!("pinyin rebuild failed: {error}"));
                        }
                        start_watchers(state.clone(), descriptors, stop.clone(), epoch.clone(), rebuild_tx.clone());
                        last_checkpoint = Instant::now();
                    }
                    Err(error) => state.set_error(error),
                }
            }
            _ = maintenance.tick() => {
                if state.pinyin_needs_rebuild.swap(false, Ordering::AcqRel)
                    && state.pinyin_status() != PinyinStatus::Disabled
                {
                    if let Err(error) = rebuild_pinyin_from_live(state.clone()).await {
                        logging::event_detail("error", "maintenance_pinyin_failed", &error, None, None);
                        // 内部重建失败已自限为 5 秒节拍重试；这里只可能是任务级故障，降级不退出。
                        state.set_error(format!("pinyin rebuild failed: {error}"));
                    }
                }
                // Everything 模式：低频持久化 + USN 前滚兜底。
                // 6 小时 / 50 万事件覆盖绝大多数会话，落盘开销降为原先的 1/6；
                // 崩溃恢复由缓存里的 next_usn 继续读日志补齐（日志包装走既有重建路径）。
                let checkpoint_due = state.index.read().ok().and_then(|guard| {
                    guard.as_ref().map(|index| index.events_since_checkpoint >= 500_000)
                }).unwrap_or(false) || last_checkpoint.elapsed() >= Duration::from_secs(6 * 60 * 60);
                if checkpoint_due {
                    match checkpoint_async(state.clone(), data_dir.clone()).await {
                        Ok(()) => {
                            state.clear_error();
                            last_checkpoint = Instant::now();
                        }
                        Err(error) => {
                            logging::event_detail("error", "maintenance_checkpoint_failed", &error, None, None);
                            // 磁盘满 / 杀软锁文件等瞬时故障：内存索引继续服务搜索，不退出。
                            // last_checkpoint 前移还避免了 50 万事件路径每 5 秒重试刷爆日志，
                            // 下一个 6 小时节拍自然重试；成功后 clear_error 撤销提示。
                            state.set_error(format!("index checkpoint failed: {error}"));
                            last_checkpoint = Instant::now();
                        }
                    }
                }
            }
        }
    }

    epoch.fetch_add(1, Ordering::AcqRel);
    if let Err(error) = checkpoint_async(state.clone(), data_dir.clone()).await {
        // 用户要求停止就是停止：退出 checkpoint 失败只记一条降级日志，
        // 不变成失败退出码去触发一次毫无意义的 SCM 自动重启。
        logging::event_detail("error", "shutdown_checkpoint_failed", &error, None, None);
    }
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
            // L4：与首建循环的 stop 检查对称。停机请求已到达时不再发布缓存索引：
            // 发布会连带拼音加载，run() 的关停路径还会把刚从盘上读入的索引再做一次
            // 全量 validate + 序列化 + fsync（秒到几十秒），白白拉长 StopPending。
            if stop.is_requested() {
                return Ok(InitialIndex {
                    descriptors,
                    watchers_started: false,
                    interrupted: true,
                });
            }
            publish_cached_index(state, index);
            // 拼音加载的任务级故障（JoinError）不该带着完好的缓存索引一起退出：
            // 内部失败已自降级为字面搜索，这里补一层降级提示即可。
            if let Err(error) = load_pinyin_from_live(state.clone()).await {
                logging::event_detail("error", "cached_pinyin_load_failed", &error, None, None);
                state.set_error(format!("pinyin load failed: {error}"));
            }
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
    // 二次重试后仍失败的卷：跳过并记录，绝不拖垮其余卷（全部失败才算真失败）。
    let mut failed_volumes: Vec<String> = Vec::new();

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

        let volume = match built {
            Ok(volume) => volume,
            Err(error) => {
                logging::event_detail(
                    "error",
                    "initial_build_volume_failed",
                    &format!("{}: {error}", descriptor.mount_path),
                    None,
                    None,
                );
                log(format!(
                    "skipping {} after retry: {error}",
                    descriptor.mount_path
                ));
                failed_volumes.push(format!("{}: {error}", descriptor.mount_path));
                state.progress.volume_done();
                continue;
            }
        };
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

    // 全部卷都失败：没有任何可服务的索引，交回真失败（SCM 兜底重启）。
    if failed_volumes.len() >= descriptors.len() {
        return Err(format!(
            "every volume build failed: {}",
            failed_volumes.join("; ")
        ));
    }

    // S1：首建落盘失败与 rebuild 分支同款降级——热索引已在内存，先继续服务。
    // persist_first_build_with 已照常发布 first_build_complete，所以 6 小时
    // maintenance checkpoint（或停机 checkpoint）会真实重试落盘，成功后
    // clear_error 撤销降级提示；致命退出只会丢弃热索引并触发 SCM 重启
    // （两次全盘 MFT 重扫后服务躺死）。
    if let Err(error) =
        persist_first_build_with(state, |snapshot| index_cache::save(snapshot, data_dir))
    {
        logging::event_detail("error", "first_build_save_failed", &error, None, None);
        state.set_error(format!("index cache save failed: {error}"));
    }
    if let Err(error) = rebuild_pinyin_from_live(state.clone()).await {
        logging::event_detail("error", "first_build_pinyin_failed", &error, None, None);
        state.set_error(format!("pinyin rebuild failed: {error}"));
    }
    if !failed_volumes.is_empty() {
        let message = format!(
            "skipped {} volume(s) during first build: {}",
            failed_volumes.len(),
            failed_volumes.join("; ")
        );
        logging::event_detail("error", "initial_build_skipped_volumes", &message, None, None);
        state.set_error(message);
    }
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
    // Clone the index under the read lock (fast: pure memcpy), then release the
    // lock before save() runs validate + serialize + fsync. validate() walks every
    // node calling path_for (O(depth) per node), which for 3.3M nodes takes many
    // seconds — holding the read lock that long blocks USN writers and starves the
    // 2-worker runtime, causing ERROR_PIPE_BUSY and apparent hangs.
    let snapshot = {
        let guard = state.index.read().map_err(|_| "index lock is poisoned")?;
        guard.as_ref().cloned().ok_or("index is not ready")?
    };
    let save_result = save(&snapshot);
    // S1：走到这里首建已在内存完成（或跳过的卷已被记录），落盘失败是可降级故障。
    // `building=false` 与 R2 门必须照常发布——否则 checkpoint() 会被
    // first_build_is_complete 拦成返回 Ok 的静默空操作，run() 还会把空操作当
    // 成功去 clear_error（假恢复），缓存从此永远写不出去。持久化失败交回调用方
    // set_error 标记。中途被打断的首建不会进入本函数，R2 门对部分索引的保护
    // 不变；只有拿到快照前的失败（锁中毒/无索引）仍按致命错误传播。
    state.finish_first_build();
    save_result
}

fn publish_cached_index(state: &ServiceState, index: IndexState) {
    state.begin_pinyin_rebuild();
    state.publish(index);
}

async fn load_pinyin_from_live(state: Arc<ServiceState>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || state.load_pinyin_from_live())
        .await
        .map_err(|error| format!("pinyin load task: {error}"))
}

async fn rebuild_pinyin_from_live(state: Arc<ServiceState>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || state.rebuild_pinyin_from_live())
        .await
        .map_err(|error| format!("pinyin rebuild task: {error}"))
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
    let mut output = Vec::with_capacity(ntfs::USN_READ_CHUNK);
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
        let (next_usn, records) =
            ntfs::read_changes(&handle, journal_id, start_usn, true, &mut output)?;
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
        let (rebuild_required, volume_number) = {
            let mut guard = service
                .index
                .write()
                .map_err(|_| "index lock is poisoned")?;
            if stop.is_requested() || epoch.load(Ordering::Acquire) != watcher_epoch {
                return Ok(());
            }
            let index = guard.as_mut().ok_or("index is not ready")?;
            let volume_number = index
                .volumes
                .iter()
                .position(|volume| volume.volume_id == descriptor.id)
                .ok_or("volume disappeared from live index")?;
            let volume = &mut index.volumes[volume_number];
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
            (rebuild_required, volume_number)
        };
        if changed > 0 {
            service.apply_pinyin_records(volume_number, &records);
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
    // Clone under the read lock (fast memcpy), then release before save(). save() calls
    // validate() which walks every node with path_for (O(depth) per node) — holding the
    // read lock that long blocks USN writers and starves the 2-worker async runtime.
    let snapshot = {
        let guard = state.index.read().map_err(|_| "index lock is poisoned")?;
        guard.as_ref().cloned().ok_or("index is not ready")?
    };
    if let Err(error) = index_cache::save(&snapshot, data_dir) {
        logging::event_detail("error", "checkpoint_save_failed", &error, None, None);
        return Err(error);
    }
    if state.pinyin_status() != PinyinStatus::Disabled {
        // Rebuild from the live tree under its read lock. A clone taken for the v5
        // checkpoint can be one USN batch behind by the time the sidecar is installed.
        state.rebuild_pinyin_from_live();
    }
    if let Ok(mut guard) = state.index.write() {
        if let Some(index) = guard.as_mut() {
            index.events_since_checkpoint = index
                .events_since_checkpoint
                .saturating_sub(snapshot.events_since_checkpoint);
        }
    }
    Ok(())
}

async fn checkpoint_async(state: Arc<ServiceState>, data_dir: PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || checkpoint(&state, &data_dir))
        .await
        .map_err(|error| format!("checkpoint task: {error}"))?
}

/// Number of concurrently armed pipe listeners.
///
/// A single listener has an unavoidable gap: after `connect()` returns, nothing
/// is listening until `create_pipe` re-arms. Clients arriving in that gap get
/// `ERROR_PIPE_BUSY` (os error 231). The gap is microseconds when idle, but the
/// runtime only has 2 worker threads, so under load the re-arm can be delayed
/// past the client's retry budget (5 x 20ms in `indexer_client::connect`).
///
/// Measured 2026-08-07 during a USN flood (index advancing ~340 generations/s
/// while Windows Update rewrote the disk): 517 of 800 searches failed with
/// ERROR_PIPE_BUSY. Sequential single-client load never reproduced it, which is
/// what ruled out a fixed capacity limit and pointed at scheduling delay.
///
/// Keeping several listeners armed means a sibling is still accepting while any
/// one of them re-arms.
const PIPE_LISTENERS: usize = 4;

async fn serve(state: Arc<ServiceState>, first_pipe: NamedPipeServer) -> Result<(), String> {
    let mut listeners = tokio::task::JoinSet::new();
    listeners.spawn(accept_loop(state.clone(), first_pipe));
    for _ in 1..PIPE_LISTENERS {
        // `first_pipe_instance` must only be set on the very first instance;
        // the caller already created that one.
        let pipe = create_pipe(false).map_err(|error| error.to_string())?;
        listeners.spawn(accept_loop(state.clone(), pipe));
    }

    // Any loop exiting means we can no longer guarantee an armed listener, which
    // the caller treats as fatal rather than silently degrading to fewer slots.
    let first = listeners.join_next().await;
    listeners.abort_all();
    match first {
        Some(Ok(Ok(()))) => Err("indexer pipe listener exited".into()),
        Some(Ok(Err(error))) => Err(error),
        Some(Err(error)) => Err(format!("indexer pipe listener task: {error}")),
        None => Err("no indexer pipe listeners were started".into()),
    }
}

/// One listener's accept loop: wait for a client, hand the connection to a task,
/// then immediately re-arm this slot.
async fn accept_loop(state: Arc<ServiceState>, mut server: NamedPipeServer) -> Result<(), String> {
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
    // 有界逐行读（1MB 上限）：无上限的 lines() 会让任意本地进程灌超长行撑爆内存。
    let mut lines = crate::ipc::BoundedLineReader::new(reader);
    let hello = lines
        .next_line()
        .await?
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
    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        let response = match serde_json::from_str::<IndexerRequest>(&line) {
            // `status()` takes `index.read()` synchronously. Calling it directly
            // on a worker thread lets a USN flood (watcher holding `index.write()`)
            // block the whole runtime: with 2 workers, two concurrent Status
            // requests starve the accept loop and new clients get
            // ERROR_PIPE_BUSY. Same reason Search already uses spawn_blocking.
            Ok(IndexerRequest::Status) => {
                let state = state.clone();
                match tokio::task::spawn_blocking(move || state.status()).await {
                    Ok(status) => IndexerResponse::Status(status),
                    Err(error) => IndexerResponse::Error {
                        message: format!("status task: {error}"),
                    },
                }
            }
            Ok(IndexerRequest::Search {
                query,
                max,
                filters,
                pinyin_enabled,
                root,
            }) => {
                let state = state.clone();
                match tokio::task::spawn_blocking(move || {
                    state.search(
                        &query,
                        max,
                        filters.as_deref(),
                        pinyin_enabled.unwrap_or(false),
                        root.as_deref(),
                    )
                })
                .await
                {
                    Ok(Ok(response)) => response,
                    Ok(Err(message)) => IndexerResponse::Error { message },
                    Err(error) => IndexerResponse::Error {
                        message: format!("index search task: {error}"),
                    },
                }
            }
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
    use crate::root_scope::RootRejection;

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
            .search("needle", 8, None, false, None)
            .is_ok_and(|response| matches!(response, IndexerResponse::Results { ref items, .. } if items.len() == 1)));
    }

    #[test]
    fn disabled_missing_and_building_pinyin_states_keep_literal_search_available() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "微信"));

        let literal = state.search("微信", 8, None, true, None).unwrap();
        assert!(matches!(literal, IndexerResponse::Results { ref items, .. } if items.len() == 1));
        assert_eq!(state.pinyin_status(), PinyinStatus::Building);

        let pinyin_without_sidecar = state.search("wx", 8, None, true, None).unwrap();
        assert!(
            matches!(pinyin_without_sidecar, IndexerResponse::Results { ref items, .. } if items.is_empty())
        );

        let snapshot = state.index.read().unwrap().as_ref().unwrap().clone();
        *state.pinyin.write().unwrap() = Some(PinyinSidecar::build(&snapshot).unwrap());
        let disabled = state.search("微信", 8, None, false, None).unwrap();
        assert!(matches!(disabled, IndexerResponse::Results { ref items, .. } if items.len() == 1));
        assert!(state.pinyin.read().unwrap().is_none());
        assert_eq!(state.pinyin_status(), PinyinStatus::Disabled);
        assert!(
            state.status().building,
            "partial build status remains independent"
        );
    }

    #[test]
    fn cached_index_publishes_before_missing_or_corrupt_sidecar_rebuild() {
        let dir =
            std::env::temp_dir().join(format!("prism-g2-cache-fail-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let missing = ServiceState::new();
        missing.set_pinyin_data_dir(&dir);
        publish_cached_index(
            &missing,
            IndexState {
                volumes: vec![test_volume("missing", "C:\\", "微信")],
                generation: 7,
                events_since_checkpoint: 0,
            },
        );
        assert!(matches!(
            missing.search("微信", 8, None, true, None).unwrap(),
            IndexerResponse::Results { ref items, .. } if items.len() == 1
        ));
        assert_eq!(missing.pinyin_status(), PinyinStatus::Building);
        missing.load_pinyin_from_live();
        assert_eq!(missing.pinyin_status(), PinyinStatus::Missing);
        assert!(missing.pinyin_needs_rebuild.load(Ordering::Acquire));

        std::fs::write(dir.join("pinyin-v1.bin"), b"not-a-sidecar").unwrap();
        let corrupt = ServiceState::new();
        corrupt.set_pinyin_data_dir(&dir);
        publish_cached_index(
            &corrupt,
            IndexState {
                volumes: vec![test_volume("corrupt", "C:\\", "微信")],
                generation: 8,
                events_since_checkpoint: 0,
            },
        );
        assert!(matches!(
            corrupt.search("微信", 8, None, true, None).unwrap(),
            IndexerResponse::Results { ref items, .. } if items.len() == 1
        ));
        assert_eq!(corrupt.pinyin_status(), PinyinStatus::Building);
        corrupt.load_pinyin_from_live();
        assert_eq!(corrupt.pinyin_status(), PinyinStatus::Corrupt);
        assert!(corrupt.pinyin_needs_rebuild.load(Ordering::Acquire));

        let _ = std::fs::remove_dir_all(&dir);
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
    fn first_build_save_runs_before_completion_is_visible() {
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

    /// S1：落盘失败不再向上致命传播。错误交回调用方 set_error 降级，但首建
    /// 完成状态与 R2 门必须照常发布——否则 checkpoint 重试被拦成空操作。
    #[test]
    fn failed_first_build_cache_save_degrades_but_opens_the_checkpoint_gate() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));

        let error = persist_first_build_with(&state, |_| Err("cache save failed".into()))
            .expect_err("save failure is still reported for set_error");

        assert_eq!(error, "cache save failed");
        assert!(
            !state.status().building,
            "the in-memory build is complete; only persistence is pending"
        );
        assert!(
            state.first_build_is_complete(),
            "the R2 gate must open so the checkpoint retry is real"
        );
    }

    /// S1 的回归锚：落盘失败后的 checkpoint（6 小时节拍或停机路径）必须真实
    /// 重试写盘。旧语义下 R2 门保持关闭，checkpoint 返回 Ok 却什么都没写，
    /// run() 随即 clear_error——用户看到"恢复"，实际永不落盘（假恢复）。
    #[test]
    fn checkpoint_really_retries_after_a_failed_first_build_save() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));

        let dir = std::env::temp_dir().join(format!("prism-s1-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        persist_first_build_with(&state, |_| Err("cache save failed".into())).unwrap_err();
        assert!(!index_cache::cache_path(&dir).exists());

        checkpoint(&state, &dir).unwrap();
        assert!(
            index_cache::cache_path(&dir).exists(),
            "the gate must be open so the checkpoint retries the save"
        );

        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn requested_root_scopes_the_service_search_and_blank_root_stays_global() {
        let state = ServiceState::new();
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "root".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            9,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "项目", true).unwrap();
        volume.upsert(11, 10, "needle-inside.txt", false).unwrap();
        volume.upsert(20, 5, "needle-outside.txt", false).unwrap();
        state.publish(IndexState {
            volumes: vec![volume],
            generation: 3,
            events_since_checkpoint: 0,
        });

        let scoped = state
            .search("needle", 8, None, false, Some(r"c:/项目/"))
            .unwrap();
        let IndexerResponse::Results { items, .. } = scoped else {
            panic!("expected results for a valid root");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, r"C:\项目\needle-inside.txt");

        // Absent and blank roots must behave identically to the pre-root protocol.
        for root in [None, Some(""), Some("   ")] {
            let global = state.search("needle", 8, None, false, root).unwrap();
            let IndexerResponse::Results { items, .. } = global else {
                panic!("expected results for {root:?}");
            };
            assert_eq!(items.len(), 2, "root {root:?} must not restrict anything");
        }
    }

    #[test]
    fn unusable_root_is_reported_as_a_structured_degradation() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));

        for (root, expected) in [
            (r"E:\somewhere", RootRejection::VolumeNotIndexed),
            (r"C:\missing", RootRejection::NotFound),
            ("relative", RootRejection::NotAbsolute),
            (r"\\server\share", RootRejection::Unsupported),
        ] {
            let response = state.search("needle", 8, None, false, Some(root)).unwrap();
            match response {
                IndexerResponse::RootUnavailable { reason, message } => {
                    assert_eq!(reason, expected, "unexpected reason for {root}");
                    assert!(!message.is_empty());
                }
                other => panic!("expected RootUnavailable for {root}, got {other:?}"),
            }
        }
    }
}
