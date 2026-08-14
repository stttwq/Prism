# Prism 代码质量修复清单（已结项 / 仅供审计）

> **状态：Q1–Q13 已逐项处理，本文不再是待办清单——但 Q5 只部分解决，见下表。**
> 制定日期：2026-07-27；结项标注：2026-08-09
> 配套文档 `PRISM-ROADMAP.md` 已废止；现行入口是
> [`PRISM-COMPREHENSIVE-PLAN.md`](./PRISM-COMPREHENSIVE-PLAN.md)。
> 本文的批次划分（R1–R4）从未按原样执行，实际是在 G1/G3 两个阶段里完成的。
>
> **注意本文若干判断在编制时就是错的**，逐项审计见
> [`PRISM-OPTIMIZATION-REPORT.md`](./PRISM-OPTIMIZATION-REPORT.md) §3。下表是最终落点：
>
> | 项 | 结论 | 落点 |
> | --- | --- | --- |
> | Q1 随机前 N 条 | **已修复** | G1 全局 Top-K，跨卷统一排序 + 稳定 tie-break |
> | Q2 `path_for` 性能 | **已修复**（原文把未来风险写成了当时事实） | G1 只为最终候选构造路径 |
> | Q3 O(N) 全扫 | **按另一路解决** | 未引入 `memchr`（大小写语义不符）；靠轻量候选 + 延迟路径构造 |
> | Q4 死代码 | **已删除** | `index.rs`、`search.rs` 均已删；`installer/setup.iss` 也已删 |
> | Q5 reveal 重复 | **只部分解决** | `ipc.rs` 那一份已删，两条路径都收敛到 broker 的 STA worker（`ShellExecutor`）。但 `explorer /select` 的参数构造**至今仍有两份**：`shell.rs::reveal`（走 `ShellOperation::Reveal`）和 `actions.rs::reveal_in_explorer`（走 `RunAction` → `run_action_direct` 的 `open_folder`）。两者都在生产路径上，`normalized` / `arg` 构造逐字相同，只有错误类型不同。**原始风险「改一处忘改另一处」依然存在。** |
> | Q6 排除目录硬编码 | **改用协议通道** | 7 项机器级硬排除保持在服务侧；用户过滤走 `filters` 的 `exclude_path`。原文的 `Arc<Arc<Vec<T>>>` atomic swap 方案是错的，未采用 |
> | Q7 协议类型偏弱 | **已修复** | `SearchResultKind` serde 枚举 + broker `Hello { protocol }` 显式协商；C# 未知值映射 `Unknown` |
> | Q8 前端零测试 | **已修复** | `src/Prism.Tests` 134 个测试；`ISearchClient` / `IDebounceTimerFactory` / `ISearchScheduler` 已抽接口 |
> | Q9a 日志粗糙 | **已修复，但未用 `tracing`** | 自建 `logging.rs`，broker 与 indexer 各写 JSONL；原文的 `tracing-appender` 1MB 大小轮转是能力误判 |
> | Q9b 校验职责混乱 | **已修复** | typed action target 取代字符串推断 |
> | Q10 `.lnk` 名称清洗 | **判断被推翻，未按原文修** | `&`、`(x86)` 不是控制字符，影响被夸大；无真实异常样本 |
> | Q11 `IndexHit` 缺元数据 | **已修复** | 候选阶段持轻量元数据，最终才生成完整结果 |
> | Q12 `Installer` 特判 | **保持父路径语义** | 未按原文移入全局 `is_excluded_name`——那会排除所有同名目录，属真实功能变化 |
> | Q13 COM 边界 | **原因判断有误** | apartment 是线程级，indexer 是另一进程，不存在原文所说的跨进程冲突 |
>
> 本文末尾「待办」里的 `.csproj` 手工复制问题也已解决：
> `scripts/prism-build.ps1` 统一构建、安装并做 SHA-256 漂移校验。

---

## 一、问题盘点（Q1–Q13）

### 🔴 严重（影响正确性/可用性，必须修）

#### Q1. `hierarchy.rs::search` 随机前 N 条
- **位置**：`src/prism-core/src/hierarchy.rs:219-250`
- **现象**：`max` 满 `break`，顺序是 MFT FRN 顺序（≈磁盘物理顺序，对用户是随机）。搜 "a" 看到的前 8 条没法用。
- **本质**：功能缺陷，不在"代码质量"范畴，但相关（见 Q2/Q11）。
- **归属**：P0（排序）。

#### Q2. `path_for` 性能炸弹
- **位置**：`src/prism-core/src/hierarchy.rs:187-217`，被 `search:237` 每个命中都调用
- **现象**：热门查询命中数万文件，每个回溯 64 层拼路径。200 万文件搜常见词可能秒级卡顿。
- **修复**：延迟拼路径（只给 top-K 拼）。
- **归属**：P0（批次 R3）。

#### Q3. `search` 的 O(N) 全扫无预过滤
- **位置**：`src/prism-core/src/hierarchy.rs:225`
- **现象**：`for (record, slot) in self.nodes.iter().enumerate()`，每次查询扫 200 万 NodeSlot。
- **修复**：SIMD（`memchr::memmem`）+ 位标志预过滤。
- **归属**：P0（批次 R3）。

---

### 🟡 中等（技术债，影响维护性，应修）

#### Q4. 死代码 / 半成品残留
- `src/prism-core/src/search.rs`：4 行占位文件，已废弃但 `lib.rs` 仍 `mod search;`
- `src/prism-core/src/index.rs`：旧扁平索引，已被 `hierarchy.rs` 取代
- `src/prism-core/src/ipc.rs:480` 的 `search` / `dispatch` 函数被 `#[cfg(test)]` 包裹，只为旧单测服务
- **影响**：误导开发，`cargo doc` 会生成废弃 API 文档。
- **归属**：批次 R1。

#### Q5. `reveal_in_explorer` 重复实现
- **位置**：`src/prism-core/src/ipc.rs:455` 和 `src/prism-core/src/actions.rs:74` **完全相同的代码**复制两份
- **影响**：改一处忘改另一处。
- **归属**：批次 R1。

#### Q6. 排除目录硬编码
- **位置**：`src/prism-core/src/hierarchy.rs:426-439` 的 `is_excluded_name` 写死了 7 个目录（node_modules/.git/$Recycle.Bin...）
- **影响**：用户无法自定义，不符合 Listary 习惯。
- **归属**：批次 R2。

#### Q7. `ipc.rs` 协议类型偏弱
- **位置**：`src/prism-core/src/ipc.rs:96-105`
- **现象**：
  - `SearchResult.kind` 是 `String`（"app"/"file"/"folder"/"web"），应枚举 + serde rename
  - `index_generation: Option<u64>` 等 Optional 字段散落，无版本协商
- **影响**：前后端版本不一致静默出错，新增 kind 时漏处理某分支编译器不报错。
- **归属**：批次 R2。

#### Q8. 前端零测试
- **位置**：`src/Prism/ViewModels/SearchViewModel.cs`
- **现象**：`SearchViewModel` 状态机复杂（Idle/Results/Actions + 防抖 + generation + 增量），但 0 测试；`SearchWindow` 窗口状态机（`_ignoreDeactivate`/`_contextMenuOpen`/`_hiding` 多 bool）也无测试。
- **影响**：这块是 bug 高发区，每次改都靠手动测试。
- **归属**：批次 R4。

---

### 🟢 轻（卫生问题，顺手修）

#### Q9a. 日志粗糙
- **位置**：`crate::log`（定义在 `src/prism-core/src/lib.rs`）
- **现象**：只是简单输出，无文件落盘、无级别、无轮转。
- **影响**：线上问题无法追溯。
- **修复方案**：引入 `tracing` + `tracing-subscriber` + `tracing-appender`。
- **归属**：批次 R4。

#### Q9b. `validate_path` 与 `is_http_url` 职责混乱
- **位置**：`src/prism-core/src/ipc.rs:341-413`
- **现象**：execute 走 `is_http_url` 分流，reveal/actions 不分流却都调 `validate_path`。
- **影响**：逻辑分散，新增"网址类 execute_id"（如将来支持 mailto:）容易漏分支。
- **归属**：批次 R2。

#### Q10. `app_from_lnk` 的 `name` 清洗不足
- **位置**：`src/prism-core/src/apps.rs:159-182`
- **现象**：`name = path.file_stem()?.to_string_lossy().trim()`，仅 trim，未 strip 控制字符。
- **影响**：某些 .lnk 文件名含特殊字符（`&`、`(x86)`、控制字符）会导致 subtitle 显示混乱，但不影响功能。
- **归属**：批次 R1。

#### Q11. `IndexHit` 缺少排序所需的元数据
- **位置**：`src/prism-core/src/hierarchy.rs:40-45`
- **现象**：只有 `name`/`path`/`is_directory`。
- **影响**：排序（方向 1）无法在不改结构的前提下做。是 Q1 的根因之一。
- **归属**：P0（批次 R3）。

#### Q12. `node_name_is` 特殊规则硬编码
- **位置**：`src/prism-core/src/hierarchy.rs:150`
- **现象**：`Installer` 在 `Windows` 下特殊排除。
- **影响**：规则藏在 upsert 里，不透明。
- **归属**：批次 R1。

#### Q13. COM 初始化边界模糊
- **位置**：`src/prism-core/src/apps.rs:211`
- **现象**：`CoInitializeEx(..., COINIT_MULTITHREADED)` 在 spawn_blocking 线程。
- **影响**：与 indexer 服务可能的 COM 调用（如果将来加）冲突。
- **归属**：暂不动，记入待办；将来加 COM 调用时统一处理。

---

## 二、修复批次（R1–R4）

### 批次 R1 · 卫生清理（0.5 天，P0 前置）

| 问题         | 修复                                                                                                                                          | 工作量 |
| ------------ | --------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| Q4 死代码    | 删 `search.rs`；删 `index.rs` + `lib.rs` 里的 `mod index;`/`mod search;`；删除或迁移 `ipc.rs:480` 的 `#[cfg(test)]` 旧函数及其测试              | 1h     |
| Q5 reveal 重复 | 抽 `shell.rs::reveal_in_path` 公共函数，`ipc.rs` + `actions.rs` 都调它                                                                        | 30min  |
| Q10/Q12 小硬编码 | 把 `Installer` 规则挪到 `is_excluded_name`；`name` 清洗加 strip 控制字符                                                                       | 30min  |

**特点**：全是删/抽函数，零功能影响。

**验收**：`cargo test` 全绿 + 手动跑核心场景（搜索/打开/reveal）无回归。

**风险**：极低。建议作为 P0 的第一个 commit，让后续改动在干净基线上进行。

---

### 批次 R2 · 类型安全 + 配置化（1 天，P0 后置）

| 问题            | 修复                                                                                                                                                  | 工作量 |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| **Q7 协议枚举化** | `SearchResult.kind: String` → `enum SearchResultKind { App, File, Folder, Web }` + `#[serde(rename_all="snake_case")]`；前端 `Models/SearchResult.cs` 的 `Kind` 保留 string（JSON 边界仍是 string），但加常量类 + 注释约束合法值 | 2-3h   |
| **Q6 排除目录配置化** | `config.rs` 加 `exclude_dirs: Vec<String>`；`is_excluded_name` 改读 config 快照（启动/reload 时生成 `Arc<Vec<String>>`，热路径 `Arc::clone` 原子读，不持锁） | 2h     |
| Q9b path 校验清理 | 抽 `id_kind(id) -> IdKind { Url, Path }`，execute/reveal/actions 统一分流入口                                                                            | 1h     |

#### 枚举化的关键约束

Rust 侧所有 `match item.kind` 必须有 `_ =>` 兜底或穷尽分支，新增 kind 时编译器强制覆盖。前端 C# 侧建一个 `SearchResultKinds` 静态类放常量（`"app"`/`"file"`/...），所有比较用常量不用字面量。

#### 排除目录配置化的并发模式

复用现有 `SharedApps`/`SharedEngines` 的 `Arc<RwLock<Vec<T>>>` 模式。但热路径（USN 回放）要避免锁——所以额外维护一个 `Arc<Arc<Vec<String>>>`（atomic Arc swap），reload 时整体替换，读路径无锁。

---

### 批次 R3 · 性能地基（P0 核心，不重复）

Q2 / Q3 / Q11，详见 `PRISM-ROADMAP.md` 的 P0 里程碑。

---

### 批次 R4 · 测试 + 日志（1-2 天，P1 后置）

| 问题            | 修复                                                                                                                                                                                          | 工作量 |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| **Q9a tracing 日志** | `Cargo.toml` 加 `tracing` + `tracing-subscriber` + `tracing-appender`；broker 和 indexer 服务各初始化一个 subscriber；日志写 `%LocalAppData%\Prism\logs\prism.log`，1MB 轮转保留 5 份；分级 INFO/WARN/ERROR | 4h     |
| **Q8 SearchViewModel 可测** | 抽 `IScheduler` 接口（`void Schedule(Action, TimeSpan)` + `void Stop()`）；生产实现 `DispatcherSchedulerAdapter` 包 `DispatcherTimer`；测试实现 `ManualScheduler` 立即触发；VM 依赖注入 `IScheduler` | 4h     |
| Q8 单测          | 覆盖：Idle→Results→Actions 三态切换、防抖取消、generation 变化重搜、增量缓存命中/失效、query 防抖期被新输入打断                                                                                | 4h     |

#### tracing 落地要点

- `main.rs` 和 `prism-indexer-service.rs` 各调 `tracing_subscriber::fmt().with_writer(...).init()`，**两个进程不能共享 appender**（会抢文件锁），各自独立的 `RollingFileAppender`。
- 现有 `crate::log` 改成一个薄封装 `tracing::info!`，保持调用点不改（渐进迁移）。
- 二进制增量经 LTO + strip 后实测，预计 < 200KB。

#### IScheduler 接口设计

```csharp
public interface IScheduler {
    void Schedule(Action callback, TimeSpan delay);
    void Stop();
}

// 生产
sealed class DispatcherSchedulerAdapter : IScheduler {
    private readonly DispatcherTimer _t = new();
    public void Schedule(Action cb, TimeSpan d) {
        _t.Interval = d;
        _t.Tick += (_,_) => { _t.Stop(); cb(); };
        _t.Start();
    }
    public void Stop() => _t.Stop();
}

// 测试
sealed class ManualScheduler : IScheduler {
    private Action? _pending;
    public void Schedule(Action cb, TimeSpan d) => _pending = cb;
    public void Stop() => _pending = null;
    public void Fire() => _pending?.Invoke();
}
```

VM 构造函数加 `IScheduler` 参数，`App.xaml.cs` 注入生产实现。

---

## 三、与路线图的整合

```
P0-pre  R1 卫生清理              0.5 天
P0      R3 性能 + 排序 + 历史     1-2 周
P0-post R2 类型安全 + 配置化      1 天
P1      失焦 + 缓存 + Milestone A 1 周
P1-post R4 测试 + 日志            1-2 天
P2      DLL 注入 B/C              3-4 周
P3      拼音 + 图标 + FM + 死代码  1-2 周（R1 已删大半）
P4      可选                      按需
```

**总工期增量**：质量修复为路线贡献约 **+3-4 天**，换来：

- 干净的代码基线（删死代码、消除重复）
- 类型安全的协议（枚举 + 穷尽 match）
- 可配置的排除规则（用户可定制）
- 可观测的运行时（分级日志 + 文件轮转）
- 可测的核心 VM（状态机单测覆盖）

---

## 四、待办（不阻塞，但需记住）

- **Q13 COM 边界**：`apps.rs:211` 的 `COINIT_MULTITHREADED` 在 spawn_blocking 线程，将来 indexer 加 COM 调用时统一处理。
- **`SearchWindow` 多 bool 状态机**：`_ignoreDeactivate`/`_contextMenuOpen`/`_contextMenuActionPending`/`_hiding` 多 bool，可考虑合并成 enum（如 `WindowDeactivatePolicy { Allow, Suppress, SuppressOnce }`）。P1 改失焦逻辑时顺手做。
- **`.csproj` 无自动 copy target**：发布后需手动复制 `prism-core.exe`。开发期用 debug 联调，发布流程不动。
