# Prism 全仓审计交接方案（2026-08-18）

> 本文档面向**执行实施的模型/工程师**。它自包含：审计发现、文件与行号、修法、优先级、约束、验证方式、环境信息全部在此。**请忽略 docs/ 下更早的审计/路线图文档**（PRISM-FULL-AUDIT-2026-08-16.md 等）——本次审计是抛开旧文档从零通读代码得出的，若与旧文档冲突以本文为准。

---

## 0. 项目与环境信息

- **产品**：Prism——类 Listary 的 Windows 启动器/文件搜索工具。常驻托盘，热键呼出搜索窗。
- **架构**（三进程）：
  1. `Prism.exe` — WPF 前端，`net8.0-windows`，`LangVersion=latest`，代码在 `src/Prism/`。
  2. `prism-core.exe` — 用户态 broker（Rust，tokio），处理搜索/动作/网页关键词，经命名管道服务前端；代码在 `src/prism-core/src/`（`main.rs` 入口，核心在 `ipc.rs`）。
  3. `prism-indexer-service.exe` — SYSTEM 权限 SCM 服务（Rust），NTFS MFT/USN 索引；入口 `src/prism-core/src/prism-indexer-service.rs`，主体在 `indexer_runtime.rs`。
- **IPC**：前端↔broker、broker↔indexer 均为命名管道 + JSON-lines（每行一条 JSON）。broker 协议版本 1，indexer 协议版本 `INDEXER_PROTOCOL = 2`（`src/prism-core/src/lib.rs:42`）。
- **工具链**：
  - Rust：stable MSVC 工具链，edition 2021。`profile.release`: opt-level 3、LTO、`panic = "abort"`（注意：`--console` 调试模式和 `cargo test` 是 unwind，锁中毒在测试下可发生）。
  - C#：.NET 8 SDK，WPF + 少量 WinForms（托盘、FolderBrowserDialog）。
  - 构建脚本：`scripts/prism-build.ps1`；基准与验收脚本在 `tools/bench/`、`scripts/`。
- **环境陷阱**（来自项目记忆，实施时务必注意）：
  - 本机只有 `py` 没有 `python`。
  - PowerShell 按 GBK 读中文脚本会报假语法错——PS1 文件需 UTF-8 with BOM。
  - Git 自带的 `link.exe` 可能抢占 MSVC link——Rust 构建若报 link 错误，先 `source scripts/msvc-env.sh` 或检查 PATH 顺序。
  - 已装 indexer 服务的**二进制可能不是当前源码编译的**——排查协议问题时先确认服务 exe 的实际版本。
- **测试**：
  - Rust：`cargo test` 在 `src/prism-core/`（单测内嵌各模块 + `ipc.rs` 尾部集成测试）。
  - C#：`dotnet test src/Prism.Tests/Prism.Tests.csproj`（xUnit；部分带 `WindowActivatorLiveTests` 这类需要桌面会话的 live 测试）。
- **代码风格约束**：仓库注释是中文、"决策 why"式注释（解释约束与教训，不复述代码）；提交信息为中文、`type: 描述` 格式。新代码需保持同风格。

---

## 1. 硬性约束（对实施模型的要求）

1. **不引入新第三方依赖**。Rust 侧 `postcard`/`serde`/`tokio`/`windows-sys` 等已有依赖可用；如确需 `arc-swap` 之类，属长期批次（P4），需人工批准后另行处理。C# 侧不加 NuGet 包。
2. **协议兼容**：任何协议改动必须保证「新前端 + 旧 broker」「新 broker + 旧 indexer 服务」组合下**显式报错而非静默降级**。索引持久化缓存版本（v5）如需升版会触发全量重扫，需在提交信息中显式声明。
3. **不得改变管道协议纪律**：写出请求后绝不在同一连接重发（防动作重复执行，`PipeClient.cs:958-964` 注释有警告）；seq 丢弃晚到结果的机制不得削弱。
4. **修改范围**：只改本文列出的问题及其直接波及面；不顺手重构、不改公共 API 形状、不动 UI 视觉与动画（那是另一条审计线的产物）。
5. **每个批次独立提交**，提交信息中文并标注本文的问题编号（如 `fix: R1 拼音重建移出读锁（AUDIT-2026-08-18 R1）`）。
6. **禁止删除任何已有测试或降低断言强度**；修复必须附带能锚定该缺陷的新测试（能写单测的写单测，涉及真实管道/服务的用 `scripts/` 下现有 PS1 验收脚本模式补一个）。
7. **偶发缺陷声称修复前必须用未修复版本做 A/B 复现**（项目铁律，"修完 0 错误"不可证伪）。

---

## 2. 问题清单

优先级：**P0 = 当下已损坏/事故级**，P1 = 高（真实用户可感知的稳定性/内存问题），P2 = 中（韧性/性能打磨），P3 = 低，P4 = 架构演进（本次不实施，仅记录方向）。

### A. 协议与 IPC

#### R-A1（P0）世代长轮询已静默失效：协议 1 vs 2
- **位置**：`src/Prism/Services/IndexerGenerationClient.cs:22`（`ProtocolVersion = 1`，第 99-105 行握手）；`src/prism-core/src/lib.rs:42`（`INDEXER_PROTOCOL: u32 = 2`）；服务端拒绝逻辑 `src/prism-core/src/indexer_runtime.rs:1503-1517`。
- **问题**：C# 世代客户端直接连 indexer 管道握手，写死协议 1，服务端要求 2，握手必失败 → mutation 后的世代等待永远走 3s 超时退化路径。历史上同类问题（协议不匹配静默降级）已发生过一次事故。
- **修法**（两步）：
  1. 立即修复：`IndexerGenerationClient.cs` 的 `ProtocolVersion` 改为 2，并核对 hello/世代请求的字段与 `indexer_runtime.rs` 当前协议一致（对照 `indexer_client.rs` 的 Rust 客户端实现逐字段核对）。
  2. 结构性修复：服务端对不匹配的 hello 不再回泛化 `Error{"incompatible or missing hello"}`，改回专用变体（如 `Error{ code: "protocol_mismatch", server: 2, client: X }`）；两侧客户端（`indexer_client.rs:180-191` 与 C#）识别该错误并向上层暴露"版本不匹配"状态（broker 把它区别于"索引不可用"冒泡，前端状态栏显示明确文案）。协议常量的"单一真源"做法：在 `lib.rs` 保持真源，C# 侧在 `IndexerGenerationClient` 顶部加注释指向 `lib.rs:42`，并新增一个 C# 单测读取双方 hello JSON 结构做形状锚定（无法跨语言直接共享常量时，至少用测试锚定）。
- **验证**：新增 Rust 单测——用协议 1 hello 连 mock 服务端收到 `protocol_mismatch` 且携带双方版本；C# 侧 `PipeClientHandshakeTests.cs` 模式补 `IndexerGenerationClient` 握手测试。手动：`scripts/indexer-pipe-roundtrip-test.ps1` 通过；mutation（重命名一个文件）后结果刷新不再固定等 3 秒。

#### R-A2（P1）管道 ACL 裸奔
- **位置**：broker 管道创建 `src/prism-core/src/ipc.rs:350-396`（无 ACL 限制）；indexer 管道 `src/prism-core/src/indexer_runtime.rs:1443` 附近 `create_pipe`（ACL 含 Authenticated Users 全权）。
- **问题**：broker 以用户身份执行删除/复制/打开动作，管道无 ACL 意味着任意本地进程可指挥它；indexer 管道 AU 全权 + 连接数无上限（见 R-A3）。
- **修法**：broker 管道 SECURITY_ATTRIBUTES 限制为当前用户 SID + SYSTEM；拒绝远程客户端（`reject_remote_clients`）。indexer 管道 AU 只保留 `GENERIC_READ | GENERIC_WRITE`（连接所需），移除多余权限；保持 SYSTEM/管理员完全控制。使用 `windows-sys` 已有绑定手写 SECURITY_DESCRIPTOR（不加新依赖）。
- **验证**：`cargo test`；`scripts/pipe-roundtrip-test.ps1` 与 `indexer-pipe-roundtrip-test.ps1` 以普通用户身份仍通过；另起一个其他用户会话（或用 `runas`）验证连接被拒。

#### R-A3（P2）indexer 服务连接数无上限
- **位置**：`src/prism-core/src/indexer_runtime.rs` `accept_loop`（约 1413 起）——每连接 `tokio::spawn`，各占任务 + 1MB 行缓冲。
- **修法**：加原子计数并发连接上限（64），超出的连接握手前直接关闭并记日志。
- **验证**：新增单测：并发 70 连接，第 65+ 个被拒，前 64 个正常服务。

#### R-A4（P2）broker→前端方向无行长上限
- **位置**：C# `PipeClient.cs` 读响应用 `ReadLineAsync`（无上限）；对称的 Rust 侧已有 `BoundedLineReader`（broker 入站 1MB `ipc.rs:523`，indexer 响应 8MB `indexer_client.rs:101`）。
- **修法**：C# 侧包一层有界读（如 16MB 上限），超限视为协议损坏 → 销毁连接走重连。
- **验证**：`Prism.Tests` 新增超长行拒收测试（内存流模拟）。

#### R-A5（P2）首连失败则 watchdog 永不 arm
- **位置**：`src/Prism/Services/PipeClient.cs` 启动路径 `TryStartBackendAsync`（catch 后未启动 watchdog；watchdog tick 在 108-153）。
- **问题**：indexer 首建期 broker 起不来/连不上时落 catch，此后无人重试，只能重启 Prism。
- **修法**：无论首连成败都启动 watchdog timer（一行级改动），确保 tick 里的 `EnsureBackendRunning` 兜底。
- **验证**：手动——改 broker exe 名使首连失败，15s 内 watchdog 应拉起（还原后）恢复；补一个可注入 process-starter 的单测更佳。

#### R-A6（P2）动作通道退化路径可把搜索通道永久冻死
- **位置**：`src/Prism/Services/PipeClient.cs:512-522`（`SendActionAsync` 退回 `_query.SendAsync(request, ct, readTimeout: null)`）；wedge 豁免逻辑 `PipeClient.cs:161-176`（`hasPendingSlowRead` 直接放行）；`_pendingSlowActionReads` 计数 877-975。
- **问题**：退化到查询通道后是无限期读，且该状态豁免 watchdog 的 wedge 判定——broker 对该 run_action 永不应答时，后续所有搜索堆在 `_ioLock` 后，UI 永远"搜索中"，无自愈。
- **修法**：退化路径给宽松超时（60s），或退化请求单独计数、不豁免 wedge 判定（超时后销毁流走重连，绝不重发——遵守约束 3）。
- **验证**：`Prism.Tests` 用 mock 流模拟"动作写出后无应答"，断言 60s（测试中缩短）后通道被判 wedge 并重建，后续搜索恢复。

#### R-A7（P2）broker 一次性短连接无并发闸
- **位置**：`src/prism-core/src/indexer_client.rs:201-214`（`try_lock` 失败 → `search_one_off` 新建连接，无全局上限）。
- **修法**：一次性降级路径加信号量（上限 2-3），拿不到就等或合并为只发最新 query。
- **验证**：单测并发 20 个 search，断言峰值一次性连接 ≤ 上限。

#### R-A8（P3）孤儿 broker 复用后逃出 Job Object
- **位置**：`src/Prism/Services/PipeClient.cs:738-740`（复用已存在 broker）、`JobObjectGuard`（PipeClient.cs:592 附近）。
- **问题**：复用的孤儿不在新 Prism 的 Job 里，新 Prism 崩溃不带走它，可积累。
- **修法**：复用前尝试 `AssignProcessToJobObject`（同权限下通常可成功）；失败则杀旧拉新。
- **验证**：手动脚本：起 Prism → 强杀 Prism 留孤儿 broker → 再起 Prism → 强杀 → 确认无 broker 残留。

### B. Rust 核心：锁与停顿

#### R-B1（P0）拼音重建在读锁内做全索引遍历 + fsync
- **位置**：`src/prism-core/src/indexer_runtime.rs:326-340`（`rebuild_pinyin_from_live` 在 `self.index.read()` 内调 `rebuild_pinyin` 283-318：全节点遍历 + `sidecar.save()` 含 `sync_all`）；被 `checkpoint()`（1332，第 1354 行）调用。对照正确做法：`persist_first_build_with`（1016）与 `checkpoint` 本体（1343 起）都是"锁内 clone、放锁后 save"，且注释明确写了原因。
- **问题**：百万级中文文件下持读锁秒~几十秒；SRWLock 写者优先 → USN 写者堆积同时新搜索读者也被阻塞 → 复现历史 ERROR_PIPE_BUSY/前端卡死。
- **修法**：与 checkpoint 同款——读锁内只提取构建 sidecar 所需快照（各卷 names 引用的最小拷贝，或直接复用 checkpoint 已 clone 的那份 `IndexState`，因为调用点就在 checkpoint 内、clone 就绪），放锁后 build + save + load 校验；装载对账沿用现有 identity/delta 机制。
- **验证**：`cargo test`（pinyin_sidecar 与 indexer_runtime 现有测试全绿）；新增测试：rebuild 期间另一线程能在 <100ms 内拿到写锁。手动：大索引机器上 `scripts/indexer-usn-latency-test.ps1`，checkpoint 触发时搜索延迟无尖峰。

#### R-B2（P1）任一卷 watcher 出错 → 全盘全量重扫
- **位置**：`src/prism-core/src/indexer_runtime.rs`：watcher 错误经 `rebuild_tx` 触发 `build_all()`（1155），epoch+1 杀掉所有健康卷 watcher；`merge_and_publish`（428）已支持按 volume_id 替换。
- **修法**：`rebuild_tx` 消息携带卷标识；单卷错误只重建该卷并 `merge_and_publish`，只在"卷集合变化"时才 `build_all`。注意与第 756-777 行 rebuild 分支的 epoch 语义配合：单卷重建只使该卷 watcher 换代。
- **验证**：新增集成测试：两卷 mock，卷 B watcher 报错后卷 A 的索引对象未被替换（指针/generation 不变）；`cargo test` 全绿。

#### R-B3（P1）一次瞬时 create_pipe 失败 = 服务退出
- **位置**：`src/prism-core/src/indexer_runtime.rs:1413-1425`（accept_loop re-arm 失败返回 Err → serve abort 全部 listener → run 致命退出）；`pipe_task` 退出同致命（725-733）。
- **修法**：re-arm 失败退避重试（100ms 起指数、上限 5s），连续失败超 60s 才升级为致命。
- **验证**：单测注入 create_pipe 前 N 次失败，断言服务存活且第 N+1 次成功后恢复服务。

#### R-B4（P1）非法 UTF-16 文件名导致解析整批失败 → 重建循环
- **位置**：`src/prism-core/src/ntfs.rs:113`（`String::from_utf16` 失败让 `parse_usn_buffer` 整批 Err → watcher 死 → 经 R-B2 还会放大为全盘重扫；MFT 枚举路径同样受影响）。
- **修法**：改 `from_utf16_lossy` 单条降级（NTFS 文件名本就允许非法代理对）；可加一条 debug 日志计数。
- **验证**：单测构造含孤立代理对的 USN 记录，断言解析成功且该条目名字为 lossy 结果、其余条目不受影响。

#### R-B5（P2）锁中毒被静默吞掉
- **位置**：`src/prism-core/src/indexer_runtime.rs` 的 `merge_and_publish`（428）、`publish`（475）、`set_error`（493）等 `if let Ok(...) = self.index.write()` 模式。
- **问题**：release 下 panic=abort 不会毒化，但 `--console`/`cargo test` 是 unwind；静默跳过让致命状态不可观测（照样 notify_waiters）。
- **修法**：统一 `.unwrap_or_else(|e| e.into_inner())`（与 `search`/`watch_volume` 的显式处理对齐），不改变 release 行为。
- **验证**：`cargo test`；grep 确认 runtime 内不再有静默 `if let Ok(...write())` 模式。

### C. Rust 核心：内存

#### R-C1（P1）首建峰值 ≈ 常驻 6–10 倍
- **位置**：`src/prism-core/src/ntfs.rs:524-569`（`enumerate_mft` 全量物化 `Vec<UsnRecord>`，每条 ~100–130B 含堆 String）+ `build_volume_with_progress`（374，排序后 64 轮 upsert）。300 万记录 ≈ 350–400MB 峰值。
- **修法**（保守方案，不动整体流程）：`UsnRecord` 的 `name: String` 改为紧凑编码——枚举时把名字字节直接追加进一个单一大 `Vec<u8>` 池 + 记录 `(offset, len)`，`UsnRecord` 变纯 POD；排序按 key 不动数据。这样每条从 ~100–130B 降到 ~64B + 名字实际字节，峰值近似砍半且分配次数从 O(n) 降到 O(1) 摊销。真正流式方案（边枚举边 upsert）风险高（父目录先行问题），列为 P4。
- **验证**：`cargo test`（ntfs/hierarchy 测试全绿）；`tools/bench/Measure-ProcessMemory.ps1` 对比首建峰值，预期降 ≥40%；`tools/bench/Invoke-G9FirstBuildAcceptance.ps1` 通过。

#### R-C2（P1）checkpoint clone 2× 峰值 + validate O(n·depth)
- **位置**：`src/prism-core/src/indexer_runtime.rs:1343-1346`（读锁内 clone 整个 IndexState）；`src/prism-core/src/index_cache.rs:51`（save → validate 对每节点跑 `path_for`，hierarchy.rs:526-563）；停机路径（run 尾部约 833）走全套，可能超 SCM 30s wait_hint（prism-indexer-service.rs:52）。
- **修法**：save 前 validate 降为抽样（如随机 1% + 全部根节点）+ 结构不变量（names 池边界、slot 计数一致性）；load 侧保留全量校验。停机路径跳过拼音重建（R-B1 修后自然如此）并考虑把 wait_hint 用 checkpoint 实测时长动态上报。clone 本身的消除（分卷增量落盘）列为 P4。
- **验证**：`cargo test`；构造一个故意损坏的缓存文件确认 load 全量校验仍拒收；大索引下 SCM Stop 在 30s 内完成（`scripts/indexer-service-install-test.ps1`）。

#### R-C3（P2）nodes 槽表只增不减
- **位置**：`src/prism-core/src/hierarchy.rs:647-666`（`ensure_slot` 只 resize 增长；delete 只 tombstone；`compact_names_if_needed` 565 不处理槽表）。上限 16,777,216 × 12B = 192MB/卷。
- **修法**：names 压缩时顺带统计尾部连续空槽（tombstone 或从未使用）并 `truncate` + `shrink_to_fit`。中段空洞不处理（记录号是外部键，不能重映射——那是 P4）。
- **验证**：单测：创建高记录号节点后删除，压缩后 `nodes.len()` 回落；序列化缓存体积同步回落。

### D. WPF 前端

#### C-D1（P0）无全局异常兜底
- **位置**：`src/Prism/App.xaml.cs:50`（OnStartup，全仓无 `DispatcherUnhandledException`/`TaskScheduler.UnobservedTaskException`/`AppDomain.UnhandledException` 注册）。
- **问题**：常驻托盘进程任何 UI 线程未捕获异常（多处 `async void`，如 `SearchWindow.xaml.cs:445/615`；主题切换中 `FindResource` 失败；C-D2 的字典损坏）→ 进程无提示消失。
- **修法**：OnStartup 注册三个钩子：记日志（复用现有日志基建；若前端无日志文件则写 `%LOCALAPPDATA%\Prism\logs\frontend.log`，简单 append + 10MB 截断，不加依赖）；`DispatcherUnhandledException` 对可恢复异常 `e.Handled = true`；`UnobservedTaskException` 一律 `SetObserved`；`AppDomain.UnhandledException` 只记日志（无法阻止退出）。关键 `async void` 事件体外层补 try/catch。
- **验证**：`dotnet test`；手动：调试用临时菜单项抛异常，进程不退、日志有记录。

#### C-D2（P1）WebIconProvider 字典跨线程无锁读写（未定义行为）
- **位置**：`src/Prism/Services/WebIconProvider.cs:53,73-81`（后台线程 `Invalidate` → `Dictionary.Clear`）vs UI 线程 `GetIcon` 读写 `_resolved`（调用链 `App.xaml.cs:214-229`、`ResultList.xaml.cs:384` DecorateVisibleItems）。
- **修法**：`Invalidate` 经 `Dispatcher.BeginInvoke` 投递到 UI 线程（首选，保持字典无锁单线程语义），或 `_resolved` 换 `ConcurrentDictionary`。
- **验证**：`dotnet test`（`WebIconFlickerTests` 全绿）；代码评审确认 `_resolved` 的全部触点在 UI 线程。

#### C-D3（P1）低级键盘钩子被摘除后无恢复
- **位置**：`src/Prism/Services/HotkeyService.cs:75-88`（WH_KEYBOARD_LL；回调超时被系统静默 unhook，DoubleCtrl 是默认触发方式）。
- **修法**：加自检重装：定时器（60s）或每次窗口呼出/隐藏时，用 `GetAsyncKeyState` 哨兵无法直接检测钩子存活，故采用「周期性 Unhook + 重装」（幂等、成本微小）；重装失败记日志并重试。
- **验证**：`dotnet test`；手动：调试器挂起进程 10s（诱发系统摘钩）恢复后 ≤60s 内双击 Ctrl 恢复响应。

#### C-D4（P1）EmptyWorkingSet / 非阻塞 Optimized GC 是无效化妆
- **位置**：`src/Prism/App.xaml.cs:349`；`src/Prism/Windows/SearchWindow.xaml.cs:164-168, 338-359`（共 4 处调用点）。
- **问题**：EmptyWorkingSet 只逐出工作集不降私有提交，代价是下次呼出软缺页变慢；`GC.Collect(2, Optimized, blocking:false, compacting:true)` 的 Optimized 常直接跳过、非阻塞下不压缩。
- **修法**：删除全部 EmptyWorkingSet 调用；隐藏窗口后的空闲回收改为**延迟 3 分钟**（沿用现有 Trim timer 机制）执行一次 `GC.Collect(2, GCCollectionMode.Forced, blocking: true, compacting: true)`（窗口已隐藏不卡交互），随后不再做工作集操作。保留结果列表/IconCache 的既有 Trim。
- **验证**：`scripts/g5-memory-soak.ps1` 前后对比：呼出→搜索→隐藏 50 轮，比较私有提交（不是工作集）曲线与二次呼出首帧耗时；预期私有提交持平或降、二次呼出不再有软缺页尖峰。

#### C-D5（P2）IconCache 假 LRU + 竞态产生不可淘汰条目
- **位置**：`src/Prism/Services/IconCache.cs:61-93`（命中不触碰 `_order` = FIFO；`ClearPathKeys` drain-重建 78-83 与 `GetAsync` 的 `Enqueue` 42 并发竞态 → 条目在 `_cache` 不在 `_order`，TrimIfNeeded 失效）。
- **修法**：换带单锁的真 LRU（`Dictionary` + `LinkedList`，锁粒度小、条目 ≤ 数百，无性能顾虑）；`ClearPathKeys` 全程持同一把锁。
- **验证**：`dotnet test`（`IconCacheKeyTests` 全绿 + 新增：并发 Get/Clear 压测后 `_cache.Count ≤ MaxEntries` 恒成立、命中会刷新驱逐顺序）。

#### C-D6（P2）favicon 无负缓存 + UI 线程同步读盘
- **位置**：`src/Prism/Services/WebIconProvider.cs:75` → `src/Prism/Services/FaviconCache.cs:58-79,232-253`。
- **修法**：`_resolved` 加负缓存条目（下载失败/文件不存在记 null，`Invalidate` 时一并清除）；磁盘加载挪到 `Task.Run`，完成后 BeginInvoke 回 UI 装饰（照抄 IconCache 的异步模式）。
- **验证**：`WebIconFlickerTests`/`WebIconFlickerVisualTests` 全绿；新增测试：同一未命中 origin 连续 decorate 只触发一次磁盘探测。

#### C-D7（P2）ThemeWatcher 同步 Invoke 死锁面
- **位置**：`src/Prism/Services/ThemeWatcher.cs:49`（SystemEvents 回调线程持内部锁时 `Dispatcher.Invoke`）。
- **修法**：改 `BeginInvoke`。
- **验证**：`dotnet test`；手动切换系统深浅色主题数次无异常。

#### C-D8（P2）SingleInstance 哑连接占死监听
- **位置**：`src/Prism/Services/SingleInstance.cs:84-88`（`ReadLineAsync` 无超时，单实例管道被不发数据的连接卡死 → 之后双开无反应）。
- **修法**：读加 2s 超时（`CancellationTokenSource.CancelAfter` + 超时 Dispose stream），循环继续接受下一个连接。
- **验证**：新增单测：连上不发数据的客户端 2s 后被断开，随后正常 `show` 信号仍生效。

#### C-D9（P3）PipeClient.Dispose 与 watchdog tick 竞态复活 broker
- **位置**：`src/Prism/Services/PipeClient.cs:653-673`（Dispose）vs 108-153（tick 可能在 Kill 后重新 `EnsureBackendRunning`）。
- **修法**：`volatile bool _disposed`，tick 入口检查；或 `Timer.Dispose(WaitHandle)` 等回调排干后再 Kill。
- **验证**：退出 Prism 后确认无 prism-core 进程残留（现有 `scripts/g5-pipe-probe.ps1` 可复用检查）。

#### C-D10（P3）RefreshAsync 世代信号覆盖
- **位置**：`src/Prism/ViewModels/SearchViewModel.cs:646-666, 104-106`（第二次 mutation 覆盖第一个 TCS；652 行置 null 又清掉新信号）。
- **修法**：局部变量持有本次 TCS + `Interlocked.CompareExchange` 归还，只清自己那份。
- **验证**：`SearchViewModelTests` 新增连发两次 mutation 的测试：两次刷新都在信号（非超时）路径完成。

### E. 架构方向（P4，本次不实施，写入后续规划）

1. **每卷独立锁 + arc-swap 式快照发布**：消除全局 `RwLock<Option<IndexState>>` 这一整类问题（R-B1/B2 的根因）。需要 `arc-swap` 依赖或手写 `Arc<AtomicPtr>` 方案，属大改。
2. **broker↔indexer 换长度前缀二进制协议**（postcard 已在依赖里），消除每键 JSON 序列化开销。
3. **SearchViewModel（1251 行）与 SearchWindow.xaml.cs（977 行）拆分**：`WebSearchCoordinator` / `ActionPanelController` / `IFolderPicker` 注入（VM 里直接弹 WinForms FolderBrowserDialog 在 619-629 行，单测不可过）；焦点策略已有 `SearchWindowFocusPolicy` 先例可推广。
4. **首建真流式**（R-C1 的彻底版）与 **checkpoint 分卷增量落盘**（R-C2 的彻底版）。

---

## 3. 实施批次与顺序

| 批次 | 问题 | 说明 |
|---|---|---|
| 1（P0，立即） | R-A1、R-B1、C-D1 | 各自独立，可并行；R-A1 先做第 1 步立即修复 |
| 2（P1 稳定性） | R-B4、R-A5、C-D2、C-D3、R-B3、R-B2 | R-B4 要先于 R-B2 验证（同链路） |
| 3（P1 内存） | C-D4、R-C1、R-C2 | 每项都要跑 bench 前后对比 |
| 4（P2） | R-A2、R-A6、C-D5、C-D6、C-D7、C-D8、R-A3、R-A4、R-A7、R-B5、R-C3 | 按列出顺序 |
| 5（P3） | R-A8、C-D9、C-D10 | 顺手修 |

每批次完成后运行完整验证门槛（第 4 节）再进入下一批。

---

## 4. 全局验证门槛（每批次必过）

```powershell
# Rust（在 src/prism-core/ 下；注意 MSVC link 陷阱，必要时先 source scripts/msvc-env.sh）
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings   # 若基线本就有 warning，只要求不新增

# C#（仓库根）
dotnet build src/Prism/Prism.csproj -c Release
dotnet test src/Prism.Tests/Prism.Tests.csproj
# live 测试（WindowActivatorLiveTests 等）需要交互桌面会话，CI 无桌面时可 --filter 排除，但本机必须跑

# 手动/脚本验收（涉及对应子系统时）
scripts/pipe-roundtrip-test.ps1
scripts/indexer-pipe-roundtrip-test.ps1
scripts/indexer-service-install-test.ps1     # 涉及服务/协议改动时
scripts/g5-memory-soak.ps1                   # 涉及内存改动时（比较私有提交，不是工作集）
tools/bench/Invoke-SearchBaseline.ps1        # 涉及搜索热路径时，对比基线无回退
```

注意：本机 Python 命令是 `py` 不是 `python`；新写 PS1 存 UTF-8 with BOM。

---

## 5. 审计推理过程摘要（供实施模型理解"为什么"）

- 审计方法：三路并行从零通读源码（Rust 核心全部模块 / WPF 前端全部 cs+xaml / IPC 链路两侧对照），刻意不读 docs/ 旧文档以避免锚定；关键发现（R-A1 的协议常量、管道 ACL）已在主会话二次核实源码确认。
- **R-A1 的判定链**：`IndexerGenerationClient.cs:22` 常量 1 → 握手 101-105 要求相等 → `lib.rs:42` 服务端为 2 → 必然 `IOException` → 上层把它当"索引不可用"静默吞掉。这与项目记忆中"协议版本不匹配会静默降级、要查已装服务的二进制"的历史事故同构，说明缺的是机制不是补丁。
- **R-B1 的判定链**：`checkpoint` 自己的注释写明"clone 后放锁再 save，否则饿死 USN 写者和 2 线程 runtime"，而它在 1354 行调用的 `rebuild_pinyin_from_live` 恰好在读锁内做了被禁止的事（全遍历 + fsync）——属"规则已知但新代码违反"，修法直接对齐既有正确模式即可，风险低。
- **C-D4 的判定链**：EmptyWorkingSet 语义是逐出工作集页面，私有提交不变；`GCCollectionMode.Optimized` 允许 CLR 判定"不值得"而跳过；非阻塞后台 GC 不执行压缩。三者叠加意味着现有"空闲降内存"路径接近空操作，而软缺页代价是真实的——所以修法是"删掉 + 一次真 GC"而非调参。审计代理实测过 `GC.Collect(compacting:true, blocking:false)` 不抛异常，故现状只是无效而非崩溃。
- **优先级原则**:「已经坏了的」>「会让进程消失/卡死的」>「真实内存数字」>「韧性打磨」；修法一律选择对齐仓库内已有正确模式的最小改动，架构级方案全部推入 P4 由人工决策。
- 已知误报排除：前端事件泄漏面经查很小（订阅方均为进程同寿命单例），BitmapSource 已全部 Freeze，Rust 每次搜索的分配已高度优化（Arc<str>、复用 buffer）——这些**不需要**再改。

---

*审计执行：Claude（三个并行审阅代理 + 主会话核实），2026-08-18。分支 `feature`，基准提交 cde0a0d。*
