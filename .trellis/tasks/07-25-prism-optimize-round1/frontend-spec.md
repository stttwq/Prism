# frontend-spec.md — Prism 第一轮优化·前端施工规范

> 本文档指导一个独立 AI（Codex / ChatGPT 等）完成 Prism 前端（WPF）第一轮优化的四个工作项。
> 阅读顺序：先读 §1–§3 建立上下文，再按 §4 的工作项顺序施工，每项完成后跑 §6 验证。
> **禁止事项见 §7，必须全程遵守。**

## 1. 项目上下文

- 仓库根：`D:\LS\DM\Listary`。前端项目：`src/Prism`（.NET 8 WPF，`net8.0-windows`，无 MVVM 框架，INotifyPropertyChanged 手写）。
- 后端：`src/prism-core`（Rust，命名管道 JSON IPC）。本规范只涉及前端；后端进度字段（§4.4）由另一施工方提供，前端须**容忍字段缺失**。
- 语言约定：代码注释、UI 文案全部**简体中文**；标识符英文。
- 构建/验证命令（在仓库根执行）：
  ```
  dotnet build src/Prism -c Release
  ```
  构建必须 0 warning 新增、0 error。

### 关键文件地图

| 文件 | 职责 |
|---|---|
| `src/Prism/Models/AppState.cs` | 全局 UI 状态（Mode/Query/Results/SelectedIndex/StatusMessage…），INPC |
| `src/Prism/Models/SearchResult.cs` | 结果模型：`Kind`（"file"/"folder"/"app"/"web"/"more"）、`Title`、`Subtitle`、`ExecuteId`、`MatchSpans`、静态工厂 `SearchResult.More(query)` |
| `src/Prism/Models/ActionItem.cs` | 动作项：`Id`、`Label`、`IsSectionHeader` |
| `src/Prism/ViewModels/SearchViewModel.cs` | 防抖搜索、结果应用、动作面板逻辑（本次改 `ApplySearchResponse`/`ShowMoreAsync` 附近） |
| `src/Prism/Controls/ResultList.xaml(.cs)` | 结果列表控件（虚拟化 ListBox + code-behind 装饰）——本次改动最重的文件 |
| `src/Prism/Controls/ActionPanel.xaml(.cs)` | → 键动作面板（右键菜单复用它的动作数据，不复用它的 UI） |
| `src/Prism/Windows/SearchWindow.xaml(.cs)` | 主窗口，事件接线中枢（`Attach`/`ApplyState`/`OnHeaderKeyDown`） |
| `src/Prism/Services/PipeClient.cs` | IPC 客户端：`SearchAsync`、`GetActionsAsync(executeId)`、`RunActionAsync(executeId, actionId)`、`ExecuteAsync` |
| `src/Prism/Themes/Tokens.xaml`、`Tokens.Dark.xaml` | 深浅色资源字典；控件用 `DynamicResource` 取色 |

### 现有交互契约（不得破坏）

- 键盘：↑/↓ 移动、Enter 执行、Ctrl+Enter 定位、Ctrl+1..9 快捷执行、→ 进 ActionPanel（仅 file/folder）、← / Esc 退回、Esc 关窗。
- 选中保持：搜索响应后按 `ExecuteId` 匹配保留旧选中（`SearchViewModel.ApplySearchResponse` 中 `prevId` 逻辑）。
- 失焦自动隐藏（Pin 固定时除外）；隐藏时 `ReleaseIdleMemory()` 清列表和 IconCache——内存预算前端空闲 ≤30MB，**不要**增加常驻缓存。
- 状态流：VM 改 `AppState` → `PropertyChanged` → `SearchWindow.OnStateChanged` → `ApplyState()` 同步到控件。控件事件回流经 `SearchWindow` 写回 VM。**不要**绕过这条链直接让控件互相调用。

## 2. 工作项总览与顺序

| # | 工作项 | 改动文件 | 依赖 |
|---|---|---|---|
| W1 | "显示更多结果"实装 | SearchViewModel.cs, ResultList.xaml.cs | 无 |
| W2 | 结果列表差量刷新（去抖动） | ResultList.xaml.cs | 无（建议在 W1 后做） |
| W3 | 结果行右键菜单 | ResultList.xaml(.cs), SearchWindow.xaml.cs, Themes/* | 无 |
| W4 | 索引构建进度文案 | Services（响应模型）, SearchViewModel.cs | 后端字段（可先做，容忍缺失） |

按 W1→W2→W3→W4 顺序施工，每项独立可编译、可手测。

## 3. 现状缺陷精确描述（为什么改）

1. **more 行**：`ApplySearchResponse` 里只要 `resp.Items.Count > 0` 就恒追加 `SearchResult.More(query)` 行；单击它只是选中（ListBox 默认行为），只有双击（`OnDoubleClick`→`ItemInvoked`）或 Enter 才触发 `ShowMoreAsync()`（limit 100→1000 重搜）。用户感知："点了没反应 = 没实装"。
2. **抖动**：每次搜索响应 `_state.Results = list`（全新 List 实例）→ `ApplyState` 里 `ReferenceEquals` 不等 → `ResultList.Items` setter 重设 `List.ItemsSource` → 全部容器重建；`DecorateVisibleItems` 把 icon.Source 先置 null 再异步加载 → 图标闪空；`UpdateListHeight` 重算 → 高度跳。删一个字母时肉眼可见"颤一下"。
3. **右键**：`ResultList.xaml` 只挂了 `MouseDoubleClick`，无任何右键处理，无 ContextMenu。
4. **索引进度**：`is_indexing=true` 时仅有静态文案「索引加载中…」，首装全盘构建分钟级，用户无进度感知。

## 4. 工作项施工细则

### W1 "显示更多结果"实装

**改动 A — 条件显示**（`SearchViewModel.ApplySearchResponse`）：

```csharp
// 现状：if (resp.Items.Count > 0) list.Add(SearchResult.More(query));
// 改为：仅当返回条数打满当前请求上限，才可能还有更多
if (resp.Items.Count > 0 && resp.Items.Count >= max)
    list.Add(SearchResult.More(query));
```

`max` 已是该方法参数。效果：结果不足 limit 时无 more 行；ShowMore 后（max=1000）返回 <1000 条时 more 行自然消失。

**改动 B — 单击触发**（`ResultList.xaml.cs`）：

- 在 ListBox 上增加 `PreviewMouseLeftButtonUp` 处理（xaml 挂事件或构造器 `+=` 均可）。
- 通过 `e.OriginalSource` 向上找 `ListBoxItem`（可复用/仿照现有 `FindDescendant` 写一个 `FindAncestor<T>`），取其 DataContext；若是 `SearchResult { Kind: "more" }`，触发 `ItemInvoked?.Invoke(r)`，`e.Handled = false`（保留选中行为即可，不需要 Handled）。
- 非 more 行：不改行为（单击选中、双击执行）。
- `SearchWindow` 侧无需改：现有 `Results.ItemInvoked` 订阅会同步 SelectedIndex 并调 `ExecuteSelectedAsync()`，其中 `Kind=="more"` 分支已调 `ShowMoreAsync()`。

**注意**：`OnQueryChanged` 里每次输入会把 `_resultLimit` 重置回 100，这是既有行为，保留。

**验收**：搜一个大结果词（如 "a"），滚到底单击"显示更多结果"→ 列表增到最多 1000 条且 more 行消失（若确实 <1000）；搜一个只有几条结果的词 → 无 more 行。

### W2 结果列表差量刷新

**目标**：`Items` setter 不再整表重设 `ItemsSource`，改为内部 `ObservableCollection<SearchResult>` 原位 diff。**对外 API（`Items` 属性、`SelectedIndex`、事件）签名不变**，`SearchViewModel`/`AppState`/`SearchWindow` 一律不改（`ApplyState` 中 `ReferenceEquals(Results.Items, state.Results)` 判断仍成立：`Items` getter 继续返回外部传入的 `IReadOnlyList`，内部 OC 是私有实现细节）。

**实现要点**（全部在 `ResultList.xaml.cs`）：

1. 新增字段 `private readonly ObservableCollection<SearchResult> _oc = new();`。构造器里 `List.ItemsSource = _oc;` 一次性绑定，**此后永不重设 ItemsSource**（含 `Items = Array.Empty<>` 的清空路径——用 `_oc.Clear()`）。
2. `Items` setter 改为：保存 `_items = next`（getter 用），然后对 `_oc` 做键控 diff：
   - 行键：`(Kind, ExecuteId, Title)` 三元组（与 `SearchWindow.IndexOfResult` 的等价性一致）。
   - 算法从简即可（结果 ≤1001 条）：逐索引 i 比较 `_oc[i]` 与 `next[i]` 的键，相同则跳过（**不要**替换元素实例），不同则 `_oc[i] = next[i]`；`next` 更长则 `_oc.Add`，更短则从尾部 `RemoveAt`。O(n) 足够，不必做 LCS。
   - 键相同但 `MatchSpans` 不同的行：不替换实例也没关系——高亮由 `DecorateVisibleItems()` 全量覆盖，但**必须**把 `_items` 中新实例的引用传给装饰逻辑。最稳妥做法：键相同但非 `ReferenceEquals` 时仍执行 `_oc[i] = next[i]`，同时在装饰阶段避免清图标（见第 4 点）。**采用后者**：diff 只用于减少增删，替换保持数据最新。
   - 说明：`_oc[i] = next[i]` 触发 Replace，WPF Recycling 模式下容器复用、不闪整表；真正要避免的是 `ItemsSource` 重设与图标清空。
3. `_syncing` 保护逻辑保留：diff 过程中置 `_syncing = true`，防止 `OnSelectionChanged` 回流。
4. **图标不闪**（`DecorateVisibleItems`）：现状已有 `if (!Equals(icon.Tag as string, path))` 才清 Source 的判断——Replace 后容器复用时 `icon.Tag` 仍是旧值，路径相同的行不会清空重载，天然满足；确认此分支未被破坏即可。more 行与 web 行分支照旧。
5. **高度不跳**（`UpdateListHeight`）：条数不变时（`rows` 与 `StatusText` 均未变导致 h 相同），跳过对 `Height/MinHeight/List.Height` 的赋值（先算 h，与当前值比较，相同 return）。
6. 清空路径（`ReleaseIdleMemory` 调 `Items = Array.Empty<SearchResult>()`）：`_oc.Clear()` 后其余逻辑照旧，确保隐藏后内存回收行为不变。

**验收**：输入 `abc` 出结果后退格成 `ab`——共有行图标不闪空、列表不整体白一下、高度无跳动；↑↓ 选中 + 退格后选中保持（ExecuteId 匹配）逻辑不回归；双击执行、Ctrl+数字均正常。

### W3 结果行右键菜单

**交互定义**：

- 右键 `Kind=="file"` 或 `"folder"` 且 `ExecuteId` 非空的行：先把该行设为选中，再在鼠标位置弹出 ContextMenu。
- 菜单内容 = 后端 `GetActionsAsync(executeId)` 返回的动作列表（与 → 键 ActionPanel 同源）：`IsSectionHeader==true` 的项渲染为不可点的分组头（或 `Separator` + 灰字标题，二选一，从简用 Separator 即可）；普通项点击后执行动作。
- 右键 `app`/`web`/`more` 行：不弹菜单，事件不处理（让其冒泡等同无事发生）。
- 菜单打开期间窗口失焦不得触发自动隐藏（ContextMenu 是独立 popup，会抢焦点——见实现要点 4）。

**实现要点**：

1. `ResultList.xaml.cs`：挂 `MouseRightButtonUp`；找到命中行（同 W1 的 FindAncestor），若可弹（file/folder + ExecuteId 非空）：`List.SelectedIndex = 该行索引`（走 `_syncing=false` 正常路径，让 SelectedIndexChanged 回流到 VM），然后触发新事件 `public event Action<SearchResult>? ContextMenuRequested;`，`e.Handled = true`。ResultList **不负责**构建菜单（保持控件哑）。
2. `SearchWindow.xaml.cs` 订阅 `Results.ContextMenuRequested += async r => …`：
   - 调 `_vm` 暴露的新方法获取动作（见第 3 点），组装 `ContextMenu`：`MenuItem.Header = action.Label`；SectionHeader → `Separator`。
   - `menu.PlacementTarget = Results; menu.Placement = PlacementMode.MousePoint; menu.IsOpen = true;`
   - `MenuItem.Click` → 调 VM 执行动作（见第 3 点），成功后现有 `HideRequested` 机制会隐藏窗口。
3. `SearchViewModel.cs` 增加两个公开方法（**复用现有私有逻辑，不复制**）：
   - `Task<IReadOnlyList<ActionItem>> GetActionsForAsync(SearchResult item)`：管道未连则 `StartAsync`，然后 `_pipe.GetActionsAsync(item.ExecuteId)`；异常时写 `StatusMessage`（沿用「动作列表失败：」文案）并返回空表。
   - `Task RunActionOnAsync(SearchResult target, ActionItem action)`：校验非 SectionHeader、Id/ExecuteId 非空 → `_pipe.RunActionAsync` → 成功 `HideRequested?.Invoke()`；异常写「动作失败：」+ ShortMsg。然后把现有 `ExecuteActionAsync` 内部改为调用它（消除重复）。**注意**：这两个方法不依赖 `Mode==Actions`，右键路径不进入 Actions 模式、不动 `_queryBeforeActions`。
4. **失焦隐藏冲突**：弹菜单前置 `_ignoreDeactivate = true`（SearchWindow 已有该字段），`menu.Closed` 事件里延迟（复用现有 300ms DispatcherTimer 模式）恢复 false。菜单项点击执行成功会走 HideAnimated，本来就要隐藏，无冲突。
5. **主题**：ContextMenu/MenuItem 样式新建于 `Themes/` 下（或直接在 SearchWindow.xaml Resources），背景/前景/悬停色全部 `DynamicResource` 引用 Tokens 键（对照 `Tokens.xaml` 与 `Tokens.Dark.xaml` 现有键名，如卡片背景、TextTitle、TextSubtitle、TextMatch；不得写死 #RRGGBB——唯一例外是现有代码里已有的 fallback 模式）。深浅色切换（`ThemeWatcher.ThemeApplied`）后下次打开菜单自动取新值（DynamicResource 天然支持，无需手动刷新）。

**验收**：浅色与深色主题下分别：右键文件行出菜单，五个动作（打开/打开所在文件夹/复制/剪切/复制路径）逐个执行成功且行为与 ActionPanel 一致（复制后可粘贴、打开后窗口隐藏）；右键 web/more/app 行无菜单；菜单弹出时窗口不消失；Esc 关菜单后窗口仍在且可继续键入。

### W4 索引构建进度文案

**IPC 契约**（后端另行施工，前端先行兼容）：`results` 响应在 `is_indexing==true` 时**可能**带：

```json
{ "type": "results", "query": "...", "items": [...], "is_indexing": true,
  "index_progress": { "scanned": 123456, "total_estimate": 800000 } }
```

`total_estimate` 为 0 表示未知（首装无历史缓存）。字段整体可能缺失（旧后端）。

**实现要点**：

1. 响应模型（`PipeClient.cs` 或其模型所在文件，找到 `SearchResponse` 定义）：加可空属性 `IndexProgress`（含 `Scanned`/`TotalEstimate` long），JSON 名 `index_progress`/`scanned`/`total_estimate`，反序列化配置与现有字段一致（缺失 → null，不抛异常）。
2. `SearchViewModel` 文案（两处：`ApplySearchResponse` 的 `resp.IsIndexing` 分支、`PollUntilReadyAsync` 的等待分支）抽一个 `private static string IndexingMessage(SearchResponse resp, bool hasItems)`：
   - 无进度字段：沿用现文案（「索引加载中，正在补充文件结果…」/「索引加载中，请稍候…」）。
   - 有进度、`total_estimate>0`：`$"正在构建全盘索引 {scanned*100/total_estimate}%（{scanned:N0}/{total_estimate:N0}）…"`（百分比 clamp 到 99，避免尾期显示 100% 还在转）。
   - 有进度、`total_estimate==0`：`$"正在首次构建全盘索引，已扫描 {scanned:N0} 项…"`。
   - `hasItems==true` 时在句尾追加「当前结果可能不全」或沿用"正在补充文件结果"措辞，保持一句话、不换行。
3. 轮询节流：`PollUntilReadyAsync` 现有 500ms×30 次逻辑不改；仅文案替换。30 次超时后的兜底文案照旧。

**验收**：配合新后端删除 data 目录冷启动，搜索时状态行显示递增的扫描数/百分比，构建完成后文案消失、结果补全；配合**旧后端**（无字段）运行不异常、显示旧文案。

## 5. 通用编码规范

- 风格向现有文件看齐：文件级 `<summary>` 中文注释、`sealed class`、`Array.Empty<T>()`、`ConfigureAwait(true)`（UI 上下文）、异常兜底写 `StatusMessage` 而非弹窗/崩溃。
- 事件命名沿用现有模式（`XxxChanged`/`XxxInvoked`/`XxxRequested`），控件保持"哑控件 + 事件上抛"结构。
- 不引入任何新 NuGet 包。不使用 `dynamic`。不新增静态可变状态。
- 每个工作项一个独立 commit 粒度的改动面；**不要**顺手重构无关代码。

## 6. 每项完成后的验证清单

1. `dotnet build src/Prism -c Release` 零新增 warning。
2. 手测该项"验收"段全部条目。
3. 回归五连：呼出→键入→↑↓→Enter 打开→再呼出；→ 进 ActionPanel ← 退回；Esc 关窗；Pin 后失焦不隐藏；深浅色切换后界面无错色。
4. 隐藏窗口后（Pin 关闭状态失焦）任务管理器观察前端私有内存仍在 ~16MB 量级、无持续增长。

## 7. 禁止事项

- ❌ 不得修改后端 `src/prism-core` 任何文件（IPC 契约以 §4.4 为准，后端另行施工）。
- ❌ 不得修改 `AppState`/`SearchViewModel` 对外既有成员签名、`PanelMode` 枚举、键盘交互契约。
- ❌ 不得重设 `List.ItemsSource`（W2 完成后）或关闭虚拟化/Recycling。
- ❌ 不得引入 MVVM 框架、行为库或第三方控件库。
- ❌ 不得改动 `.trellis/`、安装脚本 `dist/`、`README`。
- ❌ 不得执行任何 git 提交（施工完成交回人工审查提交）。
