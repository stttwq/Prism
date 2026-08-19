# Prism 剩余审计项实施方案（2026-08-20）

本文汇总两份文档的**未完成项**，给出增量实施顺序。已完成项不复述（08-18 交接 P0/P1/P2 全部 25 项见 git log 811a69c..649e1e5 及 `PRISM-P2-BATCH4-REPORT-2026-08-19.md`）。

- 依据一：`docs/PRISM-AUDIT-HANDOFF-2026-08-18.md` —— 剩 P3 批次（R-A8 / C-D9 / C-D10）。
- 依据二：`docs/PRISM-FRESH-AUDIT-2026-08-19.md` —— B1~B4 全部未实施（N1/N2/M1/M2/S1/S2/S3/S5）。
- 依据三：`docs/PRISM-P4-RESEARCH-2026-08-19.md` —— P4a/P4b/P4c 三小步待做；P4-1/P4-2/P4-4 已关闭，本方案不实施。

## 原则

1. 每完成一项：`cargo test`（涉及 Rust 时）+ `dotnet test`（涉及 C# 时）全绿后提交，提交信息标注问题编号。
2. 零协议改动、零缓存版本改动、零新第三方依赖（memchr 属新依赖且新审计自身设了实测门槛，本方案只做 N2 第一步 std::thread::scope，memchr 留触发器）。
3. 不削弱任何既有测试断言；每项修复附带能锚定缺陷的新测试。
4. 风险递增排序：C# 独立小项 → Rust 热路径 → 锁治理 → checkpoint/缓存格式行为。

## 批次与任务

### 批次 PA（08-18 P3 收尾 + P4 三小步，纯 C#）

| # | 任务 | 内容 | 验证 |
|---|---|---|---|
| PA1 | R-A8 | 孤儿 broker 复用前 `GetNamedPipeServerProcessId` 取服务进程，`AssignProcessToJobObject` 纳入本 Prism 的 Job；失败则杀旧拉新 | 单测：mock 场景下复用路径会请求收编；手动脚本强杀 Prism 留孤儿再起验证无残留 |
| PA2 | C-D9 | `PipeClient.Dispose` 与 watchdog tick 竞态：`_disposed` 标志 + `Timer.Dispose(WaitHandle)` 排干在途 tick 后再 Kill；`EnsureBackendRunning` 入口查标志 | 单测：Dispose 后 tick 委托不再拉起进程（可注入 process-starter） |
| PA3 | C-D10+P4b | RefreshAsync TCS 覆盖竞态：局部变量持有本次 TCS + `Interlocked.CompareExchange` 归还只清自己那份；`Task.Delay(3000)` 换 `_scheduler.Delay`（可测化） | 新增：连发两次 mutation 两次刷新都走信号路径；FakeScheduler 快进验证超时路径 |
| PA4 | P4a | `IFolderPicker` 注入（SearchAbstractions 加接口 + WinFormsFolderPicker 默认实现，VM 构造可选参数），`PickDestinationFolder` 转发 | 新增：FakePicker null→取消文案不执行；路径→ExecuteAsync 收到 dest |
| PA5 | P4c | 抽 `WebSearchCoordinator`（RunWebSearchAsync/BuildWebRows/BuildWebMatchSpans + 相关字段，注入 marshalToUi），VM 对外签名不变 | `WebIconFlickerTests` 零改动通过（等价性守护）+ coordinator seq 取消新测试 |

### 批次 PB（新审计 B1：低风险三小项，Rust）

| # | 任务 | 内容 | 验证 |
|---|---|---|---|
| PB1 | N1 | 搜索热路径零分配化：查询纯 ASCII 走字节级 `to_ascii_lowercase` 比较不构造 lowered 名；非 ASCII 先零分配扫 `char::is_uppercase()` 无大写直接 `find`；`path_matches` needle 循环外预降幂 | 新增 4-6 个单测（纯 ASCII/中文/含大写拉丁/代理对）+ 现有 hierarchy 单测全绿 |
| PB2 | S3 | apps 清单扫描 5 次×30s 后改指数退避到 1 小时，成功即停，永不放弃 | 单测：前 5 次失败后退避序列正确；成功一次即停 |
| PB3 | S5 | maintenance tick 每小时把 `(generation, events_since_checkpoint, memory_bytes, pinyin_status)` 写事件日志一行 | 单测：日志节拍与字段；一小时一行不刷屏 |

### 批次 PC（扫描延迟实测 → 决定 N2 形态）

| # | 任务 | 内容 | 判定 |
|---|---|---|---|
| PC1 | 实测 | 本机（~1.2M 记录）实测 `search_volumes` 单次全扫延迟：临时打点或 bench 脚本 | 全扫 <5ms 且无中文重度用户反馈 → N2 仍做第一步（方案既定），memchr 第二步维持关闭；全扫 >20ms → 考虑提前 memchr（需人工批准新依赖） |

### 批次 PD（新审计 B2：N2 第一步）

| # | 任务 | 内容 | 验证 |
|---|---|---|---|
| PD1 | N2-1 | `std::thread::scope` 按卷/按 ~256K 槽分块并行扫描，每块局部 Top-K，归并全局 Top-K；并行度 `min(available_parallelism, 8)`；`SearchOutcome` 统计按块累加；`RankedCandidate::Ord` 全序保证结果与单线程一致 | 等价性测试：构造大规模合成槽，并行与串行结果逐字节相等；延迟阈值断言 |

### 批次 PE（新审计 B3：锁治理 + 建库内存）

| # | 任务 | 内容 | 验证 |
|---|---|---|---|
| PE1 | S1 | `compact_names_if_needed` 挪出写锁：watcher 只置 `needs_compact`（O(1)）；maintenance tick 读锁 clone → 锁外压缩 → 短写锁内 `next_usn` 未变则换入（重试 3 次放弃本轮） | 单测：压缩窗口注入 USN 事件不丢（next_usn 前移换入被拒）；压缩期间搜索读锁等待无尖峰 |
| PE2 | M1 | 首建 `names` 容量预留 `min(records.len × 均值估计, live_records × 64B)` 消倍增尖峰 | 现有 ntfs/hierarchy 单测 + 容量预留单测 |

### 批次 PF（新审计 B4：checkpoint 流式 + 按卷缓存）

| # | 任务 | 内容 | 验证 |
|---|---|---|---|
| PF1 | M2 | checkpoint 逐卷短读锁流式序列化（SnapshotView 包装，envelope 头首卷前快照，卷间 generation 变化中止重试，3 次回落 clone 路径）；v5 字节流逐字节不变 | 测试：新旧路径写同一索引字节相等；构造 generation 变化触发中止回落 |
| PF2 | S2 | `validate_checkpoints` 逐卷判定，`CachedLoad::Partial`：可回放卷照常发布 + watcher，坏卷走首建循环 | 集成测试：缓存 2 卷现场 3 卷 / 缓存 3 卷现场 2 卷两方向；pinyin identity 多触发一次重建可接受 |

### 总测试

全部完成后：`cargo build --release` + `cargo test` + `cargo clippy` + `dotnet build` + `dotnet test` + 安装包重建（Inno Setup）+ 冒烟（呼出/搜索/删除文件 USN 收敛/重启缓存命中）。

### 批次 PG（新一轮从零审计，摆脱旧文档）

1. 忽略 docs/ 全部既有结论，从零重读 `src/prism-core`（约 15k 行）与 `src/Prism`（约 13k 行）。
2. 联网对标 Everything（voidtools 论坛/文档）与 Listary（changelog/论坛）最新做法，重点：核心搜索架构、内存占用、稳定性。
3. 产出新方案文档：修改建议（核心/内存/稳定性优先）、对现有仓库的影响面、实施风险评估。

## 风险控制

- PA 批次纯 C# 无协议影响，可独立回滚。
- PB/PD/PE/PF 全部不触 IPC 协议与缓存 v5 格式（M2 显式锚定字节相等）。
- 每项独立提交，出问题按提交回滚单项。
- N2 并行搜索贴 tokio blocking 池上限（64 连接 × 8 块 = 512），单次搜索并行度封顶 8，文档注明组合约束。
