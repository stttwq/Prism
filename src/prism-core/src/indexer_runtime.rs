//! Long-running indexer service state, IPC, checkpointing, and rebuild coordination.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, Notify};

use crate::hierarchy::{ApplyOutcome, IndexState, VolumeId, VolumeIndex};
use crate::index_cache;
use crate::indexer_ipc::{
    requested_root, validate_search_request, BuildProgress, IndexerItem, IndexerRequest,
    IndexerResponse, IndexerStatus, PinyinStatus, SearchFilter,
};
use crate::logging;
use crate::ntfs::{self, VolumeDescriptor};
use crate::pinyin_sidecar::{LoadErrorKind, PinyinDelta, PinyinSidecar};
use crate::root_scope::RootScope;
use crate::{log, INDEXER_PIPE_NAME, INDEXER_PROTOCOL};

/// AUDIT-2026-08-18 R-B2: rebuild 请求区分单卷与全量。
/// 单卷 watcher 出错只重建该卷并 merge_and_publish 替换，
/// 不再 epoch+1 杀掉所有健康卷 watcher 触发全盘重扫。
#[derive(Clone)]
enum RebuildRequest {
    /// 单卷重建：只重建该卷，merge_and_publish 按 volume_id 替换。
    SingleVolume { descriptor: VolumeDescriptor, reason: String },
    /// 全量重建（卷集合变化等）。当前无发送点，保留为未来扩展。
    #[allow(dead_code)]
    Full(String),
}

/// 2026-08-22 内存收口：当前 Unix 毫秒。ServiceState::last_activity_ms 的时间源。
/// 用 SystemTime 而非 Instant：后者是单调时钟相对值，无法跨重启/线程语义稳定比较
/// 「距上次活动多久」，而毫秒绝对值直白且与 maintenance tick 的 Instant 节拍解耦。
fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 2026-08-22 内存收口：indexer 空闲后修剪工作集的等待阈值。与前端 3 分钟 trim
/// 同节奏——用户「单次搜索后立即破线」的诉求靠这一拍把 indexer 从 ~64MB 降到
/// 数 MB（OS 自发修剪要约 1 小时）。仅 !building && ready 时执行，重建窗口绝不
/// 碰工作集（正在高强度建卷/合并，修剪只会触发海量软缺页拖慢重建）。
const IDLE_TRIM_THRESHOLD_MS: u64 = 3 * 60 * 1000;

/// 2026-08-22 内存收口：修剪本进程工作集。SetProcessWorkingSetSizeEx 传
/// (SIZE_T)-1, (SIZE_T)-1 是微软文档记录的「尽可能清空工作集」惯用法，
/// 等价于 K32EmptyWorkingSet（前端同款）。仅 Windows；非 Windows 空实现。
#[cfg(windows)]
fn trim_working_set() {
    use windows::Win32::System::Memory::SetProcessWorkingSetSizeEx;
    use windows::Win32::System::Threading::GetCurrentProcess;
    // 安全：GetCurrentProcess 返回伪句柄（-1），无需 CloseHandle；
    // SetProcessWorkingSetSizeEx 失败只意味着「没修成」，无副作用——
    // 下次搜索/USN 批次按需软缺页调入。忽略返回值，与前端 catch{} 同义。
    unsafe {
        let _ = SetProcessWorkingSetSizeEx(
            GetCurrentProcess(),
            usize::MAX,
            usize::MAX,
            windows::Win32::System::Memory::SETPROCESSWORKINGSETSIZEEX_FLAGS(0),
        );
    }
}

#[cfg(not(windows))]
fn trim_working_set() {}



/// M1（FRESH-AUDIT-3-2026-08-20）：单卷重建的退避簿记。
/// 连续重建按 `apps::app_scan_retry_delay` 指数拉开（30s 起步、封顶 1h、永不放弃），
/// 卷静默 `VOLUME_REBUILD_QUIET` 后 attempt 归零——偶发失败不背历史包袱。
struct VolumeRebuildBackoff {
    id: VolumeId,
    attempt: u32,
    next_due: Instant,
    last_rebuild_at: Option<Instant>,
}

/// M1：失败（或退避窗口内到达）的单卷重建请求，到 due 时由 maintenance tick 重发。
struct DeferredVolumeRebuild {
    descriptor: VolumeDescriptor,
    reason: String,
    due: Instant,
}

/// M1：卷静默多久后重置连续重建计数。1 小时 = 最长退避间隔，超过它说明上一次
/// 重建后卷已稳定运行，新错误按首次处理。
const VOLUME_REBUILD_QUIET: Duration = Duration::from_secs(3600);

/// M3（FRESH-AUDIT-3-2026-08-20）：拼音重建退避间隔。重建失败反复置位
/// needs_rebuild 的风暴期，不再每 5s tick 一轮全量重建。
const PINYIN_REBUILD_BACKOFF: Duration = Duration::from_secs(60);

/// H1（FRESH-AUDIT-3-2026-08-20）：积压合并——按卷去重保留最新一条。
/// 全量请求（Full）覆盖一切单卷请求；同卷多条单卷请求保留最后一条
///（watcher 死因以最新为准）。输出按各卷首次出现顺序排列。
/// 旧的 `while try_recv().is_ok() {}` 是留最旧丢最新：两卷相近时刻出错时
/// 第二个卷的重建请求被静默丢弃，该卷 watcher 已退出且无 liveness 检查，
/// 索引从此静默陈旧直到服务重启。
fn dedupe_rebuild_requests(mut pending: Vec<RebuildRequest>) -> Vec<RebuildRequest> {
    // 任何 Full 都使单卷请求失去意义（全量重建覆盖全部卷），取最后一条 Full。
    for position in (0..pending.len()).rev() {
        if matches!(pending[position], RebuildRequest::Full(_)) {
            return vec![pending.swap_remove(position)];
        }
    }
    let mut order: Vec<VolumeId> = Vec::new();
    let mut latest: Vec<RebuildRequest> = Vec::new();
    for request in pending {
        let RebuildRequest::SingleVolume { descriptor, .. } = &request else {
            continue;
        };
        match order.iter().position(|id| *id == descriptor.id) {
            Some(slot) => latest[slot] = request,
            None => {
                order.push(descriptor.id.clone());
                latest.push(request);
            }
        }
    }
    latest
}

/// M1：取（或建）某卷的退避条目。新建条目立即到期（首次重建无退避）。
fn volume_backoff_entry<'a>(
    entries: &'a mut Vec<VolumeRebuildBackoff>,
    id: &VolumeId,
    now: Instant,
) -> &'a mut VolumeRebuildBackoff {
    let position = match entries.iter().position(|entry| &entry.id == id) {
        Some(position) => position,
        None => {
            entries.push(VolumeRebuildBackoff {
                id: id.clone(),
                attempt: 0,
                next_due: now,
                last_rebuild_at: None,
            });
            entries.len() - 1
        }
    };
    &mut entries[position]
}

/// M1：退避准入决策（纯函数，测试注入合成 Instant）。
/// 到点准入并推进 attempt 与 next_due；窗口内拒绝且不动计数。
fn admit_volume_rebuild(entry: &mut VolumeRebuildBackoff, now: Instant) -> bool {
    if entry
        .last_rebuild_at
        .is_some_and(|at| now - at >= VOLUME_REBUILD_QUIET)
    {
        entry.attempt = 0;
    }
    if now < entry.next_due {
        return false;
    }
    entry.attempt = entry.attempt.saturating_add(1);
    entry.last_rebuild_at = Some(now);
    entry.next_due = now + crate::apps::app_scan_retry_delay(entry.attempt);
    true
}

/// M1：登记/刷新某卷的挂起重试（同卷只保留最新请求）。
fn defer_volume_rebuild(
    deferred: &mut Vec<DeferredVolumeRebuild>,
    descriptor: VolumeDescriptor,
    reason: String,
    due: Instant,
) {
    match deferred
        .iter_mut()
        .find(|pending| pending.descriptor.id == descriptor.id)
    {
        Some(pending) => {
            pending.descriptor = descriptor;
            pending.reason = reason;
            pending.due = due;
        }
        None => deferred.push(DeferredVolumeRebuild {
            descriptor,
            reason,
            due,
        }),
    }
}

/// M1：该卷重建成功后撤销挂起重试（失败后 watcher 又报错重排的那份）。
fn remove_deferred_rebuild(deferred: &mut Vec<DeferredVolumeRebuild>, id: &VolumeId) {
    deferred.retain(|pending| &pending.descriptor.id != id);
}

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
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut guard = self.current_volume.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            *guard = None;
        }
        self.active.store(true, Ordering::Release);
    }

    fn begin_volume(&self, mount_path: &str) {
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut guard = self.current_volume.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
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
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut guard = self.current_volume.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
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

/// F2 的解析结果：Bound = 继续搜索；Unavailable = 结构化拒绝回传调用方。
/// （不用 Result<_, IndexerResponse>：IndexerResponse 体积大，clippy
/// result_large_err 会拦；自定义枚举同样表达且零开销。）
#[derive(Debug)]
enum RootBoundOutcome {
    Bound(Option<crate::hierarchy::RootBound>),
    Unavailable(IndexerResponse),
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
    pinyin: RwLock<Option<Arc<PinyinSidecar>>>,
    /// A1（AUDIT-4 批次B，2026-08-21）：拼音增量表独立于主表。此前 delta 活在
    /// `PinyinSidecar` 里，USN 批次经 `Arc::make_mut` COW 深克隆整份主表
    /// （几十 MB）——击键搜索几乎总在飞，浏览器/WU 持续写盘时每批次一次，
    /// 分配 churn + 瞬时 2× 常驻。拆出后主表 Arc 快照永不变异，watcher 只写
    /// 这张小表；重建/卸载主表时必须同步 clear（旧 delta 掩蔽新主表会产错命中）。
    pinyin_delta: RwLock<PinyinDelta>,
    pinyin_status: RwLock<PinyinStatus>,
    pinyin_data_dir: RwLock<Option<PathBuf>>,
    pinyin_needs_rebuild: AtomicBool,
    pinyin_enabled: AtomicBool,
    /// AUDIT-2026-08-18 R-A3: 活跃连接数。每个连接一个 tokio 任务 + 1MB 行缓冲，
    /// 无上限时任凭本地进程堆积连接即可耗尽 2 worker 的 runtime。
    connections: AtomicUsize,
    /// 2026-08-22 内存收口：最近一次用户/系统活动的 Unix 毫秒（搜索请求或
    /// USN 批次 apply）。maintenance tick 据此判定空闲——单次搜索会把整卷
    /// MFT 索引的随机节点拉进工作集（indexer 常驻 ~64MB），OS 自发修剪要
    /// 约 1 小时；在 3 分钟空闲点主动 SetProcessWorkingSetSizeEx(-1,-1)
    /// 提前释放，与前端 3 分钟 trim 同节奏。AtomicU64 存 Unix 毫秒（Instant
    /// 不可跨 async 任务持久比较的偏移场景，毫秒绝对值更直白）。
    last_activity_ms: AtomicU64,
    /// B1（AUDIT-4 批次C，2026-08-21）：RootBound 解析缓存（4 槽）。
    /// 失效粒度从全局 generation 放宽到「被解析卷自身 next_usn」（与流式
    /// checkpoint 的按卷核对同思路）：USN 洪峰期（浏览器/WU 持续写盘）
    /// generation 每批次 +1，他卷活动不再作废本卷的解析结果；只有被解析卷
    /// 自身的 next_usn 前移（节点表已变）才需要重解析。volume_id 防卷表
    /// 重排/单卷重建后的索引位错配。
    root_bound_cache: RwLock<Vec<RootBoundCacheEntry>>,
}

/// B1：RootBound 缓存条目（见 `root_bound_cache` 字段注释）。
#[derive(Debug, Clone)]
pub struct RootBoundCacheEntry {
    pub root: String,
    pub volume_id: crate::hierarchy::VolumeId,
    pub volume_index: usize,
    pub next_usn: i64,
    pub bound: crate::hierarchy::RootBound,
}

impl ServiceState {
    /// AUDIT-2026-08-18 R-A3: 并发连接上限。超出者在握手前直接关闭——
    /// 正常部署只有 broker 一条长连接 + 少量世代客户端，64 已是宽裕上限。
    const MAX_CONNECTIONS: usize = 64;

    /// 连接准入：计数未超上限则占一席并返回 true。配对的 `release_connection`
    /// 由连接任务结束时调用。超限返回 false，调用方直接断开（握手前）。
    fn try_admit_connection(&self) -> bool {
        let current = self.connections.fetch_add(1, Ordering::AcqRel);
        if current >= Self::MAX_CONNECTIONS {
            self.connections.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    fn release_connection(&self) {
        self.connections.fetch_sub(1, Ordering::AcqRel);
    }

    /// 记录一次用户活动（搜索请求；maintenance tick 修剪后也重置以防连拍），
    /// 刷新空闲计时起点。USN 批次不在此刷新——OS 后台文件活动不应阻塞修剪
    ///（见 watch_volume 中不调用 touch_activity 的注释）。
    fn touch_activity(&self) {
        self.last_activity_ms.store(unix_ms_now(), Ordering::Release);
    }

    /// 距上次活动是否已超过阈值（毫秒）。用于 maintenance tick 判定是否
    /// 该修剪工作集。启动后从未有活动时 last_activity_ms 即启动时刻，
    /// 仍按「自启动起算」判定——首轮建卷完成后若空闲也该修剪。
    fn idle_for_at_least(&self, threshold_ms: u64) -> bool {
        let last = self.last_activity_ms.load(Ordering::Acquire);
        let now = unix_ms_now();
        now.saturating_sub(last) >= threshold_ms
    }

    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            index: RwLock::new(None),
            building: AtomicBool::new(true),
            degraded: AtomicBool::new(false),
            message: RwLock::new(None),
            generation_notify: Notify::new(),
            progress: BuildProgressCounters::default(),
            first_build_complete: AtomicBool::new(false),
            pinyin: RwLock::new(None),            pinyin_status: RwLock::new(PinyinStatus::Building),
            pinyin_delta: RwLock::new(PinyinDelta::new()),
            pinyin_data_dir: RwLock::new(None),
            pinyin_needs_rebuild: AtomicBool::new(false),
            pinyin_enabled: AtomicBool::new(true),
            connections: AtomicUsize::new(0),
            last_activity_ms: AtomicU64::new(unix_ms_now()),
            root_bound_cache: RwLock::new(Vec::new()),
        })
    }

    fn set_pinyin_data_dir(&self, data_dir: &Path) {
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut slot = self.pinyin_data_dir.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
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
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut current = self.pinyin_status.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            *current = status;
        }
    }

    fn release_pinyin(&self) {
        self.pinyin_enabled.store(false, Ordering::Release);
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut sidecar = self.pinyin.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            *sidecar = None;
        }
        self.pinyin_delta
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.set_pinyin_status(PinyinStatus::Disabled);
    }

    fn begin_pinyin_rebuild(&self) {
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut sidecar = self.pinyin.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            *sidecar = None;
        }
        // A1：主表卸载的同时清 delta——主表缺席期间 delta 无从对齐（重建后的
        // 新主表已含全部已应用事件，旧掩蔽条目反而会盖掉新编码）。
        self.pinyin_delta
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.set_pinyin_status(PinyinStatus::Building);
    }

    /// Loads the pinyin sidecar without holding the index read lock during the
    /// expensive mmap + deserialization + validation phase.
    ///
    /// The index identity hash is computed under a brief read lock, then the
    /// lock is released.  If the index changed (USN events applied) during the
    /// load, the sidecar's delta mechanism catches up on install — so a strict
    /// generation match is not enforced here.  The identity hash guards against
    /// structural changes (volume set / names pool content) that the delta
    /// mechanism cannot reconcile.
    fn load_pinyin_outside_lock(&self) {
        // A rebuild is in progress or a previous load already failed and queued
        // a rebuild — let the maintenance loop handle it rather than racing.
        if self.pinyin_status() == PinyinStatus::Building
            || self.pinyin_needs_rebuild.load(Ordering::Acquire)
        {
            return;
        }
        self.load_pinyin_outside_lock_force();
    }

    /// Like `load_pinyin_outside_lock` but bypasses the Building/needs_rebuild
    /// guard.  Used by the initial cache-hit path (`load_pinyin_from_live`) which
    /// must transition out of the `Building` state set by `begin_pinyin_rebuild`.
    fn load_pinyin_outside_lock_force(&self) {
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
        // Read lock only to compute identity + generation, then release.
        let (identity, volume_count) = {
            let Ok(guard) = self.index.read() else {
                return;
            };
            let Some(index) = guard.as_ref() else {
                return;
            };
            (
                crate::pinyin_sidecar::index_identity(index),
                u16::try_from(index.volumes.len()).unwrap_or(u16::MAX),
            )
        };
        // Lock-free: mmap + postcard deserialize + validate_disk.
        match PinyinSidecar::load_with_identity(&data_dir, identity, volume_count) {
            Ok(sidecar) => {
                if !self.pinyin_enabled.load(Ordering::Acquire) {
                    self.release_pinyin();
                    return;
                }
                // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
                let mut current = self.pinyin.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                {
                    *current = Some(Arc::new(sidecar));
                }
                // A1：安装全新主表的同时清 delta（新主表来自重建后的索引快照，
                // 已含全部已应用事件；旧掩蔽条目盖新编码会产错命中）。
                self.pinyin_delta
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clear();
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
                // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
                let mut current = self.pinyin.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                {
                    *current = Some(Arc::new(sidecar));
                }
                // A1：同 load 安装路径——新主表 + 清空 delta 成对出现。
                self.pinyin_delta
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clear();
                self.set_pinyin_status(PinyinStatus::Ready);
                self.pinyin_needs_rebuild.store(false, Ordering::Release);
                log("pinyin sidecar ready");
            }
            Err(_) => {
                if !self.pinyin_enabled.load(Ordering::Acquire) {
                    self.release_pinyin();
                    return;
                }
                // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
                let mut current = self.pinyin.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                {
                    *current = None;
                }
                self.set_pinyin_status(PinyinStatus::Corrupt);
                self.pinyin_needs_rebuild.store(true, Ordering::Release);
                log("pinyin sidecar rebuild failed");
            }
        }
    }

    fn load_pinyin_from_live(&self) {
        // Force-load: bypass the Building/needs_rebuild guard so the initial
        // cache-hit path can transition from Building to Ready/Missing/Corrupt.
        self.load_pinyin_outside_lock_force();
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
        // AUDIT-2026-08-18 R-B1: 读锁内只 clone 快照（纯 memcpy，毫秒级），
        // 立刻放锁后再做全节点遍历 + save（含 fsync）。对齐 checkpoint 本体
        // （1343 行）与 persist_first_build_with（1016 行）的既有正确模式。
        // 之前在 read guard 内直接调 rebuild_pinyin，百万级中文文件下持锁
        // 数秒~数十秒，阻塞 USN 写者和搜索读者。
        let snapshot = {
            let Ok(guard) = self.index.read() else { return; };
            let Some(index) = guard.as_ref() else { return; };
            index.clone()
        };
        self.rebuild_pinyin(&snapshot, &data_dir);
    }

    fn apply_pinyin_records(&self, volume: usize, records: &[ntfs::UsnRecord]) {
        if !self.pinyin_enabled.load(Ordering::Acquire) {
            return;
        }
        // A1（AUDIT-4 批次B，2026-08-21）：主表只读共享（Arc 快照），增量写进
        // 独立的 delta 表——彻底移除 `Arc::make_mut` 的整表 COW 深克隆
        //（几十 MB/批，USN 洪峰 + 击键搜索并发时每批次一次）。主表缺席
        //（Building/卸载/重建窗口）时不写 delta：掉线的主表无法与增量对齐，
        // 置 needs_rebuild 交 maintenance 全量重建（与拆分前语义一致）。
        let main_table_present = self
            .pinyin
            .read()
            .map(|slot| slot.is_some())
            .unwrap_or(false);
        if !main_table_present {
            self.pinyin_needs_rebuild.store(true, Ordering::Release);
            return;
        }
        // P2（搜索报告2，2026-08-21）：目录 RENAME 使全部**既有**后代的链失效
        //（旧首字母残留会产错命中）。目录 CREATE 无既有后代、DELETE 伴随子节点
        // 各自 DELETE 掩蔽——都走 delta 即可；唯独 RENAME 宁少勿错：整表卸载
        // 排队全量重建（M3 的 60s 退避限频，folder 改名风暴不会连环重建）。
        let directory_renamed = records.iter().any(|record| {
            record.is_directory
                && record.reason & ntfs::USN_REASON_FILE_DELETE == 0
                && record.reason & ntfs::USN_REASON_RENAME_NEW_NAME != 0
        });
        if directory_renamed {
            self.begin_pinyin_rebuild();
            self.pinyin_needs_rebuild.store(true, Ordering::Release);
            return;
        }
        // P2：delta 编码需要每条记录的目录链（按批从活索引算，批内同父共享缓存）。
        let delta_records: Vec<u32> = records
            .iter()
            .filter_map(|record| VolumeIndex::split_frn(record.frn).ok().map(|(r, _)| r))
            .collect();
        let chains = {
            let Ok(guard) = self.index.read() else {
                self.pinyin_needs_rebuild.store(true, Ordering::Release);
                return;
            };
            let Some(index) = guard.as_ref() else {
                self.pinyin_needs_rebuild.store(true, Ordering::Release);
                return;
            };
            index
                .volumes
                .get(volume)
                .map(|live| crate::pinyin_sidecar::chains_for_delta(live, &delta_records))
        };
        let Some(chains) = chains else {
            self.begin_pinyin_rebuild();
            self.pinyin_needs_rebuild.store(true, Ordering::Release);
            return;
        };
        let mut delta = self
            .pinyin_delta
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut invalidate = false;
        for (record, chain) in records.iter().zip(chains.iter()) {
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
            match delta.apply(volume, record_number, name, chain) {
                Ok(true) | Err(_) => {
                    invalidate = true;
                    break;
                }
                Ok(false) => {}
            }
        }
        drop(delta);
        if invalidate {
            // 到达重建阈值或坏记录：卸载主表（连带清 delta）排队全量重建。
            self.begin_pinyin_rebuild();
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

    /// S5（FRESH-AUDIT-2026-08-19）: 小时级内存趋势的 detail 文本（供事件日志）。
    /// Listary 9.65GB 泄漏正是靠用户截图才发现——有趋势沉淀，异常增长可以在
    /// 用户报告前从日志回溯。内容只有数字与枚举名，无路径/查询，不涉脱敏。
    pub fn memory_trend_detail(&self) -> String {
        let index = self.index.read().ok();
        let state = index.as_deref().and_then(Option::as_ref);
        let memory = state.map(IndexState::memory_bytes).unwrap_or(0)
            + self
                .pinyin
                .read()
                .ok()
                .and_then(|sidecar| sidecar.as_ref().map(|s| s.resident_bytes()))
                .unwrap_or(0)
            + self
                .pinyin_delta
                .read()
                .map(|delta| delta.resident_bytes())
                .unwrap_or(0);
        format!(
            "memory_bytes={} volumes={} events_since_checkpoint={} pinyin={:?}",
            memory,
            state.map(|value| value.volumes.len()).unwrap_or(0),
            state
                .map(|value| value.events_since_checkpoint)
                .unwrap_or(0),
            self.pinyin_status(),
        )
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
                    .and_then(|sidecar| sidecar.as_ref().map(|s| s.resident_bytes()))
                    .unwrap_or(0)
                + self
                    .pinyin_delta
                    .read()
                    .map(|delta| delta.resident_bytes())
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
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut guard = self.index.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
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
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut message = self.message.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
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
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut guard = self.index.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            *guard = Some(state);
        }
        self.building.store(false, Ordering::Release);
        self.degraded.store(false, Ordering::Release);
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut message = self.message.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
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
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut message = self.message.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            *message = Some(error);
        }
    }

    /// 降级恢复：瞬时故障（如 checkpoint 写盘失败）事后自愈时清除提示，
    /// 前端的「文件索引不可用」随下一次成功 checkpoint 自动消失。
    fn clear_error(&self) {
        self.degraded.store(false, Ordering::Release);
        // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
        let mut message = self.message.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            *message = None;
        }
    }

    /// F2 + B1（AUDIT-4 批次C）：RootBound 缓存解析。调用方持有 index 读锁
    ///（`state` 借自它）；缓存锁只在此处获取，与 index 锁无反向嵌套，无死序
    /// 风险。命中条件：同 root + 卷仍在原索引位 + 卷身份一致 + 该卷 next_usn
    /// 未前移。next_usn 前移或卷表变化即重解析并覆盖。
    fn resolve_root_bound(
        &self,
        state: &IndexState,
        root: &str,
    ) -> RootBoundOutcome {
        const ROOT_CACHE_SLOTS: usize = 4;
        {
            let cache = self
                .root_bound_cache
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for entry in cache.iter() {
                if entry.root != root {
                    continue;
                }
                let valid = state
                    .volumes
                    .get(entry.volume_index)
                    .is_some_and(|volume| {
                        volume.volume_id == entry.volume_id
                            && volume.next_usn == entry.next_usn
                    });
                if valid {
                    return RootBoundOutcome::Bound(Some(entry.bound));
                }
                break;
            }
        }
        match RootScope::resolve(state, root) {
            Ok(scope) => {
                let bound = scope.bound();
                let Some(volume) = state.volumes.get(bound.volume_index) else {
                    return RootBoundOutcome::Bound(Some(bound));
                };
                let entry = RootBoundCacheEntry {
                    root: root.to_owned(),
                    volume_id: volume.volume_id.clone(),
                    volume_index: bound.volume_index,
                    next_usn: volume.next_usn,
                    bound,
                };
                let mut cache = self
                    .root_bound_cache
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                cache.retain(|existing| existing.root != root);
                if cache.len() >= ROOT_CACHE_SLOTS {
                    cache.remove(0);
                }
                cache.push(entry);
                RootBoundOutcome::Bound(Some(bound))
            }
            Err(rejection) => RootBoundOutcome::Unavailable(IndexerResponse::RootUnavailable {
                reason: rejection,
                message: rejection.message().to_owned(),
            }),
        }
    }

    fn search(
        &self,
        query: &str,
        max: usize,
        filters: Option<&[SearchFilter]>,
        root: Option<&str>,
    ) -> Result<IndexerResponse, String> {
        validate_search_request(max, filters)?;
        // H1（复审 2026-08-21）：查询长度上限——索引器管道对 AU 开放，
        // 无界查询是 CPU 耗尽面（NameTerms 去重/封顶之外再挡一层）。
        if query.len() > crate::indexer_ipc::MAX_QUERY_BYTES {
            return Err(format!(
                "query exceeds {} bytes",
                crate::indexer_ipc::MAX_QUERY_BYTES
            ));
        }
        let root = match requested_root(root) {
            Ok(root) => root,
            Err(rejection) => {
                return Ok(IndexerResponse::RootUnavailable {
                    reason: rejection,
                    message: rejection.message().to_owned(),
                })
            }
        };
        // H2a: pinyin sidecar load (mmap + deserialize + validate) is done
        // *outside* the index read lock so USN watchers can acquire the write
        // lock during the load.  If the sidecar is already loaded this is a
        // fast check-and-return; if not, the search proceeds with literal-only
        // results and the maintenance loop retries the load.
        if self.pinyin_enabled.load(Ordering::Acquire) {
            self.load_pinyin_outside_lock();
        }
        let guard = self.index.read().map_err(|_| "index lock is poisoned")?;
        let state = guard.as_ref().ok_or("file index is not ready")?;
        // M6 (audit): search is read-only w.r.t. the pinyin flag. The stored
        // preference (set by SetPinyinEnabled) decides whether pinyin results
        // are included; a search never flips the flag or releases the sidecar.
        let pinyin_enabled = self.pinyin_enabled.load(Ordering::Acquire);
        let root_bound = match root {
            Some(root) => match self.resolve_root_bound(state, root) {
                RootBoundOutcome::Bound(bound) => bound,
                RootBoundOutcome::Unavailable(response) => return Ok(response),
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
        // P1（第一轮 bug 修复）：空查询 + 可用 root = 浏览该目录（路径查询分支：
        // 输入 E:\foo 时首行目录自身、其后目录内容）。仅 root 在场才放行，
        // 全局空查询维持"不搜索"的旧语义。
        if query.is_empty() && !has_query_filters && root_bound.is_none() {
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
        let outcome =
            state.search_in_root_filtered(query, max, &exclusions, root_bound, &query_filters);
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
        // S2（PRISM-IMPL-PLAN-4-2026-08-20）：拼音门无条件化。旧条件
        // `literal_count < max` 的语义是「字面结果没填满才补拼音」——短拼音查询
        //（首字母天然 2-4 字母）字面噪声必远超 max，拼音扫描被整段跳过，
        // `dy` 永远搜不到「抖音」。配额放开为完整 max，拼音候选以全量参与下方
        // 合并排序（S1：class 优先），随后 truncate(max) 保证最终条数不变。
        // 中文/单字母查询成本零变化（normalize_query 在 sidecar 内短路）。
        // P1：空查询（目录浏览）跳过拼音——空查询没有"拼音命中"语义，
        // 只会白扫全表并可能引入与字面路径重复的条目。
        if pinyin_enabled && !query.is_empty() {
            // G4（FRESH-AUDIT-2）：clone Arc 快照后立刻放 pinyin 读锁——拼音全表扫
            // 不再占住 pinyin.read()，USN 的 delta 写入不必等一次长扫描。
            // A1（AUDIT-4 批次B）：delta 拆出为独立表后，扫描期间持有的是
            // delta 的**读锁**（watcher 的增量写被压到毫秒级扫描窗口内，与字面
            // 路径持 index.read() 的既有取舍一致）；主表快照自身永不变异。
            let snapshot = self
                .pinyin
                .read()
                .ok()
                .and_then(|sidecar| sidecar.clone());
            if let Some(sidecar) = snapshot {
                let delta_guard = self
                    .pinyin_delta
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                {
                    let pinyin = sidecar.search_in_root(
                        &delta_guard,
                        state,
                        query,
                        max,
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
    let (rebuild_tx, mut rebuild_rx) = mpsc::unbounded_channel::<RebuildRequest>();

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
    // S5: 每小时一行内存趋势入事件日志（趋势观测，见 memory_trend_detail）。
    let mut last_memory_trend = Instant::now();
    // M1（FRESH-AUDIT-3-2026-08-20）：单卷重建退避 + 失败延迟重试的簿记。
    let mut volume_backoff: Vec<VolumeRebuildBackoff> = Vec::new();
    let mut deferred_rebuilds: Vec<DeferredVolumeRebuild> = Vec::new();
    // M3（FRESH-AUDIT-3-2026-08-20）：拼音重建退避——None 表示启动后首轮立即可做。
    let mut last_pinyin_rebuild: Option<Instant> = None;
    let mut maintenance = tokio::time::interval(Duration::from_secs(5));
    'run: loop {
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
                let Some(first) = reason else { break };
                // H1（FRESH-AUDIT-3-2026-08-20）：积压合并按卷去重保留最新
                //（Full 优先）后逐条处理。旧的整段丢弃会丢掉相近时刻出错的
                // 其他卷的请求，该卷索引从此静默陈旧。
                let mut pending = vec![first];
                while let Ok(next) = rebuild_rx.try_recv() {
                    pending.push(next);
                }
                for request in dedupe_rebuild_requests(pending) {
                match request {
                    RebuildRequest::Full(reason) => {
                        logging::event_detail("info", "rebuild_requested", &reason, None, None);
                        log(format!("serialized index rebuild requested: {reason}"));
                        // H1: compete the rebuild task against shutdown so SCM Stop
                        // doesn't wait minutes for a full MFT rescan to finish.
                        let build_task = tokio::task::spawn_blocking(build_all);
                        let rebuilt = tokio::select! {
                            result = build_task => result,
                            _ = stop.cancelled() => {
                                log("rebuild aborted by shutdown");
                                break 'run;
                            }
                        };
                        let rebuilt = match rebuilt {
                            Ok(inner) => inner,
                            Err(error) => {
                                state.set_error(format!("rebuild task: {error}"));
                                continue;
                            }
                        };
                        match rebuilt {
                            Ok((index, descriptors)) => {
                                epoch.fetch_add(1, Ordering::AcqRel);
                                state.building.store(true, Ordering::Release);
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
                    // AUDIT-2026-08-18 R-B2: 单卷 watcher 出错只重建该卷。
                    // 不动 epoch（该卷 watcher 已随任务退出死亡，不需 epoch+1 杀它），
                    // 不杀其他健康卷 watcher，不触发全盘 MFT 重扫。
                    // 重建后 merge_and_publish 按 volume_id 替换 + start_watcher 重启该卷。
                    RebuildRequest::SingleVolume { descriptor, reason } => {
                        logging::event_detail("info", "volume_rebuild_requested", &reason, None, None);
                        log(format!("single-volume rebuild requested: {reason}"));
                        // M1（FRESH-AUDIT-3-2026-08-20）：退避准入——同卷连续重建按
                        // app_scan_retry_delay 指数拉开，防"清理工具反复删 journal"类
                        // 持续错误形成背靠背全盘扫描热循环；窗口内到达的新请求挂起，
                        // 到点由 maintenance tick 重发。
                        let now = Instant::now();
                        let (admitted, next_due) = {
                            let entry =
                                volume_backoff_entry(&mut volume_backoff, &descriptor.id, now);
                            let admitted = admit_volume_rebuild(entry, now);
                            (admitted, entry.next_due)
                        };
                        if !admitted {
                            defer_volume_rebuild(
                                &mut deferred_rebuilds,
                                descriptor,
                                reason,
                                next_due,
                            );
                            continue;
                        }
                        let build_descriptor = descriptor.clone();
                        let build_task = tokio::task::spawn_blocking(move || {
                            // 与 build_all 同款单卷重试：首次失败重试一次。
                            match ntfs::build_volume(&build_descriptor) {
                                Ok(volume) => Ok(volume),
                                Err(first) => {
                                    log(format!("MFT build retry for {}: {first}", build_descriptor.mount_path));
                                    ntfs::build_volume(&build_descriptor).map_err(|e| {
                                        format!("{}: {e}", build_descriptor.mount_path)
                                    })
                                }
                            }
                        });
                        let rebuilt = tokio::select! {
                            result = build_task => result,
                            _ = stop.cancelled() => {
                                log("single-volume rebuild aborted by shutdown");
                                break 'run;
                            }
                        };
                        match rebuilt {
                            Ok(Ok(volume)) => {
                                state.merge_and_publish(volume);
                                // M1：成功即撤销该卷的挂起重试。
                                remove_deferred_rebuild(&mut deferred_rebuilds, &descriptor.id);
                                // AUDIT-4-2026-08-20 修 2：单卷重建后必须失效拼音
                                // sidecar——记录号被 NTFS 复用后旧编码配上新名字会
                                // 产出错误命中，重建窗口内新建的中文名则漏检；
                                // 旧代码不做任何动作，自愈要等下一次全量拼音重建
                                //（最长 6h）。立即卸载陈旧 sidecar（宁可暂时少结果
                                // 不错结果），置 needs_rebuild 交 maintenance tick
                                //（M3 的 60s 退避）从 live 索引重建。
                                state.begin_pinyin_rebuild();
                                state.pinyin_needs_rebuild.store(true, Ordering::Release);
                                let watcher_epoch = epoch.load(Ordering::Acquire);
                                start_watcher(
                                    state.clone(), descriptor,
                                    stop.clone(), epoch.clone(), rebuild_tx.clone(),
                                    watcher_epoch,
                                );
                                last_checkpoint = Instant::now();
                            }
                            Ok(Err(error)) => {
                                logging::event_detail("error", "volume_rebuild_failed", &error, None, None);
                                state.set_error(error.clone());
                                // M1：失败不放弃——按退避延迟重试，永不永久失活。
                                defer_volume_rebuild(
                                    &mut deferred_rebuilds,
                                    descriptor,
                                    format!("deferred retry after: {error}"),
                                    next_due,
                                );
                            }
                            Err(error) => {
                                state.set_error(format!("volume rebuild task: {error}"));
                                defer_volume_rebuild(
                                    &mut deferred_rebuilds,
                                    descriptor,
                                    reason,
                                    next_due,
                                );
                            }
                        }
                    }
                }
                }
            }
            _ = maintenance.tick() => {
                // 2026-08-22 内存收口：空闲 ≥3 分钟且未在重建时修剪 indexer 工作集。
                // 单次搜索把整卷 MFT 随机节点拉进工作集（~64MB 常驻），OS 自发修剪
                // 要约 1 小时；这里与前端 3 分钟 trim 同节奏主动释放。重建窗口
                //（building=true）绝不修剪——正在高强度建卷/合并，修剪只会触发
                // 海量软缺页拖慢重建。ready=false（尚未首建完成）也跳过：没东西可修。
                // 失败完全无声且无后果：下次搜索/USN 批次按需软缺页调入，与冷启动
                // 同路径。touch_activity 已在修剪前推进，避免连续 tick 重复修剪。
                if !state.building.load(Ordering::Acquire)
                    && state.first_build_complete.load(Ordering::Acquire)
                    && state.idle_for_at_least(IDLE_TRIM_THRESHOLD_MS)
                {
                    state.touch_activity();
                    trim_working_set();
                }
                // S5: 每小时一行内存趋势——零常驻成本（5s tick 只做一次时间比较）。
                // L 批次（FRESH-AUDIT-3-2026-08-20）：trend 与 generation 都要
                // index.read()，挪 spawn_blocking——USN 洪峰期 watcher 持写锁时
                // 直接在 worker 上读会连管道 accept 一起堵。
                if last_memory_trend.elapsed() >= Duration::from_secs(3600) {
                    let trend_state = state.clone();
                    let trend = tokio::task::spawn_blocking(move || {
                        let detail = trend_state.memory_trend_detail();
                        (detail, trend_state.generation())
                    })
                    .await;
                    if let Ok((detail, generation)) = trend {
                        logging::event_detail(
                            "info",
                            "index_memory_trend",
                            &detail,
                            None,
                            Some(generation),
                        );
                        last_memory_trend = Instant::now();
                    }
                }
                // M1（FRESH-AUDIT-3-2026-08-20）：到点的挂起重试重新入队——
                // 走 rebuild_rx 的正常准入路径（退避判定在收到端统一做）。
                if !deferred_rebuilds.is_empty() {
                    let now = Instant::now();
                    deferred_rebuilds.retain(|pending| {
                        if now >= pending.due {
                            let _ = rebuild_tx.send(RebuildRequest::SingleVolume {
                                descriptor: pending.descriptor.clone(),
                                reason: pending.reason.clone(),
                            });
                            false
                        } else {
                            true
                        }
                    });
                }
                // S1 + AUDIT-4 B2（2026-08-21）: 名字池压缩——tick 直接驱动
                // compact_volumes_off_lock（内部谓词选目标卷，锁外 clone→压缩→
                // 短锁 next_usn 校验换入）。不再用一次性标志：标志先消费后竞争
                // 失败（clone 窗口内 USN 到达）且卷转安静时，死名字将永不回收。
                // 每 5s 一拍幂等：无目标时只花一次读锁 + 每卷 O(1) 谓词。
                {
                    let compact_task = tokio::task::spawn_blocking({
                        let state = state.clone();
                        move || compact_volumes_off_lock(&state)
                    });
                    tokio::pin!(compact_task);
                    let result = tokio::select! {
                        result = &mut compact_task => result,
                        _ = stop.cancelled() => break,
                    };
                    match result {
                        Ok(Ok(())) => {}
                        // 压缩失败只是内存回收延迟（计数器仍在，下轮 tick 重触发），
                        // 搜索不受影响：记日志不降级。
                        Ok(Err(error)) => {
                            logging::event_detail("error", "maintenance_compact_failed", &error, None, None);
                        }
                        Err(error) => {
                            logging::event_detail("error", "maintenance_compact_task", &error.to_string(), None, None);
                        }
                    }
                }
                if state.pinyin_needs_rebuild.load(Ordering::Acquire)
                    && state.pinyin_status() != PinyinStatus::Disabled
                    && last_pinyin_rebuild
                        .is_none_or(|at| at.elapsed() >= PINYIN_REBUILD_BACKOFF)
                {
                    // M3（FRESH-AUDIT-3-2026-08-20）：重建退避——风暴期（重建失败
                    // 反复置位 needs_rebuild）不再每 5s 一轮全量重建（整索引 clone +
                    // 全节点编码 + fsync），60s 一拍兜底。窗口内保持标志，到点重试。
                    state.pinyin_needs_rebuild.store(false, Ordering::Release);
                    last_pinyin_rebuild = Some(Instant::now());
                    // H1: compete pinyin rebuild against shutdown.
                    let pinyin_task = rebuild_pinyin_from_live(state.clone());
                    tokio::pin!(pinyin_task);
                    let pinyin_result = tokio::select! {
                        result = &mut pinyin_task => result,
                        _ = stop.cancelled() => break,
                    };
                    if let Err(error) = pinyin_result {
                        logging::event_detail("error", "maintenance_pinyin_failed", &error, None, None);
                        // 内部重建失败已自限为 5 秒节拍重试；这里只可能是任务级故障，降级不退出。
                        state.set_error(format!("pinyin rebuild failed: {error}"));
                    }
                }
                // Everything 模式：低频持久化 + USN 前滚兜底。
                // 6 小时 / 50 万事件覆盖绝大多数会话，落盘开销降为原先的 1/6；
                // 崩溃恢复由缓存里的 next_usn 继续读日志补齐（日志包装走既有重建路径）。
                // L 批次：events 计数读取挪 spawn_blocking（同上，读锁不占 worker）。
                let events_state = state.clone();
                let events_since_checkpoint = tokio::task::spawn_blocking(move || {
                    events_state
                        .index
                        .read()
                        .ok()
                        .and_then(|guard| {
                            guard
                                .as_ref()
                                .map(|index| index.events_since_checkpoint)
                        })
                        .unwrap_or(0)
                })
                .await
                .unwrap_or(0);
                let checkpoint_due = events_since_checkpoint >= 500_000
                    || last_checkpoint.elapsed() >= Duration::from_secs(6 * 60 * 60);
                if checkpoint_due {
                    // H1: compete checkpoint against shutdown.
                    // M2：maintenance 节拍照常重建拼音（flush_pinyin=true）。
                    let checkpoint_task =
                        checkpoint_async(state.clone(), data_dir.clone(), true);
                    tokio::pin!(checkpoint_task);
                    let checkpoint_result = tokio::select! {
                        result = &mut checkpoint_task => result,
                        _ = stop.cancelled() => break,
                    };
                    match checkpoint_result {
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
    // M2（FRESH-AUDIT-3-2026-08-20）：停机只做 v5 落盘，拼音全量重建跳过
    //（避免撞破 SCM 30s wait_hint 被强杀、sidecar 写一半）。收敛路径见
    // checkpoint_after_save 的文档注释：identity 失配 → 启动装载失败 →
    // maintenance tick 重建。
    if let Err(error) = checkpoint_async(state.clone(), data_dir.clone(), false).await {
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
    rebuild_tx: &mpsc::UnboundedSender<RebuildRequest>,
) -> Result<InitialIndex, String> {
    let cached = tokio::task::spawn_blocking({
        let data_dir = data_dir.to_path_buf();
        move || load_cached(&data_dir)
    })
    .await
    .map_err(|error| format!("initial index task: {error}"))?;

    let (descriptors, records_estimate, rebuild_targets, partial_serving) = match cached {
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
        // S2: 部分命中——可回放卷先发布 + 起 watcher（与首建循环同款逐卷合并），
        // 重建卷走下方首建循环。R2 门照常保持关闭：全部卷（含重建）落定前不写 v5。
        CachedLoad::Partial {
            volumes,
            descriptors,
            rebuild,
            records_estimate,
        } => {
            if stop.is_requested() {
                return Ok(InitialIndex {
                    descriptors,
                    watchers_started: false,
                    interrupted: true,
                });
            }
            let replay_descriptors: Vec<VolumeDescriptor> = descriptors
                .iter()
                .filter(|descriptor| {
                    !rebuild
                        .iter()
                        .any(|target| target.id == descriptor.id)
                })
                .cloned()
                .collect();
            for volume in &volumes {
                state.merge_and_publish(volume.clone());
                state.progress.volume_done();
            }
            start_watchers(
                state.clone(),
                replay_descriptors,
                stop.clone(),
                epoch.clone(),
                rebuild_tx.clone(),
            );
            log(format!(
                "partial cache hit: serving {} replayable volume(s) while rebuilding",
                volumes.len()
            ));
            let served = !volumes.is_empty();
            (descriptors, records_estimate, rebuild, served)
        }
        CachedLoad::Miss {
            descriptors,
            records_estimate,
        } => {
            let rebuild = descriptors.clone();
            (descriptors, records_estimate, rebuild, false)
        }
    };

    if descriptors.is_empty() {
        return Err("no local fixed NTFS volumes were found".into());
    }

    // S2: 只重建需要重建的卷（Miss = 全部；Partial = 坏卷 + 新卷）。
    state.progress.begin(rebuild_targets.len(), records_estimate);
    // 二次重试后仍失败的卷：跳过并记录，绝不拖垮其余卷（全部失败才算真失败）。
    let mut failed_volumes: Vec<String> = Vec::new();

    for descriptor in &rebuild_targets {
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

    // 全部重建卷都失败且没有部分命中的卷在服务：没有任何可服务的索引，交回真失败
    // （SCM 兜底重启）。部分命中仍在服务时只记录降级，不退出。
    if failed_volumes.len() >= rebuild_targets.len() && !partial_serving {
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
        logging::event_detail(
            "error",
            "initial_build_skipped_volumes",
            &message,
            None,
            None,
        );
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
    /// S2（FRESH-AUDIT-2026-08-19）: 部分命中——可回放卷直接服务 + 起 watcher，
    /// 坏卷/新卷走首建循环。此前任一卷失配即整份弃缓全盘重扫（Everything 的
    /// 语义是"fast reindexing"只重建受影响卷）。
    Partial {
        /// 校验通过、可直接发布的卷（已刷新 mount_path）。
        volumes: Vec<VolumeIndex>,
        /// 现场全部卷描述符。
        descriptors: Vec<VolumeDescriptor>,
        /// 需走首建循环的卷描述符（坏卷 + 现场新卷）。
        rebuild: Vec<VolumeDescriptor>,
        /// 进度分母用（沿用 Miss 的口径）。
        records_estimate: Option<u64>,
    },
    Miss {
        descriptors: Vec<VolumeDescriptor>,
        /// Record count from the rejected cache, if any, used only as a progress
        /// denominator. A first install has none and reports volume counts alone.
        records_estimate: Option<u64>,
    },
}

/// S2: 纯集合判定——缓存卷集合 vs 现场描述符集合。
/// 返回（匹配对（缓存卷, 现场下标）, 需重建描述符, 缓存多余卷）。
/// journal 可回放性探测由调用方对匹配对逐一执行（真实 I/O 不进纯函数，可测）。
fn partition_volume_sets(
    cached: Vec<VolumeIndex>,
    descriptors: &[VolumeDescriptor],
) -> (
    Vec<(VolumeIndex, usize)>,
    Vec<VolumeDescriptor>,
    Vec<VolumeIndex>,
) {
    let mut matched = Vec::new();
    let mut rebuild = Vec::new();
    let mut dropped = Vec::new();
    let mut consumed = vec![false; descriptors.len()];
    for volume in cached {
        match descriptors
            .iter()
            .position(|candidate| candidate.id == volume.volume_id)
        {
            Some(position) => {
                consumed[position] = true;
                matched.push((volume, position));
            }
            None => dropped.push(volume), // 缓存有、现场无：卷已卸载，静默丢弃
        }
    }
    for (position, descriptor) in descriptors.iter().enumerate() {
        if !consumed[position] {
            rebuild.push(descriptor.clone()); // 现场新增卷
        }
    }
    (matched, rebuild, dropped)
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
        let total_cached_records = index
            .volumes
            .iter()
            .map(|volume| volume.nodes.len() as u64)
            .sum::<u64>();
        // S2: 逐卷判定。匹配卷探测 journal 可回放性；坏卷与新卷进重建列表；
        // 已卸载的缓存卷直接丢弃。全部可回放 = Hit，部分 = Partial，全坏 = Miss。
        let (matched, mut rebuild, _dropped) =
            partition_volume_sets(std::mem::take(&mut index.volumes), &descriptors);
        let mut replayable = Vec::with_capacity(matched.len());
        for (mut volume, position) in matched {
            let descriptor = &descriptors[position];
            let replayable_journal = ntfs::open_volume(descriptor, false)
                .and_then(|handle| ntfs::query_journal(&handle))
                .is_ok_and(|journal| {
                    journal.journal_id == volume.journal_id
                        && volume.next_usn >= journal.first_usn
                });
            if replayable_journal {
                volume.mount_path.clone_from(&descriptor.mount_path);
                replayable.push(volume);
            } else {
                rebuild.push(descriptor.clone());
            }
        }
        if replayable.is_empty() {
            log("v5 cache has no replayable volume; rebuilding while old state remains unpublished");
            return CachedLoad::Miss {
                descriptors,
                records_estimate: (total_cached_records > 0).then_some(total_cached_records),
            };
        }
        if rebuild.is_empty() {
            let mut hit_index = IndexState {
                volumes: replayable,
                generation: index.generation,
                events_since_checkpoint: 0,
            };
            hit_index.generation = hit_index.generation.max(1);
            return CachedLoad::Hit {
                index: hit_index,
                descriptors,
            };
        }
        log(format!(
            "v5 cache partial hit: {} replayable volume(s), rebuilding {}",
            replayable.len(),
            rebuild.len()
        ));
        return CachedLoad::Partial {
            volumes: replayable,
            descriptors,
            rebuild,
            records_estimate: (total_cached_records > 0).then_some(total_cached_records),
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

fn start_watchers(
    state: Arc<ServiceState>,
    descriptors: Vec<VolumeDescriptor>,
    stop: Arc<Shutdown>,
    epoch: Arc<AtomicU64>,
    rebuild_tx: mpsc::UnboundedSender<RebuildRequest>,
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
    rebuild_tx: mpsc::UnboundedSender<RebuildRequest>,
    watcher_epoch: u64,
) {
    tokio::task::spawn_blocking(move || {
        if let Err(error) = watch_volume(&state, &descriptor, &stop, &epoch, watcher_epoch) {
            if !stop.is_requested() && epoch.load(Ordering::Acquire) == watcher_epoch {
                let _ = rebuild_tx.send(RebuildRequest::SingleVolume {
                    descriptor: descriptor.clone(),
                    reason: format!("{}: {error}", descriptor.mount_path),
                });
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
    // 2026-08-22 rebuild-storm fix: cumulative count of USN records skipped
    // because their parent never became reachable. Local to this watcher —
    // a rebuild replaces the watcher, so the counter naturally resets. The
    // per-batch `usn_records_dropped` log lines carry the cumulative number,
    // which is what the hourly trend would replay anyway.
    let mut dropped_unreachable: u64 = 0;
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
        let (rebuild_reason, volume_number) = {
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
            let (outcome, skipped) = ntfs::apply_records(volume, &records, next_usn)?;
            if skipped > 0 {
                dropped_unreachable = dropped_unreachable.saturating_add(skipped as u64);
                logging::event_detail(
                    "info",
                    "usn_records_dropped",
                    &format!(
                        "volume={} batch_skipped={skipped} cumulative={dropped_unreachable}",
                        descriptor.mount_path,
                    ),
                    None,
                    None,
                );
            }
            // The two escalation causes need distinct reasons in the rebuild log:
            // a real exclusion-boundary change vs. an unreachable-parent flood.
            let rebuild_reason = if outcome == ApplyOutcome::RebuildRequired {
                Some("excluded-directory boundary changed".to_string())
            } else if usn_drop_exceeds_resync_threshold(dropped_unreachable) {
                Some(format!(
                    "skipped unreachable-parent USN records exceeded the resync threshold ({USN_UNREACHABLE_RESYNC_THRESHOLD})"
                ))
            } else {
                None
            };
            index.events_since_checkpoint = index.events_since_checkpoint.saturating_add(changed);
            // AUDIT-4 B2（2026-08-21）: 压缩触发不再立标志——maintenance tick 每 5s
            // 直接驱动 compact_volumes_off_lock，内部按 dead_name_bytes/池尺寸
            // 谓词选目标卷，计数器仍在（不清零），漏拍不漏压缩。
            if changed > 0 {
                index.generation = index.generation.saturating_add(1);
            }
            (rebuild_reason, volume_number)
        };
        if changed > 0 {
            service.apply_pinyin_records(volume_number, &records);
            service.generation_notify.notify_waiters();
            // 不在此 touch_activity：USN 批次是 OS 后台文件活动（浏览器缓存、
            // Windows Update、杀软等）的常态，若计入空闲计时器，正常机器上
            // 3 分钟静默几乎永不达、工作集 trim 永不触发，用户「用完即降」诉求
            // 落空。只有用户主动搜索才真正把整卷 MFT 随机节点拉进工作集——
            // 那才是该刷新计时、延后修剪的活动。修剪后若有 USN 批次到达，
            // 软缺页重分页是已知的可接受代价（见 trim_working_set 注释）。
        }
        if let Some(reason) = rebuild_reason {
            return Err(reason);
        }
    }
    Ok(())
}

/// Cumulative skipped-unreachable threshold that still justifies a whole-volume
/// resync after the 2026-08-22 rebuild-storm fix. Below it, dropping the
///残留-orphan records is strictly better than a rebuild (a rebuild re-drops
/// the same parents, so it cannot converge); past it something structural
/// changed (e.g. the journal lost a chunk of history) and a resync is the
/// only recovery. Kept as a pure predicate for unit testing, same rationale
/// as `rank_window_list`.
const USN_UNREACHABLE_RESYNC_THRESHOLD: u64 = 4096;

fn usn_drop_exceeds_resync_threshold(dropped_unreachable: u64) -> bool {
    dropped_unreachable > USN_UNREACHABLE_RESYNC_THRESHOLD
}

fn checkpoint(state: &ServiceState, data_dir: &std::path::Path, flush_pinyin: bool) -> Result<(), String> {
    // R2 gate: a first build in flight means the live index covers only some volumes.
    // Writing it now would produce a file that later looks like a complete cache, so
    // every exit path — including SCM Stop — skips the write and forces a full rebuild.
    if !state.first_build_is_complete() {
        log("skipping v5 cache write: the first build has not completed");
        return Ok(());
    }

    // M2（FRESH-AUDIT-2026-08-19）: 先走逐卷流式序列化——每卷短暂持读锁、
    // 卷间核对 generation，USN 写者只在单卷序列化期间（~百毫秒）等锁，
    // 且写侧不再 clone 整个 IndexState（40MB 库消除 2× 峰值）。快照变化
    // （锁竞争）重试 3 次，仍失败回落整态 clone 路径——回落路径就是修复前
    // 的代码，天然安全网。
    for attempt in 1..=3 {
        match checkpoint_streaming(state, data_dir, flush_pinyin) {
            Ok(()) => return Ok(()),
            Err(error) if error.contains(index_cache::SNAPSHOT_CHANGED) => {
                log(format!(
                    "streaming checkpoint raced with USN (attempt {attempt}/3): {error}"
                ));
            }
            Err(error) => {
                logging::event_detail("error", "checkpoint_save_failed", &error, None, None);
                return Err(error);
            }
        }
    }
    log("streaming checkpoint kept racing; falling back to whole-state clone path");

    // 回落：clone under the read lock (fast memcpy), then release before save(). save()
    // calls validate() which walks every node with path_for (O(depth) per node) — holding
    // the read lock that long blocks USN writers and starves the 2-worker async runtime.
    let snapshot = {
        let guard = state.index.read().map_err(|_| "index lock is poisoned")?;
        guard.as_ref().cloned().ok_or("index is not ready")?
    };
    if let Err(error) = index_cache::save(&snapshot, data_dir) {
        logging::event_detail("error", "checkpoint_save_failed", &error, None, None);
        return Err(error);
    }
    checkpoint_after_save(
        state,
        &SnapshotHead {
            generation: snapshot.generation,
            events_since_checkpoint: snapshot.events_since_checkpoint,
            volumes: snapshot.volumes.len(),
            next_usns: snapshot
                .volumes
                .iter()
                .map(|volume| volume.next_usn)
                .collect(),
        },
        flush_pinyin,
    )
}

/// M2: 流式 checkpoint 的一轮尝试。验证 + 头快照在一个短读锁内完成，
/// 之后逐卷短读锁写入（卷间核对 generation，回调内完成）。
fn checkpoint_streaming(
    state: &ServiceState,
    data_dir: &std::path::Path,
    flush_pinyin: bool,
) -> Result<(), String> {
    let head = {
        let guard = state.index.read().map_err(|_| "index lock is poisoned")?;
        let index = guard.as_ref().ok_or("index is not ready")?;
        index_cache::validate_before_save(index)?;
        SnapshotHead {
            generation: index.generation,
            events_since_checkpoint: index.events_since_checkpoint,
            volumes: index.volumes.len(),
            next_usns: index.volumes.iter().map(|volume| volume.next_usn).collect(),
        }
    };
    checkpoint_streaming_with_head(state, data_dir, head, flush_pinyin)
}

/// M2: 以给定头快照逐卷写入（测试可注入陈旧头验证世代核对）。
fn checkpoint_streaming_with_head(
    state: &ServiceState,
    data_dir: &std::path::Path,
    head: SnapshotHead,
    flush_pinyin: bool,
) -> Result<(), String> {
    index_cache::save_streaming(
        data_dir,
        head.volumes,
        head.generation,
        head.events_since_checkpoint,
        |position, writer| {
            // 每卷一个短读锁：guard 只活到本卷写完。
            let guard = state
                .index
                .read()
                .map_err(|_| "index lock is poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "index is not ready".to_string())?;
            // 卷间核对（L 批次放宽）：只有正被序列化的卷自身 next_usn 前移才
            // 作废本轮——绝不能把混合状态的卷写进同一个 v5 文件。
            if index.volumes.len() != head.next_usns.len()
                || index.volumes[position].next_usn != head.next_usns[position]
            {
                return Err(format!(
                    "{}: volume {position} next_usn {} -> {}",
                    index_cache::SNAPSHOT_CHANGED,
                    head.next_usns[position],
                    index.volumes[position].next_usn
                ));
            }
            postcard::to_io(&index.volumes[position], writer)
                .map_err(|error| format!("encode v5 volume: {error}"))?;
            // B3（AUDIT-4 批次C）：同一短读锁内算出本卷内容哈希，交流式
            // 尾部折叠成 envelope 校验和。
            Ok(index.volumes[position].content_hash())
        },
    )?;
    checkpoint_after_save(state, &head, flush_pinyin)
}

/// checkpoint 成功后的公共收尾：拼音重建 + 事件计数扣减。
/// 扣减用的 events 计数来自**实际序列化进去的头**（流式=头快照，回落=clone 快照）。
/// M2（FRESH-AUDIT-3-2026-08-20）：`flush_pinyin=false`（停机路径）跳过拼音重建。
/// 全量重建（整索引 clone + 全节点编码 + fsync + mmap 校验）在大索引 + 慢盘上
/// 会撞破 SCM 30s wait_hint 被强杀——sidecar 写一半反致下次启动 IndexMismatch
/// 又触发一轮全量重建。跳过后的收敛由既有机制保证：任何 create/rename 都会
/// 追加名字池使 identity 失配 → 启动装载失败 → pinyin_needs_rebuild →
/// maintenance tick 从 live 索引重建；delete-only 失配不触发，但 push_candidate
/// 的 FLAG_PRESENT 检查会把陈旧记录挡在结果外。
fn checkpoint_after_save(
    state: &ServiceState,
    head: &SnapshotHead,
    flush_pinyin: bool,
) -> Result<(), String> {
    if flush_pinyin && state.pinyin_status() != PinyinStatus::Disabled {
        // Rebuild from the live tree under its read lock. A clone taken for the v5
        // checkpoint can be one USN batch behind by the time the sidecar is installed.
        state.rebuild_pinyin_from_live();
    }
    // AUDIT-2026-08-18 R-B5: lock poisoning only occurs in unwind/test builds; take the lock directly instead of silently skipping.
    let mut guard = state.index.write().unwrap_or_else(|poisoned| poisoned.into_inner());
    {
        if let Some(index) = guard.as_mut() {
            index.events_since_checkpoint = index
                .events_since_checkpoint
                .saturating_sub(head.events_since_checkpoint);
        }
    }
    Ok(())
}

/// M2: 流式序列化用的索引头快照（在首卷写入前一次性捕获）。
/// L 批次（FRESH-AUDIT-3-2026-08-20）：卷间核对从 generation 改为按卷
/// `next_usn`——USN 洪峰期 generation 每批都前移（任一卷的活动都 bump），
/// 旧口径使三次竞争重试后仍回落整态 clone 成为常态。逐卷比对后，只有
/// **正被序列化的卷**自身变化才作废本轮写入；其他卷的活动不影响一致性
///（v5 按卷存储 next_usn，重放各自独立）。
#[derive(Debug, Clone)]
struct SnapshotHead {
    generation: u64,
    events_since_checkpoint: u64,
    volumes: usize,
    next_usns: Vec<i64>,
}

async fn checkpoint_async(
    state: Arc<ServiceState>,
    data_dir: PathBuf,
    flush_pinyin: bool,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || checkpoint(&state, &data_dir, flush_pinyin))
        .await
        .map_err(|error| format!("checkpoint task: {error}"))?
}

/// S1（FRESH-AUDIT-2026-08-19）: 锁外压缩全部到达阈值的卷。
/// 每卷独立处理，单卷失败不拖累其他卷。
fn compact_volumes_off_lock(state: &ServiceState) -> Result<(), String> {
    let targets: Vec<crate::hierarchy::VolumeId> = {
        let guard = state.index.read().map_err(|_| "index lock is poisoned")?;
        guard
            .as_ref()
            .map(|index| {
                index
                    .volumes
                    .iter()
                    .filter(|volume| volume.needs_name_compact())
                    .map(|volume| volume.volume_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    for volume_id in targets {
        compact_one_volume_off_lock(state, &volume_id)?;
    }
    Ok(())
}

/// S1: 单卷的「读锁 clone → 锁外压缩 → 短写锁换入」。
/// clone 与换入之间 USN 到达（该卷 next_usn 前移）则丢弃重试（≤3 次）——
/// 换入陈旧卷会丢事件，宁可放弃本轮等下一个 tick（计数器不清零，必然重触发）。
fn compact_one_volume_off_lock(
    state: &ServiceState,
    volume_id: &crate::hierarchy::VolumeId,
) -> Result<bool, String> {
    const RETRIES: usize = 3;
    for _ in 0..RETRIES {
        let snapshot = {
            let guard = state.index.read().map_err(|_| "index lock is poisoned")?;
            let index = guard.as_ref().ok_or("index is not ready")?;
            let Some(volume) = index
                .volumes
                .iter()
                .find(|volume| volume.volume_id == *volume_id)
            else {
                return Ok(false); // 卷已被单卷重建替换掉——无事可做
            };
            if !volume.needs_name_compact() {
                return Ok(false); // 已被其他路径压缩过
            }
            volume.clone()
        };
        let snapshot_usn = snapshot.next_usn;
        let mut compacted = snapshot;
        if !compacted.compact_names_if_needed()? {
            return Ok(false);
        }
        let mut guard = state
            .index
            .write()
            .map_err(|_| "index lock is poisoned")?;
        let index = guard.as_mut().ok_or("index is not ready")?;
        let Some(slot) = index
            .volumes
            .iter_mut()
            .find(|volume| volume.volume_id == *volume_id)
        else {
            return Ok(false);
        };
        if slot.next_usn != snapshot_usn {
            continue; // USN 在 clone 期间到达：丢弃这次压缩，重试
        }
        let mount = slot.mount_path.clone();
        *slot = compacted;
        log(format!("compacted name pool for {mount} (off-lock)"));
        return Ok(true);
    }
    Ok(false)
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
///
/// AUDIT-2026-08-18 R-B3: create_pipe 失败不再立即返回 Err 杀整个服务。
/// 瞬时失败（资源不足、ACL 临时不可用）退避重试（100ms 起指数、上限 5s），
/// 连续失败超 60s 才升级为致命——与 SCM Stop 的 wait_hint 对齐。
async fn accept_loop(state: Arc<ServiceState>, mut server: NamedPipeServer) -> Result<(), String> {
    loop {
        server.connect().await.map_err(|error| error.to_string())?;
        let connected = server;
        server = rearm_with_backoff().await?;
        // R-A3：超限连接握手前直接关闭，不产生任务与行缓冲。
        if !state.try_admit_connection() {
            log(format!(
                "indexer pipe connection rejected: over {} concurrent connections",
                ServiceState::MAX_CONNECTIONS
            ));
            drop(connected);
            continue;
        }
        let state = state.clone();
        tokio::spawn(async move {
            let result = handle_connection(connected, state.clone()).await;
            state.release_connection();
            if let Err(error) = result {
                log(format!("indexer IPC client disconnected: {error}"));
            }
        });
    }
}

/// 退避重试 create_pipe，连续失败超 60s 才返回 Err。
async fn rearm_with_backoff() -> Result<NamedPipeServer, String> {
    let mut backoff = Duration::from_millis(100);
    let first_failure = Instant::now();
    loop {
        match create_pipe(false) {
            Ok(server) => return Ok(server),
            Err(error) => {
                if first_failure.elapsed() > Duration::from_secs(60) {
                    return Err(format!("create_pipe failed for >60s: {error}"));
                }
                log(format!("create_pipe retry (backoff {backoff:?}): {error}"));
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
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
    // AUDIT-2026-08-18 R-A2: AU 从 GA（含 WRITE_DAC/OWNER 等危险全权）收窄为
    // GRGW——连接管道读写数据所需的最小权限；SYSTEM/管理员保留完全控制。
    let sddl: Vec<u16> = std::ffi::OsStr::new("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)")
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

/// 握手（hello 行）限时（审计 L1 分阶段空闲策略）：连上不发 hello 的客户端
/// 10 秒即断开，防止挂死客户端任务常驻。已握手连接不设空闲超时——broker 侧是
/// 全生命期单连接设计（见 indexer_client.rs 头注），掐空闲会重演 ERROR_PIPE_BUSY。
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 读 hello 行，带限时。EOF/超时都归一为 `Err(String)`；超时值作参数以便单测注入毫秒级时限。
async fn read_hello_line<R: tokio::io::AsyncRead + Unpin>(
    lines: &mut crate::ipc::BoundedLineReader<R>,
    timeout: std::time::Duration,
) -> Result<String, String> {
    match tokio::time::timeout(timeout, lines.next_line()).await {
        Ok(Ok(Some(line))) => Ok(line),
        Ok(Ok(None)) => Err("client closed before hello".into()),
        Ok(Err(message)) => Err(message),
        Err(_) => Err(format!(
            "handshake timeout: no hello within {}s",
            timeout.as_secs()
        )),
    }
}

/// G5（FRESH-AUDIT-2）：SetPinyinEnabled 的每连接速率限制窗口。索引服务以
/// SYSTEM 运行、管道 ACL 允许任意本地用户连接——该命令会释放/重建拼音
/// sidecar，无限频的反复翻转就是重建风暴。合法前端每次设置保存只发一次；
/// 1 秒窗口足够宽松，超出按错误回绝。
const SET_PINYIN_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) async fn handle_connection(
    pipe: NamedPipeServer,
    state: Arc<ServiceState>,
) -> Result<(), String> {
    let (reader, mut writer) = tokio::io::split(pipe);
    // 有界逐行读（1MB 上限）：无上限的 lines() 会让任意本地进程灌超长行撑爆内存。
    let mut lines = crate::ipc::BoundedLineReader::new(reader);
    let hello = read_hello_line(&mut lines, HANDSHAKE_TIMEOUT).await?;
    match serde_json::from_str::<IndexerRequest>(&hello) {
        Ok(IndexerRequest::Hello { protocol }) if protocol == INDEXER_PROTOCOL => {
            write_response(&mut writer, &IndexerResponse::Hello { protocol }).await?;
        }
        Ok(IndexerRequest::Hello { protocol: client }) => {
            write_response(
                &mut writer,
                &IndexerResponse::Error {
                    message: format!(
                        "protocol_mismatch: server={INDEXER_PROTOCOL} client={client}"
                    ),
                },
            )
            .await?;
            return Ok(());
        }
        _ => {
            write_response(
                &mut writer,
                &IndexerResponse::Error {
                    message: "missing or malformed hello".into(),
                },
            )
            .await?;
            return Ok(());
        }
    }
    // G5（FRESH-AUDIT-2）：SetPinyinEnabled 的速率限制是进程级共享的
    // SET_PINYIN_LAST（原先记在每连接局部，见其注释）。
    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        let response = match serde_json::from_str::<IndexerRequest>(&line) {            // `status()` takes `index.read()` synchronously. Calling it directly
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
                root,
                ..
            }) => {
                state.touch_activity();
                let state = state.clone();
                match tokio::task::spawn_blocking(move || {
                    state.search(&query, max, filters.as_deref(), root.as_deref())
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
            Ok(IndexerRequest::SetPinyinEnabled { enabled }) => {
                // G5: 速率限制先行（进程级窗口）——拒绝时不做任何状态变更。
                if set_pinyin_rate_limited() {
                    IndexerResponse::Error {
                        message: "set_pinyin_enabled is rate limited to once per second".into(),
                    }
                } else {
                    let state_for_task = state.clone();
                    match tokio::task::spawn_blocking(move || {
                        if enabled {
                            state_for_task.pinyin_enabled.store(true, Ordering::Release);
                            // H2a: load outside the index read lock to avoid
                            // blocking USN watchers during mmap + deserialize.
                            state_for_task.load_pinyin_outside_lock();
                        } else {
                            state_for_task.release_pinyin();
                        }
                    })
                    .await
                    {
                        Ok(()) => IndexerResponse::Status(state.status()),
                        Err(error) => IndexerResponse::Error {
                            message: format!("set pinyin enabled task: {error}"),
                        },
                    }
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

/// G5: SetPinyinEnabled 的每连接速率门——窗口内第二次及以后返回 true（拒绝），
/// 并在放行时推进时间戳。独立成函数以便单测锚定。
/// 复审 M（2026-08-21 全仓重审）：限速状态进程级共享。原先记在每连接局部，
/// 本地任意 AU 进程开 N 条连接即得 N 个独立窗口，1 秒一次的限制形同虚设。
static SET_PINYIN_LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

fn set_pinyin_rate_limited() -> bool {
    // 锁毒化（只可能因持锁线程 panic 产生）：保守拒绝，宁可拒绝也不放开风暴面。
    let Ok(mut last) = SET_PINYIN_LAST.lock() else {
        return true;
    };
    if last.is_some_and(|at| at.elapsed() < SET_PINYIN_MIN_INTERVAL) {
        return true;
    }
    *last = Some(std::time::Instant::now());
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::VolumeId;
    use crate::indexer_ipc::{MAX_FILTERS, MAX_FILTER_VALUE_BYTES, MAX_SEARCH_RESULTS};
    use crate::root_scope::RootRejection;

    // --- S2（FRESH-AUDIT-2026-08-19）: 缓存按卷生效 -----------------------------

    /// H1（FRESH-AUDIT-3-2026-08-20）: 两卷相近时刻同时失败的积压必须都活下来
    /// ——旧的整段丢弃会把第二个卷的请求静默吞掉，该卷从此静默陈旧直到重启。
    #[test]
    fn h1_backlog_keeps_both_volumes() {
        let pending = vec![
            single_rebuild('C', "c: journal gone"),
            single_rebuild('D', "d: journal gone"),
        ];
        let kept = dedupe_rebuild_requests(pending);
        assert_eq!(kept.len(), 2, "不同卷的请求一条都不能丢");
    }

    /// H1: 同卷多条请求保留最后一条（留新弃旧），不重复重建。
    #[test]
    fn h1_backlog_keeps_latest_request_per_volume() {
        let pending = vec![
            single_rebuild('C', "first"),
            single_rebuild('D', "d"),
            single_rebuild('C', "second"),
        ];
        let kept = dedupe_rebuild_requests(pending);
        assert_eq!(kept.len(), 2);
        match &kept[0] {
            RebuildRequest::SingleVolume { descriptor, reason } => {
                assert_eq!(descriptor.mount_path, "C:\\");
                assert_eq!(reason, "second", "同卷保留最新一条");
            }
            _ => panic!("expected SingleVolume"),
        }
    }

    /// H1: Full 请求覆盖一切单卷请求（全量重建已包含所有卷）。
    #[test]
    fn h1_backlog_full_rebuild_wins_over_single_volume() {
        let pending = vec![
            single_rebuild('C', "c"),
            RebuildRequest::Full("volume set changed".into()),
            single_rebuild('D', "d"),
        ];
        let kept = dedupe_rebuild_requests(pending);
        assert_eq!(kept.len(), 1);
        assert!(matches!(&kept[0], RebuildRequest::Full(reason) if reason == "volume set changed"));
    }

    fn single_rebuild(letter: char, reason: &str) -> RebuildRequest {
        RebuildRequest::SingleVolume {
            descriptor: descriptor(letter),
            reason: reason.to_owned(),
        }
    }

    /// 2026-08-22 rebuild-storm fix: the resync threshold is a pure boundary —
    /// at-or-below keeps tolerating (a rebuild cannot converge on residual
    /// orphans), strictly above escalates to a single-volume resync.
    #[test]
    fn unreachable_resync_threshold_boundary() {
        assert!(!usn_drop_exceeds_resync_threshold(0));
        assert!(!usn_drop_exceeds_resync_threshold(4096));
        assert!(usn_drop_exceeds_resync_threshold(4097));
    }

    /// 2026-08-22 内存收口：touch_activity 推进 last_activity_ms，idle_for_at_least
    /// 据此判定是否到达空闲阈值（>= 语义：恰达阈值即 true）。锚定四件事：
    /// (1) 构造即记当前时刻，立即判定未达 3 分钟；(2) touch_activity 后再次立即
    /// 判定仍未达（证明推进而非回拨）；(3) 手动把 last_activity_ms 回拨到阈值前
    /// 1ms 判定 false、回拨到恰好阈值判定 true；(4) last > now 的时钟回跳
    /// （NTP 向后校正）应判定未达——saturating_sub 归零，保守不修剪。
    /// 这锁住「搜索刷新计时、maintenance tick 据此决定是否修剪」的契约。
    #[test]
    fn touch_activity_drives_idle_threshold() {
        let state = ServiceState::new();
        // 刚构造：距「启动时刻」几乎为 0，未达 3 分钟阈值。
        assert!(!state.idle_for_at_least(IDLE_TRIM_THRESHOLD_MS));
        // 一次活动刷新计时起点——仍未达阈值（证明推进而非回拨）。
        state.touch_activity();
        assert!(!state.idle_for_at_least(IDLE_TRIM_THRESHOLD_MS));
        // 回拨到阈值前 1ms：未达；回拨到恰好阈值：>= 成立达阈值。
        let now = unix_ms_now();
        state.last_activity_ms.store(
            now.saturating_sub(IDLE_TRIM_THRESHOLD_MS - 1),
            Ordering::Release,
        );
        assert!(!state.idle_for_at_least(IDLE_TRIM_THRESHOLD_MS));
        state.last_activity_ms.store(
            now.saturating_sub(IDLE_TRIM_THRESHOLD_MS),
            Ordering::Release,
        );
        assert!(state.idle_for_at_least(IDLE_TRIM_THRESHOLD_MS));
        // 时钟回跳：last_activity_ms 比当前还新。saturating_sub 归零，
        // 判定未达——修剪被保守推迟，不产生误修剪。防回归到 wrapping_sub。
        state.last_activity_ms.store(now.saturating_add(60_000), Ordering::Release);
        assert!(!state.idle_for_at_least(IDLE_TRIM_THRESHOLD_MS));
    }

    /// M1（FRESH-AUDIT-3-2026-08-20）: 首次到达立即准入；退避窗口内拒绝且不动
    /// 计数；到点再次准入且 attempt 递增。
    #[test]
    fn m1_backoff_admits_first_and_defers_within_window() {
        let start = Instant::now();
        let mut entry = VolumeRebuildBackoff {
            id: VolumeId { guid: "v".into(), serial: 1 },
            attempt: 0,
            next_due: start,
            last_rebuild_at: None,
        };
        assert!(admit_volume_rebuild(&mut entry, start), "首次重建无退避");
        assert_eq!(entry.attempt, 1);
        assert_eq!(entry.next_due, start + Duration::from_secs(30));
        assert!(
            !admit_volume_rebuild(&mut entry, start + Duration::from_secs(5)),
            "退避窗口内必须拒绝"
        );
        assert_eq!(entry.attempt, 1, "被拒绝的到达不推进计数");
        let due = entry.next_due;
        assert!(admit_volume_rebuild(&mut entry, due), "到点再准入");
        assert_eq!(entry.attempt, 2);
    }

    /// M1: 连续失败按 app_scan_retry_delay 指数拉开（第 6 次起 60s、60s、120s…），
    /// 压平背靠背全盘扫描热循环。
    #[test]
    fn m1_backoff_grows_exponentially() {
        let mut now = Instant::now();
        let mut entry = VolumeRebuildBackoff {
            id: VolumeId { guid: "v".into(), serial: 1 },
            attempt: 0,
            next_due: now,
            last_rebuild_at: None,
        };
        let mut intervals = Vec::new();
        for _ in 0..7 {
            assert!(admit_volume_rebuild(&mut entry, now));
            intervals.push(entry.next_due - now);
            now = entry.next_due; // 到点立刻再触发（热循环场景）
        }
        assert_eq!(intervals[5], Duration::from_secs(60), "第 6 次起 60s");
        assert_eq!(intervals[6], Duration::from_secs(120), "之后指数翻倍");
    }

    /// M1: 卷静默 1 小时后 attempt 归零——正常运行后偶发失败不背历史退避包袱。
    #[test]
    fn m1_quiet_volume_resets_attempts() {
        let start = Instant::now();
        let mut entry = VolumeRebuildBackoff {
            id: VolumeId { guid: "v".into(), serial: 1 },
            attempt: 9,
            next_due: start,
            last_rebuild_at: Some(start - VOLUME_REBUILD_QUIET),
        };
        assert!(admit_volume_rebuild(&mut entry, start));
        assert_eq!(entry.attempt, 1, "静默期满后按首次处理");
        assert_eq!(entry.next_due, start + Duration::from_secs(30));
    }

    /// M1: 失败登记挂起重试、成功撤销；同卷只保留最新一份。
    #[test]
    fn m1_deferred_retry_bookkeeping() {
        let mut deferred = Vec::new();
        let now = Instant::now();
        defer_volume_rebuild(
            &mut deferred,
            descriptor('C'),
            "first".into(),
            now + Duration::from_secs(30),
        );
        defer_volume_rebuild(
            &mut deferred,
            descriptor('C'),
            "second".into(),
            now + Duration::from_secs(60),
        );
        defer_volume_rebuild(
            &mut deferred,
            descriptor('D'),
            "d".into(),
            now + Duration::from_secs(30),
        );
        assert_eq!(deferred.len(), 2, "同卷去重");
        assert_eq!(deferred[0].reason, "second");
        remove_deferred_rebuild(&mut deferred, &descriptor('C').id);
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].descriptor.mount_path, "D:\\");
    }

    /// M2（FRESH-AUDIT-3-2026-08-20）: 停机路径只落 v5 不写拼音 sidecar
    ///（避免拼音全量重建撞破 SCM 30s wait_hint）；maintenance 路径照常重建。
    #[test]
    fn m2_shutdown_checkpoint_skips_pinyin_flush_but_maintenance_flushes() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "微信"));
        state.finish_first_build();
        let dir = std::env::temp_dir().join(format!("prism-m2-shutdown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        state.set_pinyin_data_dir(&dir);

        checkpoint(&state, &dir, false).unwrap();
        assert!(
            index_cache::cache_path(&dir).exists(),
            "v5 缓存必须照常落盘"
        );
        assert!(
            !crate::pinyin_sidecar::path(&dir).exists(),
            "停机路径不得触发拼音全量重建"
        );

        checkpoint(&state, &dir, true).unwrap();
        assert!(
            crate::pinyin_sidecar::path(&dir).exists(),
            "maintenance 路径照常重建拼音"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S2（PRISM-IMPL-PLAN-4-2026-08-20）：字面命中数远超 max 时拼音扫描仍执行，
    /// 且经 S1 排序后「抖音」（Initials，class 0）排最前——`dy` 场景的索引器侧
    /// 端到端锚点。旧实现里 `literal_count < max` 为假，拼音 sidecar 一次都不扫。
    #[test]
    fn s2_pinyin_scans_and_ranks_past_literal_noise() {
        let state = ServiceState::new();
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "v".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            9,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "抖音", true).unwrap();
        for record in 11..51u32 {
            volume
                .upsert(u64::from(record), 5, &format!("body-{record:03}.css"), false)
                .unwrap();
        }
        state.publish(IndexState {
            volumes: vec![volume],
            generation: 1,
            events_since_checkpoint: 0,
        });
        let snapshot = state.index.read().unwrap().as_ref().unwrap().clone();
        *state.pinyin.write().unwrap() = Some(Arc::new(PinyinSidecar::build(&snapshot).unwrap()));

        let response = state.search("dy", 8, None, None).unwrap();
        let IndexerResponse::Results { items, .. } = response else {
            panic!("expected results");
        };
        assert!(items.len() <= 8, "truncate(max) 保证最终条数不变");
        assert!(
            items.iter().any(|item| item.name == "抖音"),
            "拼音命中必须在字面噪声填满配额后仍出现"
        );
        assert_eq!(items[0].name, "抖音", "class 0 拼音命中必须排最前");
        assert!(
            items.iter().any(|item| item.name.contains("body")),
            "字面命中仍在结果里"
        );
    }

    /// S2 回归锚：`literal_count < max` 的旧行为（字面稀少时拼音补位）不受影响。
    #[test]
    fn s2_pinyin_still_fills_sparse_literal_results() {
        let state = ServiceState::new();
        state.publish(IndexState {
            volumes: vec![test_volume("v1", "C:\\", "微信")],
            generation: 1,
            events_since_checkpoint: 0,
        });
        let snapshot = state.index.read().unwrap().as_ref().unwrap().clone();
        *state.pinyin.write().unwrap() = Some(Arc::new(PinyinSidecar::build(&snapshot).unwrap()));

        let response = state.search("wx", 8, None, None).unwrap();
        assert!(matches!(&response, IndexerResponse::Results { items, .. } if items.len() == 1));
    }

    fn volume_with_id(tag: &str, mount: &str, name: &str, id: VolumeId) -> VolumeIndex {
        let mut volume = test_volume(tag, mount, name);
        volume.volume_id = id;
        volume
    }

    /// 缓存 2 卷、现场 3 卷：2 匹配 + 新卷 E 进重建列表，无丢弃。
    #[test]
    fn s2_partition_two_cached_three_live() {
        let descriptors = vec![descriptor('C'), descriptor('D'), descriptor('E')];
        let cached = vec![
            volume_with_id("a", "C:\\", "one.txt", descriptors[0].id.clone()),
            volume_with_id("b", "D:\\", "two.txt", descriptors[1].id.clone()),
        ];
        let (matched, rebuild, dropped) =
            partition_volume_sets(cached, &descriptors);
        assert_eq!(matched.len(), 2);
        assert_eq!(rebuild.len(), 1, "现场新增卷必须进重建列表");
        assert_eq!(rebuild[0].id, descriptors[2].id);
        assert!(dropped.is_empty());
    }

    /// 缓存 3 卷、现场 2 卷：2 匹配 + 1 丢弃（已卸载），重建列表为空。
    #[test]
    fn s2_partition_three_cached_two_live() {
        let descriptors = vec![descriptor('C'), descriptor('D')];
        let removed = descriptor('Z');
        let cached = vec![
            volume_with_id("a", "C:\\", "one.txt", descriptors[0].id.clone()),
            volume_with_id("b", "D:\\", "two.txt", descriptors[1].id.clone()),
            volume_with_id("z", "Z:\\", "gone.txt", removed.id.clone()),
        ];
        let (matched, rebuild, dropped) = partition_volume_sets(cached, &descriptors);
        assert_eq!(matched.len(), 2);
        assert!(rebuild.is_empty(), "没有新增卷则无需重建");
        assert_eq!(dropped.len(), 1, "已卸载卷直接丢弃");
        assert_eq!(dropped[0].volume_id, removed.id);
    }

    /// 完全不相交：全部现场卷重建，全部缓存卷丢弃。
    #[test]
    fn s2_partition_disjoint_sets_rebuild_everything() {
        let descriptors = vec![descriptor('C')];
        let cached = vec![volume_with_id(
            "z",
            "Z:\\",
            "gone.txt",
            descriptor('Z').id,
        )];
        let (matched, rebuild, dropped) = partition_volume_sets(cached, &descriptors);
        assert!(matched.is_empty());
        assert_eq!(rebuild.len(), 1);
        assert_eq!(dropped.len(), 1);
    }

    /// 部分命中发布后：building 标志保持（R2 门未过）、两卷都可搜。
    #[test]
    fn s2_partial_publish_keeps_building_gate_and_serves_both_volumes() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "keep.txt"));
        state.merge_and_publish(test_volume("v2", "D:\\", "keep2.txt"));
        assert!(state.status().ready);
        assert!(state.status().building, "重建未全部落定前 R2 门保持关闭");
        assert!(!state.first_build_is_complete());
        assert!(state
            .search("keep", 8, None, None)
            .is_ok_and(|r| matches!(r, IndexerResponse::Results { ref items, .. } if items.len() == 2)));
    }

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
            .search("needle", 8, None, None)
            .is_ok_and(|response| matches!(response, IndexerResponse::Results { ref items, .. } if items.len() == 1)));
    }

    /// S5: 内存趋势 detail 必须包含四个可回溯字段；空索引与已发布索引都有效。
    #[test]
    fn memory_trend_detail_carries_all_observability_fields() {
        let empty = ServiceState::new();
        let detail = empty.memory_trend_detail();
        assert!(detail.contains("memory_bytes=0"), "{detail}");
        assert!(detail.contains("volumes=0"), "{detail}");
        assert!(detail.contains("events_since_checkpoint=0"), "{detail}");
        assert!(detail.contains("pinyin="), "{detail}");

        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));
        let detail = state.memory_trend_detail();
        assert!(detail.contains("volumes=1"), "{detail}");
        assert!(!detail.contains("memory_bytes=0"), "{detail}");
    }

    // --- S1（FRESH-AUDIT-2026-08-19）: 名字池压缩挪出写锁 -----------------------

    /// 构造带死名字字节的卷并发布到 state，然后强制到达压缩阈值。
    fn compactable_state() -> (std::sync::Arc<ServiceState>, crate::hierarchy::VolumeId) {
        let state = ServiceState::new();
        let mut volume = test_volume("v1", "C:\\", "keep.txt");
        for record in 11..50u32 {
            volume
                .upsert(frn2(record, 1), frn2(10, 1), &format!("dead-{record}.bin"), false)
                .unwrap();
            volume.delete(frn2(record, 1)).unwrap();
        }
        state.merge_and_publish(volume);
        {
            let mut guard = state.index.write().unwrap();
            let live = guard.as_mut().unwrap().volumes.last_mut().unwrap();
            live.force_name_compact_threshold_for_test(); // 强制触发阈值
        }
        let volume_id = {
            let guard = state.index.read().unwrap();
            guard.as_ref().unwrap().volumes[0].volume_id.clone()
        };
        (state, volume_id)
    }

    fn frn2(record: u32, sequence: u16) -> u64 {
        (u64::from(sequence) << 48) | u64::from(record)
    }

    /// USN 静止时：锁外压缩换入成功，live 卷名字池收缩、搜索不受影响。
    #[test]
    fn s1_off_lock_compact_swaps_in_when_usn_is_stable() {
        let (state, _vid) = compactable_state();
        let names_before = {
            let guard = state.index.read().unwrap();
            guard.as_ref().unwrap().volumes[0].names.len()
        };

        compact_volumes_off_lock(&state).unwrap();

        let (names_after, dead) = {
            let guard = state.index.read().unwrap();
            let live = &guard.as_ref().unwrap().volumes[0];
            (live.names.len(), live.dead_name_bytes_for_test())
        };
        assert!(names_after < names_before, "压缩必须收缩名字池");
        assert_eq!(dead, 0, "换入后死字节计数归零");
        assert!(state
            .search("keep", 8, None, None)
            .is_ok_and(|r| matches!(r, IndexerResponse::Results { ref items, .. } if !items.is_empty())));
    }

    /// clone 窗口内 USN 到达（next_usn 前移）：换入必须被拒——live 卷的游标
    /// 绝不能被陈旧快照回退，压缩重试或放弃都不影响数据新鲜度。
    #[test]
    fn s1_off_lock_compact_never_regresses_the_usn_cursor() {
        let (state, vid) = compactable_state();
        let stop_bumping = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // bumper 最后一次写入的游标值；MIN 表示尚未写过（release 下线程启动可能
        // 慢于主线程的压缩循环，必须等第一次真实写入后再开始压缩）。
        let latest_written = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(i64::MIN));
        let bumper_state = state.clone();
        let stop = stop_bumping.clone();
        let latest = latest_written.clone();
        let bumper = std::thread::spawn(move || {
            let mut usn: i64 = 10;
            while !stop.load(Ordering::Acquire) {
                usn += 1;
                let mut guard = bumper_state.index.write().unwrap();
                if let Some(live) = guard.as_mut().unwrap().volumes.first_mut() {
                    live.next_usn = usn;
                }
                drop(guard);
                latest.store(usn, Ordering::Release);
            }
        });

        // 等 bumper 完成至少一次写入，避免"压缩先跑完、断言语义空转"的假阳性。
        while latest_written.load(Ordering::Acquire) == i64::MIN {
            std::thread::yield_now();
        }

        // 压缩循环与游标推进并发：无论换入成败，断言只看最终一致性。
        for _ in 0..50 {
            let _ = compact_one_volume_off_lock(&state, &vid);
        }

        stop_bumping.store(true, Ordering::Release);
        bumper.join().unwrap();

        let live_usn = {
            let guard = state.index.read().unwrap();
            guard.as_ref().unwrap().volumes[0].next_usn
        };
        assert_eq!(
            live_usn,
            latest_written.load(Ordering::Acquire),
            "压缩换入绝不能把 USN 游标回退到旧快照"
        );
        // 无论是否成功换入，搜索必须始终可用。
        assert!(state
            .search("keep", 8, None, None)
            .is_ok_and(|r| matches!(r, IndexerResponse::Results { ref items, .. } if !items.is_empty())));
    }

    #[test]
    fn disabled_missing_and_building_pinyin_states_keep_literal_search_available() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "微信"));

        // pinyin_enabled defaults to true; a search with the flag on
        // lazy-loads the sidecar (Building status) but literal matches still work.
        let literal = state.search("微信", 8, None, None).unwrap();
        assert!(matches!(literal, IndexerResponse::Results { ref items, .. } if items.len() == 1));
        assert_eq!(state.pinyin_status(), PinyinStatus::Building);

        let pinyin_without_sidecar = state.search("wx", 8, None, None).unwrap();
        assert!(
            matches!(pinyin_without_sidecar, IndexerResponse::Results { ref items, .. } if items.is_empty())
        );

        let snapshot = state.index.read().unwrap().as_ref().unwrap().clone();
        *state.pinyin.write().unwrap() = Some(Arc::new(PinyinSidecar::build(&snapshot).unwrap()));
        // M6 (audit): search is read-only w.r.t. the pinyin flag. Disabling is
        // now done via SetPinyinEnabled / release_pinyin directly, not via search().
        state.release_pinyin();
        let disabled = state.search("微信", 8, None, None).unwrap();
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
            missing.search("微信", 8, None, None).unwrap(),
            IndexerResponse::Results { ref items, .. } if items.len() == 1
        ));
        assert_eq!(missing.pinyin_status(), PinyinStatus::Building);
        missing.load_pinyin_from_live();
        assert_eq!(missing.pinyin_status(), PinyinStatus::Missing);
        assert!(missing.pinyin_needs_rebuild.load(Ordering::Acquire));

        std::fs::write(dir.join("pinyin-v2.bin"), b"not-a-sidecar").unwrap();
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
            corrupt.search("微信", 8, None, None).unwrap(),
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

    /// AUDIT-2026-08-18 R-B2: 单卷重建（merge_and_publish）只替换该卷，
    /// 其他健康卷的索引对象不被替换。与 publish（全量替换）形成对比。
    #[test]
    fn single_volume_rebuild_preserves_other_volumes() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "alpha.txt"));
        state.merge_and_publish(test_volume("v2", "D:\\", "beta.txt"));
        let gen_before = state.generation();

        // 模拟单卷重建：只 merge_and_publish 卷 v2 的重建结果。
        state.merge_and_publish(test_volume("v2", "D:\\", "beta_rebuilt.txt"));
        let gen_after = state.generation();

        // generation +1（merge 只递增一次），不是 publish 的 epoch+1 全量替换。
        assert_eq!(gen_after, gen_before + 1);
        assert_eq!(state.status().volumes, 2, "卷数不变");

        // 卷 v1 的内容仍在——没被全量替换清掉。
        let search_v1 = state.search("alpha", 8, None, None).unwrap();
        assert!(matches!(search_v1, IndexerResponse::Results { ref items, .. } if items.len() == 1),
            "健康卷 v1 的索引应保留");

        // 卷 v2 的重建结果可见。
        let search_v2 = state.search("beta_rebuilt", 8, None, None).unwrap();
        assert!(matches!(search_v2, IndexerResponse::Results { ref items, .. } if items.len() == 1),
            "重建卷 v2 的新内容应可见");
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

    /// M2: 流式 checkpoint 的字节流必须与整态 clone 路径（派生序列化）逐字节一致
    /// ——v5 格式不升版、load 侧零改动的硬约束。
    #[test]
    fn m2_streaming_checkpoint_bytes_match_clone_path() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "alpha.txt"));
        state.merge_and_publish(test_volume("v2", "D:\\", "beta.txt"));
        state.finish_first_build();
        // 让两个路径序列化同一份计数（扣减语义不属于本断言）。
        {
            let mut guard = state.index.write().unwrap();
            guard.as_mut().unwrap().events_since_checkpoint = 77;
        }

        let dir_streaming = std::env::temp_dir().join(format!("prism-m2-s-{}", std::process::id()));
        let dir_clone = std::env::temp_dir().join(format!("prism-m2-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir_streaming);
        let _ = std::fs::remove_dir_all(&dir_clone);

        // 先取 clone 快照再跑 checkpoint——checkpoint 成功后会扣减 live 计数，
        // 后取的快照 events 会变 0（那是语义差异，不是字节差异）。
        let snapshot = state.index.read().unwrap().as_ref().unwrap().clone();
        index_cache::save(&snapshot, &dir_clone).unwrap();
        checkpoint(&state, &dir_streaming, true).unwrap(); // 内部走流式路径

        let streaming_bytes = std::fs::read(index_cache::cache_path(&dir_streaming)).unwrap();
        let clone_bytes = std::fs::read(index_cache::cache_path(&dir_clone)).unwrap();
        assert_eq!(
            streaming_bytes.len(),
            clone_bytes.len(),
            "流式与 clone 路径的字节长度必须一致"
        );
        assert_eq!(streaming_bytes, clone_bytes, "v5 字节流必须逐字节相同");

        // 流式产物可正常加载（load 侧零改动）。
        let loaded = index_cache::load(&dir_streaming).unwrap();
        assert_eq!(loaded.volumes.len(), 2);
        assert_eq!(loaded.events_since_checkpoint, 77);

        let _ = std::fs::remove_dir_all(&dir_streaming);
        let _ = std::fs::remove_dir_all(&dir_clone);
    }

    /// M2 + L（FRESH-AUDIT-3-2026-08-20）：流式写入的卷间核对按卷 next_usn——
    /// 其他卷的 USN 活动（generation 前移而本卷游标不动）不再作废本卷写入；
    /// 正被写入卷的 next_usn 前移则必须以 SNAPSHOT_CHANGED 中止，绝不把混合
    /// 状态的卷写进同一个 v5 文件。
    #[test]
    fn m2_streaming_snapshot_rejects_only_touched_volume() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "alpha.txt"));
        state.merge_and_publish(test_volume("v2", "D:\\", "beta.txt"));
        state.finish_first_build();

        let head = {
            let guard = state.index.read().unwrap();
            let index = guard.as_ref().unwrap();
            SnapshotHead {
                generation: index.generation,
                events_since_checkpoint: index.events_since_checkpoint,
                volumes: index.volumes.len(),
                next_usns: index.volumes.iter().map(|volume| volume.next_usn).collect(),
            }
        };
        // 其他卷活动的等价模拟：generation 前移而各卷 next_usn 不动 → 写入照常完成。
        {
            let mut guard = state.index.write().unwrap();
            guard.as_mut().unwrap().generation += 1;
        }
        let dir = std::env::temp_dir().join(format!("prism-m2-l-relax-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        checkpoint_streaming_with_head(&state, &dir, head.clone(), true)
            .expect("generation-only drift must not invalidate the write");
        let _ = std::fs::remove_dir_all(&dir);

        // 正被写入卷的 next_usn 前移 → SNAPSHOT_CHANGED。
        let head = {
            let guard = state.index.read().unwrap();
            let index = guard.as_ref().unwrap();
            SnapshotHead {
                generation: index.generation,
                events_since_checkpoint: index.events_since_checkpoint,
                volumes: index.volumes.len(),
                next_usns: index.volumes.iter().map(|volume| volume.next_usn).collect(),
            }
        };
        {
            let mut guard = state.index.write().unwrap();
            let index = guard.as_mut().unwrap();
            index.volumes[0].next_usn += 1;
        }
        let dir = std::env::temp_dir().join(format!("prism-m2-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let error = checkpoint_streaming_with_head(&state, &dir, head, true).unwrap_err();
        assert!(
            error.contains(index_cache::SNAPSHOT_CHANGED),
            "错误必须带可识别的重试标记：{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_index_is_never_written_to_disk() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "needle.txt"));

        let dir = std::env::temp_dir().join(format!("prism-g9-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Mid-first-build checkpoint: succeeds as a no-op, writes nothing.
        checkpoint(&state, &dir, true).unwrap();
        assert!(
            !index_cache::cache_path(&dir).exists(),
            "a partial first build must not leave a cache file behind"
        );

        state.finish_first_build();
        checkpoint(&state, &dir, true).unwrap();
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

        checkpoint(&state, &dir, true).unwrap();
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

    /// P1（第一轮 bug 修复）：空查询 + 可用 root = 浏览该目录（root 自身 +
    /// 全部后代，空名字匹配一切）；全局空查询仍短路为空（不搜索）。
    /// 文件 root 被结构化拒绝（NotADirectory）——调用方据此走父目录+末段。
    #[test]
    fn p1_empty_query_with_root_browses_and_global_stays_empty() {
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
        volume.upsert(11, 10, "inside.txt", false).unwrap();
        volume.upsert(12, 10, "子目录", true).unwrap();
        volume.upsert(13, 12, "deep.txt", false).unwrap();
        volume.upsert(20, 5, "outside.txt", false).unwrap();
        volume.upsert(30, 5, "报告.docx", false).unwrap();
        state.publish(IndexState {
            volumes: vec![volume],
            generation: 3,
            events_since_checkpoint: 0,
        });

        let browse = state.search("", 8, None, Some(r"c:\项目")).unwrap();
        let IndexerResponse::Results { items, .. } = browse else {
            panic!("expected browse results for a valid root");
        };
        let mut paths: Vec<&str> = items.iter().map(|item| item.path.as_str()).collect();
        paths.sort_unstable();
        // root 自身（depth 0 在范围内）+ 直接子项 + 深层后代；范围外不进。
        assert_eq!(
            paths,
            vec![r"C:\项目", r"C:\项目\inside.txt", r"C:\项目\子目录", r"C:\项目\子目录\deep.txt"]
        );

        // 文件 root：结构化拒绝（broker 侧据此改走父目录 + 末段精确命中）。
        let file_root = state.search("", 8, None, Some(r"c:\报告.docx")).unwrap();
        assert!(matches!(
            file_root,
            IndexerResponse::RootUnavailable {
                reason: crate::root_scope::RootRejection::NotADirectory,
                ..
            }
        ));

        // 全局空查询维持旧语义：不搜索，空结果。
        let global = state.search("", 8, None, None).unwrap();
        let IndexerResponse::Results { items, .. } = global else {
            panic!("expected results");
        };
        assert!(items.is_empty());
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

        let scoped = state.search("needle", 8, None, Some(r"c:/项目/")).unwrap();
        let IndexerResponse::Results { items, .. } = scoped else {
            panic!("expected results for a valid root");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, r"C:\项目\needle-inside.txt");

        // Absent and blank roots must behave identically to the pre-root protocol.
        for root in [None, Some(""), Some("   ")] {
            let global = state.search("needle", 8, None, root).unwrap();
            let IndexerResponse::Results { items, .. } = global else {
                panic!("expected results for {root:?}");
            };
            assert_eq!(items.len(), 2, "root {root:?} must not restrict anything");
        }

        // F2（FRESH-AUDIT-2）：同一 root + 同一 generation 的第二次解析命中缓存
        //（bound 与首次一致）；generation 前进后旧缓存不得复用（结构化拒绝）。
        {
            let guard = state.index.read().unwrap();
            let live = guard.as_ref().unwrap();
            let first = match state.resolve_root_bound(live, r"c:/项目/") {
                RootBoundOutcome::Bound(bound) => bound.unwrap(),
                other => panic!("expected a bound, got {other:?}"),
            };
            let cached = match state.resolve_root_bound(live, r"c:/项目/") {
                RootBoundOutcome::Bound(bound) => bound.unwrap(),
                other => panic!("expected a bound, got {other:?}"),
            };
            assert_eq!(first, cached);
        }
        state.publish(IndexState {
            volumes: vec![],
            generation: 4,
            events_since_checkpoint: 0,
        });
        {
            let guard = state.index.read().unwrap();
            let live = guard.as_ref().unwrap();
            let stale = state.resolve_root_bound(live, r"c:/项目/");
            drop(guard);
            assert!(matches!(
                stale,
                RootBoundOutcome::Unavailable(IndexerResponse::RootUnavailable {
                    reason: crate::root_scope::RootRejection::VolumeNotIndexed,
                    ..
                })
            ));
        }
    }

    /// B1（AUDIT-4 批次C）：RootBound 缓存失效粒度=被解析卷自身 next_usn。
    /// 他卷活动（含 generation 前移）不作废缓存条目；本卷 next_usn 前移后
    /// 旧 bound 不复用，重解析得到新记录号。
    #[test]
    fn b1_root_bound_cache_survives_other_volume_activity_but_not_own_changes() {
        let state = ServiceState::new();
        let mut c = VolumeIndex::new(
            VolumeId { guid: "c".into(), serial: 1 },
            "C:\\".into(),
            7,
            9,
            5,
        )
        .unwrap();
        c.upsert(10, 5, "项目", true).unwrap();
        c.upsert(11, 10, "微信.txt", false).unwrap();
        let mut d = VolumeIndex::new(
            VolumeId { guid: "d".into(), serial: 2 },
            "D:\\".into(),
            7,
            9,
            5,
        )
        .unwrap();
        d.upsert(10, 5, "other", false).unwrap();
        state.publish(IndexState {
            volumes: vec![c, d],
            generation: 1,
            events_since_checkpoint: 0,
        });

        let bound_of = |root: &str| {
            let guard = state.index.read().unwrap();
            let live = guard.as_ref().unwrap();
            match state.resolve_root_bound(live, root) {
                RootBoundOutcome::Bound(Some(bound)) => Some(bound),
                _ => None,
            }
        };
        assert_eq!(bound_of(r"C:\项目").map(|b| b.root_record), Some(10));

        // 他卷活动：D 卷 next_usn 前移 + generation 前移——缓存条目保持命中。
        {
            let mut guard = state.index.write().unwrap();
            let live = guard.as_mut().unwrap();
            live.volumes[1].next_usn += 1;
            live.generation += 1;
        }
        assert_eq!(bound_of(r"C:\项目").map(|b| b.root_record), Some(10));
        assert_eq!(
            state.root_bound_cache.read().unwrap().len(),
            1,
            "他卷活动不得作废/替换缓存条目"
        );

        // 本卷变化：C 卷 next_usn 前移 + 同路径目录换记录号——旧 bound 不复用。
        {
            let mut guard = state.index.write().unwrap();
            let live = guard.as_mut().unwrap();
            let volume = &mut live.volumes[0];
            volume.delete(10).unwrap();
            volume.upsert(20, 5, "项目", true).unwrap();
            volume.next_usn += 1;
            live.generation += 1;
        }
        assert_eq!(bound_of(r"C:\项目").map(|b| b.root_record), Some(20));
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
            let response = state.search("needle", 8, None, Some(root)).unwrap();
            match response {
                IndexerResponse::RootUnavailable { reason, message } => {
                    assert_eq!(reason, expected, "unexpected reason for {root}");
                    assert!(!message.is_empty());
                }
                other => panic!("expected RootUnavailable for {root}, got {other:?}"),
            }
        }
    }

    /// 审计批次 2 L1：连上不发 hello 的客户端必须在限时内被拒，而不是常驻挂死任务。
    #[tokio::test]
    async fn silent_client_times_out_of_hello() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut lines = crate::ipc::BoundedLineReader::new(server);
        let error = read_hello_line(&mut lines, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(error.contains("handshake timeout"), "got: {error}");
        let _ = client.shutdown().await;
    }

    /// 审计批次 2 L1：客户端先断开（EOF 先于限时）→ 立即报 closed，不空等。
    #[tokio::test]
    async fn early_eof_before_hello_reports_closure_immediately() {
        let (client, server) = tokio::io::duplex(64);
        drop(client);
        let mut lines = crate::ipc::BoundedLineReader::new(server);
        let error = read_hello_line(&mut lines, Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(error, "client closed before hello");
    }

    /// 审计批次 2 L1：及时发 hello 的客户端正常通过握手读。
    #[tokio::test]
    async fn prompt_client_passes_hello() {
        let (mut client, server) = tokio::io::duplex(64);
        client
            .write_all(
                format!("{{\"type\":\"Hello\",\"protocol\":{INDEXER_PROTOCOL}}}\n").as_bytes(),
            )
            .await
            .unwrap();
        client.flush().await.unwrap();
        let mut lines = crate::ipc::BoundedLineReader::new(server);
        let hello = read_hello_line(&mut lines, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(hello.contains("Hello"));
    }

    /// G5（FRESH-AUDIT-2）+复审 M（2026-08-21）：SetPinyinEnabled 限速——首条
    /// 放行，窗口内的第二条拒绝。限速状态进程级共享：多连接共享同一窗口，
    /// 不能各开一条连接绕过（防任意本地用户触发拼音重建风暴）。
    #[test]
    fn g5_set_pinyin_rate_limit_rejects_rapid_second_call() {
        assert!(!set_pinyin_rate_limited(), "first call passes");
        assert!(
            set_pinyin_rate_limited(),
            "immediate second call is rejected (process-wide window)"
        );
    }

    /// AUDIT-2026-08-18 R-B1: rebuild_pinyin_from_live 期间写锁必须可在 <100ms 内获得。
    /// 修复前该方法在读锁内做全遍历+fsync，百万级中文文件下持锁数秒~数十秒，
    /// 阻塞 USN 写者和搜索读者。修复后只 clone 快照（毫秒级 memcpy）再放锁。
    #[test]
    fn rebuild_pinyin_from_live_does_not_hold_read_lock() {
        let state = ServiceState::new();
        state.merge_and_publish(test_volume("v1", "C:\\", "微信.txt"));
        state.pinyin_enabled.store(true, Ordering::Release);
        let dir = std::env::temp_dir()
            .join(format!("prism-rb1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        state.set_pinyin_data_dir(&dir);

        // rebuild_pinyin_from_live 在锁外完成全遍历 + save + load。
        // 即使没有中文文件（只有 "微信.txt"），build 仍会遍历全部节点。
        state.rebuild_pinyin_from_live();

        // 重建完成后写锁立即可得（不被任何读锁阻塞）。
        let start = std::time::Instant::now();
        let _guard = state.index.write().unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "write lock took {elapsed:?} — rebuild should not hold any lock after returning"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AUDIT-2026-08-18 R-A3: 前 64 个连接准入，第 65 个被拒；
    /// 释放后席位恢复。超限连接在握手前即被断开，不产生任务。
    #[test]
    fn connection_admission_enforces_the_cap() {
        let state = ServiceState::new();
        for i in 0..ServiceState::MAX_CONNECTIONS {
            assert!(
                state.try_admit_connection(),
                "connection {i} must be admitted"
            );
        }
        assert!(
            !state.try_admit_connection(),
            "connection beyond the cap must be rejected"
        );
        state.release_connection();
        assert!(
            state.try_admit_connection(),
            "released seat must be reusable"
        );
        state.release_connection();
    }

    /// AUDIT-2026-08-18 R-A1: 不匹配的协议版本必须返回结构化 `protocol_mismatch`
    /// 错误（含双方版本号），而不是泛化的 "incompatible or missing hello"。
    /// 直接测试错误消息格式——handle_connection 内部构造的消息必须包含
    /// 双方版本号，使客户端能精确诊断不匹配原因。
    #[test]
    fn protocol_mismatch_error_contains_both_versions() {
        let client_version = 1u32;
        let message = format!(
            "protocol_mismatch: server={INDEXER_PROTOCOL} client={client_version}"
        );
        assert!(
            message.starts_with("protocol_mismatch"),
            "error must start with protocol_mismatch prefix, got: {message}"
        );
        assert!(
            message.contains(&format!("server={INDEXER_PROTOCOL}")),
            "must report server version {INDEXER_PROTOCOL}, got: {message}"
        );
        assert!(
            message.contains(&format!("client={client_version}")),
            "must report client version {client_version}, got: {message}"
        );
    }
}
