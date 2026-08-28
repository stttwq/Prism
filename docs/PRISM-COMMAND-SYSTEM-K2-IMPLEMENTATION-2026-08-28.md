# PRISM 统一命令系统 K2 施工方案

**文档版本**: K2-IMPL-2026-08-28
**上游设计**: `docs/PRISM-COMMAND-SYSTEM-DESIGN-2026-08-22.md`（v3，2026-08-26 修订）§7 / §8 / §12-K2 / §13.2 / §13.3 / §14
**K1 基线**: commit `9035c0f`（feature 分支）
**目标状态**: K2 — 动作面板命令段、命令快捷键、暂存区批量联动

---

## 0. 给施工者的阅读顺序

1. 先读 §1「施工总原则」——它决定了后面每一步为什么这么切。
2. 再读 §2「前置清理」——K1 留了一条红测试，不清掉的话 K2 全程没有干净基线。
3. §3「现状事实核对」是代码坐标表，动手前逐条确认还在，不在就先停下来问。
4. §4「关键架构决策」是本方案的核心，每条都写了「怎么做 + 为什么」。**不要跳过理由部分**——K2 的多数坑是「看起来能省一步，省了就炸」。
5. §5 起是按 commit 切分的实施序列，照序做。

---

## 1. 施工总原则

### P1 加法优先，不动稳定路径

K2 全程**不改**：16 个 `ActionId` 枚举、`run_action_direct` 的执行分支、`history-v2.json` schema、`staging.json` schema、`settings.json` 的命令无关字段、现有搜索排序内核。

命令段是在现有动作列表**之后追加的一段**，不是把现有动作重写成命令。理由：现有动作链路已经沉淀了双击在飞守卫、mutation 超时「结果未知」、目录选择失活守卫、永久删除前台协同这些成熟约束（设计 §2.3-5）。重写它们的收益是零，风险是全部。

### P2 列出与执行两处都校验

broker 在 `Actions` 返回命令段时校验一次 target 适配性，在 `ExecuteCommand` 执行时**再校验一次**。

理由：两次请求之间用户可能改了目录代际、目标文件可能已被删除、前端可能被篡改。只在列出时校验等于信任前端把校验结果带回来——这正是设计 §5.2 要求 `Execute/Reveal/Actions/RunAction/ShellExecutor` 全部显式拒绝 `TargetKind::Command` 的同一条理由：capability gating 是第一道，严格路由是第二道，两道都要在。

### P3 能力协商门控一切新字段

命令段 `ActionItem`、命令快捷键、staging surface 只对协商了 `commands_v1` 的连接返回。未协商连接拿到的 JSON 必须与 K1 逐字节一致。

理由：设计 §14-R2。不能依赖「旧前端会忽略未知字段」——K0 已经验证过这个假设不成立。

### P4 失败可见，禁止静默

批量操作发现失效路径：先汇总展示「有效 N、失效 M」，由用户确认继续或取消。超过 128 项：整体拒绝并要求缩减，**不静默截断**。部分失败：返回成功/失败/取消三类计数与有界错误摘要。mutation 超时：沿用「结果未知」文案，**禁止自动重试**。

理由：设计 §8.3 + §14-R8。批量操作静默吞掉一半输入，是用户完全无法察觉的数据事故。

### P5 先无损后有损

staging 批量命令的实施顺序**必须**是：`copy_paths`（无损，纯文本输出）→ 多目标 ZIP（有损风险低，但有输出路径/冲突语义）→ 批量 `move_to`（**K2 不做，仅评估**）。

理由：`copy_paths` 用最简单的语义把整条 staging 链路（快照→上限校验→broker 分类→handler→汇总回显）打通，链路本身的 bug 在这一步暴露成本最低。ZIP 引入输出路径选择和冲突处理，`move_to` 引入部分成功和同名冲突——把它们叠在未验证的链路上，出问题时分不清是链路还是 handler。

### P6 每个 commit 独立可编译、可测试、可回滚

不允许「这个 commit 先改一半，下个 commit 补上才能编译」。每个 commit 结束时：`cargo test` 全绿、`cargo clippy -- -D warnings` 干净、`dotnet test` 全绿（除 §2 已知隔离性 flake）、Release build 通过。

### P7 不越界到 K3/K4

K2 **不做**：用户自定义命令与设置页表单、`open_url`/`launch_program` 执行器、关键字冲突校验 `ValidateTriggerNamespace`、网页引擎目录适配、全局 `RegisterHotKey` 热键、通用 `list -> Item[]` 页面栈、剪贴板上下文、命令链。

理由：设计 §12 把这些划给 K3/K4，且 §14-R12 明确警告内核会被 note/list/plugin 提前拖重。

---

## 2. 前置清理（K2 commit 0）

K1 收尾遗留三项，**必须在 K2 正式开工前处理**，否则没有干净基线。

### 2.1 修复失效的 K0 测试（阻塞项）

`src/Prism.Tests/CommandTargetSafetyTests.cs:100` `Command_Row_Enter_Does_Not_Execute` 当前 **RED**。

它是 K0 时代写的，断言命令行按 Enter 被拒绝、状态栏显示 `"命令不可用"`。K1 的全部意义就是让命令可执行，这条前提已被推翻。实测失败信息：

```
Expected: "命令不可用"
Actual:   "命令执行不可用"
```

原因：K1 后命令行走 `SearchViewModel.ExecuteCommandAsync`（`src/Prism/ViewModels/SearchViewModel.cs:615`），该方法首行是 `if (_pipe is not PipeClient realPipe)` 降级分支，测试注入的是 fake client，因此落在 `"命令执行不可用"`。

**改法**：重命名为 `Command_Row_Enter_Does_Not_Use_File_Execute_Path`，保留真正有价值的那条断言 `Assert.Equal(0, client.ExecuteCallCount)`（命令 id 绝不能进通用文件执行路径——这是 K0 最高价值断言，K2 之后依然必须成立），把状态断言改为 `"命令执行不可用"` 并补注释说明这是 fake pipe 的降级分支。

**不要**改 `SearchViewModel` 去迁就旧测试文案。

### 2.2 补 `FilterTriggerDetector` 测试（K1 期间并入的过滤触发词功能）

`src/Prism/Services/FilterTriggerDetector.cs` 当前 **零测试覆盖**，而它恰好是易错点密集区。新建 `src/Prism.Tests/FilterTriggerDetectorTests.cs`，至少覆盖：

| 用例 | 输入 | 期望 |
|---|---|---|
| 裸关键词不触发 | `"tz"` | `TryDetect` → null（仍是普通文件搜索） |
| 关键词+空格触发 | `"tz pdf"` | `TryRewrite` → `"ext:pdf"` |
| path 类型 | `"pp docs"` | `"path:docs"` |
| 大小写不敏感 | `"TZ pdf"` | `"ext:pdf"` |
| 长词优先 | triggers 含 `t`/`tz`，输入 `"tz pdf"` | 命中 `tz` 而非 `t` |
| 空 QueryTerms 回退 | `"tz "` | `"ext:"`（broker 按无值过滤词处理） |
| 前导空白 | `"  tz pdf"` | `"ext:pdf"` |
| 非首词不触发 | `"a tz pdf"` | null |
| 未知类型 | FilterType 非 ext/path | `TryRewrite` → null |
| 空 triggers 列表 | 任意 | null |

### 2.3 更新 K1 文档状态

`docs/PRISM-COMMAND-SYSTEM-K1-IMPLEMENTATION-2026-08-26.md` 交付清单 18 项全为未勾选，且 §测试验证 的 T5 边界条件写着「目录模式：有 root → 无命令结果」——已被 commit `5d4576e`（有 root 时仍显示命令）推翻。

勾选实际完成项，并把 T5 改为反映现状：命令 lane 注入条件现为 `has_command_context && !has_filters`（`src/prism-core/src/ipc.rs:2371`），root 不再是排除条件；过滤态仍排除。

---

## 3. 现状事实核对（动手前逐条确认）

### 3.1 Rust 侧

| 坐标 | 现状 | K2 是否改动 |
|---|---|---|
| `commands.rs:35` `CommandDescriptor` | id/title/subtitle/icon_glyph/owner/trust/keywords/input/bindings/danger/enabled | 否 |
| `commands.rs:64` `CommandBindingsDto` | root_search/keyword/action_panel/staging/shortcut，后三者恒为 None | **是**（填充后三者） |
| `commands.rs:78` `CommandBindingDto` | **只有 `priority: i32`** | **是**（需扩字段） |
| `commands.rs:137` `BrokerHandlerId` | `OpenTerminalHere`、`SystemLock`（后者返回「尚未实现」） | **是**（增批量 handler） |
| `commands.rs:142` `BROKER_HANDLERS` | 仅 `prism.terminal.open` | **是** |
| `commands.rs:484` `CommandInvocationContext` | command_id/source/arguments/selection/staged_paths/current_folder/host_kind | **是**（selection 需带 target） |
| `commands.rs:524` `CommandSelection` | **只有 `title`/`subtitle`，无 target** | **是**（关键，见 §4.1） |
| `commands.rs:514` `CommandArguments` | text/destination/output_path，`deny_unknown_fields` | 否（字段已够用） |
| `commands.rs:531-535` 上限常量 | TEXT 8KiB / PATH 32KiB / STAGED 128 / TITLE 256 chars / JSON 512KiB | 否 |
| `commands.rs:317` `record_success` | `(id, now, history_enabled)` | 否（复用做 frecency） |
| `actions.rs:22` `ActionId` | 16 变体封闭枚举 | **否**（P1） |
| `actions.rs:196` `list_actions` | `(target) -> Vec<ActionItem>` | **是**（需接命令段） |
| `actions.rs:232` `action_item` | 构造内置 ActionItem | **是**（新字段默认值） |
| `ipc.rs:394` `ActionItem` | id/label/icon_glyph/has_submenu/is_section_header | **是**（见 §4.2） |
| `ipc.rs:2033` `execute_command` | 按 owner 分派，broker→handler，ui→`UiCommand` | **是**（增 source 分支） |
| `ipc.rs:3284` `command_search` | root 搜索命令 lane | 否 |
| `ipc.rs:2371` 命令注入点 | `if has_command_context && !has_filters` | 否 |
| `shell.rs:66` `ShellOperation` | Open/Reveal/Properties/OpenWith/RunAction | **是**（增批量变体） |
| `shell.rs:484` `ActionTarget::new` / `:491 validate` | target 校验入口 | 否（复用） |
| `shell.rs:743` `open_terminal_at` | K1 交付 | 否 |
| `zip.rs` `zip(target)` | **单 target**，输出路径由 `unique_zip_path` 自动派生 | **是**（见 §4.5） |

### 3.2 C# 侧

| 坐标 | 现状 | K2 是否改动 |
|---|---|---|
| `Models/ActionItem.cs` | positional record 5 参数 | **是**（见 §4.2 注意构造点） |
| `Models/StagingArea.cs:5` `StagingItem` | `(Path, Workset?)` | 否（P1） |
| `Models/StagingArea.cs:159` `Capacity` | 默认 5，工作集可达 128 | 否 |
| `Models/CommandInvocationContext.cs` | K1 交付 | **是**（对齐 selection.target） |
| `Models/ActionHotkeyCatalog.cs` | 15 条静态镜像 | **否**（命令快捷键另建，见 §4.6） |
| `Services/ActionHotkeyTable.cs` `Parse` | 组合键解析 | 否（复用解析器） |
| `Services/CommandHandlers.cs` | UI handler 表，仅 `prism.settings.open` | 视需要 |
| `Services/CommandCatalog.cs` | 目录快照 + Generation | **是**（需暴露 bindings） |
| `Services/PipeClient.cs:881` `ExecuteCommandAsync` | 返回 `string?`（ui command id） | **是**（需返回批量汇总） |
| `ViewModels/SearchViewModel.cs:493` `_actionInFlight` | 在飞守卫 | **是**（覆盖命令路径） |
| `ViewModels/SearchViewModel.cs:615` `ExecuteCommandAsync` | K1 命令执行 | **是** |
| `ViewModels/SearchViewModel.cs:647` `BuildCommandInvocationContext` | source 硬编码 `"root"` | **是**（按来源分派） |
| `ViewModels/SearchViewModel.cs:691` `EnterActionsAsync` | 门禁 `Kind is not ("file" or "folder")` | **是**（放宽至 app） |
| `ViewModels/SearchViewModel.cs:720` `GetActionsForAsync` | 已允许 `"app"` | 否（注意与上一行不一致） |
| `ViewModels/SearchViewModel.cs:806` `RunActionOnCoreAsync` | rename/copy_to/move_to 硬编码分支 | **是**（增命令分派） |
| `Controls/StagingStrip.xaml:72-85` | SaveButton / WorksetsButton / ClearButton | **是**（增「对暂存区执行」） |
| `Windows/SearchWindow.xaml.cs:1468` | 暂存区快捷键分发点 | **是**（增命令快捷键分发） |

> ⚠️ **已确认的现存不一致**：`EnterActionsAsync`（Tab 进面板）拒绝 `app`，而 `GetActionsForAsync`（右键菜单）接受 `app`。即当前右键菜单对应用有动作、Tab 键没有。K2 §5.2 顺手对齐，但**必须补回归测试**确认放宽后应用动作面板的 `.lnk` 语义未变。

---

## 4. 关键架构决策

### 4.1 【最关键】`CommandSelection` 必须带 typed target

**现状**：`commands.rs:524` 的 `CommandSelection` 只有 `title` / `subtitle` 两个展示字段，**没有 target**。

**做法**：加 `target: Option<ActionTarget>`，并在 `CommandInvocationContext::validate()` 中对其调用现有 `ActionTarget::validate()`。

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CommandSelection {
    #[serde(default)]
    pub target: Option<ActionTarget>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
}
```

**为什么这是第一件事**：动作面板命令段的整个语义是「对当前选中项执行命令」。没有 typed target，broker 收到调用后无从复核操作对象——只能退回去信任 `title`/`subtitle` 字符串。设计 §5.3-2 明确写死：`title/subtitle` **只是有界 UI 快照，不可作为路径或权限依据**。

这条不先落地，后面 action_panel 和 staging 两个 surface 全部悬空。**它是 K2 的地基 commit，必须单独提交、单独测试。**

**注意**：`ActionTarget` 需要 `Deserialize`。确认它当前的 derive——若只有 `Serialize`，本 commit 一并补上，并补一条「command kind 的 selection.target 被拒绝」的测试（选中项不可能是命令自身，防止命令递归调用命令）。

### 4.2 `ActionItem` 扩字段：Rust 加法安全，C# 有构造点陷阱

**Rust 侧**（`ipc.rs:394`）按设计 §7.1 增四个字段，全部带 serde 默认值：

```rust
pub struct ActionItem {
    pub id: String,
    pub label: String,
    pub icon_glyph: String,
    pub has_submenu: bool,
    pub is_section_header: bool,
    /// K2: "builtin_action" | "command"
    #[serde(default = "default_invocation_kind", skip_serializing_if = "is_builtin_action")]
    pub invocation_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub is_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
}
```

`skip_serializing_if` 的作用是：**内置动作的 JSON 输出与 K1 逐字节一致**。这样未协商 `commands_v1` 的旧前端拿到的动作列表完全没变化，符合 P3。

**C# 侧陷阱**：`Models/ActionItem.cs` 是 **positional record**：

```csharp
public sealed record ActionItem(string Id, string Label, string IconGlyph, bool HasSubmenu, bool IsSectionHeader);
```

直接追加位置参数会**破坏所有现有构造点**（`PipeClient` 解析、`ActionPanel`、右键菜单构造、测试夹具）。

**做法**：新字段一律作为**带默认值的 init 属性**追加，不动主构造器签名：

```csharp
public sealed record ActionItem(string Id, string Label, string IconGlyph, bool HasSubmenu, bool IsSectionHeader)
{
    public string InvocationKind { get; init; } = "builtin_action";
    public string? CommandId { get; init; }
    public bool IsEnabled { get; init; } = true;
    public string? DisabledReason { get; init; }
}
```

**为什么**：现有构造点一行不改就继续编译，且默认值天然等于「这是内置动作」。若改成位置参数，光是修构造点的机械改动就会淹没本 commit 的真实 diff，review 时看不出问题。

### 4.3 `CommandBindingDto` 需要按 surface 扩展

**现状**：只有 `priority: i32`。设计 §5.1 要求每个 surface 声明输入来源与适用性（`input`、`requires_host_root`、`cardinality`、`target_kinds`）。

**做法**：扩展为

```rust
pub struct CommandBindingDto {
    pub priority: i32,
    /// "none" | "selection" | "current_folder" | "current_folder_or_prompt" | "staged_paths"
    #[serde(default, skip_serializing_if = "str::is_empty")]
    pub input: String,
    /// action_panel/staging 用："one" | "many"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<String>,
    /// 适用的 target kind：["file","directory","application"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub requires_host_root: bool,
}
```

`CommandBindingsDto` 是 `Serialize`-only（broker→WPF 单向），因此加字段对线路是纯加法。C# `CommandDescriptor` 的解析必须容忍缺字段。

**为什么不用一组全局 `accepts`**：设计 §5.1 结尾给了具体反例——「在此打开终端」在根搜索用 `current_folder`、在动作面板用选中的 directory；**没有宿主目录时后者仍然可用**。一组全局 accepts 表达不了「同一命令在不同入口取不同输入」，会导致没有宿主目录时动作面板里的终端命令被错误禁用。

### 4.4 `ActionComposer`：分段、cap 5、两处校验

**位置**：新建 `src/prism-core/src/action_composer.rs`，由 `ipc.rs` 的 `list_actions` 包装处调用。

**组装规则**：

```
内置段 = actions::list_actions(target)            // 完全不变
命令段 = catalog 中满足以下全部条件的命令：
          - enabled
          - bindings.action_panel.is_some()
          - target.kind ∈ binding.target_kinds
          - danger != "destructive"                // 危险命令不进面板首批
          - owner handler 存在（broker 侧查 BROKER_HANDLERS / ui 侧信任目录）
        按 (command frecency desc, binding.priority asc, title asc) 排序
        cap 5
输出 = 内置段 ++ [段头 "命令"] ++ 命令段
```

段头复用现有 `is_section_header: true` 机制（现有「快捷菜单」段已在用）。命令段为空时**不输出段头**。

**必须做的两件事**：

1. 命令段仅在 `caps.commands_v1` 为真时追加。`Actions` 请求当前在连接循环外处理——需要像 `ExecuteCommand` 那样把它移进能读到 `caps` 的位置，或把 caps 透传进来。**参照 `ipc.rs:1065` `ExecuteCommand` 的现有写法**，它已经解决了同一个问题（`ipc.rs:1441` 留了「在连接循环内处理」的兜底 Error 分支）。
2. `ExecuteCommand` 收到 `source = action_panel` 时**重新执行一遍上面的适配性判断**，不通过就拒绝。这就是 P2。

**为什么 cap 5**：设计 §12-K2 明确写「命令段 frecency 排序、cap 5」。动作面板本身已有 12-16 条内置动作，命令段无上限会把面板撑成滚动列表，破坏「Tab 进面板、方向键两下够到」的手感。

**为什么 destructive 不进首批**：设计 §10.2-10 + §14-R11。面板是高频误触区域。

### 4.5 批量 ZIP 是本阶段最大技术风险

**现状**（`zip.rs`）：

```rust
pub fn zip(target: &ActionTarget) -> Result<String, ShellError> {
    let kind = target.validate()?;
    // 只接受 File | Directory
    let source = PathBuf::from(&target.value);
    let output = unique_zip_path(&source)?;      // ← 输出路径从单一 source 派生
    match detect_zip_program() {
        Ok(program) => zip_external(&program, &source, &output),
        Err(_) => zip_windows_builtin(&source, &output),
    }
}
```

批量场景下 `unique_zip_path(&source)` 无意义——N 个来源没有唯一的「源」可派生输出名。

**做法**：

1. 新增 `zip_many(sources: &[ActionTarget], output: &Path, program: Option<&str>) -> Result<ZipSummary, ShellError>`，**不复用** `unique_zip_path`。输出路径由 `arguments.output_path` 显式给出（WPF 侧 SaveFileDialog 选择）。
2. 保留 `zip(target)` 原样不动——现有单目标 `ActionId::Zip` 路径零改动（P1）。
3. `zip_external` / `zip_windows_builtin` 需要支持多输入。**先确认 `zip_windows_builtin` 的实际实现**（PowerShell `Compress-Archive` 支持 `-Path` 数组；Shell COM 的 `CopyHere` 需逐项且异步完成难判定）。若内置回退无法可靠支持多输入，**首版策略是：批量 ZIP 仅在检测到 7-Zip 时可用，否则该命令在目录中标 `is_enabled=false` + `disabled_reason="需要 7-Zip"`**，而不是勉强用 COM 拼一个完成时机不可判定的实现。
4. 输出路径冲突：**不静默覆盖**。已存在则返回 `ShellErrorKind` 化的冲突错误，由 WPF 提示用户改名。
5. 部分失败：`ZipSummary { succeeded, failed, cancelled, errors: Vec<BoundedError> }`。

**为什么单独强调**：设计 §8.2 已经点名「当前 `zip` 是单 target；必须定义输出文件名、冲突和部分失效行为」。这是 K2 里唯一一个「现有函数签名从根上不适用」的地方，其余都是加法。**如果时间紧张，砍掉 ZIP 保留 `copy_paths`，K2 依然成立**（P5 的存在就是为了让这个取舍可行）。

### 4.6 命令快捷键不复用 `ActionHotkeys`

**做法**：新建独立绑定，存储在 broker 的 `commands-v1.json`，通过目录快照下发给 WPF。WPF 侧在 `SearchWindow.xaml.cs:1468` 附近的键分发处**新增一条独立分支**，位于 `_stagingAddHotkey` 判定之后、`ActionHotkeys` 判定之前或之后（顺序需明确固定并测试）。

复用 `Services/ActionHotkeyTable.cs` 的 `Parse` 做组合键**字符串解析**（纯函数，无状态），但**不复用** `ActionHotkeyCatalog` 的 15 条静态镜像，也**不写入** `settings.json` 的 `ActionHotkeys` 字典。

**为什么必须分开**（设计 §7.2 + §14-R10）：

1. **语义不同**：`ActionHotkeys` 是「搜索窗可见 + 有选中结果时，对该 target 执行内置动作」。命令快捷键是「调用命令」——命令可能根本不需要选中项（如打开设置）。塞进同一字典后，「无选中项时该不该触发」这个判断会变成一堆特例。
2. **存储所有权不同**：`settings.json` 由 WPF 全量读-改-写。旧版 WPF 保存时会**丢弃它不认识的字段**。命令绑定写进去，用户用旧版本打开一次设置页就全没了。broker 独占的 `commands-v1.json` 没有这个问题。

**冲突检测**：命令快捷键与 `ActionHotkeys` 撞键时，在设置页给出明确提示。K2 首期只做**搜索窗口内**快捷键，全局 `RegisterHotKey` 属 K3（设计 §7.2 表格）。

### 4.7 staging surface：固定入口，不建通用 picker

**做法**：`StagingStrip.xaml` 现有三个按钮（Save/Worksets/Clear，`:72-85`）旁增加第四个「对暂存区执行」。点击弹出一个**简单的固定列表**（复用现有 ActionPanel 渲染或轻量 Popup），列出 `bindings.staging.is_some()` 的命令。

**为什么不建 `CommandPicker` 状态机**：设计 §6.4 提到 K2 可增 `CommandPicker`，但 §12-K4 同时把「通用 `list -> Item[]` 页面栈」划为未承诺项，§14-R12 警告内核被提前拖重。K2 的 staging 命令首批只有 2 条（`copy_paths` + ZIP），为 2 条命令建一套可复用页面栈是典型的过度工程。**用固定菜单，等命令数量真正增长再抽象。**

**执行流**（对齐设计 §13.3）：

```
点击「对暂存区执行」
  → WPF 在 UI 线程快照 StagingArea.Items → staged_paths（字符串数组）
  → 若 > 128 项：整体拒绝，提示缩减（P4，禁止截断）
  → 选中命令；若需输出路径（ZIP）→ 前台 SaveFileDialog 子流程 → arguments.output_path
  → 估算 JSON 大小，> 512 KiB 则拒绝（客户端先检查）
  → ExecuteCommand(source = staging)
  → broker 解码后复核上限；spawn_blocking 中分类/校验路径
  → handler 执行
  → 返回 success/failure/cancelled 汇总
  → WPF 保留暂存区内容不清空，显示汇总
```

**UNC/网络路径**：设计 §5.3-5 明确——本地路径校验放 `spawn_blocking`，但它**无法取消卡死的 UNC/离线访问**。K2 首版对 mutation 类命令（ZIP）**直接拒绝 UNC**，不做存在性探测。`copy_paths` 复制原始字符串、不探测存在性，因此不受此限。

### 4.8 在飞守卫必须覆盖命令路径

`_actionInFlight`（`SearchViewModel.cs:493`）当前覆盖 `ExecuteSelectedAsync` 与 `RunActionOnAsync`。K2 新增的三条命令入口——动作面板命令段、命令快捷键、staging 批量——**全部必须复用同一守卫或同等级独立守卫**（设计 §6.5 末条）。

特别注意 `ExecuteSelectedAsync:497` 的现有注释：动作面板路径**故意不叠加**守卫（`RunActionOnAsync` 自带，叠加会误吞）。新增分支时照抄这个结构，不要想当然地在外层再包一层。

### 4.9 执行前的代际/身份一致性检查

执行 broker-owned 命令前，确认动作通道的 server PID / build_id / features 与产生该结果的查询通道一致，且 catalog generation 不旧于结果 generation（设计 §9.2 末段 + §14-R13）。不一致 → 刷新目录并要求用户重试，**不猜测兼容**。

UI-owned 命令执行前同样要确认当前 catalog generation 仍包含同 id/owner，不能凭旧行直接本地执行。

K1 已有 `command_catalog_generation` 字段与 `CommandCatalog.Generation`，K2 把这条校验补进执行路径。

---

## 5. 实施序列

每个 commit 结束都必须满足 P6（独立可编译、测试全绿、clippy 干净）。

### commit 0 — 前置清理

按 §2 三项执行。产出：K1 红测试转绿、`FilterTriggerDetectorTests.cs` 新增、K1 文档状态更新。

**验收**：`dotnet test` 除已知隔离性 flake 外全绿。

> 已知 flake（**非本次引入，不要顺手"修"**）：`FaviconGrantTests.InvalidateDropsResolvedCacheAndReloadsFromDisk` 与 `WebIconNegativeCacheTests.Invalidate_Clears_The_Negative_Cache` 单独运行通过、全量运行失败，是测试间共享状态导致的隔离性问题。K2 期间只需确认「失败集合没有变大」。若要修，单独立项。

### commit 1 — 地基：`selection.target`

按 §4.1 执行。Rust 侧改 `CommandSelection`、`validate()`；C# 侧对齐 `CommandInvocationContext.cs`；`BuildCommandInvocationContext`（`SearchViewModel.cs:647`）填入选中项的 `ExecutionTarget`。

**测试**：
- `selection.target` 为合法 file/directory/application → 通过
- `selection.target.kind == "command"` → 拒绝（防命令递归）
- `selection.target` 路径非法 → 拒绝
- `selection` 为 None（root 来源）→ 通过（root 命令不需要选中项）
- 超长 title/subtitle → 按 256 chars 上限截断或拒绝（沿用现有语义，勿改）

### commit 2 — `ActionItem` 扩字段 + binding DTO 扩展

按 §4.2、§4.3 执行。本 commit **不产生任何用户可见变化**——只是把字段加上、默认值让 JSON 输出保持逐字节一致。

**测试**：
- 内置动作序列化后 JSON 与 K1 逐字节相同（对照固定字符串断言）
- C# 现有 `ActionItem` 构造点全部编译通过、行为不变
- `CommandBindingDto` 缺字段时反序列化用默认值

### commit 3 — `ActionComposer` + 动作面板命令段（broker 侧）

按 §4.4 执行。含 `Actions` 请求的 caps 透传改造、命令段组装、`ExecuteCommand` 的 `action_panel` 分支与二次校验。

首批命令动作：把 `prism.terminal.open` 的 `bindings.action_panel` 填上（`input: "selection"`, `cardinality: "one"`, `target_kinds: ["directory"]`）——对目录执行「在此打开终端」，正是设计 §13.2 的样例流。

**测试**：
- 未协商 `commands_v1` → 动作列表 JSON 与 K1 逐字节一致
- 已协商 + target 是 directory → 出现命令段与段头
- 已协商 + target 是 file → 终端命令**不出现**（target_kinds 不含 file）
- 命令段超过 5 条 → 截断至 5
- 命令段为空 → 不输出段头
- `ExecuteCommand(source=action_panel)` 且 target kind 不匹配 → 拒绝（二次校验）
- destructive 命令不进面板

### commit 4 — 动作面板命令段（WPF 侧）+ `app` 门禁对齐

- `RunActionOnCoreAsync`（`:806`）按 `InvocationKind` 分派：`builtin_action` 走现有 `RunAction`，`command` 走 `ExecuteCommand`。命令**不得**进入 rename/picker/delete 的硬编码分支。
- `EnterActionsAsync`（`:696`）门禁放宽至 `app`，与 `GetActionsForAsync` 对齐。
- `is_enabled=false` 的命令项渲染为禁用并显示 `disabled_reason`。
- 在飞守卫按 §4.8 接入。

**回归测试（必须全绿）**：rename 内联编辑、copy_to/move_to 目录选择、delete_permanent 确认、右键菜单、动作快捷键、`.lnk` raw/resolved 语义（`run_as_admin` 保留 `.lnk` 参数 vs 复制路径解析真实 exe——设计 §2.3-4、§14-R7）。

### commit 5 — `copy_paths`（staging 链路打通）

按 §4.7 执行。新增 broker handler `prism.staging.copy_paths`，`bindings.staging = { input: "staged_paths", cardinality: "many" }`。

剪贴板文本写入必须走 STA worker——新增 `ShellOperation` 变体，不要在 async 上下文直接调 Win32 剪贴板 API。

WPF 侧：StagingStrip 第四个按钮 + 固定菜单 + 快照/上限/汇总回显。

**测试**：空暂存区禁用、128 项边界、129 项整体拒绝、512 KiB 聚合上限、汇总文案、执行后暂存区不清空。

### commit 6 — 多目标 ZIP

按 §4.5 执行。若 `zip_windows_builtin` 无法可靠支持多输入，按 §4.5-3 降级为「仅 7-Zip 可用」并在目录中标禁用原因。

**测试**：输出路径冲突拒绝、UNC 拒绝、部分失效汇总、mutation 超时「结果未知」、7-Zip 缺失时的禁用态。

### commit 7 — 命令窗口快捷键

按 §4.6 执行。含 `commands-v1.json` 的绑定字段、目录下发、WPF 键分发分支、设置页冲突提示。

**测试**：与 `ActionHotkeys` 撞键提示、无选中项时不需要 selection 的命令仍可触发、需要 selection 的命令无选中项时禁用、旧版 WPF 保存 `settings.json` 不影响命令绑定。

> ✅ **已完成**（5cf3b9d）。Broker: `shortcut_bindings` 映射存 `commands-v1.json`，`catalog()` 合并，`SetCommandShortcut` IPC。WPF: `CommandShortcutTable` 从目录快照构建，`OnHeaderKeyDownCore` 新增独立分支（`_stagingAddHotkey` 之后），`ExecuteShortcutCommandAsync` 复用 `_actionInFlight`。`ValidateConflict` 撞键检测。Rust 473 pass + WPF 410 pass，flake 集合不变。

### commit 8 — 收尾

Release build、`dist` 产物重建、K2 文档交付清单勾选、`docs/G4-手动测试流程.md` 补 K2 手工验证步骤。

**批量 `move_to`**：K2 **不实施**。在本 commit 的文档中记录评估结论（设计 §8.2 已把它排到「K2 后半」且列出 mutation 超时/部分成功/同名冲突/DestinationPicker 四项未解合同），交由 K3 立项。

> ✅ **已完成**。Release build 全绿（Rust + WPF），dist 产物已重建。move_to 评估结论见下。

---

## 6. 验收门（对齐设计 §12-K2）

| # | 验收项 | 判定方式 |
|---|---|---|
| A1 | `.lnk` raw/resolved 语义未退化 | 自动化：`run_as_admin` 保留 `.lnk`、`copy_app_path` 解析真实 exe |
| A2 | 动作列出和执行都做 target 重校验 | 自动化：列出后篡改 target 再执行 → 拒绝 |
| A3 | 暂存路径失效处理 | 自动化：含失效路径 → 汇总提示，不静默过滤 |
| A4 | 运行时超过 128 项 | 自动化：129 项 → 整体拒绝，不截断 |
| A5 | 512 KiB 聚合上限 | 自动化：客户端预检 + broker 复核双侧 |
| A6 | UNC 明确拒绝 | 自动化：mutation 类命令收到 UNC → 类型化错误 |
| A7 | 空暂存与部分失败 | 自动化：空 → 禁用；部分失败 → 三类计数汇总 |
| A8 | 现有链路回归全绿 | 自动化：rename / 目录选择 / 永久删除 / 右键菜单 / 动作快捷键 |
| A9 | 未协商连接 JSON 不变 | 自动化：逐字节比对 K1 基线 |
| A10 | 双按不重复执行 | 自动化：三条新命令入口各覆盖 |
| A11 | clippy `-D warnings` + Release build | 命令行 |

---

## 7. 风险登记

| # | 风险 | 缓解 |
|---|---|---|
| K2-R1 | `zip_windows_builtin` 多输入不可靠，完成时机难判定 | §4.5-3 降级为 7-Zip 限定 + 禁用原因；不勉强用 COM |
| K2-R2 | C# `ActionItem` positional record 加参数破坏全部构造点 | §4.2 用 init 属性，主构造器不动 |
| K2-R3 | `Actions` 请求读不到连接 caps | 照抄 `ExecuteCommand`（`ipc.rs:1065`）的连接循环内处理写法 |
| K2-R4 | 命令段挤占动作面板、破坏手感 | cap 5 + 段头分隔 + destructive 排除 |
| K2-R5 | 命令绑定写进 `settings.json` 被旧版 WPF 抹掉 | §4.6：存 broker 独占的 `commands-v1.json` |
| K2-R6 | 在飞守卫叠加导致面板路径被误吞 | §4.8：照抄 `ExecuteSelectedAsync:497` 现有结构，不外包一层 |
| K2-R7 | 批量操作静默截断造成不可察觉的数据事故 | P4：超限整体拒绝；失效路径先汇总确认 |
| K2-R8 | staging picker 提前长成通用页面栈 | §4.7：固定菜单，2 条命令不做抽象 |
| K2-R9 | 放宽 `EnterActionsAsync` 至 app 后 `.lnk` 语义漂移 | commit 4 回归测试 A1 |

---

## 8.1 批量 move_to 评估结论（K2 不实施）

设计 §8.2 将批量 `move_to` 排到「K2 后半」，但 K2 评估后决定**不在 K2 实施**，交由 K3 立项。四项未解合同：

1. **mutation 超时**：`move_to` 是 mutation 操作，不像 `copy_paths` 无损复制。`spawn_blocking` 无法取消卡死的网络/离线访问（设计 §5.3-5）。单个文件移动超时会阻塞整批，且无法安全中断（部分文件已移动）。
2. **部分成功**：128 个文件移动到第 50 个失败时，前 49 个已不可逆地移动。`copy_paths` 失败无副作用（只写剪贴板），ZIP 失败可丢弃输出文件；`move_to` 的部分失败留下分裂的文件集，回滚不现实。
3. **同名冲突**：目标目录已有同名文件时，单个 `move_to` 弹确认对话框。批量场景下 128 个文件可能产生 N 个对话框——逐个弹不现实，全覆盖太危险，需要冲突策略选择 UI（跳过/覆盖/重命名）。
4. **DestinationPicker**：`copy_to` / `move_to` 单文件用 `BrowseFolderPicker` 模态选目标目录。批量 `move_to` 同样需要选目标，但 picker 与 staging strip 的模态守卫（`_modalDialogs` / `_ignoreDeactivate`）交互更复杂——当前 staging 命令路径的模态守卫只支持 ZIP 的 `SaveFileDialog`，不支持目录选择。

K3 立项需解决：mutation 超时策略（逐文件超时 vs 整批超时）、部分成功汇总+回滚策略、同名冲突策略 UI、DestinationPicker 模态守卫适配。

---

## 8.2 明确不做（K2 边界）

- 用户自定义命令、`open_url` / `launch_program` 执行器、导入导出、安全表单 → **K3**
- `ValidateTriggerNamespace` 关键字冲突校验、网页引擎目录适配 → **K3**
- 全局 `RegisterHotKey` 热键 → **K3 评估**
- 批量 `move_to` → **K3 立项**（§5-commit 8 记录评估结论）
- 通用 `list -> Item[]` 页面栈、note KV、剪贴板上下文、外部扩展进程、命令链 → **K4**
- 迁移 16 个 `ActionId`、改 `history-v2.json` / `staging.json` schema → **不做**
- 窗口 / Web target 的命令动作 → 首批不开（设计 §7.1 末条）

---

*文档完成时间: 2026-08-28*
*基线: commit 9035c0f*
*依赖: K1 已交付；commit 0 前置清理为开工条件*
