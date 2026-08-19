# P1 实施方案（AUDIT-2026-08-18 批次2+批次3）

> 基准：P0 已落地（commit 811a69c）。分支 `feature`。
> 审计文档：`docs/PRISM-AUDIT-HANDOFF-2026-08-18.md`（自包含，含行号、修法、验证）。
> 硬性约束见审计文档第1节：不引入新依赖、协议兼容、不重发、每批独立提交、附测试、不改 UI 视觉/动画。

---

## 0. P0 已落地确认

| 项 | 状态 | 代码位置 |
|---|---|---|
| R-A1 协议版本 | ✅ | `IndexerGenerationClient.cs` ProtocolVersion=2 |
| R-B1 拼音重建移出读锁 | ✅ | `indexer_runtime.rs:326-346` clone 快照→放锁→rebuild |
| C-D1 全局异常兜底 | ✅ | `App.xaml.cs:58-62` 三个钩子 + LogToFile 日志基建 |

---

## 1. 同类软件参考

### Everything (voidtools)
- **MFT 枚举**：直接读 MFT 遍历所有记录，文件名按 UTF-16 读出后直接使用——NTFS 文件名本就允许非法代理对（文件系统不校验 Unicode 合法性），Everything 不因单条非法名丢弃整批。
- **USN watcher**：每卷独立监控，单卷 USN 错误只重建该卷（Everything 的 "NTFS Volume" 重新索引是按卷的），不会全盘重扫。
- **服务连接**：Everything 服务管道默认无显式并发上限，但单用户场景实际并发极低（搜索+少量状态请求）。
- **低级钩子**：Everything 自身用热键注册（RegisterHotKey），不用 WH_KEYBOARD_LL。但同类 tray 工具（如 AutoHotkey、PowerToys）普遍采用"周期性重装钩子"策略应对系统摘钩。

### Listary
- **文件名处理**：Listary 经 Shell API（SHGetFileInfo）取文件名，Shell 层已做 UTF-16 lossy 转换，不会遇到整批失败。
- **索引更新**：Listary 用文件系统监控（ReadDirectoryChangesW），单目录出错只影响该目录监控，不触发全量。
- **全局异常**：Listary 常驻 tray，有崩溃日志（ListaryCrash.txt），说明注册了未捕获异常钩子写日志。
- **内存管理**：Listary 不调用 EmptyWorkingSet——窗口隐藏后靠 GC 自然回收 + 延迟释放。

### Windows 最佳实践（通用）
- **WH_KEYBOARD_LL 摘钩恢复**：系统在回调超时（LowLevelHooksTimeout，默认 ~300ms）后静默移除钩子。唯一可靠检测方式是"周期性 UnhookEx + SetWindowsHookEx 重装"（幂等，成本微小）。GetAsyncKeyState 无法检测钩子存活。
- **EmptyWorkingSet**：只逐出工作集（RAM→pagefile），不降私有提交。下次呼出触发软缺页变慢。对反复 show/hide 的 tray 工具弊大于利。正确做法：一次真正的 `GC.Collect(2, Forced, blocking:true, compacting:true)` + 延迟执行。
- **命名管道并发**：单用户 launcher 建议上限 32-64 连接，超出直接拒绝。

---

## 2. 批次2（P1 稳定性，6项）

执行顺序：R-B4 → R-A5 → C-D2 → C-D3 → R-B3 → R-B2。
R-B4 必须先于 R-B2（同链路：非法文件名→watcher 死→rebuild 循环）。

### 2.1 R-B4：非法 UTF-16 文件名单条降级

**位置**：`src/prism-core/src/ntfs.rs:108-113`

**现状**：
```rust
let name = String::from_utf16(&utf16).map_err(|error| format!("invalid UTF-16: {error}"))?;
```
`from_utf16` 失败 → `parse_usn_buffer` 整批 Err → watcher 死 → R-B2 放大为全盘重扫。

**修法**：改 `from_utf16_lossy`，单条降级。NTFS 文件名允许非法代理对（文件系统不校验 Unicode），lossy 是正确语义。

```rust
let name = String::from_utf16_lossy(&utf16);
```

**影响面**：`apply_records` 及下游消费者（`hierarchy.rs` upsert、`pinyin.rs` 建索引）已按 `&str` 处理，lossy 结果是合法 `&str`，无需改动消费者。MFT 枚举路径（`enumerate_mft`）同样调用 `from_utf16`，需同步改。

**验证**：新增单测——构造含孤立代理对（`0xD800`）的 USN 记录，断言 `parse_usn_buffer` 返回 Ok，该条目 name 为 lossy 结果（含 U+FFFD），其余条目不受影响。`enumerate_mft` 同理补一个。

### 2.2 R-A5：首连失败则 watchdog 永不 arm

**位置**：`src/Prism/Services/PipeClient.cs:74-80`（`StartAsync`）；`src/Prism/App.xaml.cs:317-351`（`TryStartBackendAsync`）

**现状**：`StartAsync` 成功才调 `StartWatchdog()`；首连失败抛异常被 `TryStartBackendAsync` catch，watchdog 永不启动 → 只能重启 Prism。

**修法**：`StartAsync` 改为先启动 watchdog 再连接，首连失败不阻止 watchdog：

```csharp
public async Task StartAsync(CancellationToken ct = default)
{
    StartWatchdog(); // 先 arm，首连成败都不影响
    await ConnectOrReconnectAsync(ct).ConfigureAwait(false);
    if (!_query.IsConnected)
        throw new IOException("无法连接到后端");
}
```

**影响面**：watchdog tick 里的 `ConnectOrReconnectAsync` 已有 `EnsureBackendRunning` 会拉进程，所以首连失败后 watchdog 15s 内自动重试。watchdog 的 `NotifyConnection(false)` 已处理断连通知。无新增竞态——watchdog timer 回调本身有 `_ioLock` 串行化。

**验证**：新增单测——注入 mock process-starter 使首连失败，断言 watchdog 已启动且 15s 后（测试中缩短间隔）重试连接成功（mock 恢复后）。手动：改 broker exe 名使首连失败，15s 内 watchdog 拉起恢复。

### 2.3 C-D2：WebIconProvider 跨线程无锁读写

**位置**：`src/Prism/Services/WebIconProvider.cs:53`（`Invalidate` 后台线程清 `_resolved`）；`src/Prism/Controls/ResultList.xaml.cs:383`（`GetIcon` UI 线程读 `_resolved`）；调用链 `App.xaml.cs:223-237`（`Task.Run` finally → `Invalidate`）。

**现状**：`DownloadFavicon` 在 `Task.Run` 后台线程的 finally 块调 `_webIcons?.Invalidate()` → `_resolved.Clear()`，同时 UI 线程 `ResultList.DecorateVisibleItems` 遍历/读写 `_resolved` —— `Dictionary` 非线程安全，UB。

**修法**：`Invalidate` 经 `Dispatcher.BeginInvoke` 投递到 UI 线程，保持字典单线程语义（首选，零锁）：

```csharp
public void Invalidate()
{
    var dispatcher = Application.Current?.Dispatcher;
    if (dispatcher is null || dispatcher.CheckAccess())
        _resolved.Clear();
    else
        dispatcher.BeginInvoke(new Action(() => _resolved.Clear()));
}
```

**影响面**：`Invalidate` 唯一调用点是 `App.xaml.cs:235`（后台线程）。`GetIcon` 只在 UI 线程调（`ResultList` 装饰是 UI 操作）。改后全部 `_resolved` 触点统一在 UI 线程，消除竞态。

**验证**：`WebIconFlickerTests` 全绿；代码评审确认 `_resolved` 全部触点在 UI 线程。无需新增并发测试——修复后不存在并发。

### 2.4 C-D3：低级键盘钩子摘除后无恢复

**位置**：`src/Prism/Services/HotkeyService.cs:75-88`（`InstallLowLevelHook`）

**现状**：WH_KEYBOARD_LL 回调超时（系统默认 ~300ms）被 Windows 静默 unhook，无检测、无恢复。DoubleCtrl 是默认触发方式，用户无感知钩子已死。

**修法**：加周期性重装。两个时机：
1. 每次窗口呼出/隐藏时重装（幂等，用户操作时自然触发）。
2. 定时器 60s 兜底（窗口长时间不操作时）。

具体：在 `HotkeyService` 内加 `Timer`（60s），tick 时若当前模式是 DoubleCtrl 则 `Uninstall` + `InstallLowLevelHook`。窗口 show/hide 时也调一次重装（通过新增 public `RefreshHook()` 方法，`SearchWindow` show/hide 调用）。重装失败记日志。

```csharp
// 新增
private System.Threading.Timer? _hookRefreshTimer;

private void StartHookRefresh()
{
    _hookRefreshTimer?.Dispose();
    _hookRefreshTimer = new System.Threading.Timer(_ => RefreshHook(), null,
        TimeSpan.FromSeconds(60), TimeSpan.FromSeconds(60));
}

internal void RefreshHook()
{
    if (_mode != HotkeyMode.DoubleCtrl) return;
    // 幂等重装：先卸再装。Uninstall 只清 hook 不清 mode。
    if (_hook != IntPtr.Zero) { UnhookWindowsHookEx(_hook); _hook = IntPtr.Zero; }
    InstallLowLevelHook();
}
```

Dispose 时 dispose timer。Apply 切换模式时停旧 timer、按需启新。

**影响面**：`HotkeyService` 已有 `Uninstall`/`InstallLowLevelHook`，重装复用。`SearchWindow` show/hide 调 `RefreshHook`——在 `ShowAndFocus` 和 `HideAnimated` 末尾各加一行 `_app.HotkeyRefresh()`（或通过已有引用）。需确认 SearchWindow 能访问 HotkeyService 实例。

**验证**：`dotnet test` 全绿；手动：调试器挂起进程 10s（诱发系统摘钩）恢复后 ≤60s 内双击 Ctrl 恢复响应。

### 2.5 R-B3：一次 create_pipe 失败 = 服务退出

**位置**：`src/prism-core/src/indexer_runtime.rs:1419-1431`（`accept_loop`）

**现状**：
```rust
async fn accept_loop(state: Arc<ServiceState>, mut server: NamedPipeServer) -> Result<(), String> {
    loop {
        server.connect().await.map_err(|error| error.to_string())?;
        let connected = server;
        server = create_pipe(false).map_err(|error| error.to_string())?; // ← 一次失败 = Err
        // ...
    }
}
```
`create_pipe` 失败 → `accept_loop` 返 Err → `serve` abort 全部 listener → `run` 致命退出。瞬时失败（资源不足、ACL 临时不可用）杀整个服务。

**修法**：re-arm 失败退避重试，不立即返回 Err：

```rust
async fn accept_loop(state: Arc<ServiceState>, mut server: NamedPipeServer) -> Result<(), String> {
    let mut backoff = Duration::from_millis(100);
    let first_failure = Instant::now();
    loop {
        server.connect().await.map_err(|error| error.to_string())?;
        let connected = server;
        loop {
            match create_pipe(false) {
                Ok(new_server) => { server = new_server; break; }
                Err(error) => {
                    if first_failure.elapsed() > Duration::from_secs(60) {
                        return Err(format!("create_pipe failed for >60s: {error}"));
                    }
                    log(format!("create_pipe retry (backoff {:?}): {error}", backoff));
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                }
            }
        }
        backoff = Duration::from_millis(100); // 成功后重置
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(connected, state).await {
                log(format!("indexer IPC client disconnected: {error}"));
            }
        });
    }
}
```

**影响面**：`accept_loop` 返回类型不变（`Result<(), String>`），调用方 `serve` 无需改。退避只影响 re-arm，不影响已连接客户端。`connect().await` 失败仍立即返回（客户端连接失败是另一回事，不影响 listener 生存）。

**验证**：新增单测——注入 mock `create_pipe` 前 3 次失败，第 4 次成功，断言 `accept_loop` 存活且第 4 次后恢复服务。60s 持续失败断言返回 Err。

### 2.6 R-B2：单卷 watcher 出错 → 全盘重扫

**位置**：
- `indexer_runtime.rs:1249`（`rebuild_tx.send(format!("{}: {error}", descriptor.mount_path))`）
- `indexer_runtime.rs:740-787`（`rebuild_rx.recv()` → `build_all()`）
- `indexer_runtime.rs:434-448`（`merge_and_publish` 已支持按 volume_id 替换）

**现状**：watcher 报错 → `rebuild_tx` 发字符串 reason → `run` loop 收到 → `tokio::task::spawn_blocking(build_all)` 全量重扫所有卷。单卷瞬时故障杀所有健康卷 watcher。

**修法**：`rebuild_tx` 消息从 `String` 改为带卷标识的枚举：

```rust
#[derive(Clone)]
enum RebuildRequest {
    /// 单卷重建：只重建该卷，merge_and_publish 替换。
    SingleVolume { volume_id: u64, mount_path: String, reason: String },
    /// 全量重建（卷集合变化等）。
    Full(String),
}
```

`start_watcher`（1248）发送 `RebuildRequest::SingleVolume`；`run` loop 收到后按类型分发：
- `SingleVolume` → 只 `build_volume` 该卷 → `merge_and_publish`
- `Full` → `build_all`（现有逻辑）

单卷重建的 epoch 语义：单卷重建只使该卷 watcher 换代。当前 `epoch` 是全局的（`epoch.fetch_add(1)`），需要改为：单卷重建不动 epoch（该卷 watcher 自己是 `spawn_blocking` 出错退出的，不需要 epoch 杀它——它已经死了）；只有 `Full` 才 epoch+1 杀所有 watcher 重新 start_watchers。

**关键**：单卷 watcher 出错后，该 watcher task 已退出（`watch_volume` 返 Err → task 结束）。重建完成后需重新 `start_watcher` 该卷。`build_all` 路径在 782 行调 `start_watchers` 重启所有卷 watcher；单卷路径需调 `start_watcher` 重启该卷。

**影响面**：
- `rebuild_tx`/`rebuild_rx` 类型从 `mpsc::unbounded_channel::<String>` 改为 `mpsc::unbounded_channel::<RebuildRequest>`。
- `start_watcher` 发送端改 `RebuildRequest::SingleVolume`。
- `run` loop 的 `rebuild_rx.recv()` 分支按类型分发。
- `acquire_initial_index`（868）也持有 `rebuild_tx`——检查其发送点是否需改（该函数在首建期间发现缓存不可用时发 Full）。
- `merge_and_publish` 不变（已支持按 volume_id）。

**验证**：新增集成测试——两卷 mock，卷 B watcher 报错后只重建卷 B；卷 A 的索引对象 generation 不变（指针未被替换）。`cargo test` 全绿。

---

## 3. 批次3（P1 内存，3项）

执行顺序：C-D4 → R-C1 → R-C2。每项跑 bench 前后对比。

### 3.1 C-D4：EmptyWorkingSet 无效化妆 + Optimized GC 不压缩

**位置**：
- `src/Prism/App.xaml.cs:349`（`TryStartBackendAsync` finally → `TrimWorkingSet`）
- `src/Prism/App.xaml.cs:354-363`（`TrimWorkingSet` → `EmptyWorkingSet`）
- `src/Prism/Windows/SearchWindow.xaml.cs:348-362`（`ReleaseIdleMemory` → 延迟3分钟 → `GC.Collect(Optimized, blocking:false, compacting:true)` + `App.TrimWorkingSet()`）
- `src/Prism/Windows/SearchWindow.xaml.cs:167`（`IdleMemoryReleaseRequested` → `GC.Collect(Optimized, blocking:false, compacting:true)`）

**现状**：
- `EmptyWorkingSet` 只逐出工作集不降私有提交，下次呼出软缺页变慢。
- `GCCollectionMode.Optimized` 允许 CLR 判定"不值得"而跳过；`blocking:false` 不阻塞但也不压缩堆。

**修法**：
1. 删除 `EmptyWorkingSet` P/Invoke 声明 + `TrimWorkingSet` 方法 + 所有调用点。
2. `ReleaseIdleMemory` 延迟3分钟 timer 改为 `GC.Collect(2, GCCollectionMode.Forced, blocking: true, compacting: true)`（窗口已隐藏不卡交互），去掉 `App.TrimWorkingSet()`。
3. `IdleMemoryReleaseRequested`（SearchWindow:167）同样改为 `Forced, blocking: true, compacting: true`。
4. `TryStartBackendAsync` finally 块去掉 `TrimWorkingSet` 调用（后端拉起后工作集涨是正常的，不需要逐出）。

**影响面**：纯删除 + 参数改。无新增依赖。`psapi.dll` import 可删。`TrimWorkingSet` 是 `internal static`，检查是否有外部调用（grep 确认只有 App.xaml.cs 内部）。

**验证**：`scripts/g5-memory-soak.ps1` 前后对比：呼出→搜索→隐藏 50 轮，比较私有提交曲线（不是工作集）与二次呼出首帧耗时。预期：私有提交持平或降、二次呼出不再有软缺页尖峰。

### 3.2 R-C1：首建峰值 ≈ 常驻 6-10 倍（name 池化）

**位置**：`src/prism-core/src/ntfs.rs:524-569`（`enumerate_mft`）；`UsnRecord` 结构体定义。

**现状**：`enumerate_mft` 全量物化 `Vec<UsnRecord>`，每条 ~100-130B 含堆 `String`（name）。300万记录 ≈ 350-400MB 峰值。排序后 64 轮 upsert 到 `VolumeIndex`。

**修法**（保守方案，不动整体流程）：
- `UsnRecord.name: String` 改为 `name: NameRef`，其中 `NameRef { offset: u32, len: u32 }` 指向一个单一大 `Vec<u8>` 池（UTF-8 编码后的文件名字节）。
- 枚举时把名字 `from_utf16_lossy` → UTF-8 字节追加进池，记录 `(offset, len)`。
- `UsnRecord` 变纯 POD（无堆指针），每条从 ~100-130B 降到 ~64B。
- 排序按 key 不动数据。
- 消费时通过 `name_ref.as_str(&pool)` 取 `&str`。

**关键改动**：
- `ntfs.rs`：`UsnRecord` 结构体、`parse_usn_buffer`、`enumerate_mft` 签名（返回 `(Vec<UsnRecord>, Vec<u8> /* name_pool */, next_usn)` 或把 pool 附在返回结构里）。
- `indexer_runtime.rs`：`build_volume_with_progress` 调用 `enumerate_mft` 后需持有 pool 引用传给 upsert。
- `hierarchy.rs`：`VolumeIndex::upsert` 接收 `&str` 不变——pool 提供 `&str` 切片。
- `apply_records`（USN 增量路径）：`UsnRecord` 也要改，但增量路径量小（每批几十~几百条），可以用独立的小 pool 或直接保留 `String`（增量路径不是峰值瓶颈）。

**风险**：这是本批改动量最大项。`UsnRecord` 被多处引用（`parse_usn_buffer`、`apply_records`、`apply_replay_records`、`watch_volume`）。两种策略：
- **策略A（统一）**：所有路径都用 pool。增量路径每批自建小 pool。
- **策略B（双结构）**：枚举路径用 `UsnRecordPooled`，增量路径保留 `UsnRecord`（含 String）。两个结构体，消费端用 trait 或泛型统一 `name() -> &str`。

策略B 改动面更大但隔离性好；策略A 更简洁但增量路径需包装。推荐策略A——`UsnRecord` 统一持 `NameRef`，`parse_usn_buffer` 返回 `(Vec<UsnRecord>, Vec<u8>)`，增量路径 `apply_records` 也返回 pool，pool 在函数结束时释放（增量量小）。

**验证**：`cargo test`（ntfs/hierarchy 全绿）；`tools/bench/Measure-ProcessMemory.ps1` 对比首建峰值，预期降 ≥40%；`tools/bench/Invoke-G9FirstBuildAcceptance.ps1` 通过。

### 3.3 R-C2：checkpoint clone 2× 峰值 + validate O(n·depth)

**位置**：
- `indexer_runtime.rs:1343-1346`（读锁内 clone 整个 IndexState）
- `index_cache.rs:51`（save → validate 对每节点跑 `path_for`，`hierarchy.rs:526-563`）
- 停机路径（`run` 尾部 ~833）

**现状**：checkpoint 在读锁内 `index.clone()` 整个 IndexState（2× 峰值），save 时 validate 全量遍历每节点 `path_for`（O(n·depth)），停机路径走全套可能超 SCM 30s wait_hint。

**修法**：
1. save 前 validate 降为抽样：随机 1% 节点 + 全部根节点 + 结构不变量（names 池边界、slot 计数一致性、volume 数量）。
2. load 侧保留全量校验（防缓存文件损坏）。
3. 停机路径：R-B1 修后已不持锁做拼音重建；进一步考虑把 `wait_hint` 用 checkpoint 实测时长动态上报（prism-indexer-service.rs:52）。

**关键改动**：
- `index_cache.rs`：`save` 的 validate 调用从全量改抽样。新增 `validate_sampled(index)` 函数。
- `hierarchy.rs`：`path_for` 不变（load 侧仍用），save 侧不调全量。
- `indexer_runtime.rs`：checkpoint 路径 clone 本身保留（R-B1 已放锁，clone 在读锁内是毫秒级 memcpy——但 2× 峰值仍在）。clone 消除是 P4，本批只降 validate 开销。

**影响面**：`index_cache.rs` validate 逻辑改抽样。`hierarchy.rs` 不动。停机路径改善靠 R-B1（已完成）。

**验证**：`cargo test`；构造故意损坏的缓存文件确认 load 全量校验仍拒收；大索引下 SCM Stop 在 30s 内完成（`scripts/indexer-service-install-test.ps1`）。

---

## 4. 提交策略

每个问题独立提交，提交信息中文并标注编号：
```
fix: R-B4 非法 UTF-16 文件名单条降级（AUDIT-2026-08-18 R-B4）
fix: R-A5 首连失败也启动 watchdog（AUDIT-2026-08-18 R-A5）
fix: C-D2 WebIconProvider Invalidate 回 UI 线程（AUDIT-2026-08-18 C-D2）
fix: C-D3 低级键盘钩子周期重装防摘除（AUDIT-2026-08-18 C-D3）
fix: R-B3 accept_loop re-arm 退避重试（AUDIT-2026-08-18 R-B3）
fix: R-B2 单卷 watcher 出错只重建该卷（AUDIT-2026-08-18 R-B2）
fix: C-D4 删 EmptyWorkingSet + GC 改 Forced blocking（AUDIT-2026-08-18 C-D4）
perf: R-C1 UsnRecord name 池化降首建峰值（AUDIT-2026-08-18 R-C1）
perf: R-C2 checkpoint validate 降为抽样（AUDIT-2026-08-18 R-C2）
```

批次2、批次3各完成后跑全局验证门槛（审计文档第4节）。

---

## 5. 风险评估

| 项 | 风险 | 缓解 |
|---|---|---|
| R-B4 | lossy 改变文件名语义，搜索匹配可能变化 | lossy 只替换非法代理对为 U+FFFD，合法文件名不受影响；原代码遇到非法名直接整批失败，lossy 是严格更好 |
| R-A5 | watchdog 提前启动可能在 broker 未就绪时空转 | watchdog tick 已有 `_consecutiveFailures < 2` 防抖，空转无害 |
| C-D2 | BeginInvoke 延迟清空可能导致短暂使用旧图标 | 图标下载完成到 UI 刷新本就是异步的，延迟一个 Dispatcher cycle 无感知 |
| C-D3 | 重装钩子时可能丢失正在进行的双击序列 | Unhook+SetWindowsHookEx 是原子级操作，重装在 60s 闲时或窗口 show/hide 时，用户大概率不在双击中途 |
| R-B3 | 退避期间客户端连不上 | 退避只影响 re-arm（新管道实例），已连接客户端不受影响；connect() 成功后 re-arm 失败不阻塞已接受的连接 |
| R-B2 | 单卷重建的 epoch 语义与全量不一致 | 单卷不动 epoch（watcher 已死不需杀），重建后 start_watcher 该卷用当前 epoch；全量才 epoch+1 |
| C-D4 | 删 EmptyWorkingSet 后任务管理器显示内存变高 | 工作集数字不代表真实占用；私有提交才是。GC 压缩降的是私有提交。用户感知是下次呼出更快 |
| R-C1 | UsnRecord 结构改动面大，所有消费者受影响 | 策略A 统一，`name() -> &str` 接口隔离；增量路径量小可独立小 pool |
| R-C2 | 抽样 validate 可能漏过缓存损坏 | load 侧全量校验是安全网；save 侧抽样只为降停机开销，损坏概率极低（save 是自己写的，不是外部输入） |

---

## 6. 验证门槛（每批次必过）

```powershell
# Rust（在 src/prism-core/ 下）
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings

# C#（仓库根）
dotnet build src/Prism/Prism.csproj -c Release
dotnet test src/Prism.Tests/Prism.Tests.csproj

# 手动/脚本（涉及对应子系统时）
scripts/pipe-roundtrip-test.ps1
scripts/indexer-pipe-roundtrip-test.ps1
scripts/indexer-service-install-test.ps1     # 涉及服务/协议改动时
scripts/g5-memory-soak.ps1                   # 涉及内存改动时
tools/bench/Invoke-SearchBaseline.ps1         # 涉及搜索热路径时
tools/bench/Measure-ProcessMemory.ps1        # R-C1 前后峰值对比
```

---

## 7. 不做的事

- 不改 UI 视觉与动画（另一条审计线，批次8已处理）。
- 不改公共 API 形状（`UsnRecord` 结构变更属内部，不跨进程）。
- 不做 P4 架构演进（每卷独立锁、arc-swap、流式枚举、分卷增量落盘）。
- 不引入新依赖。
