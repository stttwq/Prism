# Prism 综合优化与功能演进计划

> 状态：**G0–G4、G6–G9 已交付归档；G5 窗口切换器未启动**  
> 编制日期：2026-07-28；进度更新：2026-08-14  
> 编制时代码基线：`main` / `c0de808`；当前分支：`feature`  
> 输入文档：[`PRISM-OPTIMIZATION-REPORT.md`](./PRISM-OPTIMIZATION-REPORT.md)、[`POST-ROADMAP-REVISED.md`](./POST-ROADMAP-REVISED.md)  
> 性质：跨阶段实施蓝图。**已完成阶段的实际结论见 §0 与各阶段归档任务，本文正文保留编制时的
> 计划原文**——正文里的"当前""现状"均指 2026-07-28，不是今天的代码。

## 0. 进度快照（2026-08-14）

| 阶段 | 状态 | 归档位置 / 说明 |
| --- | --- | --- |
| G0 基线与基准 | **已交付** | `archive/2026-07/07-28-prism-g0-baseline`；`tools/bench/` 全套脚本 |
| G1 搜索正确性 + 协议 + 前端测试 | **已交付** | `archive/2026-07/07-28-prism-g1-search-foundation` |
| G9 首建可用性与进度 | **已交付** | `archive/2026-07/07-29-prism-g9-first-build` |
| G3 工程地基与权限边界 | **已交付** | `archive/2026-08/07-28-prism-g3-engineering-foundation` |
| G2 历史 + 拼音 | **已交付** | `archive/2026-08/07-28-prism-g2-history-pinyin` |
| G4 当前目录与宿主联动 | **已交付**（两个 adapter 默认关闭） | `archive/2026-08/07-28-prism-g4-host-integration`；兼容矩阵已签署 |
| G5 窗口切换器 | 未启动（planning，P2） | `tasks/07-28-prism-g5-window-switcher` |
| G6 完整内置动作 | **已交付** | `archive/2026-08/07-28-prism-g6-built-in-actions`；12 个动作（打开文件夹/复制/剪切/复制路径/重命名/复制到…/移动到…/回收站/永久删除/压缩ZIP/属性/打开方式） |
| G7 `ext:` / `path:` 过滤 | **已交付** | `archive/2026-08/07-28-prism-g7-query-filters`；broker 端解析 + Top-K 前过滤 |
| G8 网页图标与在线联想 | **已交付** | `archive/2026-08/07-28-prism-g8-web-enhancements`；专用 web mode + 可取消 800ms 联想 + favicon 缓存 + 内置引擎图标 |

**两个从实测中分出来的在办任务**（不在原 G 编号内）：

- `tasks/08-07-prism-ipc-resilience`（P1，in_progress）——G4 步骤 8 采集时暴露的两个缺陷。
  broker panic 不落盘**已修复并确证**；`ERROR_PIPE_BUSY` 的三处运行时阻塞已改正，但
  **因果链未证明**（A/B 对照显示未修复的旧二进制在更猛的合成洪峰下同样零失败），
  故不得标记为已修复。复现条件见该任务 PRD。
- `tasks/08-09-prism-g4-root-baseline`（P3，planning）——G4 步骤 8 唯一未完成项：G0 root
  作用域正式基准。需要安静机器 + `memory_bytes` ≥150MB 的完整规模索引。已有方向性证据
  （配对延迟差中位数 +2.0ms、`path_constructions` 因剪枝下降）不支持引入祖先缓存，但
  样本量（每组 10–14，目标 40）不足以签署。

### 未立项的已知代码问题

**`explorer /select` 的参数构造仍有两份（原 Q5，只部分解决）。** 2026-08-09 文档审计
时核实：

| 位置 | 触发路径 |
| --- | --- |
| `shell.rs::reveal` | `ShellOperation::Reveal` |
| `actions.rs::reveal_in_explorer` | `ShellOperation::RunAction` → `run_action_direct` 的 `open_folder` |

两者都在生产路径上，`normalized` / `arg` 的构造逐字相同，只有错误类型不同
（前者 `ShellError`，后者 `Result<(), String>`）。G3 删掉了 `ipc.rs` 里的第三份、
并把两条路径都收敛到 broker 的 STA worker，但**没有合并这段参数构造**。

原始风险「改一处忘改另一处」因此依然存在：同一个「打开所在文件夹」功能，
会因为走哪条入口而表现不一致。这不是用户当前能观察到的故障，是维护隐患。

未单独立项，因为改动面极小（抽一个共用函数），适合在下一次碰到 `shell.rs`
或 `actions.rs` 的任务里顺手做。[`CODE-QUALITY-FIXES.md`](./CODE-QUALITY-FIXES.md)
的 Q5 行已按此更正，不再写作「已抽取」。

### 已被实测推翻或修正的计划内容

正文相应位置保留原文，此处集中列出结论差异：

- **§2.3 的 100MB 口径已落地。** `.trellis/spec/backend/quality-guidelines.md` 的
  "Memory Acceptance" 已按三进程口径改写（G0 收尾完成）。2026-08-01 实测三进程私有工作集
  合计 55.2 MiB（拼音开启），拼音额外常驻约 0.7MB，均在门槛内。
- **§2.3 的 `opt-level` 取舍已决策：保留 `"z"`。** G0 采集的对照是 `z` 合计 31.1 MiB
  对 `3` 合计 51.1 MiB，`3` 几乎翻倍内存却未换来必要的延迟收益，`Cargo.toml` 未改。
- **§5.1「必须移除」的四条已全部移除。** 全局 Top-K、延迟路径构造、跨卷统一排序均已实现。
- **§7.1 的清理已完成。** `index.rs`、`search.rs` 已删；`installer/setup.iss` 已删（目录留空），
  真实安装脚本只有 `dist/prism.iss` 一处。
- **§13.3 的卸载缺口已补。** `dist/prism.iss` 的 `[UninstallDelete]` 现在包含
  `{commonappdata}\Prism`，索引缓存与拼音 sidecar 卸载后不残留。
- **§8.4 的放弃点未被触发。** Explorer 与 Opus 两个宿主都在时间盒内跑通并签署矩阵；
  `SystemFileDialog` 按计划**未实现**，由 `DisabledHostAdapter` 占位。
- **§14.3 的质量门当前实测**：Rust 262 通过、C# 134 通过、clippy `-D warnings` 无告警、
  WPF Release build 通过（2026-08-14）。§14.3 里的 `dotnet test` 目标是
  `src/Prism.Tests/Prism.Tests.csproj`。
- **§18 的审批约束已按阶段逐个满足**，不再适用于 G0–G4/G9；G5–G8 仍需单独审批后启动。

## 1. 目标与结论

本计划把两类工作合并到一条可执行路线：

1. 修复当前搜索结果质量、协议、缓存、测试和可观测性地基；
2. 在地基稳定后增加历史、拼音、目录联动、窗口切换、完整内置动作、有限高级语法和受控网页增强。

核心顺序是：**先测量，后修正确性；先建立共享契约，后扩展功能；每个阶段独立验收和回滚。**

当前项目已经具备 MFT/USN 索引、缓存恢复、generation 刷新、文件与应用搜索、网页关键词、WPF 交互和基础动作。主要缺口不是“功能数量”，而是：

- 搜索按 MFT record 顺序提前截断，缺少跨卷全局排序；
- 候选、路径构造和过滤尚未形成统一 Top-K 管线；
- 历史、拼音和范围搜索没有可复用评分契约；
- broker 协议缺少显式版本、稳定 kind 和截断语义；
- WPF 状态机没有自动化测试；
- Shell/COM、日志和权限边界需要在扩展动作前固化；
- 原后路线图的内存与工期数字缺少实测，部分技术方案不成立。

## 2. 真实基线与不可破坏约束

### 2.1 三进程边界

```text
Prism.exe（WPF，普通用户）
  └─ \\.\pipe\prism-core
       └─ prism-core.exe（broker，普通用户）
            └─ \\.\pipe\prism-indexer-v1
                 └─ prism-indexer-service.exe（LocalSystem）
```

| 进程 | 长期职责 | 禁止事项 |
| --- | --- | --- |
| WPF | UI、热键、宿主上下文、图标、网络联想、交互状态 | 不直接访问或修改特权索引 |
| broker | 应用/网页/窗口合并、历史、execute/reveal/actions、用户设置 | 不承担 MFT/USN 服务职责 |
| indexer | MFT/USN、缓存、generation、文件候选和只读搜索 | 不执行 Shell、剪贴板、用户命令或联网请求 |

Shell、COM 文件操作、剪贴板、用户设置、网页请求和应用启动必须留在普通用户会话。LocalSystem 管道继续保持有界、版本化、默认只读，不开放任意重建或写配置命令。

### 2.2 产品边界

- 目标用户：个人使用；
- 目标平台：Windows 11 x64；
- 兼容宿主：Explorer、Windows 系统应用标准打开/保存对话框、Directory Opus 13.23；
- UI：Prism 独立弹窗；
- 不使用 DLL 注入；
- 不提供剪贴板历史、文件预览、收藏、内容索引、HTTP API、跨设备、文本模板、JSON 动作或 DLL 插件；
- 窗口功能只做搜索与切换；
- 网页搜索是受控辅助入口，不改变本地搜索优先级与离线可用性。

### 2.3 性能门槛

| 指标 | 门槛 | 说明 |
| --- | --- | --- |
| 三进程 Release 总内存 | ≤100MB | **目标口径，尚未成为已提交门槛**，见下方说明 |
| 拼音额外常驻内存 | ≤10MB | 待实现验收目标，不是当前实测 |
| 暖查询 `max=8` | P95 ≤100ms | 端到端本地搜索 |
| 暖查询 `max=1000` | P95 ≤300ms | 展开结果兼容 |
| 在线联想 | 单次 800ms 超时 | 不计入本地搜索门槛，不阻塞直接网页结果 |

**100MB 门槛的口径必须先修正。** 现行已提交规格 [`.trellis/spec/backend/quality-guidelines.md`](../.trellis/spec/backend/quality-guidelines.md) 的 “Memory Acceptance (≤100MB hard gate)” 一节把该门槛定义为 **`Prism.exe` + `prism-core.exe` 两个进程 Private Working Set 之和**，写于三进程拆分之前，全文未提及 `prism-indexer-service.exe`。因此本文件所说的“三进程 ≤100MB”目前是**收紧后的目标**，不是已提交门槛。G0 负责按三进程口径实测，并在 G0 收尾时把该 spec 小节改写为三进程口径；在此之前任何阶段不得引用“三进程 100MB 已是硬门槛”。

**Release 构建以体积优先，与 P95 目标存在张力。** [`src/prism-core/Cargo.toml`](../src/prism-core/Cargo.toml) 的 `[profile.release]` 使用 `opt-level = "z"`（配合 100MB 内存护栏）。用体积优化的代码去追 P95 ≤100ms 会额外收紧余量，G0 必须把 profile 作为显式基线变量记录并测量 `"z"` 与 `"3"` 的差值，供 G1 判断是否需要在“体积/内存”与“延迟”之间重新取舍。该取舍属于 G1 的决策输入，G0 只提供数据。

旧两进程约 38MB、未来总内存 43–45MB、拼音固定增加 1MB、搜索提升 5–10 倍等数字不得写成事实。40–45MB 只保留为完成三进程复测后的优化期望。

## 3. 总体依赖

```text
G0 基线与基准
  └─ G1 搜索正确性 + 协议 + 前端测试
       ├─ G9 首建可用性与进度
       ├─ G3 工程地基（Shell/COM、日志、旧链、类型、schema 版本约定）
       │    └─ G2 历史 + 拼音
       ├─ G7 ext/path 过滤
       └─ G8 网页图标与联想

G2 + G3 ──> G4 当前目录与宿主联动
G2      ──> G5 窗口切换器
G2 + G3 ──> G6 完整内置动作
```

G0、G1 是所有功能阶段的强依赖。**G3 排在 G2 之前**：G3 定义 settings/history/favicon 的 schema 版本与兼容默认值约定（§7.3），而 G2 的历史文件是这套约定的第一个使用者（§6.1）。若先做 G2，同一套版本化约定会被发明两次，之后还要回改历史文件格式。**G9 建议紧跟 G1**：它是唯一直接影响首次安装体验的阶段，改动面集中在 indexer 首建路径，且 G2 的 sidecar 首建可复用其逐卷发布框架。G7、G8 与 G3 可在 G1 验收后并行，但在一个阶段内仍保持单一可回滚目标。G4–G9 不得反向修改尚未稳定的基础排序和协议语义。

## 4. G0：建立可复现基线

### 4.1 目标

建立后续所有性能、内存和正确性判断共用的测量基线，停止引用旧架构数字。

### 4.2 工作内容

- 固定 Release 构建、机器、Windows build、卷数、节点数、名字池容量和缓存版本（当前 `index-v5.bin`，见 [`index_cache.rs`](../src/prism-core/src/index_cache.rs)）；
- 把 `[profile.release]` 的 `opt-level` 记为显式基线变量，并采集 `"z"`（现状）与 `"3"` 两组延迟数据，供 G1 决定体积/延迟取舍；
- 建立查询集：常见 ASCII、中文、罕见词、无命中、精确/前缀/中间命中、跨卷、`max=8`、`max=1000`；
- 记录冷/暖 P50、P95、max、扫描节点数、匹配候选数、路径构造次数和响应大小；
- 单独测量“全量扫完所有卷所有节点”的裸耗时——G1 取消提前终止后这是每次查询的地板成本，也是判断是否必须引入并行搜索的唯一依据；
- 分别采集三个进程的 Private Working Set、Working Set、CPU，并记录 indexer `memory_bytes`；
- 预热后重复多轮，记录杀软、后台 I/O 和首次 JIT 等干扰；
- 原始命令和聚合结果必须可在同一机器复跑。

### 4.3 交付与验收

- 一组不依赖 GUI 人工计时的搜索基准脚本；
- 一组三进程内存采样脚本；
- 带环境元数据的基线记录；
- 把 [`.trellis/spec/backend/quality-guidelines.md`](../.trellis/spec/backend/quality-guidelines.md) 的 “Memory Acceptance” 小节从两进程口径改写为三进程口径（这是 G0 唯一允许的 spec 修改，且必须在 G0 收尾提交）；
- 任何后续性能主张必须同时给出基线与变更结果，不只报告最快单次。

**粗略估算：1–2 天。**

## 5. G1：搜索正确性、协议与前端测试

### 5.1 全局可解释 Top-K

文件搜索拆成三个阶段：

1. 遍历节点并生成轻量候选 `{volume, record, name_ref, flags, match_class, score}`；
2. 按请求 `max` 维护跨卷全局小顶堆，使用稳定 tie-break；
3. 只为最终候选调用 `path_for` 并生成 IPC 结果。

第一版评分只使用索引现有字段可以低成本提供的信号：字面精确、前缀、中间位置、名称长度和目录标记。应用、文件夹和文件进入同一评分框架；类型只作为最终平局规则。具体分值集中定义并用表驱动测试锁定，禁止散落 magic number。

不得（前三条是**当前 [`hierarchy.rs`](../src/prism-core/src/hierarchy.rs) 已有行为，必须移除**；后两条是**对旧提案的否决，当前代码并不存在**，勿误读为现状）：

- 命中 `max` 后按 MFT record 顺序提前终止（现状：`VolumeIndex::search` 的 `hits.len() == max` 即 `break`）；
- 每卷分别截断后简单拼接（现状：`IndexState::search` 逐卷用 `max - hits.len()` 填充）；
- 在候选阶段构造所有完整路径（现状：每个名称命中都立即 `path_for`）；
- 固定使用 200 条候选而破坏 `max=1000`（否决 [`PRISM-ROADMAP.md`](./PRISM-ROADMAP.md) 第 67 行“堆(容量=200)”的旧提案；当前代码无此上限，唯一的默认值是缺省 `max=100`）；
- 为追求速度改变现有 ASCII/Unicode 大小写语义而不写兼容规格。

### 5.2 broker 与 indexer 协议

broker 协议增加显式 handshake protocol number。当前 broker 只有 `Ping`/`Pong { version }`（[`ipc.rs`](../src/prism-core/src/ipc.rs)），而 indexer 侧已有 `Hello { protocol }` 与 `INDEXER_PROTOCOL`，本阶段把 broker 对齐到同一形态。

`results` 需要变更的字段，区分新增与已有，避免把已实现的东西再实现一次：

- `is_truncated`：**新增**，后端是否因 `max` 截断。定义必须**只**覆盖"因 `max` 放不下"这一种不完整；索引未就绪（`ready = false`）和索引部分就绪（G9 引入的 `ready && building`）都不算 truncated，各自用独立状态表达，见 §12.5.3；
- 稳定的 `kind` 字符串枚举，C# 为未知值保留 `Unknown`：**新增**。现状 `SearchResult.kind` 是裸 `String`，写入处直接用 `"app"`/`"file"`/`"folder"`/`"web"` 字面量。稳定 kind 集合**只在本阶段定义一次**，G3 §7.3 不再重复定义，只负责 typed action target；
- 可选匹配元数据，为拼音、高亮和解释排序预留兼容字段：**新增**；
- 可选 `filters` 请求字段：**本阶段只预留形状与上限，不实现任何过滤器语义**。G3 §7.4 的用户排除规则与 G7 的 `ext:`/`path:` 共用这一条通道，若 G1 不留位，二者会各自再改一次协议；
- `index_generation`：**已存在**，`Response::Results` 已有 `index_generation: Option<u64>`。本阶段只需把“索引就绪时稳定返回、不再为 `None`”写成契约并补测试，不要当作新字段实现。

indexer 协议保留显式版本，所有新搜索选项有数量、长度和 `max` 上限。协议不兼容时返回明确错误，不静默返回不完整结果。

### 5.3 正确的前端增量缓存

前端只有同时满足以下条件才可在旧结果上本地过滤：

1. 新 query 是旧 query 的前缀增长；
2. 旧响应 `is_truncated == false`；
3. generation 未变化；
4. 搜索模式、范围、过滤器和排序配置未变化。

删除字符、截断响应、模式切换、root 变化、配置变化或 generation 更新均重新请求后端。首屏 8 条不能被当作完整候选集。

### 5.4 WPF 测试地基

新增独立 C# 测试项目。把搜索客户端、generation 客户端和两个 debounce timer 抽成可替换接口；timer 处理器只绑定一次，通过 Restart/Stop 控制，避免 Tick 累积。

最低覆盖：

- Idle → Results → Actions → Results；
- 新输入取消旧业务结果，旧响应不覆盖新查询；
- 管道请求发出后仍完整读取对应响应，避免协议错位；
- generation 合并与重搜；
- 完整/截断缓存命中与失效；
- 首屏 8 条、More 行、展开 1000 条；
- 菜单、动作、Pin 和失焦隐藏状态；
- 未知 result kind 的安全降级。

### 5.5 验收

- 跨卷结果参与同一排名；
- 相同索引与查询输出稳定；
- 非最终候选不构造路径；
- 8/1000 行为不变；
- 基准同时报告正确性修复前后延迟与内存；
- Rust tests、Clippy、C# tests、WPF Release build 全绿。

**粗略估算：1–2 周。**

## 6. G2：使用历史与拼音

> 章节按 G 编号排列便于查阅，**实施顺序不同**：G2 依赖 G3 §7.3 定型的 schema 版本约定，也应在 G9 §12.5 之后（sidecar 首建复用其逐卷发布框架），见 §3 与 §17。

### 6.1 历史

历史由普通权限 broker 管理，默认开启，可关闭和一键清除：

```text
HistoryEntry {
  stable_target,
  target_kind,
  execute_count,
  reveal_count,
  destination_count,
  last_used_utc
}
```

- 最多 500 条；90 天未使用淘汰；
- 仅成功操作更新历史；
- 文件、文件夹、应用和窗口进入历史，网页 query 不进入；
- 文件带 schema 版本，使用临时文件 + 原子替换；
- 损坏时保留诊断信息并回到空历史；
- 默认日志不记录完整路径、窗口标题或 query；
- 历史只在同一匹配等级中加权，不得让弱拼音命中超过强字面命中。

窗口历史保存稳定应用身份和必要的标题信息，但空输入展示前必须与当前窗口枚举求交集，不能显示已关闭窗口。

### 6.2 拼音产品契约

完整行为以 [`POST-ROADMAP-REVISED.md`](./POST-ROADMAP-REVISED.md#32-拼音行为) 为准。关键约束是：

- 文件、文件夹、应用与窗口标题支持首字母和全拼；
- 不匹配父路径；
- 至少两个拉丁字母触发；
- 全拼可在音节边界起始并允许最后音节前缀；
- 不支持首字母/全拼混输和模糊读音；
- 简体、常用繁体和常见多音词组；
- 字面 > 全拼 > 首字母，再比较精确/前缀/中间位置；
- 命中范围映射回汉字高亮。

### 6.3 sidecar 设计门

采用独立、版本化、只读的 `pinyin-v1.bin`，不扩大 12B `NodeSlot`：

- 构建时生成紧凑音节 ID、字符到读音映射和有限词组覆盖；
- 基础数据可 mmap，USN 新增/改名使用小型 delta 与 tombstone；
- 关闭功能后可卸载可选映射；
- sidecar 缺失、损坏、版本不符或重建时只禁用拼音，不影响字面搜索；
- 不向普通客户端开放特权重建命令；如需释放缓存，只允许有界的可选缓存释放语义；
- 字典/库的许可证、繁体覆盖和多音词来源在实现前记录到阶段研究文件。

sidecar 的最终所有权和共享方式必须通过原型比较，避免 indexer 与 broker 各自复制一份大字典。选择标准是三进程总内存、关闭后释放、窗口瞬态名称评分和协议复杂度，而不是预设实现。

### 6.4 验收

- `wx`、`weixin`、`weix`、`xin` 命中 `微信`；
- `eix`、`wxkaifa` 不命中；
- `wx2026`、`weixinbeta`、大小写、`v/ü`、空格/撇号行为有测试；
- 简体、常用繁体、多音词和默认读音有固定语料；
- 高亮字符区间正确；
- 关闭和损坏时可靠回退字面搜索；
- 拼音额外常驻内存 ≤10MB；
- 暖查询满足 8/1000 P95 门槛，ASCII 查询无不可解释回归。

**粗略估算：2–3 周。**

## 7. G3：工程地基与权限边界

### 7.1 旧链与公共 Shell 层

- 把 `index.rs` 旧测试链仍有价值的测试迁到当前 `hierarchy/indexer_client` 路径；
- 删除已无生产用途的旧索引链。已核实 `src/prism-core/src/index.rs`（914 行、16 个测试；**已于 G3 的 `23dbad4` 删除，故此处不再链接**）的全部 5 处外部引用都在 `#[cfg(test)]` 下，`ipc.rs` 里用到它的 `dispatch` 函数本身也是 test-only，broker 生产路径走 `indexer_client`；但 `lib.rs` 的 `pub mod index;` 未加 cfg，lib target 仍会编译它，这正是本阶段要消除的；
- 删除孤立占位文件，**点名两个**：`src/prism-core/src/search.rs`（3 行注释占位，无任何引用）和 `installer/setup.iss`（2 行占位；真实安装脚本是 `dist/prism.iss`，两者并存会误导后续改安装包的人）；
- 抽取公共 Shell 模块，统一 reveal、属性、打开方式和外部启动；
- Rust 测试数量不得因简单删除而无理由下降。

### 7.2 专用 Shell/COM 执行器

broker 建立专用 STA Shell worker：

- 在同一线程初始化和释放 COM apartment；
- 所有 `IFileOperation`、属性、打开方式和相关 Shell COM 调用串行进入 worker；
- async IPC 只等待任务结果，不在任意 Tokio worker 上临时初始化 apartment；
- 错误返回稳定分类：取消、拒绝、目标失效、权限、冲突、系统错误；
- 用户取消不是故障，不写高等级日志。

### 7.3 协议类型与设置 schema

- 稳定 result kind 集合由 G1 §5.2 定义，本阶段只做**消费与落地**，不重新定义；
- action target 带类型，不再仅靠 `execute_id` 字符串猜测 URL、路径、应用或窗口。现状 `execute_id()` 用 `websearch::is_http_url(id)` 判断是 URL 还是路径，窗口和应用没有独立表示；
- settings、history、favicon cache metadata 均带 schema 版本。**这套版本化约定在本阶段一次定型**，G2 的 history 文件是它的第一个使用者，因此 G3 必须先于 G2 完成；
- 读取旧配置时使用兼容默认值，写入新格式前完成验证。

### 7.4 排除规则

- 保留现有机器级硬排除，并写明理由；
- `Windows\Installer` 继续保持父路径语义，不改成全局同名排除；
- 用户过滤存于普通用户设置；
- broker 随只读 search 请求传递有界过滤快照，**复用 G1 §5.2 预留的 `filters` 协议字段**，不新开一条通道；G7 的 `ext:`/`path:` 之后往同一字段里加类型。由 indexer 在 Top-K 前应用；
- 不让 LocalSystem 服务直接读取某个用户的 LocalAppData，也不开放任意写配置命令。

### 7.5 日志

- broker 与 indexer 使用不同日志文件；
- 日志具备 level、event、elapsed、generation 等结构字段；
- 默认不记录完整 query、路径、窗口标题和网页查询；
- 只有显式诊断模式允许敏感字段，并在设置中说明；
- 轮转能力按实际依赖编写规格：若要求大小轮转，选择明确支持的 writer；
- 服务启动失败保留 Windows Event Log 兜底；
- 日志失败不能阻塞搜索。

### 7.6 验收

- Shell/COM apartment 测试和取消路径稳定；
- 两进程日志不争用同一文件；
- 磁盘只读、目录创建失败、异常退出不影响主流程；
- 协议兼容矩阵覆盖旧客户端、未知 kind、缺失可选字段和明确不兼容；
- 生产依赖图不再编译旧索引实现。

**粗略估算：1–2 周。**

## 8. G4：当前目录搜索与宿主联动

### 8.1 范围搜索

broker `search` 与 indexer `search` 增加可选 root。root 必须：

- 规范化为绝对路径；
- 映射到已索引卷与记录；
- 有长度上限；
- 不存在、非 NTFS 或不在索引中时返回可解释降级结果。

低内存首版不增加 child/sibling 指针：

1. 名称匹配生成轻量候选；
2. 沿候选的 `parent_record` 链验证是否属于 root；
3. 合格候选进入全局 Top-K；
4. 最终候选才构造路径。

祖先验证设置最大深度并检测环与缺失父节点。只有 G0/G1 基准证明该方案无法满足 P95，才评估额外子树或祖先缓存，并单独计算内存。

### 8.2 WPF 宿主上下文

新增显式 HostContext 状态，至少区分：

- None / Global；
- Explorer；
- SystemFileDialog；
- DirectoryOpus。

呼出前保存前台 HWND，再识别宿主和当前目录。识别失败立即退回 Global，并显示非阻塞提示；禁止复用上次目录。

UI 显示范围标签，点击或 `Ctrl+G` 切换当前目录/全局。总开关默认开启、可关闭。

### 8.3 无注入双向联动

- Explorer：优先 Shell COM，必要时使用有测试的 UIA 地址栏路径；
- 系统文件对话框：UIA 只操作可访问控件，不把 UIA 描述成 `IFileDialog::SetFolder`；
- Directory Opus 13.23：优先官方 `dopusrt`/命令接口，参数使用结构化转义；
- 标准对话框只回填或选择，不自动点击“打开/保存”；
- Explorer/Opus 中 Enter 普通打开，`Ctrl+Enter` 才交回宿主定位；
- 不支持浏览器、Office、其他第三方管理器、提权宿主或 DLL 注入。

### 8.4 空输入与验收

- 支持宿主上下文：空输入显示 root 内相关最近项；
- 其他上下文：空输入显示当前仍存在的最近窗口；
- root 搜索递归覆盖子目录；
- 覆盖多窗口、多标签、宿主关闭、路径变化、中文路径、长路径和访问拒绝；
- UIA/COM/Opus 失败时仍可全局搜索和普通打开；
- 原型先形成兼容矩阵，再进入产品实现。

**原型阶段必须设显式放弃点。** 兼容矩阵是本计划里唯一无法靠读代码预估的部分。进入原型时先约定一个时间盒（建议 2 周）与最低可交付集：若到期时 Explorer 之外的宿主仍无法稳定取到当前目录，则本阶段只交付 Explorer + Global 两种上下文，`SystemFileDialog` 与 `DirectoryOpus` 退回 Global 并从本阶段验收项中移除，不允许无限延长原型。

**粗略估算：原型 1–2 周，产品化 1–2 周。**

## 9. G5：窗口切换器

### 9.1 行为

- `>` 前缀进入窗口模式；
- 匹配可见顶层窗口标题和应用名；
- 支持字面、拼音与历史排序；
- 窗口列表按查询实时枚举，不长期保存 HWND；
- execute 前重新验证 HWND、PID 和窗口可见性；
- 切换成功后记录历史并隐藏 Prism；
- 切换失败显示错误，不结束目标进程。

窗口模式只提供切换，不实现关闭、最小化、最大化、置顶、分屏、跨屏或进程管理。

### 9.2 接口

- 新增稳定 `window` result kind；
- `execute_id` 使用仅本次枚举有效的 opaque id，不把原始 HWND 当长期身份；
- broker 在普通用户会话枚举窗口并返回应用名、标题和必要的激活数据；
- WPF 隐藏自身后执行最终激活或调用 broker 的有界切换命令；具体所有权以可靠前台切换原型为准。

### 9.3 验收

- 多窗口同应用、标题变化、窗口关闭竞态和最小化恢复；
- Prism 自身、无标题、工具窗口和不可切换后台窗口被过滤；
- 中文标题拼音与高亮正确；
- 空输入只显示当前仍存在的历史窗口；
- 无常驻窗口列表导致的持续内存增长。

**粗略估算：3–5 天。**

## 10. G6：完整内置动作

### 10.1 动作清单

文件/文件夹：

- 定位、复制、剪切、复制路径；
- 复制到、移动到、重命名；
- 用其他应用打开（仅文件）、属性；
- 移入回收站、永久删除；
- 压缩为 ZIP。

应用：

- 定位真实可执行程序；
- 复制目标路径；
- 属性；
- 以管理员身份运行。

不增加 JSON 命令、DLL 插件、任意 Shell verb 或多选批处理。

### 10.2 动作子流程

“复制到/移动到”进入显式子状态：

1. 显示最多 8 个最近目录；
2. 先取历史操作目标，再补近期访问目录；
3. “搜索更多”切换为仅文件夹搜索；
4. 选择目标后交给 Shell worker；
5. 取消返回动作列表，恢复原查询与选择。

前端状态机必须用显式 mode/flow 表达，不继续堆叠布尔值。

### 10.3 文件系统行为

- 复制、移动、回收站、永久删除使用 `IFileOperation`；
- 冲突、权限、进度与取消使用 Windows 标准界面；
- 永久删除每次确认，不提供关闭确认设置；
- 重命名在 Prism 内联，默认不选扩展名，最终操作仍由 Shell worker 执行；
- 打开方式调用 Windows 系统选择界面；
- ZIP 输出格式固定，优先检测 7-Zip，缺失时使用 Windows 11 内置能力；
- 不调用 WinRAR，不枚举任意 Shell 压缩扩展；
- 压缩后若目标已存在，使用系统确认/冲突语义，不静默覆盖。

### 10.4 动作后状态

- 打开、定位、属性、打开方式、复制、剪切、复制路径成功后隐藏；
- 重命名、移动、删除、压缩后保留窗口；
- 等待 generation 更新或有界超时后重搜；
- 超时只提示索引尚未刷新，不把成功的 Shell 操作标记为失败。

### 10.5 验收

- 单文件、文件夹、中文、长路径、只读、权限拒绝、重名、取消；
- 复制到自身、移动到子目录、源在操作前消失；
- 回收站与永久删除语义严格区分；
- 7-Zip 存在/缺失两条路径均生成 ZIP；
- `.lnk` 启动保留参数，应用动作针对真实目标；
- 所有 Shell/COM 调用都在普通用户 broker 的专用 worker；
- mutation 后 UI 与 indexer generation 最终一致。

**粗略估算：2–4 周。**

## 11. G7：`ext:` 与 `path:` 过滤

### 11.1 语法

```text
report ext:pdf
design ext:md,pdf path:"Project Docs"
```

- 普通 token 是名称查询；
- `ext:` 逗号分隔值取 OR；
- `path:` 对规范化完整路径做不区分大小写的字面子串匹配；
- 不同过滤器与 root 之间取 AND；
- 带空格值使用引号；
- 未识别或不完整过滤 token 按普通文本处理；
- 有过滤器时只返回文件/文件夹。

不实现 `size:`、`datemodified:`、负向过滤或自然语言日期。

### 11.2 执行位置

broker 负责解析并生成结构化过滤器，**写入 G1 §5.2 预留、G3 §7.4 已开始使用的同一个 `filters` 协议字段**，不新增第二条过滤通道；indexer 在候选进入 Top-K 前应用过滤。`path:` 可能需要祖先/路径信息，第一版在名称候选通过后做有界路径或父链验证，但仍不得先取 `max` 再过滤。

### 11.3 验收

- 单值、多扩展、引号、未知 token、空值和重复过滤器；
- root + ext + path 组合；
- 过滤后仍能返回足额 Top-K；
- 不混入应用、窗口或网页结果；
- `max=8/1000` 性能与路径构造次数可解释。

**粗略估算：2–4 天。**

## 12. G8：网页图标与在线联想

### 12.1 专用网页模式

识别网页关键词后：

- 立即显示一条直接搜索结果；
- 不混排本地文件、应用或窗口；
- 在线联想开启时异步追加最多 5 条；
- 输入变化取消旧请求并丢弃迟到响应；
- 直接搜索不等待网络。

### 12.2 联想服务

- WPF 使用单例、可取消的 HTTP 客户端；
- 默认关闭，设置中明确说明查询会发送给第三方；
- 只为内置 Bing、百度、Google 提供经过测试的 adapter；
- 自定义引擎不支持 suggestion API；
- 800ms 超时，断网、限流、证书或解析失败静默退回直接搜索；
- 不记录 query、响应或联想选择到历史和默认日志；
- 限制响应字节数、JSON 深度、条目数和单条长度。

### 12.3 图标与 favicon

- 内置引擎图标随程序打包；
- 自定义引擎保存/URL 变化时单独征求 favicon 联网授权；
- 只允许 http/https，限制重定向、响应大小、MIME 和解码尺寸；
- 缓存键包含规范化 origin，使用有界磁盘缓存和版本化 metadata；
- 拒绝、失败或缓存损坏时使用通用网页图标；
- favicon 获取开关与在线联想开关独立。

### 12.4 验收

- 默认状态零联想网络请求；
- 直接结果在本地延迟门槛内出现；
- 取消、超时、迟到、无效 JSON、超大响应和断网；
- 三个内置引擎分别有固定解析样本；
- favicon 授权、拒绝、缓存命中、过期和损坏回退；
- 网页查询不进入 history 或普通日志。

**粗略估算：4–7 天。**

## 12.5 G9：首建可用性与进度

> G9 是后补阶段，编号取 12.5 以避免重排 §13–§18 及正文中的既有交叉引用；其地位与 G0–G8 等同，实施顺序见 §3 与 §17。

`0296463` 实装 USN 实时监听后，稳态新增文件延迟已降到秒级，但**首建本身没有被加速**——watcher 只在首建完成后启动。原 spec 里"USN 快速路径把首建从分钟压到秒级"的期望并未实现，这条债务此前不在本计划任何阶段内，容易被再次遗忘。

### 12.5.1 当前阻塞点

`indexer_runtime.rs::run()` 用 `spawn_blocking(load_or_build).await` **等全部卷建完**才 `state.publish(index)`；在此之前 `state.index` 为 `None`，`status()` 返回 `ready: false`，`search()` 直接返回 `Err("file index is not ready")`，broker 见 `!status.ready` 便早退回空 items。`build_all()` 是串行 `for` 循环，无卷优先级、无并行。`IndexerStatus` 没有任何进度字段，前端只能显示不确定文案。

首建期间应用与网页结果已可用（broker 的 `prefix_results` 在 indexer 调用之前执行），缺的只有文件/文件夹——这一点已成立，不需要重复实现，但需要测试锁定。

### 12.5.2 方案

核心是把"一次发布"改为"逐卷发布"：

1. 系统卷（`%SystemDrive%`）显式排首位，不依赖 `discover_volumes()` 的字母序巧合；
2. 每个卷 `build_volume` 完成即并入 live index 并发布（`generation + 1`），该卷文件立刻可搜；
3. 该卷发布后立即启动它的 USN watcher，不等首建整体结束；
4. 只有全部卷完成后才写 v5 缓存；
5. `IndexerStatus` 增加可选进度结构（卷总数/已完成数/当前卷/可选记录数与估算），broker 透传，前端显示可解释进度。

可行性依据是 `build_volume` 的既有顺序：先 `query_or_create_journal` 取 USN checkpoint，再 `enumerate_mft`，最后 `replay_until(current.next_usn)`。返回时 `volume.next_usn` 已推进到枚举结束时刻，watcher 从该点续读，**发布与起 watcher 之间的空档不会丢事件**。

### 12.5.3 两个必须处理的正确性问题

**残缺缓存。** `run()` 退出前无条件调用 `checkpoint(&state, &data_dir)`。逐卷发布后，首建中途停机会把部分索引写成 v5 缓存。现有 `validate_checkpoints` 的 `volumes.len() != descriptors.len()` 能挡住多数情况，但那是巧合式防护。必须加显式"首建完成"门。

**三种"不完整"不能混用同一字段。**

| 原因 | 表达 | 前端反应 |
| --- | --- | --- |
| 因 `max` 截断 | `is_truncated = true`（G1） | 禁止在其上本地过滤 |
| 索引完全未就绪 | `ready = false` | 等待并轮询 |
| 索引部分就绪（本阶段新增） | `ready = true && building = true` | 可用，但须随 generation 刷新 |

第三种是本阶段引入的新组合。§5.3 的前端缓存规则要求 `is_truncated == false` 才允许本地过滤——若部分索引下返回 `is_truncated = false`，前端可能在不完整结果集上本地过滤而漏结果。逐卷发布每卷 `generation + 1`、缓存键含 generation，理论上会自然失效，但这是推理不是保证，必须写针对性测试。G1 定义 `is_truncated` 时要显式写明它不含"索引未建完"。

### 12.5.4 枚举加速（数据驱动，允许为空）

以 G0 基线判定，只保留有实测收益者：`enumerate_mft` 的 256KB 缓冲区上调；`build_volume` 中最多 64 遍 pending 重试循环的实际遍数（records 已按 FRN 排序，预期 1–2 遍）；系统卷先跑、其余卷并行（单物理盘上并行会互抢 IO，必须区分测量）。任一项无收益即记录"已评估、放弃"，不为凑数保留复杂度。本节允许全部落空——逐卷发布已把可感知等待从"全部卷"降到"系统卷"。

### 12.5.5 验收

- 系统卷建完即可搜该卷文件，其余卷仍在建索引且 `status` 如实反映；
- 卷 A 发布、卷 B 仍在建索引期间，卷 A 的增删改可被搜到；
- 首建中途停机后重启走完整重建，磁盘无被误认为完整的残缺缓存；
- 部分索引下前端不会在不完整结果集上本地过滤漏结果；
- 首建期间应用与网页结果可用（测试锁定）；
- 与 G0 同口径报告"首建到系统卷可搜"与"到全部卷可搜"两个时间。

**粗略估算：4–6 天。**

## 13. 跨阶段公开契约

### 13.1 broker IPC

建议演进为显式版本协议，并在阶段内定义稳定 JSON 线格式：

- handshake：protocol number + 应用版本；
- search：query、max、可选 root、结构化 filters、启用的匹配能力；
- results：items、is_truncated、index_generation、索引状态；
- result：稳定 kind、title/subtitle、opaque target、match kind/spans，可选 icon/source key；
- actions：typed target、动作 id、显示信息和是否进入子流程；
- run_action：typed target、action id、必要的有界参数；
- 未知 kind/字段由 reader 安全忽略或映射 Unknown，不崩溃。

不得把内部 Rust enum 名、HWND、任意命令行或特权路径直接当作稳定 ABI。

### 13.2 indexer IPC

- 保留 hello/version、status、search、wait_generation；
- search 扩展可选 root、filters、匹配选项，但仍是只读查询；
- 每个字符串、列表、max 和超时都有上限；
- root/filters 在 Top-K 前应用；
- pinyin sidecar 重建由服务内部版本/缓存生命周期管理，不向普通客户端暴露 rebuild；
- protocol 不兼容返回明确错误。

### 13.3 本地数据

| 数据 | 所有者 | 位置/性质 | 恢复策略 |
| --- | --- | --- | --- |
| index-v5/后续版本 | indexer | `%ProgramData%\Prism\index-v5.bin`，机器级 | 不兼容时可解释重建 |
| pinyin sidecar | indexer/最终设计所有者 | 机器级、只读、版本化、可卸载 | 缺失时退回字面搜索 |
| history | broker | 用户目录、版本化 | 损坏回空历史 |
| settings | WPF/broker | 用户目录、版本化 | 缺字段使用安全默认 |
| favicon cache | WPF | 用户目录、有界缓存 | 损坏/过期重新获取或通用图标 |

**卸载清理目前有缺口，必须补上。** [`dist/prism.iss`](../dist/prism.iss) 的 `[UninstallDelete]` 只删除 `{app}\data`，而机器级索引缓存写在 `%ProgramData%\Prism\`（[`index_cache.rs`](../src/prism-core/src/index_cache.rs) 的 `machine_data_dir()`），卸载后会残留。G2 引入 pinyin sidecar 会再多一个同目录残留文件。因此：

- G3 负责在安装脚本里补 `%ProgramData%\Prism\` 的卸载清理（索引缓存属于可重建的机器级派生数据，不是用户数据）；
- G2 新增 sidecar 时必须同步更新安装/卸载清单，不得只加生成逻辑；
- 任何后续阶段新增机器级或用户级持久文件，都要在同一阶段内更新 `dist/prism.iss`。

## 14. 统一验收矩阵

### 14.1 自动化

- Rust：候选评分、Top-K、跨卷、路径延迟构造、root、filters、协议、history、拼音、actions；
- C#：VM 状态机、debounce、generation、截断缓存、范围切换、窗口模式、动作子流程、网页取消与超时；
- 协议：旧/new reader-writer 组合、未知 kind、缺字段、超限请求、错误配对；
- 固定语料：ASCII、Unicode、简繁中文、多音词、长路径、跨卷和无命中。

### 14.2 机器验收

- Windows 11 x64；
- 三进程 Release、索引就绪；
- Explorer 多窗口/标签；
- Windows 系统打开/保存对话框；
- Directory Opus 13.23；
- 7-Zip 安装与未安装路径；
- 普通/拒绝访问目标；
- USN mutation 后 generation 与结果刷新。

### 14.3 持续质量门

每阶段完成前执行：

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <新增的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

涉及索引、范围、拼音或动作的阶段还必须执行对应机器脚本、内存和延迟采样。静态检查通过不能替代机器验收。

## 15. 发布、回滚与功能开关

- 每个 G 阶段单独创建任务、提交和验收；不把 G0–G9 合成一个长任务；
- 协议变更按“可选字段 + reader 安全忽略未知值”设计，但**不需要跨版本分阶段发布**：三个二进制由同一个 Inno 安装包一次替换，`dist/prism.iss` 的 `PrepareToInstall` 先 `sc stop PrismIndexer` 再覆盖文件、`ssPostInstall` 重新 `sc start`，`CloseApplications=force` 处理 WPF 与 broker，因此不存在新旧混版同时运行的窗口。真正要守的是**磁盘上的持久数据跨版本可读**（缓存、history、settings、favicon metadata），而不是线上协议的灰度顺序；
- 索引缓存只在持久结构变化时升级版本，并保留可解释重建；
- 新增任何持久文件的阶段必须同步更新 `dist/prism.iss` 的安装与卸载清单（见 §13.3）；
- history、pinyin、宿主联动、在线联想分别有独立开关；
- 拼音失败回退字面搜索；宿主识别失败回退全局搜索；联想失败回退直接网页搜索；
- 文件 mutation 不使用“自动回滚”伪装成功，错误和用户取消必须可区分；
- 新排序可在阶段发布期间保留旧实现开关，完成正确性与性能对照后再删除；
- 不设置 DLL 注入 kill switch，因为注入本身不进入路线。

## 16. 风险登记

| 风险 | 概率/影响 | 缓解与决策门 |
| --- | --- | --- |
| 正确 Top-K 取消提前终止后 P95 上升 | 高/高 | G0 基准、轻候选、延迟路径构造、按 max 堆选 |
| 全量扫描地板成本本身就吃掉大部分预算 | 中/高 | G0 单独测裸扫描耗时；若 ≥60ms，G1 必须同时引入分片并行搜索，工期按 G1a/G1b 重估 |
| `opt-level = "z"` 使 P95 目标无法达成 | 中/中 | G0 采集 `"z"` 与 `"3"` 对照；取舍在 G1 决策，并同时复测 100MB 门槛 |
| 拼音数据超过 10MB 或拖慢 ASCII | 中/高 | sidecar 原型、稀疏表示、可卸载、独立门禁 |
| root/path 父链验证放大 CPU | 中/中 | 仅对名称候选验证、缓存原型必须先测内存 |
| 前端缓存漏结果 | 高/高 | is_truncated + generation + 模式/范围键 |
| UIA 对话框兼容性不足 | 高/中 | 只承诺系统应用、明确 fallback、不注入；G4 原型阶段设显式放弃点（见 §8.4） |
| Opus 版本行为变化 | 中/中 | 固定 13.23 验收、官方接口、失败退回全局 |
| IFileOperation apartment/取消错误 | 中/高 | 专用 STA worker、错误分类、机器测试 |
| 永久删除误操作 | 低/极高 | 每次系统确认、无关闭设置、动作明确分组 |
| 历史泄露敏感路径/标题 | 中/中 | 用户级文件、90 天淘汰、可关闭/清除、默认日志脱敏 |
| 联想泄露输入或阻塞搜索 | 中/高 | 默认关闭、专用模式、异步 800ms、零 query 日志 |
| favicon 恶意或超大响应 | 中/中 | 单独授权、协议/MIME/大小/解码限制、有界缓存 |
| 旧代码清理丢测试 | 中/中 | 先迁移现行链覆盖，再删除旧模块 |
| 新增持久文件未进卸载清单 | 中/低 | §13.3 要求同阶段更新 `dist/prism.iss`，卸载后检查 `%ProgramData%\Prism\` 为空 |
| G9 逐卷发布期间丢 USN 事件 | 中/高 | `build_volume` 先取 checkpoint 再枚举，watcher 从 `volume.next_usn` 续读；机器测试"卷 A 发布后在卷 A 增删改可见" |
| G9 首建中途停机写出残缺缓存 | 中/高 | 显式"首建完成"落盘门，不依赖 `volumes.len()` 巧合防护；测试中途 Stop 后磁盘无缓存 |
| 部分索引被前端当完整集缓存 | 中/高 | `ready && building` 组合 + generation 每卷递增使缓存失效；`is_truncated` 定义排除"索引未建完"，并写针对性测试 |

## 17. 建议排期

| 顺序 | 阶段 | 粗略单人工期 | 可独立交付结果 |
| --- | --- | --- | --- |
| 1 | G0 基线 | 1–2 天 | 可复跑数据 |
| 2 | G1 搜索/协议/测试 | 1–2 周 | 结果正确、可回归 |
| 3 | G9 首建可用性 | 4–6 天 | 首次安装不再分钟级不可用 |
| 4 | G3 工程地基 | 1–2 周 | 可安全扩展 Shell/协议，schema 版本约定定型 |
| 5 | G2 历史/拼音 | 2–3 周 | 个性化与中文搜索 |
| 6 | G4 当前目录/联动 | 2–4 周 | Explorer/系统对话框/Opus 工作流 |
| 7 | G5 窗口切换 | 3–5 天 | `>` 模式与最近窗口 |
| 8 | G6 内置动作 | 2–4 周 | 完整单项文件操作 |
| 9 | G7 ext/path | 2–4 天 | 有限高级过滤 |
| 10 | G8 网页增强 | 4–7 天 | 图标与受控联想 |

按每周 5 个工作日折算，**合计约 11–20 周单人工期**。总工期不能简单按最小值承诺。以上均为单人粗略估算，不含需求等待、兼容性研究、代码评审、真实机器问题和发布观察。

两处估算需要特别提醒：

- **G1 的 1–2 周偏紧。** 它同时包含 Top-K 重写、broker 协议版本化、以及从零搭建 C# 测试工程（`SearchViewModel` 目前直接 `new DispatcherTimer()`，要脱离 Dispatcher 跑测试必须先把两个 timer 和两个管道客户端抽成接口）。若 G0 的裸扫描数据显示还需并行搜索，建议拆成 G1a 排序正确性与 G1b 协议/测试地基两次交付。
- **G4 的 UIA/Opus 兼容矩阵实际耗时普遍超估算。** §8.4 已承诺失败时退回全局搜索，但没有写放弃条件；原型阶段应先设定一个明确止损点，超过即只交付 Explorer 一种宿主。

G5、G7、G8 在依赖满足后可调整先后，但不得绕过 G0/G1，也不得在 G3 之前落地任何持久化 schema。

## 18. 明确排除与文档治理

综合计划明确删除或拒绝以下旧方向：

- 剪贴板历史及 `c ` / `c:` 模式；
- 文件预览、Space 预览面板；
- 收藏夹和 `pins.json`；
- size/date 过滤；
- 内容索引或临时全文扫描；
- HTTP API、跨设备搜索；
- 内嵌/贴靠第三方窗口的搜索 UI；
- 文本片段和模板；
- JSON 自定义动作、DLL 插件和 DLL 注入；
- 窗口关闭、最小化、置顶、分屏和进程管理；
- Windows 10、ARM64、x86、浏览器/Office 对话框和其他第三方文件管理器兼容承诺。

本文件是 [`PRISM-OPTIMIZATION-REPORT.md`](./PRISM-OPTIMIZATION-REPORT.md) 与 [`POST-ROADMAP-REVISED.md`](./POST-ROADMAP-REVISED.md) 的综合规划入口。前两份文档继续保留：优化报告提供事实审计，收敛稿提供产品行为；本文件负责阶段依赖、接口、门禁和回滚。

在项目所有者批准某个具体阶段前：

- 不运行 `task.py start`，不把任何规划任务切换为 `in_progress`；
- 不修改 `.trellis/spec/`；
- 不把路线、分值、工期或目标描述为已批准实施；
- 每次只批准一个有独立验收和回滚边界的阶段。

对应 Trellis 任务树位于 `.trellis/tasks/07-28-prism-comprehensive-evolution/`。父任务只管理来源、依赖和跨阶段验收；G0-G9 子任务分别拥有 PRD、技术设计、实施清单和上下文清单（G9 位于 `.trellis/tasks/07-29-prism-g9-first-build/`）。默认从 G0 开始，任何子任务都必须在依赖归档且所有者单独审批后才能启动。
