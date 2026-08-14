# Prism 优化调研与实施建议

> 状态：**历史审计快照，已完成使命**——其结论已被采纳并转为
> [`PRISM-COMPREHENSIVE-PLAN.md`](./PRISM-COMPREHENSIVE-PLAN.md) 的阶段规划。  
> 调研日期：2026-07-28；状态标注：2026-08-14  
> 代码基线：`main` / `c0de808`（工作树中的 `docs/` 尚未纳入 Git）  
> 性质：**2026-07-28 当天的**事实审计。本文的价值在于 §3 对两份原稿的逐项纠错，
> 那部分至今有效；但正文所有"当前""现状"描述的都是 7 月 28 日的代码，**不是今天的代码**。
>
> 已经过时的具体数字与判断：
>
> - **§2.4 的质量检查**（Rust 86 通过、无 C# 测试项目）：当前是 **Rust 262 通过、
>   C# 134 通过**（`src/Prism.Tests` 已建立），clippy 与 Release build 均无告警（2026-08-14）。
> - **§2.2 表格末行"使用历史排序、拼音、对话框导航、插件——未实现"**：历史与拼音**已交付**
>   （G2）；Explorer/Opus 目录联动**已交付**（G4，开关默认关闭）；标准打开/保存对话框
>   **仍未实现**；插件按产品决策**永久排除**。
> - **§2.3 的"三进程基线属待测量"**：已测。2026-08-14 三进程私有工作集合计约 102 MiB。
>   40MB 红线从未成为门槛，现行门槛是 ≤100MiB。
> - **§10 的决策清单**：8 项均已由所有者拍定，结果写在综合计划与
>   [`POST-ROADMAP-REVISED.md`](./POST-ROADMAP-REVISED.md) 里。要点是——内存维持 ≤100MB 硬门槛；
>   拼音做首字母**加全拼**；机器级硬排除不开放用户解除，用户过滤走协议 `filters`；
>   **DLL 注入整体排除**，故 §5「P3.2 DLL 注入独立决策门」与 §9 排期里的 P3b 已作废；
>   日志默认脱敏，消息文本哈希化。
> - **§5 的 P0–P4 编号**已被综合计划的 G0–G9 取代，不再作为排期依据。

## 1. 执行摘要

Prism 已经具备可工作的本地文件搜索、应用启动、网页快捷搜索和动作面板，并完成了 MFT 全量构建、USN Journal 实时增量、缓存恢复和索引 generation 刷新。以“低内存 Windows 启动器”衡量，项目主体已经成型；以“Listary 替代品”衡量，搜索结果质量、用户行为排序、文件对话框联动和可回归验证仍是主要缺口。

本次审计确认，现有 [PRISM-ROADMAP.md](./PRISM-ROADMAP.md) 和 [CODE-QUALITY-FIXES.md](./CODE-QUALITY-FIXES.md) 大量引用了真实代码，但也混入了过时架构、未经测量的预算和若干不可直接实施的技术方案。两份文件在本次调研开始时均位于未跟踪的 `docs/` 目录，不能视为已提交、已审批的项目规格。

建议顺序如下：

1. **先建立当前三进程基线，再谈 40–45MB 目标。** 仓库已提交的硬门槛仍是 100MB；38MB 是独立索引服务加入前的旧两进程数据。
2. **先修搜索正确性，再优化吞吐。** 当前结果按 MFT record 顺序截断，缺少全局排序；优化必须兼容首屏 8 条和展开 1000 条，不能用固定 200 条 Top-K 改变产品行为。
3. **把用户历史、增量缓存和前端测试作为一个体验批次。** 缓存必须识别结果截断与 generation，否则会静默漏结果。
4. **把旧代码清理、协议、排除规则和日志作为工程质量批次。** 配置需尊重普通权限 broker 与 LocalSystem indexer 的进程边界。
5. **先做 UIA/COM 可行性验证，再决定 DLL 注入。** 注入需要独立的安全、兼容性和发布门禁，不应与普通搜索优化捆绑。

### 1.1 可信度标签

| 标签 | 含义 |
| --- | --- |
| **已验证** | 当前源码、测试或已提交 Trellis 规格直接支持 |
| **部分成立** | 核心现象存在，但范围、原因或影响被夸大 |
| **设计建议** | 可讨论的未来方案，不代表当前事实或已批准决策 |
| **待测量** | 缺少当前版本的可复现实测数据 |
| **错误/过时** | 与当前仓库、平台语义或依赖能力冲突 |

### 1.2 证据优先级

本报告采用以下优先级解决冲突：

1. 当前 `main` 源码与可重复执行的测试；
2. 已提交的 `.trellis/spec/` 和已归档任务需求/验收；
3. 开发者 journal 中带日期的记录；
4. 历史会话摘要；
5. 未跟踪的两份 `docs/` 原稿。

历史会话 `sess_f69d8c28-c657-4365-a354-b8eb0f71d9e6` 支持“又快又准”、重视约 40MB 体感、DLL 重方案、Rust cdylib、仅 x64、拼音首字母和落盘使用历史等讨论方向。但这些对话结论尚未进入已提交 PRD/spec，因此本报告将其列为**待所有者确认的产品偏好**，不写成不可变约束。

## 2. 当前真实基线

### 2.1 进程与数据流

当前不是双进程，而是三个长期角色：

```text
Prism.exe（WPF，普通用户）
  └─ \\.\pipe\prism-core
       └─ prism-core.exe（broker，普通用户）
            └─ \\.\pipe\prism-indexer-v1
                 └─ prism-indexer-service.exe（LocalSystem）
```

| 进程 | 已验证职责 | 权限边界 |
| --- | --- | --- |
| `Prism.exe` | 搜索窗口、热键、托盘、主题、图标和交互状态 | 普通用户 |
| `prism-core.exe` | 应用与网页结果合并、execute/reveal/actions、用户配置、向 indexer 转发文件搜索 | 普通用户 |
| `prism-indexer-service.exe` | MFT/USN 索引、v5 缓存、generation、只读搜索协议 | LocalSystem |

证据：两个 Rust binary 定义在 [Cargo.toml](../src/prism-core/Cargo.toml#L7-L13)，两条管道常量定义在 [lib.rs](../src/prism-core/src/lib.rs#L17-L19)，三进程边界与权限要求已写入归档 [PRD](../.trellis/tasks/archive/2026-07/07-25-prism-optimize-round1/prd.md#L19-L27)。

### 2.2 已成型能力

| 能力 | 结论 | 证据 |
| --- | --- | --- |
| MFT 全量枚举、USN 实时监听与停机回放 | **已验证** | [ntfs.rs](../src/prism-core/src/ntfs.rs)、[indexer_runtime.rs](../src/prism-core/src/indexer_runtime.rs)；归档 PRD AC2–AC7 |
| FRN 父链索引和路径按需构造 | **已验证** | [hierarchy.rs](../src/prism-core/src/hierarchy.rs#L12-L45)、[path_for](../src/prism-core/src/hierarchy.rs#L187-L217) |
| 服务协议版本与只读命令集 | **已验证** | [indexer_ipc.rs](../src/prism-core/src/indexer_ipc.rs#L1-L40) |
| generation 变化触发可见查询重搜 | **已验证** | [SearchViewModel.cs](../src/Prism/ViewModels/SearchViewModel.cs#L53-L76) |
| 首屏 8 条、展开 1000 条 | **已验证** | [SearchViewModel.cs](../src/Prism/ViewModels/SearchViewModel.cs#L14-L24) |
| 双击 Ctrl、托盘、深浅主题、基础动作 | **已验证** | `HotkeyService.cs`、`TrayService.cs`、`ThemeWatcher.cs`、[actions.rs](../src/prism-core/src/actions.rs#L9-L55) |
| 使用历史排序、拼音、对话框导航、插件 | **未实现** | 当前源树无对应模块 |

### 2.3 内存基线

- `NodeSlot` 使用 `#[repr(C)]`，字段合计 12B，并有单元测试保证；见 [hierarchy.rs](../src/prism-core/src/hierarchy.rs#L12-L19)。
- “200 万节点约 24MB”只能描述槽数组本身。实际索引还包含 UTF-8 名字池、`Vec` 预留容量、卷元数据、运行时锁和进程开销；当前 `memory_bytes()` 也明确把槽容量与名字池容量相加，见 [hierarchy.rs](../src/prism-core/src/hierarchy.rs#L252-L254)。
- 2026-07-25 的约 38MB private working set 是 `Prism.exe + prism-core.exe` 的旧两进程基线，见 [quality-guidelines.md](../.trellis/spec/backend/quality-guidelines.md#L50-L59)。
- 独立 indexer 加入后，已归档 AC9 明确要求测量三个进程且总门槛为 100MB，见 [PRD](../.trellis/tasks/archive/2026-07/07-25-prism-optimize-round1/prd.md#L41-L52)。
- journal 记录“完成内存验收”，但同一记录没有保存具体三进程数值；因此当前三进程基线、40MB 红线和 41–43MB 终态都属于**待测量**。

在重新测量前，本报告保留已提交的 **≤100MB 硬门槛**。40–45MB 可以作为产品目标候选，但不能作为已经验证的工程约束。

### 2.4 当前质量检查

以下命令于 2026-07-28 在本工作树执行：

| 检查 | 结果 |
| --- | --- |
| `cargo test --manifest-path src/prism-core/Cargo.toml` | 通过：86 passed，0 failed |
| `cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings` | 通过：0 warning |
| `dotnet build src/Prism/Prism.csproj -c Release` | 通过：0 warning，0 error |

这些结果证明当前静态质量门禁健康，但不替代搜索延迟、USN 机器测试、三进程内存或对话框兼容性测试。当前仓库没有 C# 测试项目，因此 WPF 只有“可构建”证据，没有自动行为回归证据。

## 3. 对 `CODE-QUALITY-FIXES.md` 的逐项审计

### Q1：`hierarchy.rs::search` 随机前 N 条 — **已验证**

当前实现按 `nodes` 下标遍历，命中达到 `max` 即 `break`，没有 score 或排序，见 [hierarchy.rs](../src/prism-core/src/hierarchy.rs#L219-L249)。下标来自 MFT record number；对用户可视排序而言近似无业务意义。原文称“磁盘物理顺序”不够严谨，更准确的说法是“MFT record 顺序”。

**真实影响**：首屏 8 条可能错过更相关结果，是正确性/可用性缺陷。跨卷搜索还会先填满前面的卷，见 [IndexState::search](../src/prism-core/src/hierarchy.rs#L409-L419)，因此全局排序必须覆盖所有卷。

### Q2：`path_for` 性能炸弹 — **部分成立**

`path_for` 确实最多回溯 64 层并构造完整路径，但当前搜索在达到 `max` 后停止，所以不会在一次首屏搜索中为“数万命中”全部构造路径。当前调用次数上限大致受每卷剩余 `max` 约束，而不是总匹配数。

**真实风险**：修复 Q1 后若改为全量收集再排序，而仍在候选阶段调用 `path_for`，原文描述的放大效应就会出现。因此“只对最终 Top-K 构造路径”是正确的未来约束，但原文把未来风险写成了当前事实。

### Q3：搜索 O(N) 全扫无预过滤 — **部分成立**

最坏情况下确实遍历所有节点：罕见查询、无命中查询或未来取消提前终止后都会达到 O(N)。但当前常见词可能在命中 `max` 后提前退出，因此“每次查询固定扫描 200 万项”不准确。

`memchr::memmem` 是大小写敏感搜索，不能直接替换当前 ASCII 不区分大小写逻辑。应先建立查询语料和基准，再比较：当前逐字节折叠、候选首字节定位后校验、额外小写键等方案；后者会增加索引内存。

### Q4：死代码与半成品残留 — **部分成立**

- `search.rs` 是三行占位文件，但 [lib.rs](../src/prism-core/src/lib.rs#L3-L14) 并未声明 `mod search`；“仍被模块声明”是错误信息。
- `index.rs` 仍以 `pub mod index` 编译，并被 `ipc.rs` 的 `#[cfg(test)]` 旧搜索链使用；生产 broker 已经走 indexer client。将它称为“生产旧索引残留”基本成立。
- 删除旧链之前必须先把仍有价值的排序、缓存和 IPC 测试迁移到现行 indexer 路径，不能简单删掉测试覆盖。

### Q5：`reveal_in_explorer` 重复实现 — **已验证（语义重复）**

[actions.rs](../src/prism-core/src/actions.rs#L72-L91) 与 [ipc.rs](../src/prism-core/src/ipc.rs#L453-L473) 都用 `explorer /select`。两段不是逐字符完全相同：日志和注释不同，但行为与平台分支重复，应该抽取公共 Shell 层。

### Q6：排除目录硬编码 — **已验证**

[is_excluded_name](../src/prism-core/src/hierarchy.rs#L426-L439) 写死 7 个目录。问题不只是“缺少配置”：排除发生在 LocalSystem 服务的索引构建期，而用户配置由普通权限 broker 读取。直接给 `config.rs` 加字段不能自动跨越权限和数据目录边界。

建议区分：

- 服务级硬排除：用于安全/内存保护，机器级、变更需重建；
- 用户搜索过滤：保留完整机器索引，由 broker 随只读 search 请求传递有界过滤条件，避免向服务开放写配置命令。

### Q7：IPC 协议类型偏弱 — **部分成立**

broker 的 [SearchResult](../src/prism-core/src/ipc.rs#L94-L105) 使用 `String kind`，且前端手工解析字符串；broker 协议只有带版本字符串的 ping，没有像 indexer 协议那样显式协商 protocol number。风险成立。

但仅把 Rust 字段改成 enum 不能解决前后端兼容。需要同时定义 JSON 字符串值、未知 kind 的前端行为和协议版本。Rust `match` 使用 `_` 兜底会失去“新增枚举时编译器强制覆盖”的收益，原稿对此存在自相矛盾。

### Q8：前端零测试 — **已验证**

`Prism.csproj` 是唯一 C# 项目，没有测试工程。`SearchViewModel` 同时拥有两个 `DispatcherTimer`、取消令牌、sequence、generation 和三态切换，见 [SearchViewModel.cs](../src/Prism/ViewModels/SearchViewModel.cs#L14-L51)；`SearchWindow` 还维护多组失焦布尔状态，见 [SearchWindow.xaml.cs](../src/Prism/Windows/SearchWindow.xaml.cs#L28-L38)。这是高价值测试缺口。

### Q9a：日志粗糙 — **已验证**

[crate::log](../src/prism-core/src/lib.rs#L21-L28) 只向 stderr 输出时间戳，无级别、文件、轮转和保留策略。服务错误另有事件日志路径，但无法替代统一可观测性。

### Q9b：路径与 URL 校验职责混乱 — **设计建议**

execute 先区分 http(s)，reveal/actions 只接受路径，这与当前 result kind 的调用约束一致，并非已证实 bug。真正问题是类型信息在 `execute_id: String` 中丢失，导致入口靠字符串推断。未来支持 `mailto:` 或更多动作时，可用 typed target 统一分派，但优先级低于协议版本和测试。

### Q10：`.lnk` 名称清洗不足 — **错误/影响被夸大**

[app_from_lnk](../src/prism-core/src/apps.rs#L159-L181) 确实只做 `trim()`，但 `&` 和 `(x86)` 不是控制字符，在 WPF `TextBlock` 中也不会天然导致转义问题；名称用于 title，subtitle 通常是目标路径。除非先收集到真实异常 `.lnk` 样本，否则不应把“strip 控制字符”列为既定修复。

### Q11：`IndexHit` 缺少排序元数据 — **部分成立**

[IndexHit](../src/prism-core/src/hierarchy.rs#L40-L45) 只有 name/path/directory。排序确实需要 score 或候选元数据，但缺字段不是 Q1 的根因；根因是当前算法选择“先到先得”。更合理的设计是候选阶段持有轻量 record/score，仅在最终结果生成 `IndexHit` 和完整路径。

### Q12：`Windows\Installer` 特判藏在 upsert — **已验证**

特判位于 [upsert](../src/prism-core/src/hierarchy.rs#L147-L151)。它表达的是“仅排除 Windows 下的 Installer”，不能简单移入全局 `is_excluded_name`，否则所有同名目录都会被排除，造成真实功能变化。

### Q13：COM 初始化边界模糊 — **错误原因，保留低优先级审计项**

[LnkResolver](../src/prism-core/src/apps.rs#L195-L260) 在执行扫描的线程初始化 MTA，并在成功初始化时配对 `CoUninitialize`。COM apartment 是线程级；indexer 是另一个进程，未来在 indexer 中使用 COM 不会与这里直接冲突。

可审计的真实边界是：若线程此前以不同 apartment 初始化，`CoInitializeEx` 可能返回 `RPC_E_CHANGED_MODE`；当前代码随后仍尝试 `CoCreateInstance` 并静默降级。这值得补日志/测试，但不是跨进程冲突。

## 4. 原方案中的技术与一致性问题

| 原主张 | 审计结论 | 修正方向 |
| --- | --- | --- |
| `Arc<Arc<Vec<T>>>` 可 atomic swap | **错误**：嵌套 `Arc` 本身不提供共享原子替换 | 使用 `arc-swap`，或 `RwLock<Arc<T>>`；若配置随请求传入则不需要全局 swap |
| `memchr::memmem` 直接替换现有匹配 | **错误/不完整**：不提供 ASCII case-insensitive | 先基准三种匹配策略，保持 Unicode/ASCII 语义与内存门禁 |
| Top-K 容量固定 200 | **与产品约束冲突** | 容量由请求 `max` 决定，并跨卷全局比较；必须支持 8 和 1000 |
| 输入增长时直接过滤前次结果 | **会漏结果**：前次通常被截断 | 协议返回 `is_truncated`；仅在完整响应且 generation 不变时本地过滤 |
| `UIA SetCurrentFolder` | **概念混淆** | UIA 操作可访问控件；`IFileDialog::SetFolder` 是 COM 调用，需要对象引用或进程内协作 |
| `tracing-appender` 做 1MB 大小轮转 | **能力不匹配** | 接受按时间轮转，或选择明确支持 size-based rotation 的 writer；两个进程使用不同文件名 |
| 示例 `DispatcherTimer` 每次 Schedule 追加 Tick | **会累积处理器** | 注入两个可重启的一次性 timer/debouncer，处理器只绑定一次 |
| 200 万节点 + 名字池约 24MB | **算术不完整** | 24MB 只是 slots；报告 slots、names capacity、进程 working set 三组指标 |
| “LTO + strip 后实测，预计 <200KB” | **自相矛盾且无证据** | 构建前后对比 release binary，记录测量环境和增量 |
| R1 清死代码，同时 P3 再清一次 | **阶段冲突** | 统一放入 P2，在迁移测试后一次完成 |

## 5. 修正后的优化蓝图

以下阶段是建议顺序，不代表已经获准实施。工期均为单人开发的粗略工程估算，不含需求等待、代码评审、杀软申报和发布观察。

### P0：基准、搜索正确性与性能地基

#### P0.1 建立可复现基线

| 项目 | 建议 |
| --- | --- |
| 收益 | 为内存目标、排序算法和后续回归提供同一把尺 |
| 内容 | 固定索引样本与查询集；覆盖高频 ASCII、中文、罕见词、无命中、`max=8`、`max=1000`；记录冷/暖 P50/P95/max、扫描候选数、路径构造数 |
| 内存 | 测量 `Prism`、broker、indexer 的 `WorkingSetPrivate` 与 WS，同时记录 indexer status 的 `memory_bytes` |
| 依赖 | Release 二进制与当前 v5 索引加载完成；记录机器、卷、节点数、名字池大小 |
| 风险 | 后台 I/O、杀软和首次 JIT 会污染结果；需预热并重复多轮 |
| 估算 | 1–2 天 |
| 验收 | 原始结果、命令、环境和聚合数据可复跑；不再引用旧两进程 38MB 作为当前数据 |

#### P0.2 全局可解释 Top-K

推荐把搜索拆成三步：

1. 每个可匹配节点生成轻量候选 `{volume, record, name_ref, flags, score}`；不构造完整路径。
2. 以请求 `max` 为容量维护全局小顶堆，跨卷统一比较；相同 score 使用名称、卷序和 record 形成稳定 tie-break。
3. 只为最终候选调用 `path_for`，再生成 IPC `IndexerItem`。

第一版基础评分建议只采用可以从当前索引零额外内存得到的信号：前缀命中、完整文件名命中、名称长度、目录标志。exe 标记、路径深度和历史信号分别在测量其成本后加入。分值必须集中定义并有排序单测，不能散落 magic number。

| 项目 | 建议 |
| --- | --- |
| 收益 | 解决首屏随机结果和跨卷偏置，为历史排序提供统一入口 |
| 依赖 | P0.1 查询语料；缓存格式变更策略（若扩展 flags） |
| 风险 | 常见词不再提前停止，CPU 可能上升；Unicode 小写转换可能分配 |
| 内存影响 | 堆和候选为 O(`max`)；`max=1000` 必须纳入峰值测试 |
| 估算 | 3–5 天 |
| 验收 | 8/1000 上限都正确；相同索引结果稳定；跨卷参与排序；非最终候选不构造路径；原有匹配语义不回归 |

#### P0.3 匹配热点优化

不预设“5–10 倍”。在 P0.1 harness 上比较：

- 当前逐字节 ASCII case fold；
- 使用快速首字节/子串候选定位后再验证大小写；
- 增加归一化键或辅助池的方案。

只有当第三种方案在三进程内存门禁内带来稳定 P95 收益，才允许扩大索引。Unicode 搜索必须保留当前语义或把已知差异写入产品规格。

### P1：个性化、正确缓存与前端可测试性

#### P1.1 使用历史排序

建议 v1 记录成功的 execute 与 reveal：

```text
HistoryEntry {
  normalized_path,
  execute_count,
  reveal_count,
  last_used_utc
}
```

- 用户数据由普通权限 broker 保存，不进入 LocalSystem 服务数据目录。
- 文件采用版本号、临时文件 + 原子替换；损坏时保留诊断并回到空历史。
- 建议默认最多 500 条 LRU，但真实磁盘与运行时内存必须测量，不沿用“固定 +40KB”结论。
- 排序权重先作为可测试常量；历史只提升候选，不应让已删除路径永久占位。

| 收益 | 依赖 | 风险 | 估算 | 验收 |
| --- | --- | --- | --- | --- |
| 搜索越用越准，形成 Listary 核心差异 | P0 全局评分入口 | 隐私、路径大小写/重命名、权重过强 | 2–4 天 | execute/reveal 成功才记录；重启保留；损坏恢复；相同历史产生稳定排序 |

#### P1.2 正确的增量缓存

broker `results` 建议新增 `is_truncated: bool`，并在索引就绪时稳定返回 `index_generation`。前端只有同时满足以下条件才本地过滤：

1. 新 query 以旧 query 为前缀；
2. 旧响应 `is_truncated == false`；
3. generation 未变化；
4. result kind 和排序规则未发生配置变化。

被截断的首屏查询继续走 broker，不能为了“连续输入不敲管道”牺牲正确性。若后续需要更高缓存命中率，应另行设计候选窗口或 cursor，而不是把显示的 8 条当完整集合。

| 收益 | 风险 | 估算 | 验收 |
| --- | --- | --- | --- |
| 降低窄查询连续输入的 IPC/CPU，保证不漏结果 | 协议兼容、旧 broker 没有新字段 | 2–3 天 | 截断、删除字符、generation 变化、空查询均回后端；完整响应的前缀增长不发请求 |

#### P1.3 前端状态机与测试

新增独立测试项目，并把 `PipeClient` 与两个 debounce timer 抽成可替换接口。建议每个 timer 使用“处理器绑定一次、`Restart/Stop` 控制”的接口，避免示例代码的 Tick 累积。失焦逻辑把“呼出保护、上下文菜单、动作执行、隐藏中”收敛为显式策略状态，Pin 保持独立产品状态。

最低测试集：

- Idle → Results → Actions → Results；
- 新输入取消旧搜索，旧响应不得覆盖新响应；
- generation 防抖合并；
- 完整/截断缓存命中与失效；
- 呼出保护结束后失焦隐藏；菜单和动作期间不误隐藏；
- 8 条、More 行、展开 1000 条行为不变。

估算 4–6 天；验收为测试在无可视桌面的普通 CI/命令行环境稳定运行。

### P2：工程质量、协议、配置与可观测性

#### P2.1 旧链清理与 Shell 复用

- 先把 `index.rs` 旧搜索链仍覆盖的有效测试迁到 `hierarchy/indexer_client` 现行路径；再删除旧模块及孤立 `search.rs`。
- 抽一个公共 Shell 模块供 IPC reveal 和 actions 调用；保持错误文案与日志归属清晰。
- 不把“Windows 下 Installer”改成全局同名排除。

估算 1–2 天；验收为 Rust 测试数不因简单删除而无理由下降，生产依赖图不再编译旧索引。

#### P2.2 版本化 broker 协议

建议的公开接口变化：

- 增加 `hello { protocol }` / `hello { protocol, version }`，保留旧 ping 一个兼容周期；
- Rust 增加 `SearchResultKind` serde 字符串枚举；C# 使用对应 enum，并为未知字符串保留 `Unknown`；
- `results` 增加 `is_truncated`，明确 `index_generation` 的存在条件；
- 新协议版本测试覆盖旧客户端、未知 kind、缺省可选字段与明确不兼容错误。

估算 2–4 天；JSON 线格式保持可读，不把内部 Rust 枚举名直接暴露为不稳定 ABI。

#### P2.3 排除规则的权限边界

不建议让 LocalSystem 服务直接读取某个用户的 `%LocalAppData%`。推荐：

- 现有 7 项作为有文档、有测试的机器级硬排除；
- 用户规则存于普通用户 settings；
- broker 在只读 search 请求中传递数量和长度有上限的过滤快照，由 indexer 在选 Top-K 前应用；
- 机器级硬排除的解除需要重建且不属于普通设置。

该方案保持服务协议只读，同时避免 broker 收到 `max` 条后再过滤导致结果不足。估算 3–5 天；需要协议限额、路径规范化和恶意大请求测试。

#### P2.4 可靠日志

- broker 与 indexer 分别写不同文件，避免跨进程共享同一 rolling writer；
- 使用 `tracing`/`tracing-subscriber` 提供级别和结构字段；
- 若采用 `tracing-appender`，接受它实际支持的时间轮转与保留能力；若产品坚持 1MB 大小轮转，选择明确支持 size-based rotation 的 writer 后再写规格；
- 保存 non-blocking guard 至进程生命周期结束，崩溃和服务启动失败仍保留 Windows Event Log 兜底；
- 默认不记录完整用户 query/path，或做显式调试开关，避免隐私泄漏。

估算 2–3 天。验收覆盖两个进程并发写、轮转上限、异常退出、磁盘只读和日志目录创建失败。

### P3：Listary 文件对话框联动

#### P3.1 UIA/COM 可行性原型

先产出兼容性矩阵，而不是直接进入生产实现：

| 目标 | 候选通路 | 原型必须回答的问题 |
| --- | --- | --- |
| Explorer | UIA 地址栏或 Shell COM | 能否可靠读取/设置当前目录；多窗口与标签页如何区分 |
| 现代 `IFileDialog` | UIA 操作可访问控件；进程内 COM 才能直接调用接口对象 | 外部进程能否稳定导航；不同宿主和权限下的行为 |
| 旧式文件对话框 | UIA/Win32 控件消息 | 控件层级、本地化、32 位宿主兼容性 |

原型覆盖 Explorer、记事本、Office/浏览器类应用各至少一个样本，记录普通/管理员、x64/x86 和打开/保存场景。估算 1–2 周，允许结论是“某类目标不适合 UIA”。

#### P3.2 DLL 注入独立决策门

只有 P3.1 证明轻方案达不到已确认体验，才评审 Rust cdylib 注入。评审至少包含：

- 数字签名、杀软误报与发布渠道；
- 普通进程不能注入提权进程的明确降级；
- x64-only 对 32 位宿主的产品提示；
- DLL 生命周期、卸载、线程和 COM apartment；
- Rust FFI panic/SEH 边界与宿主崩溃恢复策略；“全包 try/catch”不是 Rust 可执行设计；
- 管道 ACL、命令白名单、重放和伪客户端；
- 宿主应用崩溃责任与 kill switch。

基础设施与 hook 必须分两个里程碑，先证明“注入 + 通信且零 hook”，再处理具体接口。工程估算 3–6 周以上，不含签名和杀软申报；在独立安全评审前不进入排期。

### P4：增强项

| 项目 | 推荐边界 | 主要门禁 | 粗略估算 |
| --- | --- | --- | --- |
| 拼音首字母 | 只在测量中文节点比例与表示结构后设计；不能先承诺 +1MB | 索引/进程内存、ASCII 查询 P95、歧义排序 | 3–5 天 |
| 第三方文件管理器 | 先抽 Shell 层，再支持 dopus/TC/custom 命令模板 | 参数转义、任意命令风险、目标不存在降级 | 2–4 天 |
| Web 引擎图标 | 内置少量矢量资源，以 keyword/engine id 映射 | 主题、DPI、未知引擎回退 | 1–2 天 |
| 动作配置化 | 先做有界 JSON 命令动作，不直接开放 DLL SDK | 占位符转义、超时、权限、协议版本 | 1–2 周 |

## 6. 跨阶段接口与兼容性

本报告建议但尚未批准的接口变化包括：

1. broker `results`：新增 `is_truncated`，明确 `index_generation`；
2. broker handshake：新增 protocol number；
3. Rust/C#：共享稳定的 result kind 字符串集合与 Unknown 行为；
4. indexer search：可选、有界、只读的用户过滤快照；
5. 历史文件：带版本的用户级 schema；
6. 前端：可注入 search client 与两个 debounce timer 接口。

兼容策略统一采用“先让新 reader 接受旧消息，再让 writer 发新字段”；协议号不兼容时返回明确错误，不静默降级为错误结果。缓存格式只在 `NodeSlot` 或索引持久字段变化时升级，旧 v5 缓存应触发一次可解释重建。

## 7. 验收门与回滚原则

### G0：当前基线门

- 三进程 Release、索引就绪、固定样本下完成内存和搜索基线；
- 项目所有者决定继续以 100MB 为硬门槛，还是收紧为 40–45MB；
- 未通过 G0，不批准任何带固定内存数字的路线。

### G1：搜索正确性门

- 匹配集合、8/1000 上限、跨卷排序、稳定 tie-break 和路径正确性有自动测试；
- P95/max 与 P0.1 基线并列报告，不用单次最快值；
- 内存增长与候选堆峰值可解释；
- 失败可回滚到旧 search，不迁移用户数据。

### G2：体验门

- 历史文件可版本化和恢复；
- 增量缓存对截断与 generation 的处理有回归测试；
- 前端状态机测试在 CI 环境稳定；
- 功能开关可分别关闭历史加权与本地缓存。

### G3：工程质量门

- `cargo test`、Clippy `-D warnings`、C# tests、WPF Release build 全绿；
- 两进程日志不会互抢文件，磁盘故障不影响搜索主流程；
- 协议兼容矩阵和过滤输入限额通过测试。

### G4：联动/注入门

- UIA/COM 原型先评审；
- DLL 注入单独完成安全评审、签名策略、kill switch 和宿主崩溃测试；
- 不因 P3 阻塞 P0–P2 的搜索质量改进。

## 8. 风险登记

| 风险 | 概率/影响 | 缓解 |
| --- | --- | --- |
| 全局排序取消提前终止后延迟上升 | 高/高 | P0.1 基准、轻候选、最终才拼路径、按请求上限堆选 |
| 前端缓存静默漏结果 | 高/高 | `is_truncated` + generation；不满足条件就回后端 |
| 40MB 目标与三进程现实冲突 | 中/高 | 先重测，再由所有者确认硬/软门槛 |
| 用户排除规则越过服务权限边界 | 中/高 | 请求级只读过滤；不让服务读用户目录或开放写命令 |
| 使用历史泄露敏感路径 | 中/中 | 用户级文件、默认不写日志、清除入口、版本与容量上限 |
| 日志轮转依赖不支持规格 | 中/中 | 先选择实际能力，再写大小/时间/保留策略 |
| UIA 跨应用兼容性不足 | 高/中 | 兼容性原型、按宿主降级、不承诺全覆盖 |
| 注入导致杀软或宿主崩溃 | 高/极高 | 独立决策门、签名、零 hook 里程碑、kill switch |

## 9. 建议排期

```text
P0  基准 + 搜索正确性/性能地基         1–2 周
P1  历史 + 正确缓存 + 前端测试         1–2 周
P2  工程质量 + 协议 + 配置 + 日志      1–2 周
P3a UIA/COM 可行性原型                 1–2 周
P3b DLL 注入（仅在单独批准后）          3–6 周以上
P4  拼音/FM/图标/动作配置              按项 1 天–2 周
```

上述估算是假设需求在每阶段开始前锁定、单人全职开发且不含发布等待。报告审核通过后，应按阶段分别创建任务，而不是把 P0–P4 合并成一个长任务。

## 10. 需要项目所有者审核的决策

在本报告转为正式路线前，需要明确以下事项：

- [ ] **内存目标**：继续以 ≤100MB 为硬门槛并把 40–45MB 作为期望，还是在三进程复测后设更严格硬门槛？
- [ ] **基础排序偏好**：应用/exe、精确/前缀、文件夹、短名称、路径深度的优先关系是什么？
- [ ] **历史数据**：是否默认落盘；记录 execute 和 reveal 哪些信号；容量和清除入口如何定义？
- [ ] **排除规则**：机器级硬排除是否允许用户解除；普通用户规则支持名称、路径前缀还是 glob？
- [ ] **拼音范围**：只做首字母，还是同时保留未来全拼/模糊匹配扩展点？
- [ ] **对话框覆盖目标**：Explorer、现代 `IFileDialog`、旧式对话框分别要求什么覆盖率？
- [ ] **DLL 注入边界**：是否接受 x64-only、放弃提权目标、数字签名成本和杀软申报？
- [ ] **日志隐私**：默认是否允许记录 query/path，还是仅在显式诊断模式记录？

在这些选项获批前，任何具体分值、40–45MB 红线、拼音内存数字、DLL 技术栈或总工期都只能保留为建议/估算。

## 11. 结论

Prism 当前最值得投入的不是直接扩张功能，而是把“搜索结果可用、性能可量化、行为可回归”做成稳定地基。P0–P2 能独立提升现有用户体验，并为历史、拼音和对话框联动提供可控接口；P3 注入则属于风险级别完全不同的产品决策，应在轻方案原型和安全评审后单独批准。

本报告保留现有两份原稿作为讨论历史，同时纠正其事实与技术偏差。待项目所有者审核决策清单后，再把获批部分转为 Trellis PRD/design/implement 工件。
