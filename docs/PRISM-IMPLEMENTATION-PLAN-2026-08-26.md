# Prism 修复实施方案（2026-08-26）

对应改进清单：`docs/PRISM-IMPROVEMENTS-2026-08-26.md`。状态：**方案已定稿，暂停实施。**

> 本文档基于 2026-08-26 探索核实，并经 2026-08-26 完善校准（行号、字形源、测试规约、措辞）。凡引用行号均为校准当时。

## 范围

| 问题 | 内容 | 批次 |
|------|------|------|
| 问题1 | 别名绑定 .lnk 而非真实 exe | 批次3（IPC） |
| 问题2A | 托盘右键菜单白色、字体不一致 | 批次2 |
| 问题2B | 深色主题下设置页 DataGrid 编辑框白底 | 批次1 |
| 问题3 | 网页搜索页 DataGrid 空间不足 | 批次1 |
| 问题4A | 动作快捷键下拉看不清功能 | 批次2 |
| 问题4B | 别名列表又细又长无折叠 | 批次2 |
| 问题5 | "昨天"视图 | **搁置，不实施** |

问题5 虽搁置，但其中的 `resolve_lnk` IPC 命令是问题1自身所需，照常实施。

## 探索核实结论（对改进清单的两处修正）

实施前已对全仓核实，改进清单中两处描述与实际不符，方案已按实际修正：

1. `copy_app_path` 不是独立 IPC 命令，而是 `run_action` 请求内的 action id 字符串（actions.rs:87 映射到 `ActionId::CopyAppPath`，shell.rs:356-367 在 STA worker 上 resolve）。因此 `resolve_lnk` 的正确模板不是"仿 copy_app_path 的用法"，而是 `WorkerMessage::ScanApps` 专用消息模式（shell.rs:79-83 定义、127-151 调度、245-267 worker_loop）。
2. broker 协议是 **JSON 行协议**（UTF-8、按行分隔，ipc.rs:1 模块注释），非 postcard。新增命令只需在 `Request`/`Response` 枚举加变体，前后端各一处。

其余关键事实（已逐一核实）：

- `BROKER_PROTOCOL = 1`（ipc.rs:35），C# 侧镜像 `ProtocolVersion = 1`（PipeClient.cs:26）。仓内先例：加 `root` 字段未 bump 版本（ipc.rs:4797-4798 注释 + `broker_search_root_is_optional_and_backward_compatible` 测试）。新增命令 additive，不 bump。
- `resolve_lnk_target`（apps.rs:277-291）为 `pub(crate)`，注释明示"COM 必须已由调用方线程初始化"——所以必须走 STA worker 专用消息，禁用裸 `spawn_blocking`（ResolveWindow 那种裸法会导致 `CoCreateInstance` 失败）。
- C# 侧无请求枚举，全部是 `new { type = "..." }` 匿名对象；双通道——query 通道 8s 超时（`QueryReadTimeout`，PipeClient.cs:480）/ action 通道 5min（`ActionReadTimeout`，PipeClient.cs:864）。别名族命令（AliasSet/Delete/List）均走 `_query` 通道 + 8s 超时（PipeClient.cs:726-753），`ResolveLnkAsync` 同模板。
- `ActionHotkeyCatalog`（ActionHotkeyCatalog.cs:28-45）现状是 15 项平铺无分类；broker 侧 `actions.rs` 已有权威 `label()`（136-154）与 `icon_glyph()`（119-133）——前端 catalog 加字段时须镜像这两个表，防前后端漂移（catalog 注释已点明"与 actions.rs 并集一致"）。
- 仓库无 Expander / GroupStyle / CollectionViewSource / ListView 使用先例；虚拟化先例为附加属性（Styles.xaml:98-99）；图标字形先例 ActionPanel.xaml（绑定 IconGlyph + IconFontFamily）与 SearchWindow.xaml.cs:704-715（代码构造）。
- 引擎 DataGrid 是全仓唯一 DataGrid（SettingsWindow.xaml:633-658），改动面封闭。
- 网页搜索页是独立 Grid 而非 ScrollViewer（SettingsWindow.xaml:617-624），压缩上下两行后高度直接让给 DataGrid。

## 总原则（影响最小化）

- 每个问题独立批次、独立提交，可单独回退；三批互相不依赖。
- IPC 只新增命令，不动任何既有 arm；持久化 schema 零改动（别名表 persistence.rs:138、动作快捷键设置只存 id+value、engines 均不动）。改进清单问题1的方案B（alias 加 `launch_path` 字段）明确不做。
- 全部新 UI 走 DynamicResource 令牌，明暗主题经 ThemeWatcher（ThemeWatcher.cs:73-116 整体换 Tokens 字典）自动适配。
- 所有失败路径回退到现状行为（resolve 失败绑 .lnk、分组异常退平铺、托盘菜单异常退 WinForms Renderer）。

---

## 批次1：纯 XAML，零逻辑（问题2B + 问题3）

### 1.1 问题2B：DataGrid 编辑框白底

- SettingsWindow.xaml:330-351 `SettingsDataGridStyle` 补 `CellStyle`（DataGridCell：透明背景、去默认焦点框、`IsSelected` 用 `BgItemSelected`），否则编辑选中格仍露系统色。
- 新增 `SettingsDataGridEditTextBoxStyle`：BasedOn 现有 `SettingsTextBoxStyle`（SettingsWindow.xaml:101-133，BgSettingsInput 底 + 主题边框），仅调 Padding 为紧凑值。在三个 DataGridTextColumn（648-656 关键词/名称/URL）上设 `EditingElementStyle`。
- 不加全局无 key `<Style TargetType="TextBox">`——仓内代码构造的 TextBox（别名对话框 SearchWindow.xaml.cs:860-915）未显式设样式，全局样式会波及它们，违反影响最小。

### 1.2 问题3：网页搜索页布局（SettingsWindow.xaml:617-701）

- `SettingsEngineCardButton`（65-98，仅本页使用）高度 52→36，内容改单行：图标字形 + 标题，原两行描述移入 ToolTip。
- "在线联想"段（687-700）压成一行：标题 + 复选框 + 一行短说明同行排布，第二段说明移入信息图标 ToolTip；分隔线保留。
- DataGrid 加 `MinHeight="150"` 锁定可见下限。
- 预期净让出约 100px，3 个预设引擎免滚全见。

### 1.3 批次1验证

构建通过；明暗两主题截图对比；双击引擎单元格编辑，深色下不再白框；三引擎 + 底部按钮 + 联想复选框全部可达。

---

## 批次2：前端交互（问题2A + 4A + 4B + 整页折叠）

### 2.1 问题2A：托盘菜单（TrayService.cs:38-49）

- 去掉 `ContextMenuStrip`；NotifyIcon 保留（管图标与左键）。`OnMouseClick`（72-76，现只滤 Left）扩 Right 分支，弹出 WPF `ContextMenu`。
- 菜单项 打开设置 / 重建索引 / 分隔线 / 退出，代码构造 + `SetResourceReference` 套 `PrismContextMenuStyle`（Styles.xaml:191-213）、`PrismContextMenuItemStyle`（215-265，含 AppFontFamily）、`PrismContextMenuSeparatorStyle`（267-276）。与搜索窗右键（SearchWindow.xaml.cs:691-767 同款手法）完全一致，零新样式。
- 焦点/收起保障（托盘弹 WPF 菜单的标准防"点外面不关闭"手法）：懒创建隐藏锚点 Window（0 尺寸、ShowInTaskbar=false、离屏显示一次），`SetForegroundWindow(锚点hwnd)` 后 `menu.Placement=MousePoint`、`StaysOpen=false`、`IsOpen=true`。Dispose（113-124）同步清理锚点窗与菜单。
- 事件仍经现有 `Raise()`（79-86）汇编派到 WPF Dispatcher；App.xaml.cs:174-178 装配点不变。
- 回退预案：真机若仍收起异常，单文件回退为 WinForms 菜单 + `ToolStripProfessionalRenderer` 取 WPF 令牌色 + AppFontFamily。

### 2.2 问题4A：动作快捷键下拉富展示

- `ActionHotkeyCatalog.cs:26` Entry 加三字段：`Category`（string）、`IconGlyph`（string）、`Description`（一行短说明）。仅 catalog 自身构造 Entry（28-45），设置存储不受影响（只存 id+value）。
- **字形与标签须镜像 broker 权威源** `actions.rs`：`label()`（actions.rs:136-154）给 Label，`icon_glyph()`（actions.rs:119-133）给 IconGlyph。即：copy/copy_path/copy_app_path/copy_to → `\u{E8C8}`；cut/move_to → `\u{E8C6}`；properties/app_properties → `\u{E946}`；open_with → `\u{E7B7}`；rename → `\u{E8AC}`；recycle/delete_permanent → `\u{E74D}`；zip → `\u{E7F8}`；open_folder → `\u{E8DA}`；run_as_admin → `\u{E7EF}`。前端不再自行查 Segoe 表，避免与 broker 漂移（catalog 注释已声明"与 actions.rs 并集一致"，此步兑现）。
- 分类方案（15 项分 5 组，Entries 按此重排）：
  - 文件操作：打开所在文件夹、复制到…、移动到…、重命名
  - 剪贴板：复制、剪切、复制路径至剪贴板、复制应用路径至剪贴板
  - 打开与运行：打开方式、以管理员身份运行
  - 属性：属性（文件/文件夹）、属性（应用）
  - 删除与压缩：移入回收站、永久删除、压缩为 ZIP
  - 注意"复制路径"与"复制应用路径"两个 Label 相同（均"复制路径至剪贴板"，镜像 actions.rs:141/151），靠 Description 区分（前者"复制文件/文件夹完整路径"，后者"复制应用 exe 完整路径，自动解析快捷方式"）。
- ComboBox（SettingsWindow.xaml:510-516）：删 `DisplayMemberPath`，改**单行紧凑** ItemTemplate = 字形 + Label（13px）+ Description（11px 灰色截断）。单行是为选中态在 170px 选框内不爆高（WPF ComboBox 选中态复用 ItemTemplate）。
- 分组：GroupStyle 出 section header（Category + 灰色小字）。数据侧在 `ActionHotkeyEditItem.AvailableActions` setter（ActionHotkeyEditItem.cs:41-50）同时产出 `ListCollectionView` + `PropertyGroupDescription("Category")`，XAML ItemsSource 改绑该视图。`RefreshActionChoices` 互斥逻辑（SettingsViewModel.cs:300-309）不动。
- 回退预案：ComboBox 内 GroupStyle 若真机键盘导航异常，退为平铺（按 Category 排序 + 行尾灰色分类小标签），ItemTemplate 复用。

### 2.3 问题4B：别名列表分组折叠 + 整页折叠

- `AliasEntry.cs:7-13` 加计算属性：`KindLabel`（应用程序/文件夹/文件）、`TargetFileName`（去全路径文件名）。记录签名不变。
- `SettingsViewModel.LoadAliasesAsync`（350-369）：填充前按 kind 序（应用→文件夹→文件）排序；取 `CollectionViewSource.GetDefaultView(AliasEntries)` 一次性挂 `PropertyGroupDescription("KindLabel")`（防重复添加，RemoveAliasAsync 删除经 ObservableCollection 通知自动更新组）。
- SettingsWindow.xaml:582-612 ItemsControl 换 `ListBox`：
  - `VirtualizingPanel.IsVirtualizing="True"` + `VirtualizationMode="Recycling"`（仿 Styles.xaml:98-99）。
  - GroupStyle.ContainerStyle 用 `Expander`，默认折叠，Header 显 `KindLabel (ItemCount)`（CollectionViewGroup.Name/ItemCount 直接可绑）。
  - 行模板 = kind 字形 + 词表（WordsText）+ 文件名（ToolTip 全路径，app 别名经批次3后显示 exe 文件名而非 .lnk 全路径）+ 删除按钮（RemoveAliasCommand 不变）。
  - 新增 ListBoxItem ItemContainerStyle：透明底、hover/选中用 `BgItemSelected`（仿 ResultListBoxStyle）。
- 虚拟化保真：别名 ListBox 加 `MaxHeight≈320` 内部滚动。快速访问页是单个 ScrollViewer（SettingsWindow.xaml:460-614），无界高度会废掉虚拟化，MaxHeight 是最小代价解。
- 整页折叠：各节套 `Expander`——动作快捷键（默认展开，Header 带计数徽章）、暂存区（542-572，默认折叠）、文件别名（默认折叠）；呼出快捷键节（470-490）不折叠。新增 `SettingsExpanderStyle`（E76C 字形旋转、TextTitle SemiBold 表头），存 SettingsWindow 资源。
- 空态提示 HasNoAliases（609-612）保留在别名 Expander 内。

### 2.4 批次2验证

托盘菜单两主题外观、点击外部收起、三项动作全通；动作快捷键增删改录、下拉互斥（RefreshActionChoices）仍正确；别名页 0 条/少量/大量（构造约 200 条）三态滚动流畅、组计数正确、删除即时刷新组；设置保存重开持久（存储内容与改前逐字节等价）。

---

## 批次3：IPC（问题1，方案A：身份与执行均指 exe）

### 3.1 Rust 侧（src/prism-core）

- `Request` 枚举（ipc.rs:73-166）加 `ResolveLnk { target: ActionTarget }`，serde tag 自动得 `type="resolve_lnk"`；`Response` 枚举（187-269）加 `LnkTarget { resolved: Option<String> }`。
- STA 调度仿 ScanApps 专用消息模式（shell.rs:79-83 定义、127-151 调度、245-267 worker_loop）：
  - `WorkerMessage::ResolveLnk { path: String, reply: mpsc::Sender<Option<String>> }`；
  - worker_loop 增 arm：`reply.send(crate::apps::resolve_lnk_target(&path))`（apps.rs:277-291 已 pub(crate)，COM 单元由 STA worker 线程保证）；
  - `ShellExecutor::resolve_lnk()` 包装 spawn_blocking + 通道回传，超时预算取快档 60s（对齐 `sta_wait_budget` FAST 常量，shell.rs:214）。
- `dispatch_non_search`（ipc.rs:1019-1244）加 arm：target.value 非 `.lnk` 结尾（不区分大小写）直接返回 `resolved: None`，免 STA 往返；`.lnk` 则调 `shell.resolve_lnk`。
- alias 既有路径一律不动：`alias.rs:108-161` set/lookup、`alias_search_hits`（ipc.rs:1250-1304）、`merge_alias_rows`、`target_path_is_bindable`（alias.rs:275-280，绝对 exe 路径天然通过校验）。
- 新增测试（对齐仓规 `alias_requests_decode` ipc.rs:4060 与 `search_results_always_serialize_a_typed_target` ipc.rs:4111 先例）：
  - `resolve_lnk_request_decodes`：`{"type":"resolve_lnk","target":{...}}` 解码为 `Request::ResolveLnk`。
  - `lnk_target_response_serializes`：`Response::LnkTarget { resolved: Some(...) }` 序列化含 `type:"lnk_target"`。
  - `dispatch_non_search` 对非 .lnk target 返回 `resolved: None`（纯逻辑，不依赖 COM，可正常跑）。
  - 真实 .lnk 解析需 COM 单元，按仓规（触发 Win32 的测试须 `#[ignore]`）处理。

### 3.2 C# 侧

- `PipeClient` 加 `ResolveLnkAsync(ActionTarget, ct)`：query 通道发送 `new { type = "resolve_lnk", target = TargetPayload(target) }`（与 AliasSetAsync 同通道同 8s 超时，PipeClient.cs:726-736 模板，`TargetPayload` 见 PipeClient.cs:820），解析 `lnk_target` 的 `resolved`；任何异常返回 null（失败语义由调用方兜底）。
- `ShowAliasDialogAsync`（SearchWindow.xaml.cs:830-947）：取得 aliasTarget 后（833 行），若 `Kind=="application"` 且 Value 以 `.lnk` 结尾（OrdinalIgnoreCase），先 resolve；成功且为绝对路径则替换为 exe 目标，失败/超时回退绑 .lnk（现行为兜底，覆盖 STA 被 300s 长任务占住致 8s 超时的场景）。对话框标题（882 行）改显 `Path.GetFileName(aliasTarget.Value)` 即 exe 名。
- 存量 .lnk 别名换绑（自动清理，防双条目）：prefill（849-858）先按解析后 exe 匹配；未中则回退匹配原 .lnk 值的同 kind 条目，预填其词表并记住该旧条目；保存成功后对旧 .lnk 条目 best-effort `AliasDeleteAsync`（失败忽略）。用户重开一次对话框即完成该应用的身份迁移，无需启动期迁移。存量 .lnk 别名不迁移也照常可用（ShellExecute 自解 .lnk），仅管理页显示旧路径。
- 执行路径零改动确认：exe 别名经 alias_search_hits 产出 `execute_id = exe`，`Request::Execute` → `ShellOperation::Open` → ShellExecuteExW 直接启 exe。丢 .lnk 参数/工作目录为方案A已接受代价（改进清单问题1原文），等真实反馈再走方案B升级路。
- `_vm.NotifyAliasesChanged()`（932 行）已有，缓存失效机制不变。

### 3.3 批次3验证

- Rust：`cargo test --workspace`（含新增三测试）；真实 .lnk 解析 `#[ignore]`。
- C#：`dotnet test`（Prism.Tests 全绿；若有用位置参数构造 `ActionHotkeyCatalog.Entry` 的测试，同步补新字段——批次2改动，此处一并回归）。
- 手工：开始菜单 app 设别名 → 管理页显示 exe 文件名而非 .lnk 全路径；Enter 直接启动；对既有 .lnk 绑定的 app 重开对话框 → 词表预填、保存后旧条目消失无重复；resolve 失败场景（断开 broker / 指向已删目标）回退绑 .lnk 且可用；文件/文件夹别名流程完全不变。

---

## 回归矩阵（每批完成后过一遍）

- 搜索与排序（含拼音、web、window 结果）。
- Enter 执行：file / folder / app / web / window 各验一。
- 右键动作面板全部动作（copy/cut/copy_path/properties/open_with/rename/copy_to/move_to/recycle/delete_permanent/zip/copy_app_path/app_properties/run_as_admin/open_folder）。
- 既有动作快捷键仍触发（设置文件只存 id+value，catalog 加字段不迁移不破坏）。
- 暂存区增删/召回；历史/frecency 不受影响。
- 文件与文件夹别名设置、命中、执行。
- 网页搜索三引擎增删改、联想开关。
- 重建索引；托盘左键呼出；设置保存重开持久。

## 风险与回退汇总

| 风险 | 缓解 |
|------|------|
| 托盘 WPF 菜单点外不收起 | 锚点窗 + SetForegroundWindow 已内置；仍异常则回退 WinForms Renderer 方案（单文件回退） |
| ComboBox GroupStyle 键盘导航异常 | 退平铺 + 行尾分类标签 |
| 页面 ScrollViewer 内分组虚拟化失效 | ListBox MaxHeight 有界滚动 |
| resolve 超时（STA 忙于 300s 长任务） | 8s 超时回退绑 .lnk（现状行为） |
| 存量 .lnk 别名与新 exe 别名双条目 | 对话框重存时自动换绑 + 清理旧条目（best-effort） |
| 新旧前后端混跑 | 新命令 additive 不 bump 协议；旧 broker 收到 resolve_lnk 返回单条错误不破坏连接，前端异常即回退绑 .lnk |

## 收尾（实施时）

批次3全绿后：`cargo build --release` + `dotnet build -c Release` + ISCC（`D:\LS\Setup 7`）重建安装包，沿用既有发布流程；分批独立提交，均不推送远端。问题5继续搁置，其升级路（Recent 直读 + resolve 过滤失效项）将来与 `resolve_lnk` 复用时直接引用本文档批次3。
