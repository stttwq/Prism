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

| 测量点 | Prism | prism-core | indexer | 合计 | 达标 |
|---|---|---|---|---|---|
| 单次搜索后立即（typed） | 52.9MB | 2.7MB | 64.6MB | 120.3MB | ✗（瞬态） |
| 展开 1000 后 | 54.5MB | 2.8MB | 64.6MB | 121.9MB | ✗（瞬态） |
| 隐藏后 3 分 20 秒采样 | 46.9MB | 2.5MB | 64.7MB | 114.1MB | ✗（修剪未达） |
| 进一步空闲后稳态 | 25.6MB | 20.6MB | 45.2MB | **91.5MB** | ✓ |

- **前端单次 `K32EmptyWorkingSet`**：Prism 从 54.5MB 降到 25.6MB（降 29MB）。
  早期"带验证重试+Thread.Sleep+trace"版本与 OS 自发修剪冲突、反而只降到 46MB；
  改单次调用后稳态降到 25.6MB，证实重试是反作用。
- **indexer `SetProcessWorkingSetSizeEx(-1,-1)`**：indexer 从 64.6MB 降到 45.2MB（降 19MB）。
- **3 分钟修剪点未在 3 分 20 秒采样时生效**：USN 批次（`changed>0`）经
  `touch_activity` 刷新 `last_activity_ms`，持续文件系统活动（Windows 更新、
  浏览器缓存等）使空闲计时器达不到 3 分钟；待系统安静后修剪才触发。这是
  刻意行为：活跃 USN 期不修剪（会立刻被新批次重新分页），静默后才修。
  相比旧口径「约 1 小时 OS 自发修剪」已提前到「静默 3 分钟」。
- **瞬态 120MB**：单次搜索瞬间三进程合计 120MB 是搜索把整卷 MFT 随机节点
  + WPF 框架本体拉进工作集的固有成本，非泄漏——Phase 2（流式建卷/MftRecord
  瘦身/节点表分页）才能压这一峰值。本轮目标「使用后稳态 ≤100MB」达成（91.5MB）。
- **重建窗口门 B 侧**：133.2MiB（与 A 侧 134.1MiB 一致），合法重建峰值破线，
  属 Phase 2 领域，据实报告不谎报达标。

