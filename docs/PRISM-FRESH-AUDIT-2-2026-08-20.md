# Prism 全新独立审计与下一轮修改方案（2026-08-20）

本文档基于**从零开始的完整代码阅读**（两个并行审读代理分别通读 Rust 核心 ~18k 行与 WPF 前端 ~13k 行，**刻意不读 docs/ 既有结论与 git 历史**，避免锚定），叠加联网对标 voidtools Everything（1.5 Beta，2025）与 Listary（V7，2025-07）的最新公开资料。

上一轮（PRISM-FRESH-AUDIT-2026-08-19 的 B1~B4 与 08-18 交接 P3/P4c）已全部实施完毕：N1 零分配折叠、N2 分块并行扫描（实测 1.2M 记录 ASCII 44ms→10ms、中文 104ms→24ms）、S1 压缩出锁、S2 按卷缓存生效、S3/S5、M1/M2、P4a/b/c、R-A8/C-D9/C-D10。本轮发现与它们不重叠。

---

## 1. 对标结论（2025-08 最新）

| 维度 | Everything 1.5 Beta | Listary V7 | Prism 现状 |
|---|---|---|---|
| 搜索核心 | 多线程 SIMD strstr 全库线扫（官方 t=9463），首字母查询快 3× | "new engine"，大结果集滚动/刷新性能重做 | N2 并行扫描已落地；SIMD（memchr）未做——**已知下一步，需新依赖审批** |
| 内存 | 全库驻内存 ~75-100MB/百万文件；Service 泄漏修复后索引期 ~10MB | 未公开数字 | **~40MB/百万文件，仍比 Everything 省 60%** |
| 索引更新 | 后台更新不阻搜索；fast reindexing 按卷 | — | S1/S2/M2 已对齐（压缩出锁、按卷生效、流式 checkpoint） |
| 数据库 | 新版本**带迁移**（升级不重建）；旧版读不了新库 | — | v5 单格式；升版=全量重扫（保持不升版的决策仍正确） |
| 进程模型 | — | v6.3.5.94 把 listary-core 并回主进程；V7 引擎重写 | 三进程维持（Prism 管道协议健壮，Listary 合并的理由不适用，旧结论继续成立） |

结论：Prism 在内存效率上继续领先，索引更新纪律已对齐 Everything；剩余差距集中在 **SIMD 子串搜索**与**拼音 sidecar 的同步粒度**（Everything 无此功能，是 Prism 独有负担）。

---

## 2. 本轮发现（31 项：Rust 16 + C# 15）

严重度统计：中 8 项、低-中 1 项、低 22 项。无高危（无崩溃/泄漏/数据损坏级）。

### 高优先（中severity，8 项）

| # | 位置 | 问题 | 修法 |
|---|---|---|---|
| F1 | `index_cache.rs:151` | `validate_before_save` 抽样 path_for 的结果被 `let _ =` 丢弃——抽样校验形同虚设，损坏父链照常写入 v5（只能靠 load 侧拒载兜底，代价是整卷重建） | `let _ =` 改 `?`（一行） |
| F2 | `root_scope.rs:220-248` | 带 root 的搜索**每击键**对整个节点表线性扫描解析 root（resolve_record 全表扫 + 父链上溯），叠加在搜索自身的全量扫描上——CWD 范围搜索是常态路径，3M 记录卷上额外 ~10ms+ 持读锁 | 按 `(volume, root_record)` 缓存 RootBound，generation 失效 |
| F3 | `pinyin_sidecar.rs:689-700` + `indexer_runtime.rs:301-312` | pinyin `index_identity` 在 index.read() 内对每卷 names 池（几十~上百 MB）FNV 逐字节哈希，锁持有 ~100ms 级阻塞 USN 写者；sidecar 失配后的首次搜索必付此代价 | identity 改持久化卷级内容指纹（journal_id+池长+采样块） |
| F4 | `ipc.rs:1513-1526` | 历史注入绕过 ext:/path: 过滤器——`report ext:pdf` 时历史的非 pdf 文件仍进结果，违背 G7 过滤语义 | `history_file_candidates` 增加 filters 参数 |
| F5 | `shell.rs:321-338` / `file_ops.rs:113-159` | copy_to/move_to 的 `destination` 无输入校验（JSON `\u0000` → PCWSTR 截断 → 意外路径）；`target.value` 与 `rename.new_name` 都有校验，唯独 destination 裸奔 | 对 destination 施加与 path 类 target 相同校验 |
| F6 | `PipeClient.cs:680+905` | 动作通道（`_action`）无任何超时且豁免 wedge 判定——broker 无对话框状态下死锁时 execute 永久挂起，后续动作在通道锁上无限排队，无自愈 | 不弹对话框的动作类给宽松超时（5 分钟级）；或"无 pending slow read 且超时"兜底判 wedge |
| F7 | `PipeClient.cs:664` + `SearchViewModel.cs:605` | 动作退化路径 60s 超时后报"动作失败"，但超时≠未执行——broker 可能已完成 delete 而响应丢失，用户重试=重复删除 | mutation 类动作超时后文案改"结果未知，请核实列表" |
| F8 | `ResultList.xaml.cs:21,188-213` + `SearchWindow.xaml.cs:875` | 结果面板高度不含状态行：空结果态 `Results.Height=NaN` → 动画目标 MinHeight=0，**状态文案（"无匹配结果"/"正在建立索引…"）按代码推演被整体裁剪**；`StatusRowHeight=36` 是死常量，疑似历史回归 | UpdateListHeight 计入可见状态行 36px；空态目标改显式"仅状态行"高度。**需先实机截图验证再修** |

### 次优先（低-中 1 项）

| # | 位置 | 问题 | 修法 |
|---|---|---|---|
| F9 | `ntfs.rs:194,217,459` | 错误分类靠消息字符串前缀（`starts_with("broken parent chain")` 决定 USN 延迟重试还是整卷重建；HRESULT 用 `contains` 判断）——文案一改重放静默变重建 | upsert 返回类型化错误 enum；IoctlError 模式推广 |

### 打磨项（低severity，22 项摘要）

**Rust**：pinyin 双读锁下全表扫（sidecar 可改 Arc 快照释放 `pinyin.read()`，`pinyin_sidecar.rs:362`）；history 每动作两次全量 clone+全量 JSON+fsync（`history.rs:320,577`，可借切片免二次 clone+写盘节流）；AU 全体可连 SYSTEM 服务触发重建风暴（`indexer_runtime.rs:1959`，多用户机器上为中——加每连接速率限制）；zip `-aoa` 静默覆盖与注释矛盾（`zip.rs:264`）；is_truncated 两套口径（`ipc.rs:1622`）；`metadata_kind=None` 排最前（`ipc.rs:2226`）；进程路径缓冲 260 截断长路径（`window_list.rs:446`）；首建 replay_until 无界累积 USN 记录（`ntfs.rs:524`，分批 apply）；sanitize 12 字节阈值漏短用户名（`logging.rs:131`）；ensure_slot 增长全表计数（`hierarchy.rs:706`，维护计数器字段）。

**C#**：`e.Handled=true` 写在 await 之后无效（`SearchWindow.xaml.cs:618`，键盘分支先置 Handled 再异步）；查询清空时 UI 线程阻塞压缩 Gen2 GC（`SearchWindow.xaml.cs:159`，可见期改 Gen0/1，全量压缩仅留隐藏后定时器）；每击键 BoundedLineReader 新分配 StringBuilder+4KB（`PipeClient.cs:290`，缓冲提为字段）；CancelSearch 直接 Dispose 在途 CTS（`SearchViewModel.cs:1066`，只 Cancel 不 Dispose）；Dispose 最坏阻塞 UI 12s + `_ioLock.Dispose()` ODE 噪音（`PipeClient.cs:827`，退出 Dispose 移后台线程）；EnsureHookThread 无锁双检可双建钩子泵线程（`HotkeyService.cs:126`，加锁）；IndexerGenerationClient 裸 ReadLineAsync 无上限无超时（`IndexerGenerationClient.cs:148`，复用 BoundedLineReader）；async void 菜单异常面（多处，内包 try/catch 写 StatusMessage）；目录图标缓存键坍缩为常量 `"dir:"` 与注释矛盾（`IconCache.cs:154`）；`RunWebSearchAsync` CS1998 伪装 async（改同步方法）；ExcludedPaths 设置 UI 不可达（补 UI 或注明仅配置文件）；TrayService/Task.Delay 小释放缺口。

---

## 3. 实施方案（按风险递增分五批）

### 批次 G1：一行级快赢（~10 行，风险≈0）
F1（let _→?）、sanitize 阈值、dir: 注释对齐、CS1998 改同步、TrayService menu.Dispose、`metadata_kind=None` 排序档位。
验证：现有测试全绿 + 新增 F1 锚定测试（构造损坏父链 → save 报错）。
**影响面**：单函数级。**风险**：极低。

### 批次 G2：输入校验与语义修正（~80 行，低风险）
F4（历史注入过 filters）、F5（destination 校验）、zip `-aoa` 语义、is_truncated 口径统一。
验证：ipc/shell/file_ops 新增单测（destination 含 NUL 拒绝；历史注入被 ext: 过滤）。
**影响面**：协议字段语义不变，行为收紧。**风险**：低——F4/F5 都是收紧方向，旧客户端无兼容问题。

### 批次 G3：前端体验与动作可信度（~150 行，中低风险）
F6（动作通道超时兜底）、F7（超时文案"结果未知"）、F8（状态行高度——**先实机验证**）、GC 策略（可见期 Gen0/1）、e.Handled 时序、BoundedLineReader 缓冲复用、CTS 不 Dispose、退出 Dispose 后台化、钩子线程加锁、IndexerGenerationClient 有界读。
验证：Prism.Tests 新增（超时文案分支、有界读复用）；F8 手动截图对比。
**影响面**：仅 src/Prism。**风险**：F6 需区分"对话框合法等待"与"死锁"——沿用 `_pendingSlowActionReads` 计数语义扩展，不动协议；F8 触碰动画高度计算，需截图回归。

### 批次 G4：热路径与拼音架构（~300 行，中风险）
F2（RootBound 缓存，generation 失效）、F3（pinyin identity 持久化指纹）、pinyin sidecar Arc 快照（扫拼音不再占 `pinyin.read()` 之外的锁）、history 免二次 clone + 写盘节流、ensure_slot 计数器化。
验证：root_scope 新增缓存命中/失效测试；identity 指纹稳定性测试（重排不改指纹、内容变必改）；history 持久化等价测试。
**影响面**：indexer_runtime/pinyin_sidecar/history/root_scope 内部。**风险**：中——F2 缓存失效错误会导致 CWD 搜索范围错误（必须 generation 失效严谨测试）；F3 指纹碰撞会静默用错 sidecar（采样块+长度+journal_id 三重组合，碰撞概率工程上为零，但需换 sidecar 重建一轮验证）。

### 批次 G5：结构性改造（人工决策后再做）
- **memchr SIMD 子串搜索**：Everything 的核心差距项。新增 ~30KB 依赖，需人工批准（交接文档约束 1）。收益预估：ASCII 扫描再 2-4×。
- **F9 类型化错误 enum**：触及 USN 重放/重建判定链，改错=重放变重建（有测试兜底但波及面大）。
- **AU 速率限制**：多用户机器部署前做。
- **首建 replay 分批 apply**：繁忙卷窗口期峰值缓解，仅在用户报告首建 OOM 后做。

### 明确不做（防止重复提案）
1. **进程合并**（Listary v6.3.5.94 路线）：Prism 管道协议健壮、权限隔离是资产，维持三进程。
2. **倒排/trigram 索引**：Everything 用多线程线扫达到目标且内存更省，Prism 已对齐此路线。
3. **v6 缓存格式**：Everything 1.5 的"带迁移"需要双格式读路径，Prism 单用户场景升版重扫的代价（几分钟）不值得双路径复杂度。
4. **属性索引（size/date）**：功能级决策，超出本轮范围。

---

## 4. 风险控制

- 每项独立提交，标注 F 编号；G1/G2/G3 可随时发布，G4 建议单独一轮回归（重点：拼音开关循环 + CWD 搜索 + 删除风暴 soak）。
- 全程不触 IPC 协议版本、不触 v5 缓存格式、零新依赖（G5 的 memchr 除外，且需审批）。
- 每批次收尾跑完整门禁（cargo test + cargo clippy + dotnet test + 安装包重建）。

## 5. 来源

- 代码：两个独立审读代理从零通读（Rust 18k 行 / C# 13k 行），未读 docs/ 与 git log。
- Everything：[1.5 Beta 页面](https://www.voidtools.com/everything-1.5/)、[What's New](https://www.voidtools.com/support/everything/whats_new/)、[索引算法 t=9463](https://www.voidtools.com/forum/viewtopic.php?t=9463)、[1.5 Beta 讨论 t=9787](https://www.voidtools.com/forum/viewtopic.php?t=9787)、[内存讨论 t=12117](https://www.voidtools.com/forum/viewtopic.php?t=12117)、[大库性能 t=15122](https://www.voidtools.com/forum/viewtopic.php?t=15122)、[多线程索引 t=10471](https://www.voidtools.com/forum/viewtopic.php?t=10471)、[属性索引 t=9874](https://www.voidtools.com/forum/viewtopic.php?t=9874)。
- Listary：[V7 官方页](https://www.listary.com/v7)、[V7 Beta 公告](https://discussion.listary.com/t/listary-v7-beta-is-here-the-launcher-now-recommends-plus-a-new-engine-multi-select-fresh-themes-updated-to-7-0-0-7-on-july-20/10259)、[官方 changelog](https://help.listary.com/changelog)、[beta changelog](https://help.listary.com/changelog-beta)。
- 其他：[StackOverflow 2B 文件搜索原理](https://stackoverflow.com/questions/32552353/how-exactly-everything-search-can-give-me-immediately-searchable-list-of-2bln)。
