# 内存收口 II 施工方案（2026-08-30）

> 交付对象：执行本方案的实现方（另一模型 / 另一次会话）。
> 本文件即施工说明书，包含**关键点在哪、怎么做、为什么这么做、总体原则**。
> 落地时请把本文件复制为 `docs/PRISM-MEMORY-PLAN-II-2026-08-30.md` 一并提交。

---

## 0. 背景与问题

`docs/PRISM-MEMORY-PLAN-2026-08-22.md` 收口后的实测基线：

| 测量点 | Prism | prism-core | indexer | 合计 |
|---|---|---|---|---|
| 单次搜索后立即 | 64.4MB | 2.7MB | 64.4MB | 131.5MB（瞬态） |
| 隐藏后 3 分钟 trim | 45.4MB | 2.6MB | **0.79MB** | 48.8MB ✓ |
| 进一步空闲稳态 | 22.4MB | 20.7MB | 2.0MB | 45.1MB ✓ |

commit `be29781`（G7c：stat 过滤下沉索引器）之后，用户实测：

| 进程 | 现在 |
|---|---|
| Prism | 32.8MB |
| prism-core | 2.6MB |
| **prism-indexer-service** | **150.2MB** |

且「空闲约 3 分钟后只掉到 ~80MB」，而不是基线里的 0.79MB。

**回归 100% 在 indexer**。前端 32.8MB 与 broker 2.6MB 都在历史正常区间。

---

## 1. 根因（两条，同一个来源）

### 根因 A：`VolumeIndex.stats` 是第三张按记录号全量分配的表

`hierarchy.rs:88-94`、`:117-121`：

```rust
pub(crate) struct RecordStat {
    pub size: u64,      // 8
    pub mtime: i64,     // 8
    pub ctime: i64,     // 8
}                       // = 24 B
```

`stats: Vec<RecordStat>` 与 `nodes: Vec<NodeSlot>`（`#[repr(C)]`，12 B/槽）**平行、
按 MFT 记录号稀疏索引**，在 `prepare_initial_capacity` 里一起 resize 到
`max_record + 1`（`hierarchy.rs:490-491`）——注意是**槽位数**不是活跃文件数。

固定每槽开销因此从 12 B 变成 36 B（**3 倍**）。本机 2 卷，按 150 − 64 ≈ 86MB 的
涨幅反推约 360 万槽。

附带缺陷：`finish_initial_build`（`hierarchy.rs:463-468`）对 `nodes`/`names` 做了
`shrink_to_fit`，**漏了 `stats`**，倍增增长留下的多余容量不还。

### 根因 B：填充 tick 每 5 秒把整张表拉回工作集，空闲修剪等于白做

`fill_stats_chunk_off_lock`（`indexer_runtime.rs:2536-2626`）由 maintenance tick
每 5 秒调一次（`indexer_runtime.rs:1559-1578`）。它的第一步是：

```rust
for record in 0..len as u32 {          // len = nodes.len().min(stats.len())
    let slot = &volume.nodes[record as usize];
    if ... || volume.stats[record as usize].is_known() { continue; }
```

即**从记录 0 线性扫完整张 nodes[] + stats[]**（本机约 360 万 × 36 B ≈ 130MB 的
顺序读）去找未知项，收敛后也照扫不误——`batch.is_empty()` 的提前返回发生在**扫完
之后**。

后果：

1. `trim_working_set()`（`indexer_runtime.rs:1485-1491`，`SetProcessWorkingSetSizeEx(-1,-1)`）
   在空闲 3 分钟时把工作集清空，**5 秒后就被这轮全表扫描重新拉回**。这就是
   「只掉到 80MB」而不是 0.79MB 的直接原因。
2. `fill_stats_chunk_off_lock` **不调 `touch_activity`**（这是当初 T7 的正确设计，
   为了不让后台活动阻塞修剪），所以它会在修剪后立刻重跑，没有任何抑制。
3. 首填阶段每拍都从 0 重扫已填部分，整体 O(n²)：1.2M 记录 / 20000 每拍 ≈ 60 拍，
   累计约 30 次全表扫描的无谓开销。

### 附带发现：G7c 的全索引覆盖今天其实是坏的

- 缓存命中启动走 `recompute_derived_counters`（`hierarchy.rs:928-936`），它重算
  `names_fingerprint` / `initial_name_bytes` / `present_slots`，**不 resize `stats`**。
- `ensure_slot`（`hierarchy.rs:876-885`）只在 `required > self.nodes.len()` 时增长，
  缓存载入后 `nodes` 已经是满长度，条件不成立，`stats` 保持长度 0。
- 于是 `record_stat`（`:890-893`）恒 `None` → `stat_matches_record`（`:1279-1297`）
  恒 `false` → **`size:` / `dm:` / `dc:` 查询在缓存启动后永久返回零结果**。
- 而 `fill_stats_chunk_off_lock` 的 `len = nodes.len().min(stats.len())` = 0，
  永远填不上，自己也修不好自己。

也就是说：**24 B/槽的常驻代价只有「全新建卷的那一次启动」才换来功能，其余启动是
纯付费不办事。** 这条事实是选择「删表」而不是「瘦身表」的决定性依据。

---

## 2. 总体施工原则

1. **删代码优先于加代码。** 本方案的主体是删除，不是优化。新增的只有一个预算计数器
   和一个 `needs_metadata()` 判定。
2. **不改 IPC 协议、不改缓存格式（v5）、不改 XAML、不改搜索排序。** 与
   `PRISM-MEMORY-PLAN-2026-08-22.md` 的约束一致。`stats` 本来就是 `#[serde(skip)]`，
   删它不动盘上格式，也不需要版本号。
3. **语义单点。** `filters.rs` 的 `stat_passes`（`:473`）是索引侧与 broker 候选侧
   共用的唯一判定函数，今天已经共用。改造只换「元数据从哪来」，**不碰 `stat_passes`
   本身**，判定语义逐条保持不变。
4. **每个改动点配一条可跑的检查。** 非平凡逻辑（预算截断、按需 stat 的过滤顺序）
   必须留断言。不加框架、不加 fixture。
5. **分两个提交**：`refactor(index): 删 stat 表 + 按需 stat` 与
   `perf(ui): 收口两处无界字典`。前者出问题可单点回退。
6. **不谎报达标。** 实测数字进文档，达不到就写达不到，按 `PRISM-MEMORY-PLAN-2026-08-22.md`
   末尾 Phase 2 的口径据实报告。

---

## 3. Part A：indexer —— 删 stat 表，改扫描内按需 stat

这是本轮的全部收益来源。预期 150MB → ≤65MB（回到 be29781 之前），空闲修剪恢复到
个位数 MB。

### A1. 删除全索引 stat 表

**`src/prism-core/src/hierarchy.rs`**

- 删 `RecordStat` 结构体及 `is_known()`（`:88-100`）。
- 删 `VolumeIndex.stats` 字段（`:117-121`）。
- 删 `record_stat`（`:890-893`）、`invalidate_stat`（`:895-899`）、
  `store_stat`（`:901-905`）。
- `VolumeIndex::new`（`:429-455`）去掉 `stats: Vec::new()`。
- `prepare_initial_capacity`（`:490-491`）去掉 `self.stats.resize(...)`。
- `ensure_slot`（`:884`）去掉 `self.stats.resize(...)`。
- 名字池压缩路径里对 `stats` 的重排/搬移一并删（搜 `stats` 确保零残留）。
- `memory_bytes()`（`:651-655`）去掉 stats 项 —— 见 A3 会顺手改这个函数。

**`src/prism-core/src/indexer_runtime.rs`**

- 删 `STATS_FILL_CHUNK`（`:2521`）、`unstatable_stat()`（`:2525-2531`）、
  `fill_stats_chunk_off_lock()`（`:2536-2626`）、`system_time_to_epoch()`（`:2628-2634`）。
- 删 maintenance tick 里的填充块（`:1557-1578` 整块，含它的 `spawn_blocking` +
  `tokio::select!` + 日志分支）。tick 恢复成「新卷探测 / 空闲修剪 / 内存趋势 /
  挂起重试 / 名字池压缩 / checkpoint」。
- 删相关测试里对 `store_stat` 的调用（`:3410` 附近的 stats-fill 集成测试整个删掉，
  它测的是被删掉的机制）。

**`src/prism-core/src/ntfs.rs`（重要，别漏）**

`WATCH_REASON_MASK`（`:12-26`）回退掉 G7c 加进去的四个 reason：

```rust
pub const WATCH_REASON_MASK: u32 = USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_RENAME_OLD_NAME
    | USN_REASON_RENAME_NEW_NAME;
```

**为什么**：`DATA_OVERWRITE / DATA_EXTEND / DATA_TRUNCATION / BASIC_INFO_CHANGE`
是 G7c 专门为「原地覆写不改名也要让 stat 失效」加的。索引不再携带 size/mtime，
这个需求消失。留着的代价很实：浏览器缓存、Windows Update 每次写入都成批产生 USN
记录，`replay: Vec<UsnRecord>`（每条含一个堆 `String`，`ntfs.rs:29-35`）无界累积
到整个 replay 窗口，apply 路径的分配 churn 也全是白花的。删掉直接降 USN 批次频率。

保留 `DATA_OVERWRITE` 等常量定义本身（其他地方若有引用），只从 mask 里摘掉；
若确认无引用则一并删。

### A2. 扫描内按需 stat（替代查表）

**`src/prism-core/src/filters.rs`**

给 `StatFilterSet`（`:420-436`）加一个判定：

```rust
/// 是否需要磁盘元数据。file:/folder: 从节点 flags 就能判，不需要 stat。
pub fn needs_metadata(&self) -> bool {
    !self.sizes.is_empty() || !self.modified.is_empty() || !self.created.is_empty()
}
```

`stat_passes`（`:473`）**一行不改**。

**`src/prism-core/src/hierarchy.rs` —— `QueryFilters`**

`stat_matches_record`（`:1279-1297`）改签名与实现：不再接 `&VolumeIndex` + `record`
查表，改接**已构造好的路径**：

```rust
/// stat 条件判定。file:/folder: 零 I/O；size/dm/dc 现场 fs::metadata。
/// 元数据取不到 = 不匹配（与 broker 候选侧 apply_stat_filters 同口径）。
pub(crate) fn stat_matches_path(&self, path: &str, is_directory: bool) -> bool
```

内部：`std::fs::metadata(path)` 成功则用 `meta.len() / meta.modified() /
meta.created()` 喂 `stat_passes`，失败则 `stat_passes(set, is_directory, None, None, None)`
（保持「未知 = 不过 size/date 条件、但 file:/folder: 仍生效」的既有语义）。

**`scan_slot_range`（`hierarchy.rs:1393-1456`）—— 判定顺序改造**

这是本方案唯一需要小心的地方。现在的顺序是：

```
name → root → exclusion → ext → stat(查表,零I/O) → path(构造路径) → 进堆
```

改成：

```
name → root → exclusion → ext
  → file:/folder:（读 slot.flags，零 I/O）
  → 若 has_path_filter || filters.stat.needs_metadata()：构造一次 path_for
  → path 过滤（若有）
  → size/dm/dc：预算内则 fs::metadata + stat_passes；预算耗尽则丢弃并置 truncated
  → 进堆
```

要点：

- **路径只构造一次**，path 过滤与按需 stat 复用同一个 `path`。原来的
  `has_path_filter` 局部变量升级为 `needs_path`。
- **`file:` / `folder:` 必须留在路径构造之前**。它们零成本，且能在
  `folder:` 这类查询上直接砍掉大部分候选，避免为它们付路径构造与 stat。
  为此把 `stat_passes` 里的 `file_only`/`folder_only` 两个判定在扫描侧提前做一次
  （直接读 `is_directory`），或者给 `StatFilterSet` 加一个只判 flags 的
  `dir_flag_passes(is_directory)`。**不要**因此在 `stat_passes` 里复制逻辑——
  抽一个小函数，两边共用。
- `acc.path_constructions` 计数照旧（现在也覆盖为 stat 构造的路径，语义仍是
  「构造了几次路径」，注释说明一下）。

**预算**

新增常量与计数器：

```rust
/// 单次搜索允许的现场 stat 次数上限。超出后不再 stat，剩余记录不进堆，
/// 结果集置 is_truncated —— 宁可少给也不能让一次 `size:>1gb` 打成全盘 I/O。
const STAT_CALL_BUDGET: u64 = 20_000;
```

- 通过 `search_volumes_impl` 往下传一个 `&AtomicU64`（已消耗数）。并行路径 8 线程
  （`SCAN_MAX_THREADS`，`:1321`）**共享同一个计数器**——一次 `fetch_add` 相对一次
  `metadata` 系统调用可以忽略，不要为省这个原子操作去做每线程配额，那会让
  串行/并行结果不一致。
- 超预算时：设一个 `budget_exhausted` 标志，`SearchOutcome.is_truncated = true`。
- `ScanAccumulator`（`:1343-1352`）加 `stat_calls: u64` 诊断计数，`merge_from`
  （`:1369-1387`）里累加，走 `IndexerResponse::Results` 现有的可选诊断字段模式
  （对齐 `path_constructions`，`indexer_ipc.rs:61-68`）回传一个
  `stat_calls: Option<u64>`。**这是新增可选字段，向后兼容，不算破协议。**

**语义变化（必须落账，见第 5 节）**

- 带名字词的 `size:` / `dm:` / `dc:` 查询：候选先被名字/扩展名收敛到很小的集合，
  覆盖与今天等价，且**修好了缓存启动零结果的 bug**。
- 裸 `size:>1gb`（无名字词、无 root）：只覆盖预算内的 20000 条记录，超出置
  `is_truncated`。这是刻意的取舍——用 20000 次磁盘 stat 换 86MB 常驻。
- broker 侧 `apply_stat_filters`（`ipc.rs:3155-3168`）**保持不动**。它对最终候选集
  再 stat 一次是幂等的正确性兜底，不要因为「索引侧已经过了」就删掉它。

### A3. `memory_trend_detail` 输出分项明细

`indexer.jsonl` 现有的 `index_memory_trend` 只给一个合计数（最近实测
89.4MB / 101.3MB / 118.1MB，volumes=2），无法定位是哪张表在涨——这次排查全靠读代码
反推。改掉，A/B 对比直接读日志：

- `VolumeIndex::memory_bytes()`（`hierarchy.rs:651-655`）拆成分项，或新增
  `nodes_bytes()` / `names_bytes()`。
- `ServiceState::memory_trend_detail()`（`indexer_runtime.rs:799-823`）的 format
  改成 `memory_bytes={} nodes={} names={} pinyin={} volumes={} ...`。

一行 format 改动，零常驻成本（每小时一次）。

### A4. 本次顺带修掉的既有缺陷

| 缺陷 | 怎么消失的 |
|---|---|
| 缓存启动后 `size:`/`dm:`/`dc:` 永久零结果 | 表没了，按需 stat 与启动路径无关，构造性消失 |
| `finish_initial_build` 漏 `shrink_to_fit(stats)` | 表没了 |
| 首填阶段 O(n²) 全表重扫 | 填充没了 |
| 空闲修剪被 5 秒 tick 抵消 | 全表扫描没了 |
| USN 批次被内容写入放大 | `WATCH_REASON_MASK` 回退 |

---

## 4. Part B：前端 Prism.exe（32.8MB）—— 只做两处收口，不做大改

**先说结论：前端不是这次回归的来源，且已经优化得相当到位，不要在这里花时间。**
已核实到位的：

- 结果行 1000 上限（`ViewModels\SearchViewModel.cs:15-16`）；容器虚拟化 + Recycling
  （`Controls\ResultList.xaml:9-10`），可见窗口 9 行（`Controls\ResultList.xaml.cs:19-20`），
  实际 realize 的容器 ≤10 个；行对象只持字符串，无位图。
- `IconCache` 有界 LRU 128 项（`Services\IconCache.cs:22`），位图按 DPI 实际尺寸
  32/48/64 px materialize（`Controls\ResultList.xaml.cs:763-771`）且全部 `Freeze()`，
  上限约 2MB；`FaviconCache` 上限 64（`Services\FaviconCache.cs:20`）。
- 隐藏路径 `ReleaseIdleMemory()`（`Windows\SearchWindow.xaml.cs:533-590`）：清结果、
  `ClearPathKeys()`、`ClearTransientCaches()`，3 分钟后
  `GCSettings.LargeObjectHeapCompactionMode = CompactOnce`（`:567-568`，
  **T4 已落地，不用再做**）+ `GC.Collect(2, Forced, blocking, compacting)`（`:569`）
  + `K32EmptyWorkingSet`（`:583`）。
- `StagingStore`（`Services\StagingStore.cs:18-23`）、`SuggestionService`
  （`:72-73`、64KB HTTP 缓冲）、窗口枚举（固定栈缓冲）全部硬上限。

只有两处**无界集合**，各一两行，顺手收掉：

**B1. `Services\WebIconProvider.cs:30` 的 `_resolved` / `_negative`**

按 origin 累积，**无容量上限**，唯一的清理是隐藏时的 `ClearTransientCaches()`
（`:109-115`）。长时间不隐藏的会话（常驻搜索框）会无界增长。
加容量上限：沿用 `IconCache` 的 LRU 口径，或最简单——超过阈值（如 256）整清。
后者足够，且代码最短。

**B2. `Controls\ResultList.xaml.cs:38` 的 `_iconFailedAttempts`**

每个失败的 `path@size` 一条，**只在 DPI 变化时清**（`:335`），进程生命周期内只增不减。
在隐藏路径里（和 `ClearPathKeys()` 同一处，`Windows\SearchWindow.xaml.cs:544` 附近）
一并 `Clear()`。失败重试计数本来就是「本次会话内不要反复重试」的语义，隐藏时清掉
不影响行为。

**不要动的**：`SettingsWindow.xaml`（1300+ 行）首次打开后 BAML/类型/样式机器永久驻留，
这是 WPF 固有成本；`CommandCatalog._snapshot` 是功能必需且随命令数线性，命令是几十条
量级；`_completeCache` 的第二份 1000 行副本在隐藏时已清。

**`prism-core` 2.6MB：无动作。**

---

## 5. 验证

按顺序，全部要跑：

1. **单元/静态门**
   - `cargo test`（`src/prism-core`）
   - `cargo clippy -- -D warnings`
   - `dotnet test src/Prism.Tests`
   - WPF Release 构建 0 警告

2. **改写 G7c 的既有 stat 测试**（`hierarchy.rs:1869-2010` 那一组）
   它们现在靠 `store_stat` 手工塞元数据，表删了必然编译不过。改写为按需 stat 版本：
   在 `tempdir` 里造真文件（大/小各一、一个目录），建真索引，断言
   `size:>1kb`、`dm:today`、`file:`、`folder:`、多 size 条件 OR 全部仍命中原来的
   期望结果。**逐条对齐原断言的期望值**，不要顺手改语义。
   新增一条：把 `STAT_CALL_BUDGET` 参数化（或用一个测试专用的小预算入口），
   断言超预算时 `is_truncated == true` 且结果条数被截断。

3. **重建内存门禁**
   `tools/bench/Invoke-RebuildMemoryGate.ps1`（`PRISM-MEMORY-PLAN-2026-08-22.md` T1
   建的脚本）跑**修改前 A 侧 / 修改后 B 侧**两组，留 artifacts。

4. **实机三进程采样**，对齐 `PRISM-MEMORY-PLAN-2026-08-22.md` 的四个测量点：
   单次搜索后立即 / 展开 1000 后 / 隐藏后 3 分钟 / 进一步空闲稳态。

   目标：

   | 指标 | 目标 |
   |---|---|
   | indexer 搜索后立即 | ≤65MB（回到 be29781 之前） |
   | indexer 空闲 3 分钟修剪后 | 个位数 MB（修剪恢复生效） |
   | 三进程「使用后立即测量」合计 | ≤100MB |

5. **功能回归（人工，必做）**
   - 缓存命中启动（不删缓存，直接重启服务）后立即测 `size:>1mb`、`dm:today`——
     **修改前应当零结果，修改后应当有结果**。这条是 A4 那个 bug 的验收。
   - 带名字词的 `报告 size:>1mb`、`ext:pdf dm:thisweek` 结果与修改前一致。
   - 裸 `size:>1gb` 有结果且标 truncated，响应时间可接受（预算内 20000 次 stat）。

6. **落账**
   - `docs/PRISM-MEMORY-PLAN-2026-08-22.md` **新增一节**记本轮 A/B 数字，
     不改历史表格。
   - `.trellis/spec/` 里记语义变化：**裸 stat 查询有 stat 调用预算，超出置
     `is_truncated`**；以及「索引不再携带 size/mtime/ctime，stat 过滤是现场 I/O」
     这条契约（避免下一个人再把它下沉回索引）。
   - 本文件复制为 `docs/PRISM-MEMORY-PLAN-II-2026-08-30.md`。

7. **dist / 安装包**随代码更新（沿用现有 `scripts/build-installer.ps1` 流程）。

---

## 6. 提交切分

| # | 提交 | 内容 |
|---|---|---|
| 1 | `refactor(index): stat 过滤改按需 I/O，删除全索引 stat 表` | A1 + A2 + A4，含测试改写 |
| 2 | `perf(index): USN watch mask 回退内容变更 reason` | A1 的 `ntfs.rs` 部分（可并入 1，但单独更好回退） |
| 3 | `chore(index): memory_trend 输出 nodes/names 分项` | A3 |
| 4 | `perf(ui): WebIconProvider 缓存加上限、隐藏时清图标失败计数` | B1 + B2 |
| 5 | `build(dist)` | 产物 |

每个提交单独跑一遍第 5 节的第 1 项。

---

## 7. 风险与回退

| 风险 | 判断 | 应对 |
|---|---|---|
| 裸 `size:` 查询覆盖变窄 | 已知取舍，用户已确认选择「按需 stat」 | truncated 标志如实回传；spec 落账 |
| 20000 次 stat 的延迟 | 冷缓存下可能到秒级 | 预算是常量，实测后可调；`stat_calls` 诊断计数就是为了调它 |
| 并行路径共享原子计数导致结果不确定 | 超预算时哪些记录被砍取决于线程调度 | 只影响「已经标了 truncated」的结果集，可接受；测试不要断言超预算时的具体条数，只断言 truncated |
| 过滤顺序改动引入回归 | 中风险，是本方案唯一需要小心的地方 | 第 5 节第 2 项的逐条对齐断言就是这条的防线 |

回退：提交 1 单独 `git revert` 即可回到 G7c 行为（含它的 150MB 和缓存启动 bug）。
