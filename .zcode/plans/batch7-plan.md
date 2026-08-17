# 审计批次7 实施方案：服务长任务与锁治理（H1/H2）

## 参考软件调研结论

### Everything (voidtools)
- **全内存索引**：运行时整索引驻 RAM，USN journal 紧凑追加（默认 1MB 环形覆盖），退出时写盘。
- **关停策略**：Everything 是用户进程（非 Windows 服务），关停时直接退出——不涉及 SCM StopPending/checkpoint 机制。但它的"退出才写盘"模式印证了：**服务进程不能靠退出落盘，必须在运行时定期 checkpoint 并在关停时快速 flush**。
- **并发模型**：Everything 内部是单线程事件循环 + USN 读线程，搜索和索引更新不共享锁——搜索读的是索引的不可变快照，USN 更新写的是可变副本，通过双缓冲（double buffering）切换。这印证了"读侧不持写锁"的核心方向。

### Windows SCM (Microsoft 官方文档)
- **控制处理器必须 30 秒内返回**：SCM 要求 Handler 函数快速返回，长任务应放到辅助线程。
- **StopPending 期间必须周期递增 dwCheckPoint**：SCM 用 dwCheckPoint 判断服务是否在取得进展。如果 dwCheckPoint 不变，SCM 在 dwWaitHint 超时后强杀。
- **Services snap-in 限制 125 秒**：用户从服务管理器停止时，最多等 125 秒。系统关机时由 `WaitToKillServiceTimeout` 控制（默认约 20 秒）。
- **最佳实践**：服务应尽快完成清理；保存未保存的数据时，只保存必要数据，不花时间释放内存或释放其他系统资源。
- **启示**：Prism 当前 wait_hint 仅 10 秒、checkpoint 固定 1，且 rebuild 分支不响应 stop——SCM 会在 10 秒后强杀。正确做法是：StopPending 期间周期递增 checkpoint + 拉长 wait_hint；rebuild 和 checkpoint 长任务内嵌 stop 检查。

### tokio-util CancellationToken
- `CancellationToken::cancel()` 唤醒所有等待者；`cancelled()` 返回 Future，可放入 `select!`。
- `child_token()` 创建子 token，父取消时子也取消，子取消不影响父。
- `is_cancelled()` 可在同步代码中轮询，用于 `spawn_blocking` 内部的 stop 检查。
- Prism 已有等效的 `Shutdown`（AtomicBool + Notify），`is_requested()` 对应 `is_cancelled()`，`cancelled()` 对应 select 分支。**不需要引入 tokio-util 依赖**。

### 对 Prism 批次7 的启示
1. **H1 核心**：`run()` 的 `select!` 循环里，rebuild 分支（分钟级）和 checkpoint 分支（秒到数十秒）都是 `await` 在 `spawn_blocking` 上，期间 `stop.cancelled()` 无法被选中。修复方式：用 `tokio::select!` 把 stop 和 spawn_blocking 的 await 竞争，stop 到达时 abort blocking 任务。同时，SCM handler 的 StopPending 需要周期上报 checkpoint。
2. **H2 核心**：拼音加载在 `search()` 内持读锁时调用 `ensure_pinyin_loaded → load_pinyin`，后者做 mmap + postcard 反序列化 + validate_disk（含全量 names hash）——这些 I/O 和 CPU 在读锁内完成，阻塞 USN 写锁。compaction 在写锁内做全卷 O(n) 扫描+重建。修复方式：拼音加载移出读锁（锁外加载 + 世代比对 + 重取锁安装）；compaction 改"读锁克隆 → 锁外压缩 → 写锁 mem::swap"。

---

## H1：run() select 分支长任务响应 SCM Stop

### 现状
`run()`（indexer_runtime.rs:697-776）的 `select!` 循环有三个分支：
1. `stop.cancelled() => break` — 关停信号
2. `rebuild_rx.recv() =>` — 重建请求，内部 `spawn_blocking(build_all).await` 是分钟级
3. `maintenance.tick() =>` — 5 秒节拍，内部可能 `checkpoint_async().await` 是秒到数十秒级

问题：分支 2 和 3 的 `await` 期间，`stop.cancelled()` 无法被 select 选中——因为 select 只在所有分支都 pending 时才 poll。`spawn_blocking(...).await` 是 pending 状态，但 `stop.cancelled()` 也在 pending——实际上 select 会同时 poll 两者，哪个先 ready 就走哪个。**但当前代码没有把 stop 和 rebuild/checkpoint 放在同一层 select**——它们是嵌套在分支内部的顺序 await。

具体来说：
- rebuild 分支：`rebuild_rx.recv()` 返回后，进入 `spawn_blocking(build_all).await`——这个 await 是顺序的，不在 select 里。stop 到达时，必须等 `build_all` 完成才能回到循环顶部的 select 检测 stop。
- maintenance 分支：`checkpoint_async(...).await` 同理。

### 做法

#### 1. rebuild 分支：select 竞争 stop
```rust
reason = rebuild_rx.recv() => {
    let Some(reason) = reason else { break };
    epoch.fetch_add(1, Ordering::AcqRel);
    state.building.store(true, Ordering::Release);
    // ...log...
    while rebuild_rx.try_recv().is_ok() {}
    let build_task = tokio::task::spawn_blocking(build_all);
    let rebuilt = tokio::select! {
        result = build_task => result.map_err(|error| format!("rebuild task: {error}"))?,
        _ = stop.cancelled() => {
            // build_all 不接受 stop（它是全量 MFT 重建），但我们可以放弃等待结果。
            // abort 确保任务不再占用 blocking pool；epoch 已 +1 使旧 watcher 失效。
            // 内存索引保持 rebuild 前的状态，SCM 退出流程继续。
            // 注意：abort 不会中断正在运行的 spawn_blocking——它只丢弃 JoinHandle。
            // build_all 会在 MFT 枚举自然完成后结束，但本函数不再等它。
            epoch.fetch_add(1, Ordering::AcqRel); // 再 +1 撤销刚才的 epoch 变更
            break;
        }
    };
    // ... 后续 publish 逻辑不变 ...
}
```

**关键约束**：
- `build_all` 本身不检查 stop（它调用 `ntfs::build_volume` 不带 should_cancel 回调）。这与首建路径不同（首建有 `build_volume_reporting` 带 stop）。但 rebuild 是低频事件（USN 边界变更触发），且 abort 后 blocking 线程会自然完成、不占 tokio worker。等待才是问题。
- epoch 已经 `fetch_add(1)`，如果 abort 后 break，epoch 再 +1 撤销——但更简洁的做法是：不在 select 内做 epoch 变更，只在确定要 rebuild 后才变 epoch。调整：把 epoch 变更移到 rebuild 确认成功后。

**修正方案**：
```rust
reason = rebuild_rx.recv() => {
    let Some(reason) = reason else { break };
    logging::event_detail("info", "rebuild_requested", &reason, None, None);
    log(format!("serialized index rebuild requested: {reason}"));
    while rebuild_rx.try_recv().is_ok() {}
    let build_task = tokio::task::spawn_blocking(build_all);
    let rebuilt = tokio::select! {
        result = build_task => result,
        _ = stop.cancelled() => {
            log("rebuild aborted by shutdown");
            break;
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
            // ... publish / save / pinyin / watchers（不变）...
        }
        Err(error) => state.set_error(error),
    }
}
```

这样 stop 在 rebuild 期间能立即 break 出循环。epoch 变更只在 rebuild 成功后做——abort 时不做 epoch 变更，旧 watcher 不受影响。

#### 2. maintenance 分支：select 竞争 stop
maintenance 分支有两个长操作：`rebuild_pinyin_from_live` 和 `checkpoint_async`。

```rust
_ = maintenance.tick() => {
    if state.pinyin_needs_rebuild.swap(false, Ordering::AcqRel)
        && state.pinyin_status() != PinyinStatus::Disabled
    {
        let pinyin_task = rebuild_pinyin_from_live(state.clone());
        tokio::pin!(pinyin_task);
        let pinyin_result = tokio::select! {
            result = &mut pinyin_task => result,
            _ = stop.cancelled() => break,
        };
        // ... 处理 pinyin_result ...
    }
    // checkpoint_due 判断（不变）
    if checkpoint_due {
        let checkpoint_task = checkpoint_async(state.clone(), data_dir.clone());
        tokio::pin!(checkpoint_task);
        let checkpoint_result = tokio::select! {
            result = &mut checkpoint_task => result,
            _ = stop.cancelled() => break,
        };
        // ... 处理 checkpoint_result ...
    }
}
```

**关键约束**：
- `rebuild_pinyin_from_live` 内部是 `spawn_blocking`，abort 后 blocking 线程自然完成。pinyin sidecar 的 rebuild 是幂等的——下次 maintenance 会重试。
- `checkpoint_async` 内部是 `spawn_blocking(checkpoint)`。abort 后 blocking 线程仍会运行完 checkpoint（写盘 + fsync），但 `run()` 不再等它。**风险**：如果 checkpoint 正在写临时文件，abort 后线程继续写完并 atomic_replace——这是安全的，因为 atomic_replace 保证原子性。但如果 SCM 强杀进程，临时文件可能残留——`atomic_replace` 下次调用时会覆盖。可接受。
- **shutdown checkpoint（run() 末尾 :779）**：循环 break 后的 `checkpoint_async` 仍保留——这是关停时的最终落盘，需要做。但如果 stop 是因为系统关机（WaitToKillServiceTimeout ~20s），这个 checkpoint 可能来不及。这正是 H1 修复的意义——至少让循环快速 break 出来，给 shutdown checkpoint 留时间。

#### 3. SCM handler 拉长 wait_hint + 周期 checkpoint
当前 `prism-indexer-service.rs:46-58`：
```rust
ServiceControl::Stop => {
    status.set_service_status(service_status(
        ServiceState::StopPending,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        1,                                    // checkpoint 固定 1
        Duration::from_secs(10),              // wait_hint 10s
    ));
    handler_stop.request();
    // ... 返回，run() 的 select! 检测到 stop.cancelled() ...
}
```

问题：`wait_hint=10s` 意味着 SCM 等 10 秒后强杀。如果 rebuild 正在跑（分钟级），10 秒不够。

修复：拉长 `wait_hint` 到 30 秒（Services snap-in 允许 125s，但 30s 对 rebuild/checkpoint 足够），并在 StopPending 期间周期递增 checkpoint。

但 `run_runtime` 是 `block_on(run(stop))`——SCM handler 在另一个线程。当前 handler 发出 stop 后就返回了，`run()` 在异步运行时中检测 stop 并 break。`run_runtime` 的 `runtime.shutdown_timeout(Duration::from_secs(2))` 给了 2 秒清理。

**方案**：在 `run_service()` 中，handler 发出 stop 后，起一个后台线程周期上报 StopPending + 递增 checkpoint，直到 `run_runtime` 返回。

```rust
fn run_service() -> Result<(), String> {
    let stop = prism_core::indexer_runtime::Shutdown::new();
    let handler_stop = stop.clone();
    let status_slot = Arc::new(OnceLock::<ServiceStatusHandle>::new());
    let handler_status = status_slot.clone();
    let status = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop => {
            if let Some(status) = handler_status.get() {
                let _ = status.set_service_status(service_status(
                    ServiceState::StopPending,
                    ServiceControlAccept::empty(),
                    ServiceExitCode::Win32(0),
                    1,
                    Duration::from_secs(30),   // 拉长到 30s
                ));
            }
            handler_stop.request();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .map_err(|error| error.to_string())?;
    let _ = status_slot.set(status);
    status
        .set_service_status(service_status(
            ServiceState::Running,
            ServiceControlAccept::STOP,
            ServiceExitCode::Win32(0),
            0,
            Duration::default(),
        ))
        .map_err(|error| error.to_string())?;

    // StopPending 期间周期上报 checkpoint，让 SCM 知道服务仍在取得进展。
    // handler 发出 stop 后，handler 线程就返回了——这里在另一个线程里递增 checkpoint。
    let reporter_stop = stop.clone();
    let reporter_status = status_slot.clone();
    let reporter = std::thread::spawn(move || {
        let mut checkpoint: u32 = 1;
        while !reporter_stop.is_requested() {
            std::thread::sleep(Duration::from_secs(3));
        }
        // Stop 已请求——周期递增 checkpoint 直到进程退出
        while reporter_stop.is_requested() {
            checkpoint = checkpoint.saturating_add(1);
            if let Some(status) = reporter_status.get() {
                let _ = status.set_service_status(service_status(
                    ServiceState::StopPending,
                    ServiceControlAccept::empty(),
                    ServiceExitCode::Win32(0),
                    checkpoint,
                    Duration::from_secs(30),
                ));
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });

    let result = run_runtime(stop);
    // reporter 线程在 run_runtime 返回后自然退出（stop 已 requested，is_requested() 返回 true，
    // 循环继续——但进程即将进入 Stopped 状态并退出，不会泄漏）。
    // 需要让 reporter 知道可以停了：
    // 方案：reporter 在 run_runtime 返回后由 drop 自然结束——但 is_requested() 仍为 true。
    // 更简洁：reporter 只在 stop 后跑有限轮次（30s / 5s = 6 轮），然后退出。
    let exit_code = if result.is_ok() {
        ServiceExitCode::Win32(0)
    } else {
        ServiceExitCode::ServiceSpecific(1)
    };
    status
        .set_service_status(service_status(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            exit_code,
            0,
            Duration::default(),
        ))
        .map_err(|error| error.to_string())?;
    // 不 join reporter——它会在 SetServiceStatus(Stopped) 后被 SCM 终止进程时自然结束。
    // 但为安全起见，让 reporter 在有限轮次后自行退出。
    result
}
```

**简化方案（推荐）**：不使用后台线程。直接在 handler 中设 wait_hint=30s + checkpoint=1，然后 `handler_stop.request()`。`run()` 的修复（select 竞争 stop）确保最长等一个 spawn_blocking 完成或 abort。`runtime.shutdown_timeout(2s)` 确保异步任务清理。如果 rebuild 正在跑，`select!` 竞争会让它 abort 并 break——不需要 30 秒。只需把 wait_hint 从 10s 拉到 30s 作为安全余量。

**最终方案**：拉长 wait_hint 到 30 秒 + `run()` 内部 select 竞争 stop。不引入后台 reporter 线程——run() 修复后最长等一个 spawn_blocking 的 abort（微秒级），shutdown checkpoint 是秒级，30 秒余量足够。

### 风险与对策
- **rebuild 被 abort 后的内存状态**：`build_all` 是纯函数（读 MFT 构建新 IndexState），不修改现有 live index。abort 后内存索引保持 rebuild 前的状态——安全。
- **checkpoint 被 abort 后的磁盘状态**：`checkpoint` 先 clone index（读锁内 memcpy），释放锁，然后 save（validate + serialize + fsync）。abort 后 blocking 线程继续写完——atomic_replace 保证原子性。如果进程被强杀在 fsync 中间，临时文件残留——下次启动 load_cached 会拒绝损坏缓存走全量重建。安全。
- **epoch 竞态**：rebuild 分支 abort 时不做 epoch 变更，旧 watcher 不受影响。如果 rebuild 成功后才 stop，epoch 已 +1，watcher 会检测到 epoch 变化退出——安全。

### 测试
- 新增测试：`run()` 在 rebuild 期间收到 stop 时能快速 break（用 mock build_all 或短超时）。
- 已有的 stop 相关测试（build_volume_reporting_with 的 stop 测试）保持不变。
- SCM handler 的 wait_hint 变更不需要单测（是 SCM 状态上报）。

---

## H2：读锁内重活移出 + compaction 锁外压缩

### 现状
两个问题：

**H2a：拼音 sidecar 加载持读锁做重活**
`search()`（:509-511）在读锁内调用 `ensure_pinyin_loaded(state)` → `load_pinyin(index)` → `PinyinSidecar::load(data_dir, index)`：
1. mmap 打开 sidecar 文件
2. postcard 反序列化（大卷可达百 MB）
3. `validate_disk`：全量 names hash（O(n) 遍历所有卷的 names 字节）
4. 安装到 `self.pinyin` 写锁

步骤 1-3 全在读锁内完成，步骤 4 需要写锁（但 pinyin 是独立的 RwLock，不是 index 的 RwLock）。

**实际上**：`load_pinyin` 在 `search()` 的 `self.index.read()` 持有期间调用，但 `load_pinyin` 内部操作的是 `self.pinyin`（另一个 RwLock）和磁盘 I/O。问题是 `search()` 持有 `self.index` 的读锁——如果 `load_pinyin` 里有耗时操作（mmap + 反序列化 + validate），这段时间 `self.index` 的读锁不释放，USN watcher 的 `self.index.write()` 被阻塞。

**H2b：compaction 在写锁内做全卷 O(n)**
`watch_volume`（:1254-1258）在 `self.index.write()` 持有期间对**所有卷**调用 `compact_names_if_needed()`。每个卷的 compaction 做：
1. 遍历所有 nodes（O(n)），filter present + name_off != NO_NAME
2. 对每个 slot 调用 `name_at` 读取名字长度
3. 计算 live_bytes（sum of name.len() + 1）
4. 如果 dead bytes > threshold，重建整个 names pool

这是 O(所有卷的所有节点)，在写锁内完成。大卷（3.3M nodes）可能需要数秒。

### 做法

#### H2a：拼音加载移出读锁

`load_pinyin` 需要 `&IndexState` 做两件事：
1. `validate_disk`：对比 `index_identity(index)`（全量 names hash）和 `volume_count`
2. 如果 validate 失败，可能需要 `rebuild_pinyin(index)`（需要遍历所有节点编码拼音）

**方案**：在 `search()` 中，先从读锁获取 index 的浅快照（只取 validate 需要的数据），释放读锁，在锁外做 mmap + 反序列化 + validate，然后重取读锁验证世代，安装 sidecar。

但 `index_identity` 需要全量 names hash——这本身是 O(n) 操作。如果在锁外做，需要 clone 整个 IndexState（O(n) memcpy）。这和 checkpoint 路径的做法一致（checkpoint 也是 clone index 后释放锁）。

**更实际的方案**：拼音 sidecar 的 `load` 已经 mmap 了磁盘文件。validate_disk 的 `index_identity` 遍历 names 做哈希——这个操作是只读的，不修改 index。**关键洞察**：读锁是共享的——多个搜索可以并发持有读锁。`load_pinyin` 在读锁内做 mmap + 反序列化，不影响其他搜索的读锁。它只阻塞 USN watcher 的写锁。

所以 H2a 的真正问题是：拼音加载（mmap + 反序列化 + validate）在 search() 的读锁内执行，阻塞了 USN watcher 获取写锁。USN watcher 持有写锁的时间是 `apply_records` + compaction——如果搜索持续阻塞写锁，USN 积压，最终 broker 8 秒超时。

**修复方案**：`load_pinyin` 改为不在 `search()` 的读锁内调用。改为：
1. `search()` 检查 pinyin 是否已加载——如果已加载，直接用。
2. 如果未加载，search 走字面匹配（当前已有这个降级路径——pinyin 延迟加载，字面匹配不受影响）。
3. 在 `search()` 结束后（读锁已释放），异步触发 pinyin 加载。

**但**：当前 `ensure_pinyin_loaded` 是同步的——第一次搜索就需要拼音结果。如果改为异步，第一次搜索会缺失拼音结果，用户体验降级。

**更优方案**：`load_pinyin` 拆成两步：
1. 锁外：mmap + 反序列化（不需要 index 引用——postcard 反序列化不依赖 IndexState）
2. 读锁内：`validate_disk`（需要 index 引用做 identity hash）

但 `validate_disk` 的 `index_identity` 是 O(n) 遍历 names——这在读锁内仍然是重活。

**最终方案**：用世代（generation）做乐观并发。`load_pinyin` 改为：
1. 在 `self.index.read()` 内获取 `index.generation`，释放读锁。
2. 锁外 mmap + 反序列化 + `validate_disk`（用刚才获取的 generation——但 validate 需要 `index_identity(index)`，这需要 index 内容）。

问题：`validate_disk` 需要 `&IndexState` 来计算 `index_identity`。不能只用 generation——generation 不包含 names 内容。

**真正的最终方案**：

`load_pinyin` 当前签名：`fn load_pinyin(&self, index: &IndexState)`。
它需要 `index` 来：a) `validate_disk` 调 `index_identity(index)`，b) 失败时 `rebuild_pinyin(index)`。

**方案 A：clone index 快照，锁外做**
```rust
fn load_pinyin_outside_lock(&self) {
    // Step 1: 读锁内只取 generation + clone IndexState
    let snapshot = {
        let guard = self.index.read().unwrap();
        guard.as_ref().cloned()  // O(n) memcpy，但纯内存操作
    };
    let Some(index) = snapshot else { return; };
    // Step 2: 锁外做 mmap + 反序列化 + validate + 可能的 rebuild
    // ... PinyinSidecar::load(&data_dir, &index) 或 PinyinSidecar::build(&index) ...
    // Step 3: 读锁重取，验证 generation 未变
    let current_gen = self.generation();
    if current_gen != index.generation {
        return; // 丢弃，下次重试
    }
    // Step 4: 写锁安装
    if let Ok(mut current) = self.pinyin.write() {
        *current = Some(sidecar);
    }
    self.set_pinyin_status(PinyinStatus::Ready);
}
```

**clone 成本分析**：IndexState 的 clone 是 `Vec<VolumeIndex>` 的 clone——每个 VolumeIndex 有 `nodes: Vec<NodeSlot>`（8 bytes each）和 `names: Vec<u8>`。3.3M nodes × 8 bytes = 26MB，names 约 100MB。Clone 总量约 130MB memcpy——在现代硬件上约 10-30ms。这远小于 mmap + 反序列化的时间（百 MB 文件可达数百 ms）。且 clone 不持锁——读锁只在 clone 期间持有。

但这与 checkpoint 路径的 clone 完全一样（checkpoint 也是 clone 后释放锁再 save）。所以这是仓库已有范式。

**方案 B：只取 identity hash，不 clone index**
```rust
fn load_pinyin_outside_lock(&self) {
    // Step 1: 读锁内计算 identity hash + volume_count，释放锁
    let (identity, volume_count, generation) = {
        let guard = self.index.read().unwrap();
        match guard.as_ref() {
            Some(index) => (index_identity(index), index.volumes.len() as u16, index.generation),
            None => return,
        }
    };
    // Step 2: 锁外 mmap + 反序列化
    let sidecar = /* PinyinSidecar::load_raw(&data_dir) */;
    // Step 3: 锁外 validate（用预计算的 identity，不需要 &IndexState）
    // Step 4: 世代比对 + 安装
}
```

这需要把 `validate_disk` 拆成"需要 index"和"不需要 index"两部分，或把 `index_identity` 的结果传入。

**方案 B 更优**：identity hash 是 O(n) 但只做一次 hash（不 clone 整个 index），内存开销从 130MB 降到 8 bytes。但这需要修改 `PinyinSidecar::load` 的签名——从 `load(data_dir, &IndexState)` 改为 `load(data_dir, identity_hash, volume_count)`。

**决策：采用方案 B**——修改 `PinyinSidecar::load` 接受预计算的 identity + volume_count，避免在读锁内做 mmap + 反序列化。

详细改动：

1. `pinyin_sidecar.rs`：`load` 拆成 `load_raw(data_dir) -> Result<SidecarDisk, LoadError>` 和 `validate(disk, identity, volume_count) -> Result<(), LoadError>`。
   ```rust
   pub fn load(data_dir: &Path, index: &IndexState) -> Result<Self, LoadError> {
       let (identity, volume_count) = compute_identity(index);
       Self::load_with_identity(data_dir, identity, volume_count)
   }

   pub fn load_with_identity(data_dir: &Path, identity: u64, volume_count: u16) -> Result<Self, LoadError> {
       let disk = Self::load_raw(data_dir)?;
       validate_disk_with(&disk, identity, volume_count)?;
       Ok(Self { disk, delta: BTreeMap::new() })
   }

   fn load_raw(data_dir: &Path) -> Result<SidecarDisk, LoadError> {
       // mmap + postcard 反序列化，不需要 &IndexState
   }
   ```

2. `indexer_runtime.rs`：`load_pinyin` 改为锁外加载：
   ```rust
   fn load_pinyin(&self, index: &IndexState) {
       if !self.pinyin_enabled.load(Ordering::Acquire) {
           self.release_pinyin();
           return;
       }
       if self.pinyin.read().is_ok_and(|sidecar| sidecar.is_some()) {
           self.set_pinyin_status(PinyinStatus::Ready);
           return;
       }
       let data_dir = self.pinyin_data_dir.read()
           .ok().and_then(|v| v.clone());
       let Some(data_dir) = data_dir else {
           self.set_pinyin_status(PinyinStatus::Missing);
           return;
       };
       // 锁外加载 + validate（identity 在调用前已计算传入）
       let (identity, volume_count) = compute_identity(index);
       match PinyinSidecar::load_with_identity(&data_dir, identity, volume_count) {
           Ok(sidecar) => { /* 安装（原逻辑不变）*/ }
           Err(error) => { /* 原逻辑不变 */ }
       }
   }
   ```

   但 `load_pinyin` 当前被 `search()` 在读锁内调用（通过 `ensure_pinyin_loaded`）。需要改 `search()`：

   ```rust
   fn search(&self, ...) -> ... {
       let guard = self.index.read()...;
       let state = guard.as_ref()...;
       if self.pinyin_enabled.load(Ordering::Acquire) {
           // 不在读锁内加载——改为检查是否已加载
           if self.pinyin.read().is_ok_and(|s| s.is_some()) {
               // 已加载，正常使用
           } else {
               // 未加载——标记 needs_rebuild，本搜索走字面匹配
               self.pinyin_needs_rebuild.store(true, Ordering::Release);
           }
       }
       // ... 搜索逻辑不变 ...
   }
   ```

   然后 maintenance tick 或 SetPinyinEnabled 在锁外触发 `load_pinyin_outside_lock`。

   **但**：这样第一次搜索就没有拼音结果了。当前行为是第一次搜索时同步加载拼音。

   **修正**：`search()` 在读锁外做拼音加载。改 `search()` 为两步：
   1. 读锁内：检查 pinyin 是否已加载。如果已加载，执行搜索。如果未加载且 pinyin_enabled，标记 needs_rebuild。
   2. 读锁外：如果刚才标记了 needs_rebuild，调用 `load_pinyin_outside_lock`（锁外 mmap + 反序列化 + validate + 安装），然后重取读锁重做搜索。

   但"重做搜索"有性能问题——两次搜索。

   **更实际**：接受第一次搜索缺失拼音结果。maintenance tick 5 秒内会加载拼音。或者：在 `search()` 进入读锁前，先检查 pinyin 是否需要加载，如果需要就在锁外加载（用 clone 或 identity hash），然后进读锁做搜索。

   **最终方案**：
   ```rust
   fn search(&self, query, max, filters, root) -> Result<IndexerResponse, String> {
       // ... validate, root resolve ...

       // 如果 pinyin_enabled 但未加载，先在锁外加载
       if self.pinyin_enabled.load(Ordering::Acquire)
           && self.pinyin.read().is_ok_and(|s| s.is_none())
       {
           self.load_pinyin_outside_lock();
       }

       let guard = self.index.read()...;
       let state = guard.as_ref()...;
       // ... 搜索逻辑不变（pinyin 已加载或确定不可用）...
   }
   ```

   `load_pinyin_outside_lock`：
   ```rust
   fn load_pinyin_outside_lock(&self) {
       if !self.pinyin_enabled.load(Ordering::Acquire) { self.release_pinyin(); return; }
       if self.pinyin.read().is_ok_and(|s| s.is_some()) { self.set_pinyin_status(PinyinStatus::Ready); return; }
       let data_dir = self.pinyin_data_dir.read().ok().and_then(|v| v.clone());
       let Some(data_dir) = data_dir else { self.set_pinyin_status(PinyinStatus::Missing); return; };
       // 读锁内只取 identity + generation
       let (identity, volume_count, generation) = {
           let guard = match self.index.read() { Ok(g) => g, Err(_) => return };
           let Some(index) = guard.as_ref() else { return };
           (pinyin_sidecar::compute_identity(index), index.volumes.len() as u16, index.generation)
       };
       // 锁外做 mmap + 反序列化 + validate
       match PinyinSidecar::load_with_identity(&data_dir, identity, volume_count) {
           Ok(sidecar) => {
               if !self.pinyin_enabled.load(Ordering::Acquire) { self.release_pinyin(); return; }
               // 世代比对：generation 未变才安装
               let current_gen = self.generation();
               if current_gen == generation {
                   if let Ok(mut current) = self.pinyin.write() { *current = Some(sidecar); }
                   self.set_pinyin_status(PinyinStatus::Ready);
               } else {
                   // 世代变了——sidecar 可能不匹配，标记重试
                   self.pinyin_needs_rebuild.store(true, Ordering::Release);
               }
           }
           Err(error) => { /* 原逻辑不变 */ }
       }
   }
   ```

   **注意**：`index_identity` 在读锁内计算——它是 O(n) 遍历 names，但只做 hash（不 clone）。这与当前行为一致（当前也在读锁内做 `validate_disk` 调 `index_identity`）。区别是：当前在读锁内做 mmap + 反序列化 + identity hash；改后只做 identity hash，mmap + 反序列化移到锁外。

#### H2b：compaction 移出写锁

`compact_names_if_needed` 在 `watch_volume` 的写锁内对所有卷调用（:1254-1258）。

**当前代码**：
```rust
if changed > 0 && events_before / 10_000 != index.events_since_checkpoint / 10_000 {
    for volume in &mut index.volumes {
        if volume.compact_names_if_needed()? { ... }
    }
}
```

**方案：读锁克隆 → 锁外压缩 → 写锁 swap**

但 compaction 修改的是 `volume.names` 和 `volume.nodes[].name_off`——这不是简单的 swap，它修改了 nodes 的 offset。需要在写锁内完成 offset 更新。

**更实际的方案**：compaction 改为"读锁内计算 live_bytes 判断是否需要压缩 → 锁外构建新的 names pool + 新的 offset 映射 → 写锁内 swap names + 更新 offsets"。

但"更新 offsets"需要遍历所有 nodes——这仍然在写锁内。不过这个遍历是 O(n) 纯内存操作（只改 name_off），比当前的 compact_names_if_needed（O(n) 读名字 + O(n) 重建 pool）快。

**最终方案**：把 `compact_names_if_needed` 拆成两步：
1. `compact_names_plan(&self) -> Option<CompactPlan>`（读锁内，O(n) 判断是否需要 + 计算 live_bytes）
2. `apply_compact(&mut self, plan: CompactPlan)`（写锁内，O(n) 重建 pool + 更新 offsets）

但当前 `compact_names_if_needed` 已经是一步到位的——拆分不会减少总工作量。真正的问题是它在写锁内做。

**更优方案**：把 compaction 移到 `checkpoint` 路径——checkpoint 已经 clone 了 index（读锁内 memcpy），然后在锁外做 save（含 validate）。在 clone 上做 compaction 不影响 live index。但 compaction 修改的是 live index 的 names pool——clone 上做 compaction 不会反映到 live index。

**真正的最终方案**：接受 compaction 在写锁内完成，但优化它：
1. 只对**当前卷**做 compaction，而不是所有卷（当前是对所有卷）。
2. 用死字节计数器（dead_name_bytes: AtomicUsize）消除预扫——每次 delete/upsert 累计死字节，达到阈值才触发 compaction。compaction 本身仍 O(n)，但不需要每次 USN batch 都做预扫判断。

**方案 B（推荐）**：
1. 在 VolumeIndex 加 `dead_name_bytes: usize` 字段。
2. `delete` 时 `dead_name_bytes += name.len() + 1`（如果 name_off != NO_NAME）。
3. `upsert` 覆盖旧名字时 `dead_name_bytes += old_name.len() + 1`。
4. `compact_names_if_needed` 用 `dead_name_bytes` 替代全量预扫：
   ```rust
   pub fn compact_names_if_needed(&mut self) -> Result<bool, String> {
       let threshold = (8 * 1024 * 1024usize).max(self.initial_name_bytes / 4);
       if self.dead_name_bytes <= threshold {
           return Ok(false);
       }
       // 重建 names pool（O(n)，但只在 dead bytes 超阈值时触发）
       let mut replacement = Vec::with_capacity(self.names.len() - self.dead_name_bytes);
       for slot in &mut self.nodes { ... }  // 与当前逻辑一致
       self.names = replacement;
       self.dead_name_bytes = 0;
       Ok(true)
   }
   ```
5. `watch_volume` 改为只对当前卷做 compaction：
   ```rust
   // 改前：for volume in &mut index.volumes { volume.compact_names_if_needed()? }
   // 改后：只对当前 volume 做
   let volume = &mut index.volumes[volume_number];
   if volume.compact_names_if_needed()? { ... }
   ```

   **但**：当前对所有卷做 compaction 的原因是——USN 事件可能影响任何卷（不太可能，每个 watcher 只读自己卷的 USN）。实际上 watcher 是 per-volume 的，只 apply 自己卷的 records。所以只对自己卷做 compaction 是正确的。

**关键改动**：
1. VolumeIndex 加 `dead_name_bytes` 字段（serde skip，运行时累积）。
2. `delete` 和 `upsert`（覆盖时）累计 `dead_name_bytes`。
3. `compact_names_if_needed` 用 `dead_name_bytes` 替代全量预扫。
4. `watch_volume` 只对当前卷做 compaction，不对所有卷。
5. `finish_initial_build` 重置 `dead_name_bytes = 0`。

**注意**：`dead_name_bytes` 不参与序列化（serde skip），从磁盘加载时为 0。首次 compaction 前会低估死字节——但 `compact_names_if_needed` 仍会在 `names.len() > initial_name_bytes + threshold` 时触发（如果 `dead_name_bytes == 0` 但实际有死字节，需要兜底）。

**兜底**：`compact_names_if_needed` 在 `dead_name_bytes <= threshold` 时，额外检查 `self.names.len() > self.initial_name_bytes + threshold * 2`——如果 names pool 远超初始大小，强制做一次全量预扫。这保证从磁盘加载后不会漏判。

### 风险与对策
- **dead_name_bytes 精度**：upsert 覆盖旧名字时需要知道旧名字的长度——`name_at(old.name_off)` 可以获取，但 delete 时 name_off 已被设为 NO_NAME。需要在设 NO_NAME 前记录死字节。
  - `delete`：`slot.name_off` 在清除 FLAG_PRESENT 后设为 NO_NAME。需要在设 NO_NAME 前读旧 offset → `name_at(offset)` → 累加 len+1。
  - `upsert`：`old` 是旧 slot 的拷贝。如果 `old.flags & FLAG_PRESENT != 0 && old.name_off != NO_NAME`，需要读旧名字长度累加。
- **compaction 只对当前卷**：确保不会遗漏其他卷的 compaction。每个 watcher 只处理自己卷的 USN 事件，只自己的 delete/upsert 产生死字节——其他卷的死字节由各自的 watcher 处理。正确。
- **拼音 sidecar 加载后世代变了**：如果 USN watcher 在 sidecar 加载期间更新了 index（generation 变了），sidecar 可能与 index 不匹配。世代比对检测到后会标记 needs_rebuild，下次 maintenance 重试。这期间搜索走字面匹配——可接受降级。
- **`load_pinyin_outside_lock` 的 generation 比对**：`generation()` 需要读锁——但它是在 `load_pinyin_outside_lock` 的最后调用的，此时没有持有任何锁。安全。但 generation 可能在比对后、安装前再次变化——`apply_pinyin_records` 在写锁内做 delta 更新，如果安装后 USN 事件到来，delta 会增量更新 sidecar。所以即使 generation 变了，sidecar 仍可通过 delta 机制保持同步。安全。

### 测试
- 已有的 pinyin 测试保持不变（`load_pinyin` 签名不变，内部改为锁外加载）。
- 新增测试：`load_pinyin_outside_lock` 在 index generation 变化时丢弃 sidecar。
- 新增测试：`dead_name_bytes` 在 delete/upsert 后正确累加。
- 新增测试：`compact_names_if_needed` 用 `dead_name_bytes` 判断是否需要压缩。
- 已有的 compaction 测试保持不变。

---

## 实施顺序

1. **H2b（compaction 死字节计数器 + 只对当前卷）** — 独立改动，不影响 H1/H2a → 验证测试
2. **H2a（拼音加载移出读锁）** — 修改 pinyin_sidecar.rs + indexer_runtime.rs → 验证测试
3. **H1（run() select 竞争 stop + wait_hint 拉长）** — 修改 indexer_runtime.rs + prism-indexer-service.rs → 验证测试
4. **质量门 + 安装包**

## 质量门
- `cargo fmt && cargo clippy && cargo test`（src/prism-core）
- `dotnet build && dotnet test`（src/Prism、src/Prism.Tests）
- ISCC 重建安装包（批次7是里程碑批次）

## 影响面分析

### 不受影响的功能
- **搜索逻辑**：search() 的匹配/排序/Top-K 逻辑不变。拼音改为锁外加载只影响"首次搜索是否有拼音结果"——字面匹配不受影响。
- **首建流程**：acquire_initial_index 不变。首建已有 stop 检查（build_volume_reporting 带 stop）。
- **watcher 流程**：watch_volume 的 USN apply 逻辑不变，只改 compaction 的触发方式和范围。
- **IPC 协议**：无变更。
- **前端**：无变更。
- **history**：无变更（批次6已处理）。

### 需要注意的交互
- **pinyin_needs_rebuild 的触发时机变化**：当前是 search() 内 `ensure_pinyin_loaded` 失败时设置。改后是 `load_pinyin_outside_lock` 失败时设置。maintenance tick 5 秒内重试——与当前行为一致。
- **compaction 触发条件变化**：从"每 10000 events 全卷预扫"改为"dead_name_bytes 超阈值时只对当前卷压缩"。阈值不变。
- **run() 的 select 分支语义变化**：rebuild 和 maintenance 分支内增加了 stop 竞争。stop 到达时立即 break，不等长任务完成。
