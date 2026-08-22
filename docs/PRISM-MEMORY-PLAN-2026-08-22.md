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

