# Prism P4 架构方向调研与实施方案（2026-08-19）

本文针对交接文档 `docs/PRISM-AUDIT-HANDOFF-2026-08-18.md` 第 E 节的四项 P4 架构演进方向逐项调研，结合 `docs/PRISM-FRESH-AUDIT-2026-08-19.md`（下称"新审计"）的结论，对照 voidtools Everything / Listary 的公开做法，给出**是否实施**的判定、影响面、风险与详细实施方案。

调研基线：`feature` 分支提交 93f0941（2026-08-19）。此时交接文档 P0–P3 全部 25 项已完成（见 git log 811a69c..93f0941）；新审计的 B1–B4（N1/N2/M1/M2/S1/S2/S3/S5）**均未实施**。本文所有结论以该基线的代码实况为准。

---

## 0. 结论速览

| P4 项 | 判定 | 一句话理由 |
|---|---|---|
| P4-1 每卷独立锁 + arc-swap 快照发布 | **关闭**（留量化触发器） | R-B1/R-B2 已修 + S1 落地后，长持锁只剩每批 USN apply（毫秒级）；arc-swap 的 COW 语义要求每批 USN 克隆整卷（百 MB 级 memcpy），净亏损 |
| P4-2 broker↔indexer 二进制协议 | **不做** | 热路径 JSON 双侧编解码约 1–3ms，相对扫描与 50ms 防抖是噪声；Everything 1.5 SDK 自己就走命名管道 JSON；Listary gRPC 是反面教材；且 JSON 行协议被 `scripts/` 验收脚本依赖 |
| P4-3 SearchViewModel / SearchWindow 拆分 | **部分实施**（P4a/P4b/P4c，共 3 小步） | 只拆有测试收益的部分：IFolderPicker 注入（解锁 copy/move 分支测试）、RefreshAsync 可测化（与 C-D10 合并）、WebSearchCoordinator 抽取；ActionPanel 与 SearchWindow.xaml.cs 不拆 |
| P4-4 首建真流式 + checkpoint 分卷落盘 | **按新审计 M1/M2/S2 执行，P4 原案否决** | 真流式被 M1 的 ponytail 决策否决（父目录先行问题，~60MB 换高复杂度）；"分卷"落地为**单文件内逐卷短锁序列化**（M2）+**逐卷缓存生效**（S2），不是分卷多文件——Everything 也是单一 Everything.db |

总体建议：P4 不新增独立实施批次。Rust 侧收益全部并入新审计 B1–B4；C# 侧三项小拆分（P4a/P4b/P4c）独立无协议影响，可与 B 批次并行或穿插。

---

## 1. 对标调研：Everything / Listary 怎么做

### 1.1 落盘与按卷粒度

- Everything 全部索引在**单一 Everything.db**文件里，不按卷拆文件；用户要求拆库（[forum t=16083](https://www.voidtools.com/forum/viewtopic.php?t=16083)、[t=10108](https://www.voidtools.com/forum/viewtopic.php?t=10108)）官方答案是"没有，想拆就跑多实例多配置"。
- 但"卷集变化"的代价是**按卷**的：官方文档（[Indexes](https://www.voidtools.com/support/everything/indexes/)）明确新增/移除卷触发的是"fast reindexing"——只采集新卷数据、复用已有索引属性（[t=16456](https://www.voidtools.com/forum/viewtopic.php?t=16456)、1.5a 进一步在 reindex 时保留属性 [t=16985](https://www.voidtools.com/forum/viewtopic.php?t=16985)）。**即：单文件存储 + 按卷失效/重建，正是新审计 S2 的形状，而不是 P4 原案"分卷文件"的形状。**
- 保存性能：1.4 保存会阻塞（680MB 约 4 秒，新审计已引）；1.5 把库整体驻内存、周期性自动保存，速度大幅改善（[t=9793](https://www.voidtools.com/forum/viewtopic.php?t=9793)、[t=12310](https://www.voidtools.com/forum/viewtopic.php?t=12310)、[t=12651](https://www.voidtools.com/forum/viewtopic.php?t=12651)——1.5 索引更新在后台，搜索不被阻塞）。Prism 现状（锁外 clone + 抽样 validate + 原子替换）已优于 1.4，M2 之后接近 1.5 的"保存不扰搜索"。

### 1.2 IPC 协议

- Everything 1.4 IPC 是自定义二进制（窗口消息 + `EVERYTHING_IPC` 结构体，[SDK IPC 文档](https://www.voidtools.com/support/everything/sdk/ipc/)、[t=5119](https://www.voidtools.com/forum/viewtopic.php?t=5119)）；**1.5 SDK 改走命名管道并支持 JSON 查询**（[t=15853](https://www.voidtools.com/forum/viewtopic.php?t=15853)）。voidtools 自己在"本地单机、结果集 Top-K"场景下认为 JSON 足够——与 Prism 场景相同。
- Listary v6.3.6.99 因**进程间 gRPC 崩溃**把 listary-core 并回主进程（官方公告 t=9405，新审计已引）。教训不是"二进制坏"，而是：IPC 层越重，崩溃面越大。Prism 的 JSON-lines + 协议版本握手 + 有界行读 + 连接上限是轻而稳的形态，换协议的收益必须先证明序列化是瓶颈。

### 1.3 索引并发模型

- Everything 无公开源码，但官方口径是"多线程 SIMD strstr 扫全库"（t=9463）+ 保存/重建期间继续服务旧库。它是共享内存库 + 细粒度锁 + 周期快照落盘，**没有走"每次变更发布不可变快照"路线**。这与下文 P4-1 的判定一致：高频 USN 增量场景下，原地小写 + 短锁比 COW 快照发布更便宜。

---

## 2. 逐项调研与方案

### P4-1 每卷独立锁 + arc-swap 式快照发布

> 原案：消除全局 `RwLock<Option<IndexState>>`（R-B1/B2 的根因），需 `arc-swap` 依赖或手写 `Arc<AtomicPtr>`。

#### 现状复核（基线 93f0941 实测）

数据结构：`IndexState { volumes: Vec<VolumeIndex>, generation, events_since_checkpoint }`（hierarchy.rs:319-324），由 `ServiceState.index: RwLock<Option<IndexState>>` 持有（indexer_runtime.rs:156）。生产代码全部锁位点：

| 位点 | 行 | 持锁时长 |
|---|---|---|
| search 全库扫描（读） | :605 | 10–40ms（N2 后 <10ms） |
| watch_volume USN apply + compact（写） | :1416-1448 | apply 每批毫秒级；compact 数百 ms（S1 目标，未做） |
| checkpoint / persist_first_build clone（读） | :1471-1474 / :1150-1153 | memcpy 40–190MB，百 ms 级（M2 目标，未做） |
| rebuild_pinyin 快照 clone（读） | :396 | 同上（低频） |
| merge_and_publish 单卷换入（写） | :489-513 | 微秒（指针替换） |
| publish 整态替换（写） | :532-557 | 微秒 |
| 其余（generation/status/游标读/计数扣减） | :453/:463/:533/:927/:1388/:1485 | 微秒 |

关键事实：R-B1（拼音重建出锁）、R-B2（单卷重建）已修。**剩余的长持锁只有三处：USN apply（毫秒级，本质必要）、compact（S1 将移出）、checkpoint clone（M2 将移出）**。原案所称"R-B1/B2 的根因"已经不存在。

#### 必要性判定：关闭

两条技术路线都算不过账：

1. **arc-swap / 不可变快照发布**：读侧零锁的前提是写侧每次变更产出新快照。USN apply 是每秒~每 250ms 一批的高频原地写；COW 意味着每批克隆整卷 `nodes + names`（3M 记录卷约 100MB，memcpy 30–60ms）——把"搜索等写锁几毫秒"换成"每批百 MB 分配+拷贝"，吞吐净亏。快照发布适合低频写、高频读的数据（配置、路由表），不适合索引增量流。
2. **每卷独立 RwLock（原地写，不做 COW）**：只隔离"卷 A 写 vs 卷 B 读"。但 `search_volumes` 扫全部卷，必然要拿卷 A 的读锁——同卷的读者/写者互斥一点没少；跨卷隔离的收益仅在"多卷机器 + 单卷写风暴 + 搜索恰好耗在其他卷"这一窄场景成立。代价却是：`search_in_root_filtered`、`index_cache::save(&IndexState)`、`validate`、pinyin identity、v5 序列化顺序全部改签名，约 600–900 行波及，且必须保证 v5 字节流不变。风险收益比不成立。

Everything 用共享库 + 细粒度锁达到亚毫秒搜索，佐证"锁不是问题，长持锁才是"——而长持锁已被 R-B1/S1/M2 逐个消灭。

#### 影响与风险（若强行实施）

影响面：indexer_runtime.rs 全部锁位点（上表）、hierarchy.rs `search_in_root_filtered`/`search_volumes`、index_cache.rs `save/validate/load`、pinyin_sidecar identity 计算；`Cargo.toml` 加 arc-swap（违反交接约束 1，需人工批准）。风险：generation 语义分裂（全局单调 vs 每卷版本）、v5 缓存字节序回归、拼音 identity 含 volume_count 与部分发布的耦合（S2 同款坑）、锁序错误引入死锁面。**结论：不做。**

#### 留档的触发器（满足才重开调研）

S1 落地后，用 S5 的小时级日志 + 临时加的 `apply_lock_wait_us`/`search_lock_wait_us` 直方图，在删除风暴场景（大仓库 `git clean`）实测：若**搜索读锁等待 P99 持续 >50ms** 且归因于同卷 USN apply，先试"apply 分片提交"（把一批 USN 拆成多次短写锁）；仍不达标再评估每卷锁。arc-swap/COW 路线在任何实测下都不推荐。

---

### P4-2 broker↔indexer 换长度前缀二进制协议（postcard）

> 原案：消除每键 JSON 序列化开销。

#### 现状复核

热路径：前端击键（50ms 防抖）→ broker `Request::Search`（ipc.rs:761-784 解析）→ `indexer_client::search_in_root`（持久连接 :201-214，或信号量=2 的一次性连接 :291-313）→ 服务端 spawn_blocking 搜索（indexer_runtime.rs:1713-1732）→ `serde_json::to_vec` 单行回包（:1767-1778）→ broker `serde_json::to_writer` 复用缓冲回前端（ipc.rs:794-804）。postcard 目前只用于磁盘（index_cache.rs、pinyin_sidecar.rs），活体 IPC 全 JSON。

量级：请求 `max = clamp(3 × result_slots, 1, 1000)`（ipc.rs:1738-1742，上限 `MAX_SEARCH_RESULTS=1000` indexer_ipc.rs:8）。每 `IndexHit` = name + path + is_directory + MatchMetadata（hierarchy.rs:65-70），JSON 约 150–250B/条（serde_json 不转义非 ASCII）。最坏 1000 条 ≈ 200KB，典型 150 条 ≈ 30KB。双侧 JSON 编解码合计约 1–3ms；对比：单线程全扫 10–40ms（N2 后 <10ms）、击键防抖 50ms、8MB 行上限。**序列化占搜索预算 <5%，绝对值在 UI 不可感知区间。**

#### 必要性判定：不做

1. 量级不构成瓶颈（上表）；N1/N2 才是延迟大头，先把 B1/B2 做完再谈。
2. 同行先例：Everything 1.5 SDK 本地场景自己选了命名管道 JSON（t=15853）；Listary 的教训是重 IPC 崩溃面（t=9405）。Prism 的 JSON-lines + 版本握手 + 有界读是已验证的稳态，动它没有收益只有风险。
3. 隐性成本：`INDEXER_PROTOCOL` 2→3 升版 + 交接约束 2 的显式报错兼容矩阵 + `scripts/indexer-pipe-roundtrip-test.ps1` 等验收脚本全部重写（它们说 JSON 行协议）+ 双端联调回归。为一个 <5% 的优化项付全额协议升级成本。

#### 影响与风险（若强行实施）

影响面：indexer_client.rs 全部请求/响应、indexer_runtime.rs `write_response`、indexer_ipc.rs 协议常量、全部集成测试与 scripts/*.ps1。风险：新旧组合静默降级（约束 2 红线，历史上出过事故）；长前缀帧与现有 BoundedLineReader 行语义冲突需双轨。**结论：不做。**

#### 留档的触发器

满足其一再评估：(a) 未来加入 size/date 等元数据过滤使典型响应 >1MB；(b) N1/N2 落地后 CPU 剖析显示 serde_json 占搜索总耗时 >10%；(c) 出现前端之外的高频第三方消费者（届时也优先 JSON 流式而非二进制）。

---

### P4-3 SearchViewModel / SearchWindow.xaml.cs 拆分

> 原案：抽 `WebSearchCoordinator` / `ActionPanelController` / `IFolderPicker` 注入；推广 `SearchWindowFocusPolicy` 先例。

#### 现状复核

- `SearchViewModel.cs` 1251 行。责任分布：搜索编排 ~280 行、动作面板 ~200 行（458-616 + 703-743）、web 搜索 ~148 行（1069-1216，自成一块）、世代刷新 ~70 行（631-701）、结果选择执行 ~130 行（325-456）、其余为接线。
- `FolderBrowserDialog` 全解决方案唯一一处：SearchViewModel.cs:621（`PickDestinationFolder` 618-629，唯一调用方 `RunActionOnAsync` :573 的 copy_to/move_to 分支）。**这是唯一直接卡死可测性的 UI 耦合**：该分支零测试覆盖。
- `RefreshAsync`（631-667）用真 `Task.Delay` 做 3s 超时，不走已有 `ISearchScheduler` 缝，超时路径不可测；且其 TCS 覆盖竞态即交接 P3 的 C-D10（**尚未实施**，P3 批次还在待办）。
- 已有缝与先例：`ISearchClient/IDebounceTimer/ISearchScheduler/IWindowActivator/ISuggestionService`（SearchAbstractions.cs）、`HostScopeController`（421 行，已从窗口抽出）、`SearchWindowFocusPolicy`（12 行）。`WebSearchCoordinator`/`ActionPanelController`/`IFolderPicker` 均不存在。
- 测试现状：SearchViewModelTests 36 例覆盖前缀缓存/世代竞态/窗口模式/动作面板状态机等；**未覆盖：copy/move 分支、rename 流、RefreshAsync 超时、FilterActions 防抖、web dispatcher 分支**。
- `SearchWindow.xaml.cs` 1004 行，但性质是"事件接线 + 薄转发"：最大块是上下文菜单 108 行、键盘 switch 75 行、动画布局 ~130 行。逻辑真身已在 VM / HostScopeController / FocusPolicy 里。

#### 判定：部分实施——只拆有测试收益的，三小步

**P4a：IFolderPicker 注入**（约 40 行改动，半天）

1. `Services/SearchAbstractions.cs` 加接口：
   ```csharp
   public interface IFolderPicker
   {
       /// 返回所选目录；取消返回 null。
       string? PickFolder(string? description);
   }
   ```
2. 新增默认实现 `WinFormsFolderPicker`（包住现 618-629 的 `FolderBrowserDialog` 调用；调用方在 UI/STA 线程，保持同步签名，与现状行为逐字段一致）。
3. `SearchViewModel` 构造函数加可选参数 `IFolderPicker? folderPicker = null`（缺省 `new WinFormsFolderPicker()`，照抄 `ISuggestionService` 的可选注入先例）；`PickDestinationFolder` 改为转发。
4. 新增测试（SearchViewModelTests，Fake 已有 FakeSearchClient 模式）：
   - FakePicker 返回 null → copy_to 动作走到取消文案、不调 `ExecuteAsync`；
   - 返回路径 → `ExecuteAsync` 收到该 dest；
   - 返回路径与上次不同 → 不落缓存错路径。
5. 验证：`dotnet test`；手动跑一次 copy_to 确认对话框行为不变。

**P4b：RefreshAsync 可测化**（与 C-D10 合并做，约 20 行）

C-D10 修 TCS 覆盖竞态（局部变量持有 + `Interlocked.CompareExchange` 归还）时，顺手把 `Task.Delay(3000)` 换成 `ISearchScheduler.Delay`（缝已存在）——注意这是 C-D10 修复的**直接波及面**，不属"顺手重构"。新增测试：连发两次 mutation，两次刷新都走信号路径非超时路径；调度器快进验证超时路径也能触发。顺序上必须排在 P3 批次（C-D10）里或其后。

**P4c：WebSearchCoordinator 抽取**（约 150 行搬家 + 30 行缝，1–1.5 天）

1. 新建 `ViewModels/WebSearchCoordinator.cs`：搬 `RunWebSearchAsync/BuildWebRows/BuildWebMatchSpans`（VM 1069-1216）及 `_suggestionCts/_suggestionSeq/_webEngines` 字段；依赖 `ISuggestionService` + 注入的 `Action<Action> marshalToUi`（VM 传入 `a => Dispatcher.BeginInvoke(a)`，测试传 `a => a()`）。
2. VM 保留 `Suggestions` 集合与对外方法签名（`UpdateWebSettings/CancelSuggestions` 转发），`WebIconFlickerTests` 应零改动通过（等价性守护）。
3. 新增测试：coordinator 独立验证 seq 取消（新查询作废旧 suggestion 任务）与 marshal 分支被调用——这两条现在测不到。
4. 不改行为：`Task.Run` 内部结构、GBK 解码、engine 隔离逻辑原样搬。

**不做的部分及理由**

- `ActionPanelController`：~200 行状态机与 VM 的 selection/execution 字段交织，抽出要带 6+ 个回调，接口比代码长；现状已有 36 例测试覆盖状态机主体。触发器：动作系统要加新功能或 `FilterActions` 防抖出 bug 时再抽。
- `SearchWindow.xaml.cs` 拆文件：纯接线薄层，拆了没有测试收益只有 churn。维持"触碰时抽纯策略类"的既有规则（FocusPolicy/HostScopeController 模式）。

#### 影响与风险

影响面：仅 `src/Prism`（VM/Services/Tests），不触协议与缓存。风险：P4a 近零（默认实现保行为）；P4b 与 C-D10 绑定，遵守其验证要求；P4c 是搬家重构，靠 `WebIconFlickerTests` 等价性守护，风险低-中。三项均可独立提交回滚。

---

### P4-4 首建真流式 + checkpoint 分卷增量落盘

> 原案：R-C1/R-C2 的"彻底版"。

#### 现状复核

- 首建：R-C1 已做（UsnRecord name 池化，7a63f2f），峰值已近砍半；剩余尖峰是 `names` 池倍增瞬间（M1 将用容量预留消除，~30 行）。真流式（边枚举边 upsert）受制于 MFT 记录序不保证父先于子，延迟重试列表会放大——新审计 M1 已做 ponytail 决策：**只预留，不重构建库流程**。
- checkpoint：现状仍是"读锁内 clone 整个 IndexState → 锁外 save"（:1471-1475），写侧 2× 峰值；M2 方案（单文件内逐卷短读锁序列化 + generation 核对重试 + 3 次回落 clone 路径）就是"分卷增量落盘"的正确落地形态——**postcard 字节流逐字节不变，load 侧零改动，不升 v5**。
- 缓存生效粒度：现状 `validate_checkpoints`（:1309-1329）任一卷 journal 失配即整份弃缓全盘重扫；S2 改逐卷判定 + `CachedLoad::Partial`。这正是 Everything "fast reindexing" 的按卷语义（见 §1.1）。

#### 判定：P4 原案两种"彻底版"均否，由 M1/M2/S2 接管

- **真流式**：不做。理由同新审计 M1（复杂度 vs ~60MB 峰值不成立），R-C1+M1 后峰值已从 ~350-400MB 降到 ~150MB（3M 记录卷），继续压只剩边际收益。
- **分卷多文件（每卷一个缓存文件）**：不做。三条理由：(1) 同类标杆 Everything 就是单一 db，按卷拆库是用户求了多年没给的功能（t=16083/t=10108）；(2) v5→v6 格式升版会在升级当天触发一次全盘重扫——恰好是这项改造想消灭的痛；(3) S2 在内存侧已拿到"只重建坏卷"的全部收益，磁盘侧拆文件只多付复杂度不多拿收益。
- **要做的是 M2 + S2**（已在新审计 B4 排期），实施要点与风险以新审计 §3 为准，本文补充两点落地细节：
  - M2 的 `SnapshotView` 序列化包装必须保证 `RwLockReadGuard` 不跨 `serialize_element` 存活（每卷取放），且 envelope 头（generation/events_since_checkpoint/volume_count）在首卷前一次性快照；卷间 generation 变化即中止重试，3 次后回落现有整态 clone 路径——回落路径就是现在的代码，天然安全网。
  - S2 的 `CachedLoad::Partial` 发布后，pinyin `index_identity` 含 volume_count，部分发布→重建卷并入→sidecar 会多触发一次重建，正确但慢一轮，可接受；集成测试必须含"缓存 2 卷、现场 3 卷"与"缓存 3 卷、现场 2 卷"两个方向。

#### 影响与风险

见新审计 M2/S2 条目（风险：中）。额外约束：M2 不得改变 v5 字节流（用"新旧路径写同一索引、字节相等"的测试锚定）；S2 动 `CachedLoad` 枚举属内部类型，不触 IPC 协议。

---

## 3. 总批次与顺序建议

| 批次 | 内容 | 依赖 | 风险 |
|---|---|---|---|
| B1–B4 | 新审计既定（N1+S3+S5 → N2 → S1+M1 → M2+S2） | 无（P4-4 的正式落地就是 B3/B4） | 见新审计 |
| P4a | IFolderPicker 注入 + 测试 | 无，可随时（建议先做，最小） | 近零 |
| P4b | C-D10 修复 + RefreshAsync 走 ISearchScheduler | 排入 P3 批次一起 | 低 |
| P4c | WebSearchCoordinator 抽取 + 测试 | 无硬依赖，建议在 P4a 后 | 低-中 |

验证门槛沿用交接文档 §4（cargo test / dotnet test / 脚本验收 / 安装包重建）。P4 三小步不触协议与缓存格式，前后端可独立发布。

---

## 4. 关闭项重开条件汇总（防止重复调研）

| 关闭项 | 重开条件 |
|---|---|
| P4-1 arc-swap / 每卷锁 | S1 落地后实测：删除风暴下搜索读锁等待 P99 >50ms 且归因同卷 USN apply；先试 apply 分片，仍不达标才重开 |
| P4-2 二进制协议 | 响应典型 >1MB（元数据过滤上线）/ 剖析 serde 占搜索耗时 >10% / 高频第三方消费者出现 |
| P4-4 真流式首建 | 用户级机器首建峰值被证实为真实痛点（当前是理论数字）且 M1 预留后仍不达标 |
| P4-4 分卷多文件缓存 | 出现"单卷缓存 >500MB 级"用户群，整文件写入时长成为 checkpoint 主导成本 |
| P4-3 ActionPanelController | 动作系统加新功能或 FilterActions 出缺陷 |
| P4-3 SearchWindow 拆文件 | 维持"触碰时抽策略类"规则即可，无重开条件 |

---

## 5. 调研来源

代码基线：`feature` @ 93f0941，两轮代理通读（Rust 核心 / WPF 前端）+ 主会话复核关键段（indexer_runtime.rs merge/publish/checkpoint、Cargo.toml、C# 序列化点）。

Everything：[Indexes 文档](https://www.voidtools.com/support/everything/indexes/)（fast reindexing 按卷）、[SDK IPC 文档](https://www.voidtools.com/support/everything/sdk/ipc/)、[Troubleshooting](https://www.voidtools.com/support/everything/troubleshooting/)；论坛 [t=9793](https://www.voidtools.com/forum/viewtopic.php?t=9793)（1.5）、[t=12310](https://www.voidtools.com/forum/viewtopic.php?t=12310)（保存性能/周期保存）、[t=12651](https://www.voidtools.com/forum/viewtopic.php?t=12651)（1.5 后台索引不阻搜索）、[t=16083](https://www.voidtools.com/forum/viewtopic.php?t=16083) / [t=10108](https://www.voidtools.com/forum/viewtopic.php?t=10108)（拆库不可）、[t=16456](https://www.voidtools.com/forum/viewtopic.php?t=16456) / [t=16985](https://www.voidtools.com/forum/viewtopic.php?t=16985)（reindex 复用属性、只采新卷）、[t=15853](https://www.voidtools.com/forum/viewtopic.php?t=15853)（1.5 SDK 命名管道）、[t=5119](https://www.voidtools.com/forum/viewtopic.php?t=5119)（1.4 二进制 IPC）。

Listary：官方 changelog / 论坛 t=9405（gRPC 崩溃并回进程）、t=8875（9.65GB 泄漏）——转引自新审计 §6（其内有直链）。

其他：HN item=46938615（1.5 库驻内存）。

---

*调研执行：ZCode（两个并行探查代理 + 主会话核实 + 联网对标），2026-08-19，基线 93f0941。*
