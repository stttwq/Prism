# 命令系统简化 + 内存收口 III 施工方案（2026-08-30）

> 交付对象：执行本方案的实现方（另一模型 / 另一次会话）。
> 本文件即施工说明书：**关键点在哪、怎么做、为什么这么做、总体原则**。
> 上游材料：`D:\下载\Everything内存机制深度解析-技术调研报告.md`、
> `D:\下载\Listary命令系统与新一代启动器调研报告.md`、
> 已落地的 `docs/PRISM-MEMORY-PLAN-II-2026-08-30.md`（本方案是它的后续，不是替代）。

---

## 0. 本轮范围

三件事，合并成一次施工：

| Part | 内容 | 收益 |
|---|---|---|
| **A** | 命令系统简化（含「记事本打开空白文件」bug 的根因修复） | 可用性；小白能配对 |
| **B** | 内存收口 III（Everything 报告 §10 表 4 的剩余项 + 一条自查发现） | 峰值内存；防复发 |

Part A 与 Part B 互不依赖，可并行也可分两次做。**先做 A**——它含一条用户已实际踩到的缺陷。

---

## 1. 背景

### 1.1 用户实际故障：命令系统「用记事本打开」开出空白新文件

复现：设置页套用模板「用记事本打开」→ 保存 → 在搜索框输入关键字 `np`（或在根搜索里选中该命令行）执行 → 记事本打开一个**空白新文件**，Prism 报成功，而不是打开当前选中的 `.txt`。

根因是**模板配置与引擎行为两处合谋**，缺一不会出现：

1. `src/Prism/ViewModels/SettingsViewModel.cs:1251-1252` 的模板同时预填了
   `args_template = "{selection.target}"` **和**关键字 `np`：

   ```csharp
   new CommandTemplate("用记事本打开", "launch_program", "用记事本打开",
       "", "C:\\Windows\\System32\\notepad.exe", "{selection.target}", "", "np", "..."),
   ```

2. `{selection.target}` **只有动作面板入口有值**。四条执行路径里只有一条填 `Selection`：

   | 入口 | 代码位置 | 是否填 `Selection` |
   |---|---|---|
   | 动作面板 | `SearchViewModel.cs:1110-1123`（`ExecutePanelCommandAsync`） | ✅ `Target = target.ExecutionTarget` |
   | 根搜索 | `SearchViewModel.cs:698-712`（`BuildCommandInvocationContext`） | ❌ |
   | 关键字路由 | `SearchViewModel.cs:1996-2004`（`ExecuteCommandKeywordAsync`） | ❌ |
   | 快捷键 | `SearchViewModel.cs:895` 附近 | ❌ |

3. 展开为空之后，`src/prism-core/src/commands.rs:1596-1603` 把空参数**静默丢弃**：

   ```rust
   template
       .split_whitespace()
       .filter_map(|tok| match expand_template_raw(tok, ctx) {
           Ok(v) if !v.trim().is_empty() => Some(v),
           Ok(_) => None,              // ← 空展开 = 丢参数，命令照跑
           Err(_) => Some(tok.to_string()),
       })
   ```

   于是 `notepad.exe` 无参启动 = 新建空白文档，`execute_command` 返回成功。

这个「丢空参数」本身是 G7c 的修复（在此之前是把字面 `{selection.target}` 当路径传给记事本，记事本报「文件不存在」）。当时用「不报错」换掉了「报错文案难看」，代价是**把一个配置错误变成了静默的错误行为**——比原来更难排查。本方案把它改回报错，但报**正确的错**。

### 1.2 命令系统过于复杂

`src/Prism/Windows/SettingsWindow.xaml:1203-1337`，新建一条命令要面对 12 个控件：
标题 / 关键字（逗号分隔）/ 参数表格（名称·必填·默认值，可多行）/ 处理器类型 / URL 模板 /
程序路径 / 参数模板 / 工作目录 / 触发词 / 快捷键 / 在根搜索中显示 / 无结果时作为回退 / 启用。
外加 `:1280-1282` 一段 12 个占位符 + 5 个修饰符的说明文字。

对照调研报告：Listary v7 的 Custom Command 只有 9 个字段、4 个占位符（报告 §3.3），
而 Prism 现在是 12 个控件 + 12 个占位符。**我们在"比 Listary 强"的路上把配置面做成了比 Listary 更难。**

三个最伤的点：

- **「关键字」与「触发词」两个字段语义重叠。** 用户看不出区别；`CommandEditItem.cs:231-236`
  实际上在 `Trigger` 为空时用 `keywords[0]` 兜底——也就是说这两个字段本来就是一回事。
- **两套参数模型并存。** `{query}`（整段透传）与 `{arg.名}`（`CommandEditItem.cs:254-262` 的声明式
  参数表格）同时暴露在一级表单。小白两个都不懂，power user 只用其中一个。
- **占位符与入口的合法组合是隐式的。** `{selection.target}` 只在动作面板有值、
  `{current_folder}` 只在 Explorer 宿主有值——表单里没有任何提示，也没有任何校验。
  §1.1 的 bug 就是这条的直接后果。

---

## 2. 总体施工原则

1. **减少"创建一条命令时要做的决定数"，而不是重构内部对象模型。**
   小白友好的来源是决定数，不是抽象干净度。内部模型换一遍不减少任何一个决定，还要付迁移代价。
2. **能力一条不删。** 今天能配出来的命令，改造后必须仍能配出来（在「高级」区）。
   这是与 Listary「不支持就是不支持」的区别，不能自废武功。
3. **让非法组合配不出来，而不是配出来之后提示。**
   校验放在 broker（唯一裁决者，已有先例：`ipc.rs:2419-2442` 的命名空间冲突检查），
   不放在 WPF——WPF 只做表单收敛，绕过 WPF 直接改 `commands-v1.json` 也不能产生跑不通的命令。
4. **broker 持久化格式（`commands-v1.json`）与 IPC 协议不新增必填字段。**
   Part A 只允许「新增可选字段 + 新增拒绝分支」，向后兼容，旧文件直接可读。
5. **删代码优先于加代码**（沿用内存方案 II 的原则）。Part B 主体是删与设限。
6. **每个非平凡改动配一条可跑的断言。** 不加框架、不加 fixture。
7. **不谎报达标。** 实测数字进文档，达不到就写达不到。

---

## 3. Part A：命令系统简化（方案 C′：命令类型向导 + 渐进式表单）

### A0. 为什么不是"按报告重做命令抽象"

调研报告 §5.1 描述的 Raycast 模型（统一 Manifest + 类型化 Arguments + 多 mode）确实是当前最完整的范式，
但**照搬它解决不了 Prism 的实际问题**：

- Raycast 的 Manifest 是给**扩展开发者**写的（`package.json`），不是给终端用户在设置页填的。
  Prism 没有扩展生态，用户就是作者，Manifest 抽象对他没有价值。
- Prism 今天的对象模型（`UserCommandDefinition` + 5 个 surface binding + handler + handler_params）
  在**表达力上已经不输 Raycast**——问题是这份表达力被 1:1 摊平到了表单上。
- 换内部模型要付：持久化迁移、IPC 版本、`CommandEditItem` / `CommandDescriptor` /
  `UserCommandDefinition` 三套 DTO 同步改、全部命令测试重写。收益是"内部更整齐"，
  **用户一个决定都没少做**。

C′ 取 Raycast 真正管用的两条——**`mode` 这类"这个命令吃什么输入"的一等声明**（报告 §5.1 Command Mode）
与**渐进式复杂度**（报告 §6.4 矛盾 3 的行业共识：默认形态 + 可选形态）——落到 Prism 现有模型上，
**broker 侧几乎零改动**。

### A1. 核心：把「命令类型」提为第一个也是唯一必答的问题

四种类型。它们覆盖 Listary 13 个内置 Command（报告 §3.2）与 Everything 12 个 Custom Open Command
（报告 §4.1）的**全部**形态：

| # | 类型（UI 文案） | 心智 | handler | 可用占位符 | 默认绑定入口 | 关键字 |
|---|---|---|---|---|---|---|
| ① | **用网站搜索** | "用 X 搜我打的字" | `open_url` | `{query}` | keyword + root_search | 必填 |
| ② | **对选中的文件做事** | "拿这个文件去做 X" | `launch_program` | `{selection.target}` | action_panel + shortcut | **禁用**（输入框置灰） |
| ③ | **打开某个东西** | "启动 X / 打开网址" | `launch_program` 或 `open_url` | 无 | root_search + keyword + shortcut | 可选 |
| ④ | **在当前文件夹做事** | "在这里开终端 / 新建" | `launch_program` | `{current_folder}` | keyword + action_panel(directory) | 必填 |

**类型决定之后，一级表单只显示该类型需要的字段：**

- ① 三个输入框：网站 URL 模板（`{query}` 处高亮）、关键字、标题。
- ② 两个输入框：程序路径（带「浏览…」按钮）、标题。外加一行灰字：
  「在搜索结果里选中文件后按 `→` 打开动作面板执行」。
- ③ 两到三个：路径或网址、标题、关键字（可选）。
- ④ 三到四个：程序路径、参数（可选，`{current_folder}` 已预填）、关键字、标题。

**「高级」折叠区（默认收起）**保留今天的全部自由度，一个字段都不删：
处理器类型下拉、原始参数模板、工作目录、声明式参数表格（`{arg.名}`）、快捷键、
在根搜索中显示、无结果时作为回退、danger、以及完整的 12 个占位符 + 5 个修饰符速查。

### A1.1 类型不进持久化，靠反推

**不给 `UserCommandDefinition` 加 `kind` 字段。** 重新编辑既有命令时按以下顺序反推类型：

```
handler == open_url  且 url_template 含 {query}                    → ①
handler == launch_program 且 args_template 含 {selection.target}   → ②
handler == launch_program 且 args_template 含 {current_folder}     → ④
其余（含内置 prism.* 命令、含 {arg.x}/{clipboard}/{date}/{uuid} 的） → ③，且直接展开「高级」区
```

理由：反推逻辑约 10 行，零迁移、零协议改动、对手改 JSON 的用户天然兼容。
推错的代价只是"表单默认展开了高级区"，不影响任何行为。

> ponytail: 反推够用。等到反推被证明不够（例如用户抱怨"我明明选了②它记成③"）再加 `kind` 字段，
> 那时也是 `#[serde(default)]` 的可选字段，仍然不需要迁移。

### A2. 引擎侧三条硬约束（这才是根因修复，不是 UI 化妆）

三条都在 broker。**只做 UI 收敛而不做这三条，等于把 §1.1 的 bug 留在系统里，只是让它更难被触发。**

#### A2.1 空占位符 = 报错，不再静默丢参

**文件**：`src/prism-core/src/commands.rs`，`validate_launch_program`（`:1556-1619`）。

改签名，加一个显式的严格开关：

```rust
pub fn validate_launch_program(
    handler_params: &BTreeMap<String, String>,
    ctx: &ExpansionContext,
    require_non_empty_args: bool,      // ← 新增
) -> Result<(String, Vec<String>, Option<String>), String>
```

`args_template` 的展开循环（`:1589-1606`）改为：

- token **不含 `{`**（字面量）→ 原样保留，与今天一致。
- token 含占位符且展开非空 → 保留，与今天一致。
- token 含占位符且展开为空：
  - `require_non_empty_args == false` → 丢弃（今天的行为）。
  - `require_non_empty_args == true` → `Err`，文案按占位符分流：
    - `{selection.target}` → `"此命令需要先选中一个文件：在搜索结果里选中后按 → 打开动作面板执行"`
    - `{current_folder}` → `"此命令需要当前文件夹：请在资源管理器窗口中呼出 Prism"`
    - `{query}` / `{arg.x}` → 复用 `resolve_arguments` 的既有文案口径 `"缺少必填参数：{名}"`
    - 其余 → `"占位符 {名} 取值为空"`
- token 展开出错（未知占位符 / 坏修饰符）→ 保留原文，**与今天一致**（暴露拼写错误的既有设计，别动）。

**三个调用点的取值（这是必须精确的地方）**：

| 调用点 | 文件:行 | `require_non_empty_args` | 为什么 |
|---|---|---|---|
| 保存 `handle_command_set` | `ipc.rs:2405-2411` | **false** | 保存时上下文是空的（`ipc.rs:2364-2384` 显式构造 `selection: None, query: ""`）。传 true 会让**任何**带占位符的命令都存不进去。 |
| 执行 `execute_command` | `ipc.rs:2812` 附近 | **true** | 根因修复点。 |
| 预览 `handle_command_preview` | `ipc.rs:2534` | **true** | 报告 §3.5 痛点 8：Listary 没有 dry-run。Prism 有，就要让它显示**执行时真会发生的事**，包括"这条命令在这个入口跑不起来"。 |

> ⚠️ 若漏了保存点必须传 false 这一条，症状是"所有命令都保存失败"，且错误文案会指向占位符——
> 很容易被误判为占位符实现坏了。这是本条改动唯一的坑。

**断言**（放 `commands.rs` 的 `#[cfg(test)]`）：
1. `args_template = "{selection.target}"`、`ctx.selection = None`、strict=true → `Err` 且文案含「选中」。
2. 同上 strict=false → `Ok`，`args` 为空（保存路径不回归）。
3. `args_template = "--flag {selection.target}"`、有 selection、strict=true → `args == ["--flag", "<路径>"]`。
4. `args_template = "--flag"`（纯字面）、strict=true → `Ok(["--flag"])`（不误报）。

#### A2.2 占位符 × 入口 的合法性在保存时裁决

**文件**：`src/prism-core/src/ipc.rs`，`handle_command_set`（`:2354`），插在 handler 校验
（`:2394-2417`）之后、命名空间检查（`:2419`）之前。

纯结构判定，不需要展开：

```
若 (url_template ∪ args_template ∪ working_dir) 含 "{selection.target}"：
    则 bindings.keyword 必须为 None
    且 bindings.root_search.show_in_root_search 必须为 false
    否则 Err("{selection.target} 只在动作面板有值。请去掉关键字与根搜索显示，
              或改用 {query} 让用户手动输入路径")

若 上述模板含 "{current_folder}"：
    仅记一条日志提示（不拒绝）——Explorer 宿主外它合法地展开为空，
    且 A2.1 已在执行时给出可读错误。
```

**为什么放 broker 而不是 WPF**：设计上 broker 是唯一裁决者（`commands.rs:1647-1652` 的
`validate_trigger_namespace` 已是此先例）。放 WPF 的话，手改 `commands-v1.json` 或
从旧版导入（`SettingsViewModel.cs:1309` 的 `ImportCommandsAsync`）仍能产生跑不通的命令。

**为什么 `{current_folder}` 只警告不拒绝**：它在 Explorer 宿主下经关键字路由是**完全合法**的
（`SearchViewModel.cs:2001` 确实填了 `CurrentFolder`），拒绝会砍掉 Listary `cmd`/`mkdir`/`touch`
这一整类命令（报告 §3.2）。`{selection.target}` 不同——它在关键字路由下**恒为空**，没有任何合法用法。

**兼容性**：用户既有的 `np` 命令（含 `{selection.target}` + 关键字）**再次保存时会被拒绝**。
这是预期行为，且 A3 的表单会在选中类型②时自动清空关键字与根搜索勾选，用户点一次保存就修好了。
不做静默自动迁移——静默改用户配置比报错更坏。

#### A2.3 关键字与触发词合一

**WPF 侧**：删掉 `SettingsWindow.xaml:1301-1304` 的「触发词」输入框，改为只读展示
「关键字路由触发词：`np`（= 关键字第一项）」。冲突提示 `:1305-1307` 保留原位。

**`CommandEditItem.cs`**：`ToDefinition()`（`:231-236`）的兜底逻辑
（`Trigger` 为空时取 `keywords[0]`）保持不变——它现在成为唯一路径。
`Trigger` 属性（`:150-154`）保留供反序列化回显，但不再双向绑定到输入框。

**broker 侧零改动。**

理由：这两个字段今天就是同一个东西（`CommandEditItem.cs:233` 已经这么处理），
把重复的那个从 UI 上删掉是纯减法。对照 Listary：它只有一个 Keyword 字段（报告 §3.3）。

### A3. WPF 侧渐进式表单

**文件**：`src/Prism/Windows/SettingsWindow.xaml:1180-1346`（编辑表单整块）、
`src/Prism/Models/CommandEditItem.cs`、`src/Prism/ViewModels/SettingsViewModel.cs`。

1. `CommandEditItem` 加 `CommandKind` 属性（`string`：`web` / `selection` / `open` / `folder` / `advanced`），
   **不参与 `ToDefinition()` 的输出**，只驱动 XAML 可见性与保存前的字段归一。
   构造函数里按 A1.1 反推初始值。
2. 表单顶部加一行四选一（RadioButton 组或 ComboBox），选中即：
   - 设置对应的 `Handler`；
   - 类型② 额外置 `KeywordsText = ""`、`ShowInRootSearch = false`（配合 A2.2，保存必过）；
   - 若 `ArgsTemplate` 为空，按类型预填（②→`{selection.target}`，④→`{current_folder}`）。
3. 用 `CommandKind` 驱动可见性（沿用现有 `BoolToVis` 与 `IsOpenUrl`/`IsLaunchProgram` 的写法，
   加一个 `StringEqualsToVis` 转换器即可）。
4. 「高级」`Expander`，默认 `IsExpanded=False`，装：处理器类型下拉（`:1259-1268`）、
   参数模板（`:1291-1294`）、工作目录（`:1295-1298`）、参数表格（`:1216-1257`）、
   快捷键（`:1309-1312`）、三个复选框（`:1314-1326`）、完整占位符速查（`:1280-1282`）。
   反推为 `advanced` 时自动展开。
5. **一级表单的占位符提示按类型收敛**：只显示该类型可用的那 1 个，一句话说明。
   12 个占位符的完整表只在「高级」里。

**不要做的**：不要为此新建窗口/页面/向导对话框。四选一 + 一个 `Expander` 就够，
多一个窗口就多一份 BAML 常驻（内存方案 II §4 已记：`SettingsWindow.xaml` 首次打开后永久驻留）。

### A4. 模板库扩充（报告 §3.5 痛点 2）

**文件**：`SettingsViewModel.cs:1243-1257`（`CommandTemplates`）、
`CommandEditItem.cs:270-280`（`CommandTemplate` record）。

1. `CommandTemplate` record 加一个 `Kind` 字段（第一位），`ApplyTemplate`（`:1227-1240`）随之设置
   `SelectedCommand.CommandKind`。
2. **修掉现有两条错模板**：`:1251-1254` 的「用记事本打开」「用 VS Code 打开」改为 `Kind = selection`，
   **关键字位改为空串**。这是 §1.1 bug 的配置侧修复。
3. 每类补到 2–3 条，覆盖 Listary 内置命令形态：

   | Kind | 模板 |
   |---|---|
   | web | Google 搜索 `g`、百度 `bd`、GitHub 仓库 `gh`（现有三条，不动） |
   | selection | 用记事本打开、用 VS Code 打开（改 Kind + 清关键字）、用默认程序打开 |
   | open | 打开 hosts 文件（对齐 Listary `hosts`）、打开网络连接（`connections`） |
   | folder | 在此打开终端 `cmd`（对齐 Listary `cmd`/`psh`）、在此新建文件夹 |

### A5. 可发现性：让类型②不出现在够不到的地方

- 类型② 命令 `show_in_root_search = false`（A3 第 2 点已设）——根搜索里**不出现**
  "选中了按 Enter 却打开空白记事本"的行。
- 它的唯一入口是动作面板的「命令」段（`action_composer.rs:41` 的段头 + `:93-106` 的条目）。
  `CommandEditItem.ToDefinition()`（`:223-230`）今天已经默认给所有用户命令绑 `ActionPanel`，
  **不要改这条**——注释里写明的理由仍然成立。
- 若要进一步降低门槛：在结果行的动作提示里体现 `→`。这属于锦上添花，**列为可选，不做也不算未完成**。

### A6. 语义变化（必须落账，见第 5 节）

| 变化 | 影响面 |
|---|---|
| `launch_program` 的参数占位符展开为空 → 执行/预览报错（此前静默丢参） | 所有 `launch_program` 命令 |
| 含 `{selection.target}` 的命令不能同时绑关键字 / 进根搜索（保存时拒绝） | 既有此类命令**重新保存**时会被拒；运行时行为按上一条改变 |
| 「触发词」输入框消失，触发词恒 = 关键字第一项 | 曾把 Trigger 填成与 keywords[0] 不同值的用户，触发词会变 |

第三条是唯一可能"改变既有用户可见行为"的地方。落地前用一条一次性日志把此类命令记出来
（`catalog()` 里发现 `bindings.keyword.trigger != keywords[0]` 时 `log` 一行），便于事后核对。

---

## 4. Part B：内存收口 III

Everything 报告 §10 表 4 的剩余项，加一条自查发现（B2）。
**Part B 不改 IPC、不改缓存格式、不改搜索语义。**

### B1. spec 红线：maintenance tick 不得触碰 O(n) 内存

**文件**：`.trellis/spec/backend/quality-guidelines.md`（`:88` 已有内存方案 II 的 stat 契约，紧随其后加一条）。

内容要点（用与既有条目同样的措辞密度写）：

> maintenance tick（`indexer_runtime.rs:1494-1675`，5 秒一拍）中的任何任务，其单拍复杂度上限是
> **O(卷数)**，不得线性遍历 `nodes` / `names` / 任何按 MFT 记录号分配的表。
> 依据：空闲修剪（`trim_working_set`，`:1530-1536`）把工作集清到个位数 MB，
> 任何 5 秒一次的全表触碰都会立刻把它拉回——G7c 的 stat 填充 tick 正是这样让修剪白做，
> 导致"空闲只掉到 80MB"（内存方案 II §1 根因 B）。
> 需要全表工作的任务必须：① 由计数器/阈值谓词门控（`needs_name_compact`，`hierarchy.rs:712-724`
> 是正例：每拍只花每卷 O(1)）；② 或有独立退避（拼音重建的 `PINYIN_REBUILD_BACKOFF`，`:1606-1615`）；
> ③ 或按事件量/时间门控（checkpoint 的 50 万事件 / 6 小时，`:1648-1649`）。
> 对照：Everything 二十年没有周期性全表扫描，不是靠自觉而是靠架构——增量 USN 读取是唯一通路。

零运行时成本，是本轮**性价比最高的一项**，先做。

### B2. 名字池压缩去掉整卷 clone（自查发现，报告未覆盖）

**现状**：`indexer_runtime.rs:2549-2593` 的 `compact_one_volume_off_lock`：

```rust
let snapshot = { /* 读锁 */ volume.clone() };   // :2568 —— 整卷深拷贝
let mut compacted = snapshot;
compacted.compact_names_if_needed()?;           // 锁外
let mut guard = state.index.write()?;           // 短写锁
*slot = compacted;                              // :2588 —— O(1) 换入
```

`volume.clone()` 复制 `nodes`（`capacity × 12 B`）+ `names`。本机 2 卷约 360 万槽，
`nodes` 约 43 MB。压缩瞬间常驻翻倍：**原卷 + 克隆 + `compact_names_if_needed` 内部的
`replacement` 池（`hierarchy.rs:767`，`with_capacity(self.names.len())`）**。
八百万文件量级的机器上这是实打实的 OOM 风险面。

**改法**（保持"重活在锁外"的既有正确模式不变，只把搬运的数据量降下来）：

1. 读锁内不再 `volume.clone()`，改为收集一份**紧凑快照**：
   - `names` 的一份拷贝（压缩必须读它）；
   - `Vec<(u32 record, u32 parent_record, u32 name_off)>`，只收 `FLAG_PRESENT` 的槽。
   1.2M 活跃文件 ≈ 12 字节 × 1.2M ≈ 14 MB，对比 43 MB 的 `nodes` 全表。
   同时记下 `next_usn`（乐观并发校验用，语义与今天完全一致）。
2. 锁外用这份快照算出：新 `names` 池、`Vec<(u32 record, u32 new_name_off)>` 映射、
   新 `names_fingerprint`、新 `initial_name_bytes`、尾部 `live_bound`
   （`hierarchy.rs:802-809` 的算法原样搬过来，输入换成快照里的 record/parent 对）。
3. 写锁内：先校验 `next_usn` 未变（不变式与 `:2584` 一致，变了就重试，重试上限仍是 3），
   然后就地施工——`for (rec, off) in &map { nodes[rec].name_off = off; }`、
   `volume.names = new_names`、`dead_name_bytes = 0`、写回指纹与基线、
   `nodes.truncate(live_bound); nodes.shrink_to_fit()`。

**代价与判断**：写锁持有时间从 O(1) 变成 O(活跃节点)（约 120 万次 u32 随机写，毫秒级）。
USN apply 本来就在写锁内做同量级的工作，可接受。峰值内存少一份 `nodes`（本机 ~43 MB，
大机器上按 12 B × 槽数线性放大）。

**为什么不改成"锁内分块压缩"**：名字池压缩必须原子——偏移改到一半的 `nodes` 是损坏索引。

**断言**：
1. 既有 `compact_names_reclaims_trailing_node_slots`（`hierarchy.rs:2440`）必须继续通过，
   且**期望值一条不改**。
2. 新增：构造带死名字的卷 → 走新路径压缩 → 断言 `path_for` 对每个在位节点的输出
   与压缩前逐字节相同（这是"偏移映射没错"的唯一有效防线）。
3. 新增：模拟 clone 与换入之间 `next_usn` 前移 → 断言压缩被放弃且 `dead_name_bytes` 未清零
   （下拍必然重触发，与今天语义一致）。

> ⚠️ 这是 Part B 里**唯一有正确性风险**的改动。若实测收益不足以支撑风险（例如目标机器
> 槽数远小于 360 万），**允许只做 §B2 附注的一行版本并如实记录**：把
> `hierarchy.rs:767` 的 `Vec::with_capacity(self.names.len())` 改为
> `with_capacity(self.names.len().saturating_sub(self.dead_name_bytes))`，
> 省掉 `replacement` 的死字节那一份。一行，零风险，收益小。

### B3. USN replay 无界累积加上限

**文件**：`src/prism-core/src/ntfs.rs`，`replay_until`（`:560-592`）。

```rust
let mut replay = Vec::new();
while cursor < high_water {
    let (next, mut records) = read_changes(...)?;
    replay.append(&mut records);          // :577 —— 无上限
    cursor = next;
}
apply_replay_records(volume, &replay, cursor)?;
```

积压有多大就吃多少内存，每条 `UsnRecord`（`:21`）还带一个堆 `String`。
服务停了很久再启动、或 Windows Update 期间重建，这里是一个不设防的峰值。

**改法**：加上限，超限即走全量重建。

```rust
/// 单次 MFT 建卷后回放的 USN 记录数上限。超过说明积压已大到"重扫 MFT 更划算"
/// （Everything 同口径：卷变化大时走 fast reindex 而不是无限回放日志）。
/// 超限返回 Err，由既有的卷级重建路径接管——这条路径本来就存在（journal wrap）。
const REPLAY_MAX_RECORDS: usize = 500_000;
```

`replay.len() > REPLAY_MAX_RECORDS` → `Err("USN backlog exceeds replay budget; full rebuild")`。

**为什么不改成分块 apply**：`apply_replay_records`（`:176`）依赖"整批可见"来解析
子记录先于父记录到达的情形（既有测试 `replay_resolves_child_before_parent_and_skips_stale_orphans`，
`:1096`）。按 256 KiB 分块 apply 会把跨块的父子关系误判为不可达并跳过——**静默丢文件**，
比内存峰值坏得多。要正确分块必须把跳过的记录带到下一块，`apply_replay_records` 现在只返回计数
（`:581` 的 `skipped`）不返回记录本身。不值得为此改接口。

**断言**：构造 `REPLAY_MAX_RECORDS + 1` 条记录的回放 → 断言返回 `Err` 且文案含 `budget`。
（若难以在单测里造出真实句柄，退而在 `apply_replay_records` 之外抽一个纯函数做上限判定并测它。）

### B4. USN 读取参数复查（结论：一改一不改）

报告 §10 表 4「低/借鉴」项。复查结果：

| 位置 | 现值 | 结论 |
|---|---|---|
| `ntfs.rs:536` `USN_READ_CHUNK`（watcher 的 `read_changes`，跨调用复用一块缓冲） | 256 KiB | **不改。** 监听循环用 `BytesToWaitFor=1` 在内核阻塞（`:551-552`），有变更立即返回，缓冲大小只在洪峰期影响 ioctl 次数。而它是**每卷常驻**——升到 1 MiB 等于常驻 +1.5 MB 换洪峰期的次要收益，与本轮方向相反。 |
| `ntfs.rs:610` `enumerate_mft` 的 `output`（建卷时枚举 MFT） | 256 KiB | **改到 1 MiB。** 对齐 Everything（报告 §3.1：第三方复现用 1 MB 缓冲）。360 万记录下 ioctl 次数降到 1/4。这块缓冲是**建卷期临时**的，函数返回即释放，稳态零成本。 |

顺带核实、**无需动作**的两项（写进文档免得下一个人重查）：
`enumerate_mft` 的 `records: Vec<MftRecord>` + `name_pool`（`:607-609`）在 360 万记录下约 115 MB + 名字，
属于建卷期峰值——Everything 官方口径是"索引期间每 100 万文件约 200 MB"（报告 §7.1），
Prism 明显更省，**不是问题**。

### B5. 内存锚点落档（报告 §10.1 第三点）

**文件**：`docs/PRISM-MEMORY-PLAN-2026-08-22.md` 末尾新增一节（不改历史表格，沿用方案 II 的落账纪律）。

从 `index_memory_trend` 日志（`indexer_runtime.rs:1541-1558`，已由方案 II A3 拆出
`nodes=` / `names=` / `pinyin=` 分项）取实测值，写成 Everything 式的线性锚点：

```
Prism indexer 稳态内存锚点（YYYY-MM-DD 实测，2 卷 / N 万活跃文件 / M 万 MFT 槽）：
  nodes  ≈ 12 B × MFT 槽数        （稀疏，按 max_record 分配，与活跃文件数无关）
  names  ≈ X B × 活跃文件数
  pinyin ≈ Y B × 含汉字的文件数
  合计   ≈ Z MB / 百万活跃文件
判读规则：memory_bytes 超出锚点 1.5 倍即为异常，先看哪一个分项在涨。
```

**这一条的全部价值在"下次回归能在读日志的第一分钟内定性"**——本轮 G7c 的 150 MB
排查全靠读代码反推，正是因为缺这个口径（报告 §10.1）。

### B6. 查过了，不要动（防止下一个人"顺手优化"）

| 位置 | 看起来像浪费 | 实际 |
|---|---|---|
| `indexer_runtime.rs:604-606` 拼音 `build → save → load` 往返 | 建好的表又从盘上读一遍 | `PinyinSidecar::load`（`pinyin_sidecar.rs:425-448`）在 Windows 上走 `MappedFile`，且 `load` 会做 identity 校验。**动之前必须先确认 `SidecarDisk` 反序列化后是否仍借用映射**：若是零拷贝，往返是把匿名堆换成文件页（可被 OS 回收），删掉它反而让内存变差；若是 owned `Vec`，才是真的往返浪费。**没确认之前不要删。** |
| `indexer_runtime.rs:670-678` 拼音重建的 `index.clone()` | 整索引深拷贝，峰值翻倍 | `PinyinSidecar::build`（`pinyin_sidecar.rs:371-400`）要 `record_chain` 做父链遍历，需要完整节点图，抽不出紧凑子集。已由 60 秒退避（`PINYIN_REBUILD_BACKOFF`）限频。**本轮不动**，作为已知代价写进 B5 的文档。 |
| `ipc.rs:3155` 附近 broker 侧 `apply_stat_filters` | 索引侧已经 stat 过一遍 | 内存方案 II §3 A2 明写：这是幂等的正确性兜底，保留。 |
| `CommandCatalog._snapshot`（`Services/CommandCatalog.cs:17`） | 前端常驻列表 | 随命令数线性，命令是几十条量级。方案 II §4 已判定不动。 |

---

## 5. 验证

按顺序，全部要跑。

### 5.1 静态门（每个提交各跑一遍）

- `cargo test`（`src/prism-core`）
- `cargo clippy -- -D warnings`
- `dotnet test src/Prism.Tests`
- WPF Release 构建 0 警告

现有测试里会因 Part A 编译不过的（`validate_launch_program` 换签名）：
`commands.rs:2804`、`:2851`、`:2874`、`:2882` 附近，以及 `ipc.rs:7046-7048`。
**改法一律是补第三个实参，不要顺手改断言的期望值。**

### 5.2 Part A 功能回归（人工，必做）

1. **记事本 bug 的验收**：套模板「用记事本打开」→ 保存（关键字应为空、根搜索勾选应为关）→
   在搜索结果里选中一个 `.txt` → `→` → 「命令」段 → 执行 → **记事本打开的是那个 txt**。
2. **旧配置的行为**：手工在 `commands-v1.json` 里保留一条带 `{selection.target}` + 关键字 `np` 的命令 →
   输入 `np` 执行 → **应显示「此命令需要先选中一个文件…」，不再打开空白记事本**。
3. **保存拒绝**：在设置页给类型② 命令强行填上关键字 → 保存 → **应被 broker 拒绝并给出可读文案**。
4. **保存不回归**：类型①（`{query}`）、类型④（`{current_folder}`）、含 `{arg.x}` 的命令
   全部能正常保存（验证 A2.1 的 `require_non_empty_args=false` 传对了）。
5. **预览**：类型② 命令点「预览」→ 应显示"需要选中项"而不是一条会打开空白记事本的命令行。
6. **反推**：重启设置页，重新选中每一条既有命令 → 类型下拉的回显符合 A1.1 的规则；
   含 `{clipboard}`/`{date}` 的命令落到「高级」且字段完整回显（`CommandEditItem.cs:61-72` 的回显路径不受影响）。

### 5.3 Part B 验证

1. **B1** 是文档，随提交 review。
2. **B2**：`tools/bench/Invoke-RebuildMemoryGate.ps1` 跑改前 A 侧 / 改后 B 侧两组，留 artifacts。
   另外构造一次真实压缩（大量删除文件把 `dead_name_bytes` 顶过 8 MB 阈值），
   用 `Measure-ProcessMemory.ps1` 采压缩期间的峰值，A/B 对比。**达不到预期就如实写达不到。**
3. **B3**：单测覆盖上限分支即可，不必造真实积压。
4. **B4**：建卷计时 A/B（`Invoke-G9FirstBuildAcceptance.ps1`），确认 1 MiB 缓冲没有让建卷变慢。
5. **三进程实机采样**，对齐 `PRISM-MEMORY-PLAN-2026-08-22.md` 的四个测量点
   （单次搜索后立即 / 展开 1000 后 / 隐藏后 3 分钟 / 进一步空闲稳态），确认无回退。

### 5.4 落账

- `.trellis/spec/backend/quality-guidelines.md`：B1 红线；A6 的三条语义变化
  （尤其"`{selection.target}` 与关键字/根搜索互斥"这条契约，防止下一个人再把模板配回去）。
- `docs/PRISM-MEMORY-PLAN-2026-08-22.md`：B5 的锚点节 + B2 的 A/B 数字。
- 本文件保留在 `docs/`。
- `dist/` 与安装包随代码更新（`scripts/build-installer.ps1`）。

---

## 6. 提交切分

| # | 提交 | 内容 | 可单独回退 |
|---|---|---|---|
| 1 | `fix(cmd): 参数占位符展开为空改报错，不再静默丢参` | A2.1（含三个调用点与测试） | ✅ 本轮最关键的一条，独立提交 |
| 2 | `feat(cmd): {selection.target} 与关键字/根搜索互斥（保存时裁决）` | A2.2 | ✅ |
| 3 | `feat(ui): 命令类型向导 + 高级区折叠；关键字/触发词合一` | A1 / A1.1 / A2.3 / A3 | ✅ |
| 4 | `feat(ui): 命令模板库按类型扩充，修正记事本/VS Code 模板` | A4 | ✅ |
| 5 | `docs(spec): maintenance tick 复杂度红线` | B1 | ✅ |
| 6 | `perf(index): 名字池压缩去掉整卷 clone` | B2 | ✅ 风险最高，务必单独 |
| 7 | `fix(index): USN replay 加记录数上限；MFT 枚举缓冲 1 MiB` | B3 + B4 | ✅ |
| 8 | `docs(memory): 内存锚点落档 + 收口 III A/B 数字` | B5 | — |
| 9 | `build(dist)` | 产物 | — |

提交 1 与 3 有先后依赖（3 的表单依赖 1 的报错文案），其余互不依赖。

---

## 7. 风险与回退

| 风险 | 判断 | 应对 |
|---|---|---|
| A2.1 漏传保存点的 `false` | **最可能的实施错误**，症状是所有命令保存失败 | 5.2 第 4 项就是这条的验收；三个调用点在本文档里已逐一列明 |
| A2.2 拒绝了用户既有配置 | 已知取舍，见 A2.2 末段 | 不做静默迁移；表单在选中类型②时自动清关键字，一次保存即修复 |
| A2.3 触发词与 keywords[0] 不同的存量用户行为改变 | 少数 | A6 的一次性日志先把这类命令记出来 |
| 类型反推推错 | 只影响表单默认展开状态 | 反推不出一律落「高级」，不丢字段 |
| B2 偏移映射写错 → 索引损坏 | **本轮最高风险** | 5.3 第 2 项的 `path_for` 逐节点比对断言是唯一有效防线；单独提交，`git revert` 即回到今天的 clone 版本 |
| B3 上限过小导致正常场景走全量重建 | 50 万条对应"服务停机期间发生 50 万次文件增删改名" | 常量集中一处，实测后可调；走的是既有 journal-wrap 重建路径，不是新路径 |
| B4 1 MiB 缓冲导致建卷变慢 | 不太可能（顺序大块读本就更快） | 5.3 第 4 项计时 A/B |

---

## 8. 明确不做的事

- **不引入插件系统 / 脚本命令 / 命令链。** 报告 §5 里 Raycast 的 `launchCommand`、
  Flow 的 `ChangeQuery`、uTools 的 `redirect` 都需要生态支撑，Prism 现在没有生态，
  加了只是多一套没人用的 API。
- **不做 AI / 自然语言参数抽取**（报告 §6.3 第 7 条趋势）。
- **不做云同步**。导入导出已具备（`SettingsViewModel.cs:1262-1306`），
  比 Listary 强（报告 §3.5 痛点 6/7），够用。
- **不为"对齐 Everything"引入可配置索引层的 UI**（Everything 报告 §10 表 4「避坑」第二条）。
- **不改内存哲学。** Prism 的空闲修剪路线已验证，不切换到 Everything 的"常驻不动"
  （Everything 报告 §9.2）。
- **不动 `SettingsWindow.xaml` 的整体结构**（1300+ 行，方案 II §4 已判定其常驻成本是 WPF 固有）。
  Part A 只在命令页内部重排。
