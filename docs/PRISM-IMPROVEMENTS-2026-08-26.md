# Prism 改进项清单 2026-08-26

本文档汇总 2026-08-26 会话讨论确认的改进项，含根因、方案与优先级。**均为待办，尚未实施。**

会话另有一项已**否决**的设想记录在末尾，供回溯。

---

## 问题1：别名设置绑到快捷方式而非应用程序

### 现象
用户想给"应用程序"设别名，但搜索结果里应用以 `.lnk` 快捷方式呈现，别名绑到 `.lnk` 路径而非真实 `.exe`。别名管理界面路径列暴露 `Start Menu\Programs\微信.lnk` 全路径。

### 根因
- 搜索结果里 app 的 `target.value` / `execute_id` 是 `.lnk` 路径（`ipc.rs:1659`、`ipc.rs:1675`），apps.rs:245 `launch_path = path`（.lnk 自身），仅副标题用 resolve 后的 exe。
- 右键设别名入口 `ShowAliasDialogAsync`（`SearchWindow.xaml.cs:833`）直接拿 `target.ExecutionTarget` 绑定，对 app = `(application, .lnk路径)`。
- 别名表 `alias.set` 存 `target.value` = .lnk 路径（`alias.rs:153-158`）。
- apps.rs 注释明示刻意留 .lnk 做 ShellExecute 以保留快捷方式参数/工作目录，但别名身份该指向真实身份。

### 方案（已定：方案A，暂不实施）
右键设别名时，对 `target.Kind=="app"` 且 `ExecuteId` 是 `.lnk` 时，先调新增 IPC 命令 `resolve_lnk` 拿 exe 路径，把 `aliasTarget.Value` 换成 exe 再 `AliasSetAsync`。执行仍走 .lnk（启动稳），仅别名身份改指 exe。

- **方案A（采用）**：身份和执行都走 exe。简单，直觉一致（别名指 exe，启动 exe）。大多数开始菜单 .lnk 无特殊参数，等价。少数带参数的 app 丢参数——等真有人报再说。
- **方案B（升级路径）**：alias 表加 `launch_path` 字段，identity=exe、执行=lnk。身份准 + 执行稳，但改 alias schema（`persistence.rs:138`），需迁移存量别名。是 A 的升级路径。

### 落点
- Rust：`apps.rs:277 resolve_lnk_target` 已能解任意 .lnk，但限 `pub(crate)`。新增 IPC 命令 `resolve_lnk`（仿 `copy_app_path` 的 resolve 用法），走 STA worker（复用 `shell.scan_apps()` 同款调度，apps.rs:123），不新增机制。
- 前端：`SearchWindow.xaml.cs` 设别名入口对 app 先 resolve，resolve 失败回退绑 .lnk（当前行为兜底）。
- `resolve_lnk` 命令同时是"昨天"功能升级路径的复用件（见问题5）。

### 代价
中。碰 IPC 协议（+Request 变体 + `dispatch_non_search` 分支 + STA worker 协调），但调度复用现成，无新机制。价值高（别名身份正确性）。

---

## 问题2：深色主题风格不统一

两个独立根因，拆 2A / 2B。

### 2A：托盘右键菜单是白色，字体不一致

#### 现象
深色主题下右键托盘图标弹出的面板是白色，字体与设置/搜索窗明显不一致。

#### 根因
`TrayService.cs:38` 用 `System.Windows.Forms.ContextMenuStrip`，WinForms 原生菜单不参与 WPF 主题资源系统。全仓零 Renderer 定制，字体也是 WinForms 默认 9pt，与 WPF 的 `AppFontFamily`（`Tokens.Dark.xaml:39`）不一致。

对比：搜索窗右键用 WPF `ContextMenu` + `PrismContextMenuStyle`（`Styles.xaml:191`），深色正常。

#### 方案
托盘菜单换 WPF `ContextMenu` + 同款 `PrismContextMenuStyle` + 显式绑 `AppFontFamily`。NotifyIcon 本身留 WinForms（只管托盘图标），仅 `ContextMenuStrip` 换 WPF ContextMenu 显示。

#### 落点
`TrayService.cs:38-49` 一处。

#### 代价
小。

### 2B：深色主题下设置界面框是白色

#### 现象
深色主题下设置界面（引擎 DataGrid 等）编辑框是白色。

#### 根因
引擎 DataGrid（`SettingsWindow.xaml:633`）的 `SettingsDataGridStyle`（330-351）只设 `Background` / `RowBackground` / `ColumnHeaderStyle` / `RowStyle`，**未设 `CellStyle` / `EditingElementStyle`**。`DataGridTextColumn` 双击编辑生成的内部 TextBox 用 WPF 默认模板 = 系统白底，深色下变白框。

仓库无全局 `<Style TargetType="TextBox">`（只有带 key 的局部 `QueryTextBoxStyle`、`SettingsTextBoxStyle`），DataGridTextColumn 不继承父级 Style，编辑态 TextBox 拿不到主题色。

#### 方案
给 `DataGridTextColumn` 补 `EditingElementStyle` 指向已有 `SettingsTextBoxStyle`（绑定 `BgSettingsInput`），深色编辑框跟着主题。或更彻底加一个全局 `<Style TargetType="TextBox">` 兜底所有漏网 TextBox。

#### 落点
`SettingsWindow.xaml:330-351` `SettingsDataGridStyle` 补 `EditingElementStyle`。

#### 代价
小。

---

## 问题3：网页搜索界面空间太少，只能显示一个引擎

### 现象
网页搜索设置页 DataGrid 实际只露出约一数据行，第 2/3 个引擎要滚才看见。

### 根因
非数据限制（预设 3 引擎，可加任意多），是垂直布局。Web 标签页 Grid 四行：
- Row0 标题（约 60px）
- Row1 DataGrid（`*`，被夹挤）
- Row2 三个 52px 大卡片按钮（约 76px）
- Row3 "在线联想"说明段（约 120px）

窗口高 480 扣完外层 margin、底部按钮、状态行，DataGrid 实际可见高度常不足两行（列头 30px + 一行 28-32px），第 2/3 行需滚动。

### 方案
压缩占位的，给 DataGrid 让高度：
- 三个 52px 大卡片按钮改紧凑：高度降到 36 左右，或改 DataGrid 上方一行小按钮（添加/删除/恢复）+ 图标，不占三整行。
- "在线联想"段收紧：说明文字从两段压成一行，或移到标题旁的小提示图标（Tooltip 展开），省掉约 120px。

两处一压，DataGrid 多出约 100px，稳定显示 3-4 行（=预设引擎数），不用滚全看见。

#### 落点
纯 XAML 布局调整，`SettingsWindow.xaml:617-701`，不动逻辑。

#### 代价
小。

---

## 问题4：快速访问界面又细又长（重点）

两个子问题，共用"折叠 + 分组 + 富展示"原则。

### 原则
Prism 差异化是"安静且薄"，不是"功能多"。折叠解决"太长"，富展示解决"看不清"，但都在"薄"框架内，不引第三方控件库，用 WPF 原生 `Expander` / `GroupStyle`。

### 4A：动作快捷键选择"看不清是什么功能"

#### 现象
"增加功能"的下拉里每个动作只显一个中文 Label（如"复制路径至剪贴板"），无图标、无描述、无分类，靠字面猜。

#### 根因
`ActionHotkeyCatalog`（`catalog.cs:28`）Entry 仅 3 字段：`Id` / `Label` / `Kinds`。界面 ComboBox `DisplayMemberPath="Label"`（`xaml:514`）只显 Label 一个字符串。15 个动作平铺无分组（`ActionHotkeyKinds` 是适用目标位标志，非功能分类）。

#### 方案
Catalog 加 Icon + Category + Description，下拉分组富展示：
- `ActionHotkeyCatalog.cs` Entry 加三字段：`Category`（文件操作/剪贴板/打开方式/删除/压缩/应用）、`Icon`（Segoe 图标字符或 pack URI）、`Description`（一行短说明）。
- 下拉 ComboBox 用 `GroupStyle` 按 Category 分组（section header）+ DataTemplate 显 `图标 + Label + Description 灰色小字`，一行看全"是什么 + 干啥"。
- 15 个动作分 5-6 类，分组后扫一眼定位。

#### 落点
- `src/Prism/Models/ActionHotkeyCatalog.cs` 加字段。
- `SettingsWindow.xaml:499-533` ComboBox 改分组 DataTemplate。
- `ActionHotkeyEditItem.cs` 透传新字段。

#### 代价
中。纯前端，不碰 broker/协议。

### 4B：别名太长没折叠

#### 现象
别名管理界面 `ItemsControl` 平铺，上限 2000 条，无分组无折叠无虚拟化，整页 ScrollViewer 滚。每行 `词表 / 路径 / 删除`，app 别名路径列显示 `.lnk` 全路径（与问题1同源）。

#### 方案
按目标类型分组的折叠列表 + 虚拟化：
- `ItemsControl` 换 `ListView` + `VirtualizingStackPanel`（超几十条不卡）。
- 按目标 Kind 分组（`application` / `file` / `directory`），`GroupStyle` 出 section header（如"应用程序 (12)"）+ `Expander` 折叠，默认折叠，点开看组内。
- 每行富化：加目标图标（应用/文件/文件夹），路径列去全路径只显文件名 + Tooltip 全路径（app 别名配合问题1 resolve 后显 exe 文件名，不再是 lnk 全路径）。

#### 落点
- `SettingsWindow.xaml:576-612` 别名区改 ListView + GroupStyle。
- `SettingsViewModel.cs:342` `AliasEntries` 加分组视图（`CollectionViewSource` 按 `Target.Kind` 分组）。

#### 代价
中。纯前端。

### 4A+4B 共用：整页折叠结构

两个区都在"快速访问"这一个 ScrollViewer 里挤着。整页加一层折叠：
- "动作快捷键"（标题 + Expander）默认展开
- "文件别名"（标题 + Expander）默认折叠
- 暂存区设置同理可折叠

用 WPF 原生 `Expander`（`Header` 标题 + 计数徽章），不引第三方。配合分组后，"快速访问"页成三个可折叠 section，不再一条长龙。

---

## 问题5："昨天干了啥"视图（新功能，已定方案，暂不实施）

### 需求
第二天开机呼出 Prism，打"昨天"确认前一天打开了哪些文件。

### 数据源
`%AppData%\Microsoft\Windows\Recent` 的 .lnk，Windows 每打开文件自动写入，.lnk 修改时间 ≈ 最后打开时间。跨重启不丢、零侵入。Windows 自己在记，Prism 只读不抄。

### 方案：打"昨天"出合成行（方案一）
打 `昨天` / `yesterday` → `ApplySearchResponse` 注入一行 `Kind="recent"`，Title=`昨天 · N 个文件`，Subtitle 串前几个文件名，列表行直接展示昨天文件，Enter 直接打开（**不载入暂存区**——搜索直接出列表即确认，载入暂存是多余）。

复用 workset 召回注入机制（`SearchViewModel.cs:1161-1163` 同位 `list.Insert(0, ...)`），零新 UI 控件，零新界面。

### "昨天"边界
自然日昨天（`DateTime.Today.AddDays(-1)` 到 `DateTime.Today`），不取滚动 24h。贴合"第二天开机确认前一天"直觉。

### 数据源读取（两阶路）
- **MVP（前端直读）**：前端 `Directory.GetFiles(*.lnk)` + `File.GetLastWriteTimeUtc` 过滤昨天区间 + 降序排（仿 `FaviconCache.cs:311-314` 排序先例）。显示名靠 .lnk 文件名去后缀，对 Recent 够准。不碰 Rust/IPC，改动最小。
- **升级路（Rust resolve）**：加 `resolve_lnk` IPC 命令（与问题1共用），resolve .lnk→真实路径 + `Path::exists()` 过滤失效。前端只改数据源调用点，合成行/Enter 逻辑不变。

### 落点
- 新建 `src/Prism/Services/RecentItemsQuery.cs`（取 Recent 目录 + 时间过滤 + 排序）。
- `SearchViewModel.cs` 加 `BuildRecentRecallRow(query)`，仿 `BuildWorksetRecallRow`（:1274），命中 `"昨天"` / `"yesterday"` 注入合成行。
- `ExecuteSelectedCoreAsync`（:501 workset 分支旁）加 `Kind=="recent"` 分支，Enter 走 `ExecuteAsync` + `HideRequested`（直接打开，仿 file 分支）。

### 代价
MVP 小（纯前端）；升级路中（+ IPC，与问题1共享 `resolve_lnk`）。

### 不碰
staging.json schema、workset 结构、新 UI 控件。

---

## 已否决设想：拖入 + 载入暂存复盘

会话最初提出"拖入外部文档到工作集 + 开机确认昨天"，讨论后拆解：

- **拖入**：可行，是拖出（已完成）的天然对称，基础设施齐（StagingStrip 挂 AllowDrop+Drop 即可，`StagingArea.Add` 现成）。**保留为可选后续，未排期。**
- **载入暂存复盘"昨天"**：否决。Recent Items 已持久化，按时间过滤即可展示，再复制进 staging.json 丢时间戳（staging 无时间字段）、和主动命名的工作集混淆语义、重复存储。"保存在暂存区"是多余拷贝。改为问题5的"搜索直接出列表"。

"昨天"复盘的正确形态是**一次搜索**（打"昨天"出列表），不是**一处存储**。

---

## 优先级建议（按痛点 × 代价）

| 序 | 问题 | 痛点 | 代价 | 说明 |
|----|------|------|------|------|
| 1 | 问题4 | 最重 | 中 | 折叠+分组+富展示，纯前端 |
| 2 | 问题2B | 中 | 极小 | 补 DataGrid EditingElementStyle，立刻见效 |
| 3 | 问题2A | 中 | 小 | 托盘菜单换 WPF，根治风格不一致 |
| 4 | 问题3 | 中 | 小 | Web 引擎布局压缩，纯 XAML |
| 5 | 问题1 | 高 | 中 | 别名 resolve exe，碰 IPC |
| 6 | 问题5 | 独立 | 小~中 | "昨天"视图，MVP 纯前端，升级与问题1共享 resolve_lnk |

各问题互相独立，可拆批实施。问题1与问题5的 Rust 侧 `resolve_lnk` 命令共享，宜同批。
