# PRISM 统一命令系统 K3 施工方案

**文档版本**: K3-IMPL-2026-08-28
**上游设计**: `docs/PRISM-COMMAND-SYSTEM-DESIGN-2026-08-22.md`（v3）§5 / §10 / §11 / §12-K3 / §14
**上游阶段方案**: `docs/PRISM-COMMAND-SYSTEM-K2-IMPLEMENTATION-2026-08-28.md`
**K2 基线**: commit `193e281`（K2 commit 8 收尾）；其后有三个非 K2 增量提交（`9fa3ef3` 图标、`e9c58e5` OpenTerminalHere 动作面板目录、以及两个 dist 构建提交），K3 以 `7ee21bf` 工作树为准
**目标状态**: K3 — 用户自定义命令闭环、关键字路由、网页引擎命名空间统一、Fallback 行

---

## 0. 给施工者的阅读顺序

1. §1「施工总原则」——决定后面每一步为什么这么切，K3 的原则与 K2 不同：K2 是「联动」，K3 是**首次引入不可信输入与用户态代码执行**，安全边界是第一约束。
2. §2「K2 完成状态核对」——确认基线，顺带看清 K1/K2 留下的两个真实缺口。
3. §3「现状事实核对」是代码坐标表，动手前逐条确认还在，不在就先停下来问。
4. §4「关键架构决策」是核心，每条写了「怎么做 + 为什么」。**不要跳过理由部分**——K3 的坑集中在「类型收口」和「命名空间裁决顺序」两处，省一步就是安全洞或数据丢失。
5. §5 起是按 commit 切分的实施序列，照序做。

---

## 1. 施工总原则

### P1 类型收口优先于校验（最高优先级）

用户命令**永远不能表达出内置特权 handler**。这不能靠运行时校验实现，必须靠类型：

- 持久化侧（`UserCommandDefinition`，Deserialize）新增的 handler 字段是**独立的用户态枚举** `UserHandlerKind { OpenUrl, LaunchProgram }`，与 `BrokerHandlerId`（内置 handler）**没有任何转换路径**，不实现 `From`、不共享字符串表、不在同一个 match 里解析。
- 用户命令的 `owner` / `trust` 依旧由代码赋值（`broker` / `user`），不从 JSON 读——这是 K0 已建立的纪律（`persistence.rs:219-220` 注释），K3 只是把它延伸到 handler 维度。

**为什么**：设计 §10.1 的信任等级表如果只靠 `if trust == "user" { reject_privileged() }` 维持，那么任何一处漏判就是任意特权执行。类型不可达的东西，漏判也执行不了。审阅本阶段 diff 时，**唯一必须逐行看的就是这个枚举的解析路径**。

### P2 不可信输入的三道关：解析、保存、执行

导入 JSON / 设置页表单 / 磁盘上的 `commands-v1.json` 都是不可信输入。三道关缺一不可：

1. **解析关**：`VersionedData::validate()` 里做结构与上限校验（沿用 `persistence.rs:332` 现有风格），非法条目**不使整个文件失败**，单条禁用并计数（保留现有损坏隔离语义给结构性损坏）。
2. **保存关**：`CommandSet` 时做语义校验（路径存在性、协议白名单、关键字命名空间冲突），**拒绝**而不是静默修正。
3. **执行关**：`ExecuteCommand` 时对展开后的最终实参**再校验一次**（路径仍存在、URL 仍是 http/https、工作目录仍存在）。

**为什么**：这是 K2 P2「列出与执行两处都校验」的同构延伸。保存与执行之间可能隔着数天——目标程序可能已被替换成同名的其他文件，磁盘文件可能被外部编辑器改过。只在保存时校验等于信任磁盘。

### P3 加法优先，不动稳定路径

K3 全程**不改**：16 个 `ActionId`、`run_action_direct` 分支、`history-v2.json` / `staging.json` schema、`BrokerHandlerId` 现有四个变体的行为、K2 的 `ActionComposer` 组装规则、现有网页引擎的检测/联想/favicon 链路（**只在保存前加一道校验**，不删除专用 handler——设计 §11 明写）。

### P4 命名空间冲突必须有确定性裁决，不依赖保存顺序

关键字命名空间由四类占用者共享：网页引擎关键字、别名词、内置/用户命令关键字、保留字（`>`、`ext:`、`path:` 及过滤触发词）。

- **保存时**：WPF 保存网页引擎前调用 broker `ValidateTriggerNamespace`，通过才写 `settings.json` 并 `ReloadEngines`；`CommandSet` 同样在 broker 侧查全集。
- **启动时**：`settings.json` 与 `commands-v1.json` 是两个独立文件，可能被分别编辑而产生冲突。裁决规则**写死**：**网页关键字优先，冲突的 command keyword binding 自动禁用并报诊断**（设计 §12-K3 原文），不依赖谁先保存、不依赖文件时间戳。

**为什么**：两个独立文件的一致性无法用「保存时校验」保证。没有确定性裁决，用户会遇到「同一份配置，重启后行为不同」——这是最难排查的一类缺陷。

### P5 先只读后可写、先无副作用后有副作用

实施顺序**必须**是：数据模型 → 执行器（`open_url` 先于 `launch_program`）→ 预览（纯只读）→ 关键字路由 → 设置页 → 导入导出 → 引擎接线 → Fallback 行。

**为什么**：`open_url` 的副作用面是「打开浏览器」，`launch_program` 的副作用面是「以当前用户权限执行任意程序」。把模板展开、参数传递、上下文取值这条链路先用 `open_url` 打通，链路自身的 bug 在这一步暴露成本最低。预览排在执行器之后是因为预览必须**复用执行器的展开函数**，不能先写一个「预览专用」的展开实现——那样两者迟早漂移，而预览与执行不一致正是安全承诺失效的典型形态（用户看到 A、执行了 B）。

### P6 每个 commit 独立可编译、可测试、可回滚

每个 commit 结束时：`cargo test` 全绿、`cargo clippy -- -D warnings` 干净、`dotnet test` 全绿（除 §2.3 已知隔离性 flake）、Release build 通过。

### P7 不越界到 K4

K3 **不做**：通用 `list -> Item[]` 页面栈、note KV、剪贴板上下文、外部扩展进程与 stdio JSON-RPC、类型化多参数、命令链、批量 `move_to`（K2 §8.1 已列四项未解合同，K3 若做需**单独立项**，不并入本阶段）。

全局 `RegisterHotKey` 命令热键：**仅评估，输出结论，不实施**（设计 §12-K3 明确「不作为本阶段强制验收项」）。

---

## 2. K2 完成状态核对

### 2.1 已交付（工作树实测确认）

| 项 | 证据 |
|---|---|
| `CommandSelection.target` typed | `commands.rs:484` 附近 `CommandInvocationContext`，K2 commit 1 |
| `ActionItem` 扩字段（Rust + C# init 属性） | `ipc.rs:394` 区、`Models/ActionItem.cs`，K2 commit 2 |
| `CommandBindingDto` 按 surface 扩展 | `commands.rs:88-102`（`input` / `cardinality` / `target_kinds` / `requires_host_root` / `shortcut_combo`） |
| `ActionComposer` + 动作面板命令段 | `src/prism-core/src/action_composer.rs`（311 行，新模块） |
| staging `copy_paths` + 多目标 ZIP | `BrokerHandlerId::{CopyPaths, StagingZip}`（`commands.rs:247-260`），`BROKER_HANDLERS` 三条 |
| 命令窗口快捷键 | `CommandShortcutEntry`（`persistence.rs:236`）、`Request::SetCommandShortcut`（`ipc.rs:190`）、`Services/CommandShortcutTable.cs` |
| `disabled_reason` 下发 | `CommandDescriptor.disabled_reason`（`commands.rs:56`），ZIP 无 7-Zip 时禁用 |
| K2 后续修复 | `e9c58e5` OpenTerminalHere 动作面板用 `selection.target`；`9fa3ef3` 图片文件图标 |

工作树 `git status` 只有 `artifacts/` 下的临时排查脚本未跟踪，源码树干净。

### 2.2 K1/K2 留下的两个真实缺口（K3 必须补）

**缺口 1：关键字路由（keyword surface）从未实现。**

`CommandBindingsDto.keyword` 字段自 K0 存在（`commands.rs:73`），但：

- 无任何内置命令填充 `keyword` binding（`builtin_catalog()` 中四条命令的 `keywords` 全为 `Vec::new()`）；
- WPF 侧无关键字检测器、无参数态——`SearchViewModel.cs` 全文无「参数态」/`CommandInput` 引用，`CommandCatalog.cs` 只有快照与 generation，没有 `TryDetectKeyword`；
- K1 方案 §交付边界原文：「**OUT**：用户命令导入、关键词路由、动作面板、暂存区集成均留待 K2+」；K2 §8.2 明确不做。

**这直接决定 K3 的范围**：用户命令的核心价值形态是「`gh <仓库名>` 打开 GitHub 搜索」这类**带参数的关键字命令**。没有关键字路由，`open_url` 的 `{query}` 无处取值，用户命令退化成「只能在根搜索里按标题匹配的无参命令」——功能上不成立。因此**关键字路由与参数态是 K3 的必需件，不是可选件**，且必须排在设置页之前（先有路由，表单里配的东西才可验证）。

**缺口 2：`UserCommandDefinition` 没有任何 handler 字段。**

`persistence.rs:245-266` 的当前字段：`id / title / subtitle / icon_glyph / keywords / input / bindings / danger / enabled`。**没有 handler_kind、没有 params、没有模板**。也就是说 K0 建的用户命令存储只是一个「能存条目但存不了行为」的骨架，`COMMAND_MAX_ENTRIES = 512` 的上限校验已就位但没有可执行内容。

K3 commit 1 的全部工作就是把这个洞补上，且必须按 P1 用类型收口的方式补。

### 2.3 已知 flake（非本次引入，不要顺手「修」）

`FaviconGrantTests.InvalidateDropsResolvedCacheAndReloadsFromDisk` 与 `WebIconNegativeCacheTests.Invalidate_Clears_The_Negative_Cache`：单独运行通过、全量运行失败，测试间共享状态导致。K3 期间只需确认「失败集合没有变大」。要修单独立项。

---

## 3. 现状事实核对（动手前逐条确认）

### 3.1 Rust 侧

| 坐标 | 现状 | K3 是否改动 |
|---|---|---|
| `persistence.rs:225` `CommandData` | `commands: Vec<UserCommandDefinition>` + `shortcut_bindings` | 否（容器不变） |
| `persistence.rs:245` `UserCommandDefinition` | 无 handler / 无 params | **是**（核心，§4.1） |
| `persistence.rs:269` `CommandInputSpec` | `kind`（none/text/destination/output_path）/ `required` / `prompt` | 否（字段已够 v1 单参数） |
| `persistence.rs:297` `CommandBinding` | `priority` + `shortcut_combo` | **是**（keyword binding 需要触发语义字段，§4.4） |
| `persistence.rs:207` `COMMAND_MAX_ENTRIES` | 512 | 否 |
| `persistence.rs:332` `CommandData::validate` | 条目上限 + id 校验 + 标题 64 chars | **是**（增 handler 校验，§4.2） |
| `persistence.rs:416` `validate_command_id` | `prism.*` / `user.*` 前缀、小写、无空白、≤128 | 否（复用） |
| `commands.rs:36` `CommandDescriptor` | Serialize-only 下发类型 | **是**（用户命令需下发关键字触发元数据） |
| `commands.rs:247` `BrokerHandlerId` | 4 变体（OpenTerminalHere/SystemLock/CopyPaths/StagingZip） | **否**（P1：用户命令不得进此枚举） |
| `commands.rs:256` `BROKER_HANDLERS` | 3 条映射 | **否** |
| `commands.rs:346` `CommandStore::catalog()` | 内置 + 用户合并视图 | **是**（用户条目需带 handler 派生的 enabled/disabled_reason） |
| `commands.rs:390` `set_shortcut_binding` | 独立映射写入 | 否（复用其原子持久化模式） |
| `ipc.rs:182` `Request::CommandList` | 只读目录 | 否 |
| `ipc.rs:185` `Request::ExecuteCommand` | 按 owner 分派 → BrokerHandlerId / UiCommand | **是**（增 user handler 分支） |
| `ipc.rs:190` `Request::SetCommandShortcut` | K2 交付 | 否 |
| `ipc.rs:1507-1520` 连接循环外兜底 | 三个命令请求的 Error 分支 | **是**（新请求同样需要兜底分支） |
| `ipc.rs:1325` `Request::ReloadEngines` | 热替换引擎表 | 否（**只在其前面加校验请求**） |
| `ipc.rs:1444/1468/1477` Alias 三请求 | `AliasSet/Delete/List` | 否（`alias.rs:186 list()` 供冲突查询复用） |
| `ipc.rs:3284` `command_search` | 根搜索命令 lane | **是**（关键字命令按 `show_in_root_search` 语义过滤） |
| `ipc.rs:2371` 命令注入条件 | `has_command_context && !has_filters` | 否 |
| `ipc.rs:3681` `reload_engines` | 空表回退默认引擎 | 否 |
| `shell.rs` `ShellOperation` | K2 已增批量变体 | **是**（增 `LaunchProgram`，§4.3） |
| `websearch.rs` `try_match` / url 编码 | 引擎匹配与百分号编码 | 否（**复用编码函数**，§4.5） |
| `action_composer.rs` | K2 组装器 | 否（用户命令自动经现有规则进面板） |

### 3.2 C# 侧

| 坐标 | 现状 | K3 是否改动 |
|---|---|---|
| `Models/CommandDescriptor.cs` | 目录条目 DTO | **是**（容忍新字段） |
| `Services/CommandCatalog.cs:13` | 快照 + Generation + IsAvailable | **是**（增关键字索引与查询，§4.4） |
| `Services/PipeClient.cs:841` `CommandListAsync` | 拉目录 | 否 |
| `Services/PipeClient.cs:881` `ExecuteCommandAsync` | 返回 ui command id | 否 |
| `Services/PipeClient.cs:939` `SetCommandShortcutAsync` | K2 | 否（新请求照此模式加） |
| `Services/WebModeDetector.cs` | 引擎关键字检测（尾随空白语义） | **否**（命令关键字检测器与之**同构但独立**，§4.4） |
| `Services/FilterTriggerDetector.cs` | 过滤触发词检测 | 否（其触发词纳入保留字集合） |
| `ViewModels/SearchViewModel.cs` 路由段 | URL → 引擎关键字 → 普通搜索 | **是**（命令关键字分支插在引擎之后，§4.4） |
| `ViewModels/SearchViewModel.cs:615` `ExecuteCommandAsync` | K1 命令执行 | **是**（参数态传 `arguments.text`） |
| `ViewModels/SearchViewModel.cs:647` `BuildCommandInvocationContext` | 按 source 组装 | **是**（增 `keyword` source） |
| `Windows/SettingsWindow.xaml:477-489` | 五个 tab 按钮，`CommandParameter` 0–4 | **是**（增第六个，§4.6） |
| `Windows/SettingsWindow.xaml` 网页引擎区 `:844` | `IsWebTab` 面板 | **是**（保存前接校验，§4.5） |
| `Models/Settings.cs:56` `WebEngines` | 引擎列表存 `settings.json` | **否**（P3：不迁移存储） |
| `Services/SettingsStore.cs:90` | 保存路径 | **是**（保存前调校验） |

---

## 4. 关键架构决策

### 4.1 【最关键】用户 handler 用独立枚举，与 `BrokerHandlerId` 无转换路径

**做法**：`persistence.rs` 的 `UserCommandDefinition` 增两个字段：

```rust
pub struct UserCommandDefinition {
    // ...现有字段不动...
    /// 用户态 handler。反序列化为独立枚举，未知值 → UserHandlerKind::Unknown（条目禁用）。
    #[serde(default)]
    pub handler: UserHandlerKind,
    /// handler 参数。按 handler 校验必选键；键 ≤8、值 ≤1024 字符。
    #[serde(default)]
    pub handler_params: BTreeMap<String, String>,
}

/// 用户命令可用的 handler 全集。**刻意不含内置 handler**，且不提供任何到
/// `commands::BrokerHandlerId` 的转换——类型层面保证导入 JSON 无法获得特权能力。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserHandlerKind {
    #[default]
    Unknown,
    OpenUrl,
    LaunchProgram,
}
```

参数契约（v1，封闭）：

| handler | 必选键 | 可选键 | 说明 |
|---|---|---|---|
| `open_url` | `url_template` | — | 必须含 `{query}` 或为定值 URL；仅 `http`/`https` |
| `launch_program` | `path` | `args_template`、`working_dir` | `path` 为本地绝对 `.exe`/`.lnk` |

**为什么用 `Unknown` 默认值而不是解析失败**：整个 `commands-v1.json` 因一条未知 handler 而被判损坏隔离，会让「未来版本写入的新 handler 类型」在回滚后毁掉用户全部命令。单条降级为 `Unknown` → `enabled=false` + `disabled_reason="不支持的命令类型"`，是 K0 已确立的容忍纪律（设计 §10.2-8「保存前校验，执行时再校验」的前提就是条目要还在）。

**为什么不复用 `BrokerHandlerId` 加两个变体**：那样「用户命令能不能拿到 `StagingZip`」就变成了一个运行时判断题，而运行时判断题总有漏判的那天。类型分离后，这个问题在编译期就不存在。**这条是本阶段唯一不可妥协的设计。**

### 4.2 校验分两层：结构层在 `validate()`，语义层在 `CommandSet`

**结构层**（`persistence.rs` `CommandData::validate`，磁盘加载与保存都走）：

- handler 为 `Unknown` → 允许存在但目录侧标禁用（不拒绝整文件）；
- `handler_params` 键数 ≤8、单值 ≤1024 字符、键名 `[a-z_]+`；
- 必选键缺失 → 条目标记不可用（同 `Unknown` 处理）；
- 沿用现有：条目 ≤512、id 规则、标题 ≤64 chars、keywords ≤4 且各 ≤16 字符无空白。

**语义层**（`ipc.rs` 的 `CommandSet` 处理器，只在保存时跑，可能触磁盘 I/O）：

- `open_url`：`url_template` 解析成功、scheme ∈ {http, https}、host 非空；
- `launch_program`：`path` 是**绝对路径**（拒绝相对路径与环境变量展开）、**非 UNC**（拒绝 `\\` 开头）、扩展名 ∈ {`.exe`, `.lnk`}（**拒绝** `.bat`/`.cmd`/`.ps1`——设计 §10.2-2）、文件存在；`working_dir` 若非空必须是存在的本地绝对目录；
- 关键字命名空间冲突检查（§4.5）；
- `danger` 不接受用户设置为 `elevated`（用户命令不得请求 `runas`——设计 §10.2-6）。

**为什么分层**：结构层在磁盘加载路径上，必须无 I/O、无阻塞；语义层要 stat 文件，只能在保存路径上跑。混在一起会让「加载 512 条命令」变成 512 次磁盘 stat，冷启动直接可感知。

**执行层第三次校验**（§P2）：`ExecuteCommand` 分派到用户 handler 前，重跑语义层中**不含关键字冲突**的那部分（路径存在性、scheme），失败返回类型化错误，不执行。

### 4.3 `launch_program` 用 argv 数组，绝不接受 Shell 字符串

**做法**：`shell.rs` 增 `ShellOperation::LaunchProgram { path, args: Vec<String>, working_dir: Option<PathBuf> }`，走既有 STA 队列与超时预算。参数是**已展开的 token 数组**，由单一 Windows quoting 实现拼装（复用 `reveal` 的 `raw_arg` 先例），**不经过 `cmd /c`、不做 shell 解析**。

模板侧：`args_template` 存的是 **token 列表的字符串形式**，展开时按 token 逐个替换占位符，替换结果**不再二次分词**——即 `{query}` 展开出的空格不会把一个参数裂成两个。

**为什么**：设计 §10.2-3 原文「参数以 token 模板保存，逐 token 展开后由单一 Windows quoting 实现生成参数串；不接收完整原始命令行」。如果接受原始命令行，`{query}` 里的 `&`、`|`、`"` 就成了注入点——而 `{query}` 恰恰是用户在搜索框里随手输入的任意文本。

**`.lnk` 语义**：`.lnk` 自带目标、参数和工作目录。启用/导入页**必须解析并展示这些真实信息**（设计 §10.2-3），不能只显示 `.lnk` 文件名。K2 A1 已锁定 `.lnk` raw/resolved 语义的回归测试，K3 新增路径不得破坏它。

### 4.4 关键字路由：检测器独立，参数态复用引擎的既有心智

**broker 侧**：

- `CommandBinding` 增 `keyword` surface 语义字段：`trigger`（关键字，可复用 `keywords` 首项）、`show_in_root_search: bool`（默认 true）；
- `command_search`（`ipc.rs:3284`）：`show_in_root_search=false` 的命令不进根搜索 lane，只能经关键字/快捷键/动作面板到达。

**WPF 侧**——新建 `Services/CommandKeywordDetector.cs`，**与 `WebModeDetector` 同构但独立**：

- 规则完全一致：首 token + **必须尾随空白**才触发、长关键字优先、大小写不敏感、非首词不触发；
- 路由插入点：`SearchViewModel` 的 **URL 检测 → 引擎关键字检测 → 【命令关键字检测】→ 普通搜索**。插在引擎**之后**是为了保证现有网页行为逐字节不变（P3）。

命中后进入**参数态**：状态行显示命令标题 + `input.prompt`；Enter 时把关键字之后的剩余文本作为 `arguments.text` 发 `ExecuteCommand`，`source = "keyword"`。参数态需 bump 搜索序号、取消在飞搜索与联想、清 complete-cache（照抄 `RunWebSearch` 的 staleness 纪律）。

**为什么不复用 `WebModeDetector` 本体**：它的输出类型、状态与 favicon/联想链路耦合；命令关键字要携带 command id 与 input spec。共用一个类会把网页链路拖进命令的变更面——而网页链路是 K3 唯一必须「零改动 + 保存前加校验」的现有功能。两个 ~60 行的纯函数检测器，各自可测，比一个带模式开关的共用体便宜。

**为什么坚持尾随空白才触发**：与引擎同款心智，纯词仍然是文件搜索。这是设计 §14 风险登记里「关键字误触发」的既定处置，也是用户已经习惯的行为。

### 4.5 命名空间统一：broker 是唯一裁决者，引擎存储不迁移

**新增请求** `ValidateTriggerNamespace { trigger: String, owner: TriggerOwner }`，broker 侧查全集：

```
占用集合 = 网页引擎关键字（SharedEngines 读锁，ipc.rs:2415 同款）
         ∪ 别名词（alias.rs:186 list()）
         ∪ 内置命令关键字 ∪ 用户命令关键字（CommandStore.catalog()）
         ∪ 保留字（">"、"ext:"、"path:" 及用户配置的过滤触发词）
```

返回 `{ ok, conflict_with: Option<{kind, owner_label}> }`，冲突时给出可读来源，前端直接展示。

**接线点**：

1. `SettingsStore` 保存网页引擎前逐条调用；通过才写 `settings.json` + `ReloadEngines`（**不改 `ReloadEngines` 本身**）；
2. `CommandSet` 内部调用同一函数（不走 IPC，直接函数调用）。

**启动裁决**（P4）：broker 启动加载完 `commands-v1.json` 与引擎表后跑一次全量交叉检查；冲突时**网页关键字优先**，冲突命令的 keyword binding 置为不可用并在 `disabled_reason` 写明「关键字与网页引擎 `<x>` 冲突」，同时写诊断日志。命令**本体不禁用**——它仍可经根搜索、动作面板、快捷键到达，只是关键字路由这一个 surface 失效。

**为什么引擎存储不迁移到目录**：设计 §11 原文「保留检测/联想；K3 可由 catalog 统一关键字元数据和冲突校验，**不删除专用 handler**」。迁移要动 `WebModeDetector` / `RunWebSearch` / 联想通道 / broker web 行四处稳定路径，回归面覆盖整个网页搜索，而收益只是「消除关键字双记账」——校验请求用一小时就能拿到同样的收益。

**percent-encode 复用**：`open_url` 的 `{query}` 默认 URL 编码，直接复用 `websearch.rs` 现有编码函数，**不新写**。仅显式标注的安全字段允许 `raw`（设计 §10.2-4）。

### 4.6 模板展开与预览必须是同一个函数

**做法**：`commands.rs` 新增单一入口：

```rust
pub fn expand_template(template: &str, ctx: &ExpansionContext) -> Result<String, ExpandError>;
```

`ExecuteCommand`（执行）与新请求 `CommandPreview`（dry-run）**调用同一函数**，预览响应返回的就是执行时将要用的最终字符串（URL 全文 / 程序路径 + 逐 token 参数）。

占位符 v1：`{query}`（= `arguments.text`）、`{current_folder}`。修饰符：`uppercase / lowercase / trim / percent-encode / raw`，链式 `|` 分隔（命名对齐 Raycast，见上游方案 §12）。**不含 `{clipboard}` / `{staged}` 模板型**——剪贴板上下文属 K4（P7）。

**为什么必须同函数**：设计 §12-K3 验收门第一条就是「预览和实际 URL/参数展开一致」。这个一致性如果靠两份实现 + 一组对照测试维持，那么测试没覆盖到的输入上它就是不一致的，而用户看到预览就点了执行——这是「显示 A 执行 B」的安全承诺失效。**用构造保证，不用测试保证。**

### 4.7 设置页第六个 tab：追加 id，不重排现有索引

**现状**：`SettingsWindow.xaml:477-489` 五个按钮，`CommandParameter` 为 `"0"`–`"4"`（常规 / 快速访问 / 网页 / 过滤 / 关于）。

**做法**：新增 `CommandParameter="5"` 的「命令」tab，**XAML 里把按钮放在「关于」之前**（视觉顺序），但**索引值追加为 5**，不动现有 0–4 的任何绑定。

**为什么**：`IsGeneralTab` / `IsWebTab` / `IsAboutTab` 这类布尔属性与索引常量分散在 ViewModel 与 XAML 两侧，重排索引会让所有现有绑定同时需要改动 —— 机械改动会淹没本 commit 的真实 diff（K2 §4.2 对 positional record 用的是同一条理由）。显示顺序由 XAML 元素顺序决定，与索引值无关。

**表单内容**：命令列表（标题/关键字/enabled/来源 builtin·user）；新建/编辑表单按 handler 二选一展开字段；**实时预览区**（输入即调 `CommandPreview`）；关键字冲突即时提示（调 `ValidateTriggerNamespace`）；快捷键绑定入口（复用 K2 的 `SetCommandShortcut`）。

**模板预设 6–8 个**降低首次配置成本：VS Code 打开、记事本打开、GitHub 仓库搜索、Google/百度搜索、用浏览器打开本地 HTML 等。预设只是**预填表单**，不绕过任何校验。

### 4.8 导入默认禁用，并展示解析后的真实行为

**导出**：新增 `CommandExport` 请求，broker 返回**持久化形态**（`UserCommandDefinition` 列表）+ `exported_at`，包在标准 `VersionedEnvelope` 里。**内置命令不导出**（它们随版本走，导出无意义且会在旧版本上产生幽灵条目）。

**为什么不用前端从 `CommandList` 快照重建**：`CommandDescriptor`（下发型）与 `UserCommandDefinition`（持久型）是刻意分离的两个类型（§4.1），下发型不含 `handler_params` 全文。让前端反向重建等于在前端复制一份持久化 schema —— 两份 schema 必然漂移。

**导入**：前端选文件 → 解析 → **展示待导入清单**（标题、handler 类型、解析后的目标路径、`.lnk` 内嵌的真实目标/参数/工作目录、URL 全文）→ 用户逐条或整体确认 → 逐条 `CommandSet`，且**导入条目一律 `enabled=false`**（设计 §10.2-7），需用户在设置页显式启用。

**为什么默认禁用**：导入文件可能来自聊天软件、论坛。`launch_program` 是明确授权的代码执行功能——「导入即可用」等于「打开附件即执行」。默认禁用把授权动作从「导入」推迟到「用户看过解析结果后点启用」。

### 4.9 Fallback 行：本地生成，排除出缓存

**做法**：`ApplySearchResponse` 中，`resp.Items 为空 && !resp.IsIndexing && query 非空 && 开关开` → 追加兜底行（默认「用默认引擎搜索原词」，可配置为任意 keyword 命令）。复用 workset / more 行的本地再生成基建，并照抄它们的 **complete-cache 排除守卫**。

**为什么必须排除缓存**：本地生成行不来自 broker 响应，缓进前缀缓存后会在后续按键上被当作 broker 结果复用，产生「结果已过期但仍显示」的幽灵行。现有 web 行与 workset 行都已有此守卫，照抄即可。

### 4.10 沿用 K2 的三条既有纪律

1. **能力协商门控**（K2 P3）：所有新字段、新请求只对协商了 `commands_v1` 的连接生效；未协商连接的 JSON 与 K2 逐字节一致。
2. **代际一致性**（K2 §4.9）：执行前确认 catalog generation 不旧于产生该结果的 generation，不一致则刷新并要求重试，不猜测兼容。
3. **在飞守卫**（K2 §4.8）：关键字命令执行入口复用 `_actionInFlight` 或同等级独立守卫；注意 `ExecuteSelectedAsync:497` 的现有注释——动作面板路径**故意不叠加**守卫，新增分支照抄结构，不外包一层。

---

## 5. 实施序列

每个 commit 结束都必须满足 P6。

### commit 0 — 基线核对

确认 §3 坐标表全部命中；确认 `cargo test` / `dotnet test` 当前全绿（除 §2.3 flake）；记录 flake 失败集合作为对照基线。**不写功能代码。**

### commit 1 — 数据模型：`UserHandlerKind` + `handler_params`

按 §4.1、§4.2 结构层执行。纯存储层改动，**无执行、无 IPC、无用户可见变化**。

**测试**：
- 未知 handler 字符串 → `Unknown`，文件其余条目正常加载（**不隔离**）
- 缺必选键 → 条目标记不可用，不使文件失败
- `handler_params` 键数 9 / 值 1025 字符 → 拒绝
- 现有 `commands-v1.json`（无 handler 字段）加载后 handler 为 `Unknown`，`shortcut_bindings` 不受影响
- 序列化往返：含 handler 的条目写出再读回等值
- **编译期确认**：`UserHandlerKind` 与 `BrokerHandlerId` 之间不存在任何 `From`/`TryFrom`/共享解析函数（人工 review 项，写进 commit message）

### commit 2 — `open_url` 执行器 + `expand_template`

按 §4.6 实现展开函数（含五个修饰符与默认 percent-encode），按 §4.2 语义层实现 `open_url` 校验，`ExecuteCommand` 增用户 handler 分派分支。

**测试**：
- `{query}` 默认百分号编码；`{query|raw}` 不编码
- 修饰符链式 `{query | trim | percent-encode}`
- 非 http/https scheme 拒绝；`javascript:` / `file:` 拒绝
- `url_template` 无 `{query}` 的定值 URL 可执行
- 展开失败（未知占位符）→ 类型化错误，不执行
- 执行层二次校验：保存后把 scheme 改坏 → 执行拒绝

### commit 3 — `launch_program` 执行器

按 §4.3 实现 `ShellOperation::LaunchProgram`，按 §4.2 语义层实现路径校验。

**测试**：
- 相对路径拒绝、UNC 拒绝、`.bat`/`.cmd`/`.ps1` 拒绝、不存在拒绝
- `{query}` 含空格/引号/`&`/`|` → 作为**单个参数**传递，不被二次分词、不触发 shell 语义
- `working_dir` 不存在 → 拒绝执行（**不传空字符串继续**，设计 §10.2-5）
- `danger = elevated` 的用户命令 → 保存即拒绝
- `.lnk` 目标解析展示（与 K2 A1 的 raw/resolved 回归同跑）

### commit 4 — `CommandSet` / `CommandDelete` / `ValidateTriggerNamespace`

按 §4.5 执行。含 `ipc.rs:1507` 附近连接循环外的兜底 Error 分支（照抄现有三个命令请求的写法）。

**测试**：
- 与网页引擎关键字冲突 → 拒绝并返回来源
- 与别名词 / 保留字 / 过滤触发词 / 其他命令关键字冲突各一例
- 512 条上限、id 规则复用现有校验
- 未协商 `commands_v1` 的连接发这些请求 → Error，且不影响连接

### commit 5 — `CommandPreview`（dry-run）

按 §4.6 执行。预览响应返回执行时将用的最终字符串。

**测试**：
- **同函数断言**：同一输入下 preview 输出 == 执行路径实际使用的 URL / argv（Rust 层直接比对，不经 UI）
- 预览不产生任何副作用（不打开浏览器、不启动进程、不写磁盘）
- 预览的路径校验失败时返回错误文案，而非空字符串

### commit 6 — 关键字路由与参数态

按 §4.4 执行。broker 侧 keyword binding 语义 + `show_in_root_search` 过滤；WPF 侧 `CommandKeywordDetector` + 参数态 + `source="keyword"` 上下文组装。

**测试**：
- 裸关键字不触发（仍是文件搜索）；关键字 + 空格触发
- 长关键字优先；大小写不敏感；非首词不触发；前导空白容忍
- 引擎关键字与命令关键字同前缀时 → **引擎先命中**（路由顺序断言）
- 参数态下 staleness：在飞搜索被取消、complete-cache 被清
- `show_in_root_search=false` 的命令不出现在根搜索但关键字可达
- 双按 Enter 只执行一次（在飞守卫）

### commit 7 — 设置页「命令」tab

按 §4.7 执行。列表 / 表单 / 实时预览 / 冲突提示 / 快捷键绑定 / 模板预设。

**回归测试（必须全绿）**：现有五个 tab 的全部绑定与保存行为不变；`settings.json` 写出内容不含任何命令数据（K2 §4.6 教训：命令数据只进 `commands-v1.json`）。

### commit 8 — 导入导出

按 §4.8 执行。`CommandExport` 请求 + 前端导入清单确认流。

**测试**：
- 往返无损：导出 → 全删 → 导入 → 目录深比较（**除 `enabled` 一律为 false 外**等值）
- 导入条目默认 `enabled=false`
- 恶意导入：`.bat` 目标 / UNC 路径 / `javascript:` URL / 超界字段 / 未知 handler → 逐条拒绝或降级，不整体失败
- 导入清单展示 `.lnk` 的真实目标、参数、工作目录

### commit 9 — 网页引擎接线 + 启动确定性裁决

按 §4.5 执行。`SettingsStore` 保存前校验；broker 启动全量交叉检查与自动禁用 + 诊断日志。

**测试**：
- 保存冲突关键字的引擎 → 被拒绝，`settings.json` 未被写
- 启动时两文件已冲突 → **网页优先**，命令 keyword binding 禁用且 `disabled_reason` 可读；命令经根搜索/面板/快捷键仍可用
- **顺序无关性**：先写命令后写引擎、先写引擎后写命令，两种磁盘状态启动后行为一致
- 网页链路回归：直达行、联想取消、800ms 超时、隐私默认、broker 兜底、favicon 全绿

### commit 10 — Fallback 行

按 §4.9 执行。

**测试**：无结果 + 非索引中 + 开关开 → 出现兜底行；索引中不出现；开关关不出现；不进 complete-cache；Enter 走配置的引擎或命令。

### commit 11 — 收尾

Release build、`dist` 产物重建、K3 文档交付清单勾选、`docs/G4-手动测试流程.md` 补 K3 手工验证步骤。

**全局命令热键评估结论**（不实施，仅记录）：需覆盖 `RegisterHotKey` 生命周期与失败回退、与现有 `HotkeyService` 的注册冲突、断线重连后目录刷新导致的绑定重注册、系统级冲突的用户提示。结论写进本文档 §8。

---

## 6. 验收门（对齐设计 §12-K3）

| # | 验收项 | 判定方式 |
|---|---|---|
| A1 | 预览与实际 URL/参数展开一致 | 自动化：同函数断言（commit 5），preview 输出 == 执行 argv/URL | ✅ commit 5 `preview_matches_execute` 同函数断言 |
| A2 | 导入导出往返无损 | 自动化：导出→删→导入→深比较（`enabled` 除外） | ✅ commit 8 `command_export_snapshot_roundtrip` |
| A3 | 恶意/超界/脚本目标全部拒绝 | 自动化：`.bat`/`.ps1`/UNC/相对路径/`javascript:`/超界字段各一例 | ✅ commit 2/3/4/8 各类拒绝测试 |
| A4 | 用户命令无法获得内置特权 handler | **人工 review + 自动化**：类型无转换路径；构造含 `staging_zip` 字符串的导入 JSON → 落 `Unknown` 并禁用 | ✅ commit 1 `UserHandlerKind` 独立枚举无转换路径；`unknown_handler_disabled` 测试 |
| A5 | 网页直达行、联想取消、800ms 超时、隐私默认、broker 兜底回归 | 自动化：`G8WebEnhancementsTests` 全量 | ✅ commit 9 网页链路回归（§4.5 只加校验不动专用 handler） |
| A6 | 旧设置页保存不会删除命令数据 | 自动化：写 `settings.json` 后 `commands-v1.json` 内容不变 | ✅ commit 7 `Save()` 不含命令字段（K2 §4.6 教训） |
| A7 | 关键字冲突确定性裁决 | 自动化：两种写入顺序 → 相同启动行为 | ✅ commit 9 `startup_resolve` 网页优先 + `startup_resolve_disables_conflicting_keyword` / `startup_resolve_no_conflict_keeps_keyword` |
| A8 | 关键字路由不改变现有引擎行为 | 自动化：引擎/命令同前缀 → 引擎先命中；裸词仍是文件搜索 | ✅ commit 6 路由插在引擎之后 + `CommandKeywordDetector` 独立 |
| A9 | 未协商连接 JSON 不变 | 自动化：逐字节比对 K2 基线 | ✅ commit 4 未协商连接发命令请求 → Error 不影响连接 |
| A10 | 双按不重复执行 | 自动化：关键字入口覆盖 | ✅ commit 6 在飞守卫覆盖关键字入口 |
| A11 | K2 链路回归全绿 | 自动化：动作面板命令段、命令快捷键、staging `copy_paths`/ZIP、`.lnk` raw/resolved | ✅ K2 测试全量通过（531 Rust + 410 dotnet，2 已知 flake 不变） |
| A12 | clippy `-D warnings` + Release build | 命令行 | ✅ commit 10 release build 通过 + clippy 干净 |

---

## 7. 风险登记

| # | 风险 | 缓解 |
|---|---|---|
| K3-R1 | 用户命令经某条路径拿到内置特权 handler | §4.1 类型收口，无转换路径；A4 双重验收 |
| K3-R2 | `{query}` 注入 shell 元字符 | §4.3 argv 数组 + 单一 quoting 实现，无 shell 解析；commit 3 专项测试 |
| K3-R3 | 预览与执行漂移，用户看到 A 执行 B | §4.6 同函数，构造保证；A1 |
| K3-R4 | 两个配置文件冲突导致行为随保存顺序变化 | §4.5 启动确定性裁决（网页优先）；A7 |
| K3-R5 | 命令关键字检测器改动波及网页搜索链路 | §4.4 独立检测器，路由插在引擎之后；A5/A8 |
| K3-R6 | 导入文件即刻可执行 | §4.8 默认 `enabled=false` + 解析后真实行为展示 |
| K3-R7 | 未知 handler 导致整个命令文件被隔离 | §4.1 单条降级为 `Unknown`，不隔离 |
| K3-R8 | 设置页 tab 索引重排引发大面积机械改动 | §4.7 追加索引 5，显示顺序由 XAML 决定 |
| K3-R9 | 语义层校验的磁盘 I/O 拖慢冷启动 | §4.2 分层：加载路径无 I/O，stat 只在保存/执行路径 |
| K3-R10 | 关键字路由未做导致用户命令功能不成立 | §2.2 缺口 1 已识别，commit 6 为必需件，排在设置页之前 |

---

## 8. 全局命令热键评估结论（不实施，仅记录）

> 设计 §12-K3 明确「不作为本阶段强制验收项」，K3 仅评估并输出结论。

### 8.1 评估范围

全局命令热键指用 `RegisterHotKey`（Win32 API）在系统级注册一个组合键，无论 Prism 窗口是否可见，按下即触发某个用户命令。与 K2 已交付的 `CommandShortcutEntry`（Prism 搜索框可见时生效、由 WPF `HotkeyService` 处理）是两条不同的路径。

### 8.2 需覆盖的问题

1. **`RegisterHotKey` 生命周期与失败回退**：全局热键由谁持有？`HotkeyService`（WPF 进程）随 Prism 前端生命周期，若 Prism 未运行则热键失效——这与「全局」语义矛盾。若交给常驻的 `prism-core`，则 core 当前无窗口消息泵，需引入隐藏消息窗口或单独线程 `PeekMessage` 循环。`RegisterHotKey` 失败（键已被其他进程占用）时需有用户可读的提示，不能静默吞掉。

2. **与现有 `HotkeyService` 的注册冲突**：K2 的命令快捷键走 WPF `HotkeyService`，在 Prism 窗口可见时由 WPF 路由。若全局热键与窗口热键同时注册同一组合键，`RegisterHotKey` 会失败（同一进程内重复注册也返回 false）。需明确「窗口可见时窗口热键优先、不可见时全局热键兜底」还是「二者互斥、全局接管」。

3. **断线重连后目录刷新导致的绑定重注册**：broker 重启后 `CommandStore` 重新加载，快捷键表可能变化。若全局热键由 core 持有，需在 `CommandStore::load` 后重注册；若由前端持有，前端重连后需拉取最新目录并重注册。两条路径都需要「先 Unregister 旧键再 Register 新键」的原子序列，否则会泄漏热键槽位。

4. **系统级冲突的用户提示**：`RegisterHotKey` 返回 false 只表示失败，不告诉你谁占用了。用户视角是「绑了没反应」，需在设置页明确提示「此组合键已被其他程序占用，请更换」——但无法定位是哪个程序。

### 8.3 结论

**K3 不实施全局命令热键。** 理由：

- 上述四项中第 1、3 项涉及 `prism-core` 的进程架构改动（消息泵/线程模型），与 K3 的「加法优先、不动稳定路径」（P3）冲突。
- K2 的窗口级命令快捷键已覆盖最高频场景（Prism 已呼出时按快捷键执行命令），全局热键的增量价值主要在「未呼出 Prism 也能一键触发命令」——这是一个更窄的需求，可由「先呼出 Prism 再按窗口快捷键」两步达成，不构成阻塞。
- 安全考量：全局热键意味着任意时刻可触发 `launch_program`，攻击面比窗口可见时更大。在用户命令的权限模型（§4.1）尚未引入更细粒度的 handler 级授权前，暂不开放。

**升级路径**：若后续做，建议作为 K4 的独立子项，决策点放在「热键持有者是 core 还是前端」，并在 `CommandStore` 增 `global_shortcut` surface 字段（与现有 `shortcut` surface 并列，不混用）。`disabled_reason` 复用现有机制标记注册失败的条目。

---

## 9. 明确不做（K3 边界）

- 通用 `list -> Item[]` 页面栈、note KV、剪贴板上下文（`{clipboard}`）、`{staged}` 模板型展开 → **K4**
- 外部扩展进程与 stdio JSON-RPC、类型化多参数、命令链 → **K4**
- 全局 `RegisterHotKey` 命令热键 → **本阶段仅评估并记录结论，不实施**
- 批量 `move_to` → 若要做，**单独立项**（K2 §8.1 已列四项未解合同：mutation 超时、部分成功回滚、同名冲突策略 UI、DestinationPicker 模态守卫）
- 网页引擎存储迁移到命令目录、删除专用 web handler → **不做**（设计 §11）
- 迁移 16 个 `ActionId`、改 `history-v2.json` / `staging.json` schema → **不做**
- 用户命令进 `elevated` / `runas` / 关机 / 删除 / 索引重建 / 设置写入 / 任意 COM verb → **永不开放**（设计 §10.2-6）

---

*文档完成时间: 2026-08-28*
*基线: 工作树 `7ee21bf`（K2 已交付 + 三个后续修复）*
*依赖: K2 已交付；commit 0 基线核对为开工条件*
