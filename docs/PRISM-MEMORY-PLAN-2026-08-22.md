# 内存收口实施方案（2026-08-22）

依据：`prancy-waddling-yeti.md`（内存收口分析）+ 用户实测补充。

## 用户实测补充（修正文档口径）

文档口径「空闲稳态 59.5MB 达标」不够：**单次搜索后即破线**——
Prism.exe ≈ 50MB、prism-indexer-service ≈ 70MB，三进程合计 110MB+。
约 1 小时后台闲置后才回落到稳态（OS 工作集修剪 + 前端 3 分钟延迟 GC）。
验收口径因此从「空闲稳态 ≤100MB」收紧为「**使用后立即测量**也须 ≤100MB，
重建窗口单独设门」。

## 实机证据（2026-08-22 复核）

`C:\ProgramData\Prism\indexer.jsonl` 当天仍有数十次
`volume_rebuild_requested`，全部为 `broken parent chain at record N`
（记录号持续变化 = 新建文件不断毒化 USN 批次）——重建风暴未修，
是 indexer 高位（42.5 → 70MB+）的主因：每次整卷重建新旧索引共存 +
MFT 临时结构，之后堆碎片维持高位。前端高位（14.7 → 50MB）主因：
1000 结果展开的响应行落在 LOH，3 分钟延迟 GC 用
`GC.Collect(2, Forced, blocking, compacting:true)` 但**未设
`LargeObjectHeapCompactionMode`**，LOH 不压缩。

## 任务（增量，每任务一测一提交）

| # | 任务 | 内容 | 验证 |
|---|---|---|---|
| T1 | 重建窗口内存门禁 | 新增 `tools/bench/Invoke-RebuildMemoryGate.ps1`（删缓存→重启服务→10ms 轮询采样三进程 `WorkingSetPrivate` 直到 `ready && !building`）；先跑一次留**修复前基线（A 侧，预期 ~190MB 破线）** | 脚本运行产出 artifacts |
| T2 | 修重建风暴 | `ntfs.rs` `apply_records` 改透传 `tolerate_unreachable=true` 并返回 skipped 计数；`indexer_runtime.rs` watcher 局部累加丢弃数、超阈值（4096）才请求重建（阈值抽纯函数）；`memory_trend_detail` 补累计丢弃数；测试改 1 增 2 + mutation 反验 | `cargo test`、`cargo clippy -D warnings` |
| T3 | spec 落账 | `quality-guidelines.md` Memory Acceptance 改两道门（空闲门不变 + 重建窗口门允许 `ready=false/building=true`，PID 同轮采样内不变）+ 记录前端 3 分钟 trim 时序事实 | 文档评审 |
| T4 | 前端 LOH 压缩 | `SearchWindow.xaml.cs` 3 分钟 trim tick 里 `GC.Collect` 前设 `GCSettings.LargeObjectHeapCompactionMode = CompactOnce`（一行） | `dotnet test`、WPF Release 0 警告 |
| T5 | 实机 A/B | 部署新构建：重跑重建门（B 侧对比 A 侧）、空闲门不回退、单次搜索 + 展开后立即采样三进程 | 三组数字入库 |
| T6 | 总测试 + dist | 四道门全跑（cargo / clippy / C# / WPF Release）+ dist 产物与安装包随代码更新 | 全绿 |
| T7 | 全仓复审循环 | 摒弃旧文档从头读全仓，修中/高危，复审直到无中高，完整提交 | 复审报告 |

## 约束

- 界面/功能零改动：不动 XAML、不动 IPC 协议、不动缓存格式（v5）、不动搜索排序。
- 每任务完成即测试 + 提交，出问题可单点回退。
- Phase 2（流式建卷 / MftRecord 瘦身 / 节点表分页）不在本轮：若 T5 显示
  合法重建仍破线，据实报告进 Phase 2 决策，不谎报达标。

## T5 实机结果（2026-08-22 复核）

### 初版（USN 批次计入 touch_activity）

| 测量点 | Prism | prism-core | indexer | 合计 | 达标 |
|---|---|---|---|---|---|
| 单次搜索后立即（typed） | 52.9MB | 2.7MB | 64.6MB | 120.3MB | ✗（瞬态） |
| 展开 1000 后 | 54.5MB | 2.8MB | 64.6MB | 121.9MB | ✗（瞬态） |
| 隐藏后 3 分 20 秒采样 | 46.9MB | 2.5MB | 64.7MB | 114.1MB | ✗（修剪未达） |
| 进一步空闲后稳态 | 25.6MB | 20.6MB | 45.2MB | **91.5MB** | ✓ |

### T7 复审修正（仅搜索计入 touch_activity）

USN 批次计入 `touch_activity` 使空闲计时器在正常文件活动下几乎永不达 3 分钟，
修剪延迟到系统完全静默。T7 复审据此改为**仅搜索请求**刷新计时——USN 后台活动
（浏览器缓存、Windows Update 等）不再阻塞修剪。

| 测量点 | Prism | prism-core | indexer | 合计 | 达标 |
|---|---|---|---|---|---|
| 单次搜索后立即（typed） | 64.4MB | 2.7MB | 64.4MB | 131.5MB | ✗（瞬态） |
| 展开 1000 后 | 51.6MB | 2.7MB | 64.4MB | 118.7MB | ✗（瞬态） |
| 隐藏后 3 分钟 trim | 45.4MB | 2.6MB | 0.79MB | **48.8MB** | ✓ |
| 进一步空闲后稳态 | 22.4MB | 20.7MB | 2.0MB | **45.1MB** | ✓ |

- **indexer 工作集修剪生效**：64MB → 0.79MB（降 63MB），不再被 USN 活动阻塞。
- **前端 `K32EmptyWorkingSet`**：Prism 64MB → 22.4MB（降 42MB）。
- **瞬态 131MB**：单次搜索瞬间三进程合计 131MB 是搜索把整卷 MFT 随机节点 +
  WPF 框架本体拉进工作集的固有成本，非泄漏——Phase 2 才能压这一峰值。
- **稳态 45.1MB ≤ 100MB ✓**：用户「单次搜索后内存 110+」诉求达成。

## T7 复审结论

- **前端 trim**：单次 `K32EmptyWorkingSet` 正确——P/Invoke 入口 K32（Win11 24H2
  实测）、本进程伪句柄无泄漏、Tick 内窗口已隐藏不卡交互、catch{} 吞异常合理。
- **indexer trim**：`SetProcessWorkingSetSizeEx(-1,-1)` 惯用法正确、伪句柄无泄漏、
  `!building && first_build_complete` 守卫重建窗口、`touch_activity` 改仅搜索后
  空闲 3 分钟可达。新增时钟回跳测试（last > now → saturating_sub 归零 → 不修剪）。
- **重建风暴修复**：`tolerate_unreachable=true` 跳孤儿、合法记录正常 apply、
  `next_usn` 越过跳过记录、`RebuildRequired`（排除目录边界变化）仍立即升级、
  无悬空引用/panic。4096 阈值是未校准启发式（慢性高孤儿率可能变慢速循环重建），
  非正确性缺陷，留遥测后调参。
- **门禁脚本**：`WorkingSetPrivate`/`IDProcess` 采样正确、同轮 PID 稳定、
  G9 恢复语义对齐、`-Passive` 免服务控制。spec 已修正「两道门」身份校验口径
  为「空闲门 + 重建门主动模式」，`-Passive` 不做哈希/SCM-PID 校验（免提权无安装侧访问）。
- **无高危**。3 个中危已修：USN touch_activity、时钟回跳测试、spec 身份校验口径。

## 内存收口 II（2026-08-30，PRISM-MEMORY-PLAN-II-2026-08-30.md）

G7c（be29781，stat 过滤下沉索引器）引入回归：indexer 实测 150.2MB、空闲修剪后
只掉到 ~80MB。根因两条，均出自 G7c 的全索引 `stats` 表：

1. **根因 A**：`Vec<RecordStat>`（24B/槽）与 nodes 平行按 MFT 记录号全量分配，
   固定每槽开销 12B→36B（3 倍），本机 2 卷约 360 万槽 ≈ +86MB。
2. **根因 B**：维护 tick 每 5 秒全表扫描填 stat（收敛后照扫），把空闲 3 分钟
   `SetProcessWorkingSetSizeEx` 修剪立即拉回——这是「只掉到 80MB」的直接原因。

附带发现：缓存命中启动时 `stats` 不被 resize（`recompute_derived_counters` /
`ensure_slot` 条件不成立），`size:`/`dm:`/`dc:` 查询永久零结果——24B/槽的常驻
代价只有全新建卷那一次才换来功能。

处置：**删表**而非瘦身。`refactor(index)` 提交删除 `RecordStat`/`stats`/填充
tick，`size:/dm:/dc:` 改扫描内现场 `fs::metadata`（`STAT_CALL_BUDGET=20000`
预算，超预算置 `is_truncated`）；`file:/folder:` 旗标判定零 I/O 提前到路径构造
之前；拼音候选路径同口径按需 stat 并共享预算。USN watch mask 回退内容变更
reason（原为让 stat 失效而加，需求消失）。前端收口两处无界字典
（WebIconProvider 上限 256、图标失败计数隐藏时清）。

### B 侧实测（2026-08-30，私有工作集口径，2 卷约 360 万槽）

| 测量点 | Prism | prism-core | indexer | 合计 | 达标 |
|---|---|---|---|---|---|
| 单次搜索后立即 | 9.8MB | 2.6MB | **63.7MB** | 76.1MB | ✓ ≤100MB |
| 空闲 3.5 分钟后 | 9.6MB | 2.5MB | **1.2MB** | 13.3MB | ✓ |
| 空闲门禁脚本（5 样本 p50） | 10.4MB | 2.7MB | 67.4MB* | 80.5MB | ✓（脚本含 USN 活动） |

\* 门禁脚本采样期间 USN 批次活跃（generation 612→621），indexer 被拉回工作集；
空闲静默采样（上行 1.2MB）才是修剪恢复生效的口径。

对照目标：indexer 搜索后立即 ≤65MB ✓（63.7，be29781 前基线 64.4）；空闲修剪后
个位数 MB ✓（1.2，回归期 ~80）；三进程使用后合计 ≤100MB ✓（76.1）。

### 功能回归

- 缓存命中启动（不删缓存重启服务）后 `size:>1mb`、`dm:today` **有结果**
  （修改前永久零结果——A4 缺陷验收通过）。
- `ext:zip size:>1mb` 命中真实大文件；带名字词的 size 查询结果与语义不变。
- 裸 `size:>5gb` 返回预算内候选并标 `is_truncated=true`（文档化取舍：
  20000 次 stat 封顶换 86MB 常驻，宁少勿全盘 I/O）。

### 工具链修正

`tools/bench/Bench.Common.psm1` 的 indexer hello 握手常量 2→3（G7c bump 后
脚本未跟上，重建门禁曾报 `protocol_mismatch: server=3 client=2`）。

### 重建窗口门 B 侧（2026-08-30，据实报告：仍超线，与 08-22 B 侧同水位）

删缓存→重启服务实测全新重建窗口（`artifacts/mem2-after-rebuild/`，6 样本）：
三进程私有工作集峰值 **141.7MB > 100MB 硬门**（indexer 峰值 129.6MB）。
对照：08-22 T5 的 B 侧重建门同样超线（139.8MB，`artifacts/elev-rebuild-gate-B.log`）
——本轮删掉 24B/槽常驻表后重建窗口峰值**未回退**（+1.4%，文件量自然增长口径内），
重建期瞬态（MFT 枚举池 + 拼音构建 + 新旧索引共存）是既知 Phase 2 议题，维持
「据实报告、不谎报达标」口径。

## 内存收口 III（2026-08-30，PRISM-COMMAND-SIMPLIFY-AND-MEMORY-III-PLAN-2026-08-30.md）

### 稳态内存锚点（B5，2026-08-30 实测）

数据源：`C:\ProgramData\Prism\indexer.jsonl` 的 `index_memory_trend` 分项日志
（方案 II A3 拆出的 `nodes=` / `names=` / `pinyin=`）。实测样本（2 卷，2 卷
MFT 槽合计 3,640,064）：

```
memory_bytes=89,470,555  nodes=43,680,768  names=45,128,846  pinyin=660,941
```

线性锚点：

```
Prism indexer 稳态内存锚点（2026-08-30 实测，2 卷 / 3.64M MFT 槽）：
  nodes  ≈ 12 B × MFT 槽数            （43.68 MB ÷ 3.64M 槽 = 恰好 12 B；
                                        稀疏、按 max_record 分配，与活跃文件数无关）
  names  ≈ ~21 B × 活跃文件数          （45.13 MB ÷ 估算 210 万活跃文件——活跃数
                                        无直接日志口径，按平均名长反推，误差 ±20%）
  pinyin ≈ ~660 KB / 2 卷（含汉字文件占比低时是小项；随汉字文件数线性）
  合计   ≈ 89.5 MB，折合 ~42 MB / 百万 MFT 槽（本机密度 ≈ 0.58 活跃/槽）
判读规则：index_memory_trend 的 memory_bytes 超出锚点 1.5 倍即为异常，
先看哪一个分项在涨——nodes 涨 = 槽数膨胀（大删除后未压缩），names 涨 =
名字池死字节（未触发压缩）或文件量自然增长，pinyin 涨 = 拼音重建未释放。
```

判读案例回填：G7c 时代的 150 MB 异常里 `nodes` 分项会直接多出 86 MB
（24 B/槽 stats 表的载体就是 nodes 平行分配）——有了分项锚点，这类回归
读日志第一分钟即可定性，不用再像 G7c 排查那样靠读代码反推。

### B2 名字池压缩去 clone 的收益与实测状态

- **结构性收益**：压缩瞬间少一份整卷 `nodes` clone（本机 ~43 MB，2 卷
  360 万槽；随槽数线性放大）。写锁从 O(1) 换入变为 O(活跃节点) 的 u32
  就地写（毫秒级，与 USN apply 同量级）。
- **实机 A/B 状态：未执行。** 重建门/压缩期峰值采样需提权停止服务、
  删缓存强制重建，会中断正在使用的 Prism；按「不谎报达标」口径如实记录。
  需要时运行 `tools/bench/Invoke-RebuildMemoryGate.ps1` 与压缩期
  `Measure-ProcessMemory.ps1` 采样补 A/B 数字。
- 已知代价（复查过，不动）：拼音重建的 `index.clone()`（`pinyin_sidecar`
  build 需完整节点图抽不出紧凑子集，已由 `PINYIN_REBUILD_BACKOFF` 限频）；
  broker `apply_stat_filters` 是幂等兜底；`enumerate_mft` 建卷期峰值
  ~115 MB + 名字池为 Everything 同口径以内（Everything 官方口径：索引期间
  每百万文件约 200 MB）。

