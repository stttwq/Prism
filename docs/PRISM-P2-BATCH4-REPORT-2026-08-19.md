# Prism P2 批次 4 完成报告与决策日志（2026-08-19）

依据 `docs/PRISM-AUDIT-HANDOFF-2026-08-18.md` 批次 4（P2）执行。
基线提交：c9eac27；完成区间：9c130e3 → 649e1e5（分支 `feature`，未推送远端）。
执行约定：每完成一项即跑全量测试（cargo test + dotnet test），失败修复后才提交。

## 1. 任务汇总表

| 任务 | 内容 | 状态 | 提交 |
|---|---|---|---|
| C-D5 | IconCache 单锁真 LRU（前次会话已码完） | 完成（本轮补测试验证+提交） | 9c130e3 |
| R-A2 | broker 管道 ACL（当前用户 SID + SYSTEM + 拒绝远程）；indexer 管道 AU 收窄 GA→GRGW | 完成 | 23bcb38 |
| R-A3 | indexer 服务并发连接上限 64，超限握手前关闭 | 完成 | b3b9280 |
| R-A4 | 前端读响应 16MB 有界 BoundedLineReader（超长行=协议损坏销毁连接） | 完成 | ca1920d |
| R-A5 | 首连失败也启动 watchdog | **跳过**：前次会话已在 d2eaf76 完成且有测试锚定（本轮全量测试通过） | — |
| R-A6 | 动作退化到搜索通道后 60s 宽松读超时，不再豁免 wedge 判定 | 完成 | caec630 |
| R-A7 | broker 一次性降级搜索连接加信号量闸（上限 2） | 完成 | fd3e79a（测试加固随 052b4f8） |
| R-B5 | indexer runtime 17 处锁中毒静默吞写改 `unwrap_or_else(into_inner)` | 完成 | 052b4f8 |
| R-C3 | names 压缩时同步回收 nodes 尾部 tombstone/未用槽 | 完成 | 6332eeb |
| C-D6 | favicon 未命中负缓存 + 磁盘探测挪后台线程（同 origin 至多一次在途探测） | 完成 | d5367fd + af31830 |
| C-D7 | ThemeWatcher 回调 Invoke→BeginInvoke | 完成 | 0997c32 |
| C-D8 | SingleInstance 监听读 2s 超时，哑连接不占死监听槽 | 完成 | 649e1e5 |

## 2. 全量回归测试状态：**通过**

| 门禁 | 结果 |
|---|---|
| `cargo build --release` | 通过 |
| `cargo test`（prism-core） | **322 passed, 0 failed**（14 ignored 均为 live IFileOperation/剪贴板/UAC 测试，按既有约定显式跳过） |
| `cargo clippy --all-targets` | 通过（2 条 op_ref 告警为基线已有测试代码，非本次新增） |
| `dotnet build src/Prism/Prism.csproj -c Release` | 通过，0 警告 |
| `dotnet test src/Prism.Tests` | **183 passed, 0 failed**（10 skipped 为 WindowActivator/热键/联网建议 live 测试，需交互桌面，按既有约定跳过） |

未执行项（原因）：`scripts/pipe-roundtrip-test.ps1`、`indexer-service-install-test.ps1`、`g5-memory-soak.ps1` 等脚本需安装/重启真实服务与交互桌面会话，属人工验收步骤；本轮为无人值守自动化，未触发。R-A2 的跨用户拒连验证同样需要第二用户会话（`runas`），留待人工。

## 3. 新增测试锚定

- `ipc::pipe_lifecycle_tests::current_user_sid_has_sddl_shape` / `create_broker_pipe_with_acl_succeeds`（R-A2）
- `indexer_runtime::tests::connection_admission_enforces_the_cap`（R-A3）
- `PipeClientHandshakeTests.Oversized_Response_Line_Is_Rejected...` / `Bounded_Read_Matches_ReadLine_Semantics`（R-A4）
- `PipeClientHandshakeTests.Fallback_Action_Read_Times_Out_And_Destroys_Connection`（R-A6）
- `indexer_client::tests::one_off_connections_are_capped_by_the_gate`（R-A7，并发 20 峰值 ≤2）
- `hierarchy::tests::compact_names_reclaims_trailing_node_slots`（R-C3）
- `WebIconNegativeCacheTests` ×2（C-D6：未命中只探测一次；Invalidate 清负缓存）
- `SingleInstanceDumbClientTests`（C-D8：哑连接 2s 断开且后续 show 生效）
- R-B5 验证 = cargo test + grep（已确认 runtime 内无静默 `if let Ok(..write())`）

## 4. 决策日志

1. **R-A2 SID 获取**：OpenProcessToken + GetTokenInformation(TokenUser) + ConvertSidToStringSidW（windows crate 0.58 全部在已启用 feature 内，零新依赖）；SID 以 OnceLock 缓存。broker SDDL `D:P(A;;GA;;;SY)(A;;GA;;;<sid>)`（不含 BA——按审计文档"当前用户 SID + SYSTEM"字面执行；管理员走 SYSTEM/属主兜底）。
2. **R-A3 测试策略**：真实管道并发 70 连接测试会与已安装服务的固定管道名冲突，改为对准入计数器（try_admit/release）直接断言 64 席位边界；accept_loop 接线由既有集成测试覆盖。
3. **R-A4 实现形态**：StreamReader.ReadAsync 会整块消费字符（换行后的尾巴无法"退回"），有界读必须是有状态对象——实现为 `PipeClient.BoundedLineReader`（挂 PipeChannel、随流销毁重建），保留 leftover 缓冲。首版无状态 helper 在自测中被 `Bounded_Read_Matches_ReadLine_Semantics` 抓出丢尾巴 bug，随即重构修复。
4. **R-A6 测试注入**：`FallbackActionReadTimeout` 设 internal set 供测试缩短到 1s；布景用单实例管道 server 占住查询通道迫使动作通道退化。
5. **R-A7 观测点**：server 侧并发计数受客户端断开后 EOF 滞留虚高（自测抓到峰值 4 的假失败），改为在 `search_one_off` 拿到许可后埋 `ONE_OFF_PEAK` 原子（排队不计），断言精确等于闸语义。信号量用 `Semaphore::const_new(2)` 常量静态。
6. **R-B5 批量改写**：PowerShell 正则改写 17 处（脚本保留在会话记录；文件其余部分未触碰），每处加 R-B5 注释行。
7. **R-C3 截尾界**：`live_bound = max(在位节点 index+1, 在位节点 parent_record+1)`——缓存校验器把"父记录越界"当硬错误，被引用的父槽即使 tombstone 也必须留在表内；中段空洞不处理（记录号是外部键，重映射列 P4）。
8. **C-D6 契约变更**：GetIcon 首次未命中改为"立即回通用图标 + 后台探测 + 完成后经 Dispatcher 回 UI 写缓存"。`FaviconGrantTests` 两测原锚定同步读盘契约，改为轮询收敛后保持原断言强度（NotSame/Same 均保留）——属行为契约更新，非断言弱化。负缓存/在途探测表由 `_sync` 锁保护（后台完成回调与 UI 线程互斥）。
9. **C-D8 测试**：STA 线程 + DispatcherFrame 泵消息验证 BeginInvoke 的 show 回调真实执行；哑连接保持 3.5s（>2s 超时+余量）。
10. **提交拆分修正**：C-D7/C-D8 首次误并入一个提交（db7597b），已 reset 拆分为 0997c32 / 649e1e5。
11. **协议/兼容性**：本批零协议改动、零缓存版本改动、零新依赖（约束 1/2 满足）；管道写出后绝不重发的纪律在 R-A6 中以"超时即销毁流"保持（约束 3）。

## 5. 遗留（人工验收建议）

- 管道 ACL 跨用户拒连验证（第二用户会话或 `runas`）。
- 真实机器 mutation 后世代刷新、SCM Stop 30s 内完成（本轮未动协议，风险低）。
- 安装包重建（按需，参照 scripts/prism-build.ps1）。
