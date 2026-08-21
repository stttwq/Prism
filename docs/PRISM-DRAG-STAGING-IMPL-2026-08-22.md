# Prism 拖拽/暂存区/工作集 具体实施计划

> **文档性质：实施计划（任务级分解）。** 由 `PRISM-DRAG-STAGING-PLAN-2026-08-21.md`（方向性规划）与两份
> 设想探索（`E:\下载\拖拽与暂存区.md`、`E:\下载\暂存区与工作集探索.md`）收敛为可逐项执行、逐项提交的
> 任务清单。影响面原则沿用方向规划第 0 节：**全部改动落在 `src/Prism/`（WPF 前端）**，broker、IPC
> 协议、`prism-core`、索引服务一概不动。
>
> 日期：2026-08-22
> 执行纪律：每任务一测一提交（`dotnet test` 全绿 + 前端部署 + `scripts/ui-shot.ps1` 真机渲染验证）。
> 与方向规划的差异点（经代码核实后的修正）见各任务备注。

## 任务清单

### 任务 0：本计划入库（docs 提交）

### 任务 1：阶段一 拖出——结果列表 → 任意应用

改动 3 处：

1. **失焦闸**：`SearchWindowFocusPolicy.ShouldHide` 增第 6 参 `isDragging`（任一为真即不隐藏）。
   `SearchWindow` 增 `_isDragging` 字段，`OnDeactivated` / `OnForegroundChanged` 两个调用点补实参；
   `OnForegroundChanged` 的早退守卫同步纳入。既有 5 参锚测试（`SearchViewModelTests` L282-287）更新为
   6 参，新增锚：`isDragging=true` 时其余五参任意组合均不隐藏。
2. **拖源**：`ResultList.xaml.cs` 新增（约 60 行）：
   - `PreviewMouseLeftButtonDown` 记录起点与命中行索引，**不置 Handled**（保单击选中）；
   - `MouseMove` 左键按下且位移超过 `SystemParameters.Minimum*DragDistance` 时起拖；
   - 行解析走既有 `ItemAt(index)`（容器 DataContext 可能是旧实例，不可用）；
   - 载荷 `DataObject.SetFileDropList`，`DragDropEffects.Copy`，不放行 Move；
   - 放行条件：`Kind ∈ {file, folder, app}` 且 `ExecuteId` 非空且路径存在（File/Directory.Exists）；
     `web`/`window`/`more` 一律不放行；
   - 三条纪律：起拖前快照路径；`catch (COMException)` 不外冒；`finally` 里复位 `_isDragging` 并
     通知窗口（`DragOutStarted` / `DragOutFinished` 事件）。
3. **拖拽结束后的隐藏复核**：`DoDragDrop` 嵌套消息循环期间 `Deactivated`/前台变更已被 `isDragging`
   挡下，松手后不会再有新事件——若不在结束时补判，Prism 拖到别的应用上松手后会残留可见（违反方向
   规划 1.6 验收第 5 条）。`DragOutFinished` 处理器在复位后复核：窗口非激活且 `ShouldHide` 为真才
   `HideAnimated()`（拖回 Prism 自身取消/落点在本窗口时不隐藏）。

明确不做（方向规划 1.5）：拖入、多选拖拽、入场动画。
真机验收说明：OLE 拖拽无法脚本化模拟，真机验证 = 部署新前端后 ui-shot 渲染无异常 + frontend.log
无新增错误；实际拖拽验收（资源管理器/邮件/聊天）留给用户日常使用。

### 任务 2：阶段二 暂存区

新增/改动 6 处：

1. **`Models/StagingArea.cs`（新）**：
   - `record StagingItem(string Path, string? Workset)`（`Workset` 为工作集名标记，阶段二恒 null）；
   - `StagingPolicy`（纯静态，单测锚）：`Add`（去重 → 容量满时 LRU 挤掉最早未标记项；全满且全是
     标记项 → 拒绝）、`ClearUnmarked`（只清未标记）、`LoadWorkset`（阶段三用）；
   - `StagingArea`（有状态）：`Items` + `Changed` 事件 + `Capacity`（由 App 从设置注入），
     变更后由订阅方落盘。独立于 `AppState`（`ReleaseIdleMemory`/`ResetForShow` 每次隐藏都清
     `AppState.Results`，暂存区必须活过呼出周期）。
2. **`Services/StagingStore.cs`（新）**：独立 `staging.json`（**不进 settings.json**——设置页
   "载入快照、保存全量写盘"会把搜索窗侧写入的暂存条目整体覆盖掉）。原子写复用 SettingsStore 模式
   （`.tmp` + `File.Move(overwrite)`），损坏回落空态。schema 一次到位：`items` + `worksets`（阶段二
   恒空表）+ `activeWorkset`（阶段二恒 null）。载入防御：空路径条目丢弃、条目/工作集/路径数设上限。
3. **`Settings`**：`StagingCapacity`（默认 5，界 1..32）+ `StagingAddHotkey`（默认 `Ctrl+D`，空串=
   禁用）。`SettingsStore.Validate` 增：容量界检查；快捷键可解析、不落保留集、不与 `ActionHotkeys`
   撞键。`Load` 归一化：非法快捷键静默置空、容量越界回落 5（与 `NormalizeActionHotkeys` 同纪律）。
4. **设置页**：`SettingsViewModel` 增 `StagingCapacityText`（字符串，保存时解析报错）与
   `StagingAddHotkey`；保存校验规范化后落盘。`SettingsWindow.xaml` 快速访问 tab 增「暂存区」节：
   容量输入 + `HotkeyRecorderBox` + 说明文字。
5. **快捷键接入**：`SearchWindow.SetStagingAddHotkey(string)`（启动与保存后由 App 调用，解析为
   (Key, Mods) 缓存）；`OnHeaderKeyDown` 在动作快捷键匹配之后、导航键 switch 之前查该绑定；适用性
   预检（Results 模式 + 选中行 file/folder/app + ExecuteId 非空）不通过则不吞键。**不塞进
   `ActionHotkeys` 字典**（那是 broker 动作面板的前端镜像，有防漂移锚测试）。
6. **`Controls/StagingStrip.xaml`（新）**：插在 `SearchWindow.xaml` 的 `ScopeNotice` 与 `Divider`
   之间（窗口 `SizeToContent="Height"` 自动跟随高度）。空态整体 `Collapsed`；有文件时一行高：
   左侧计数 + 文件名 chips 横排（截断、悬停 tooltip 全路径、悬停出 × 删除）+「清」按钮（只清未
   标记）。chip 点击 = 打开文件（`Process.Start` UseShellExecute，不经 broker、不记历史）。chip 是
   第二拖源（复用任务 1 的阈值+快照+Copy 语义；拖出不移除引用）。**不接 `AnimatePanelHeight`**
   （那条路径是 PanelHost 逐帧栅格化敏感区）。[存]/工作集入口按钮留任务 3（工作集存储不存在前无
   处可存——方向规划 2.3 该按钮与阶段三强耦合，此为顺序修正非裁剪）。

App 接线：启动建 `StagingStore` + `StagingArea`（载入 staging.json、容量/快捷键来自设置），
`EnsureSearchWindow` 注入，`ApplySettings` 热更新容量与快捷键，`Changed` 落盘。

### 任务 3：阶段三 工作集

1. **模型与存储**：`record WorksetEntry(string Name, string? Note, IReadOnlyList<string> Paths)`
   进 `StagingStore`（同一文件 `worksets` 表 + `activeWorkset`）。工作集间路径可重叠（两表各自独立，
   不去重）。
2. **`StagingArea` 工作集操作**（策略纯函数 + 单测锚）：
   - `SaveAsWorkset(name, note)`：当前全部条目路径（保序）upsert 进工作集表（同名覆盖，UI 层先确
     认），全部条目改标 `name`，`activeWorkset = name`；
   - `LoadWorkset(name)`：移除所有带标记条目（档案不动）→ 未标记保留（同路径改标不重复加）→ 工
     作集路径逐个加入并标记 → `activeWorkset = name`；
   - `DeleteWorkset(name)`：删表项，对应标记条目改为未标记（不删条目——文件留在暂存区，删不删由
     用户「清」决定）；
   - 零摩擦加入：`activeWorkset` 非空时 `Add` 自动带标并同步追加进该工作集表；删除带标条目同步从
     工作集表移除该路径（「工作集不锁死，加减文件更新落盘」）。
3. **名字召回（合成行，零协议改动）**：`SearchViewModel.ApplySearchResponse` 前置注入——查询与工
   作集名完全一致（OrdinalIgnoreCase）时在列表头插入 `Kind="workset"` 合成行（Title=名，Subtitle=
   `工作集 · N 个文件 · Enter 载入`，稳定 `RowKey`）。`ExecuteSelectedAsync` 增 workset 分支：触发
   `WorksetRecallRequested(name)` 事件，由 `SearchWindow` 载入暂存区，**不隐藏窗口**（用户载入后
   通常要拖出）。`RevealSelectedAsync` 对 workset 早退。`ResultList` 装饰：workset 行用内置文件夹
   矢量图标（`MoreIcon` 同款 DrawingImage 手法）；右键菜单既有 Kind 过滤天然排除；拖拽放行条件天然
   排除。
4. **条带 UI 扩展**：带标 chip 显示小圆点角标；条带头部在 `activeWorkset` 非空时显示「暂存区 · 名
   字」（tooltip=备注）；「存」按钮（起名+备注对话框，同名覆盖需确认，复用 `ShowAliasDialogAsync`
   的对话框手法）；工作集入口按钮展开条带下方第二行：工作集横排（名字+文件数，tooltip 备注，
   [载]/[删]按钮，**整行即拖源=整组 FileDropList**，拖前过滤不存在路径）。

### 任务 4：总测试

`dotnet test` 全量 + `cargo test` 全量（应不受影响，跑一遍确认）+ 前端部署三进程 + ui-shot 真机
渲染 + frontend.log 复核。

### 任务 5：全仓从头重审

摒弃既有审计文档结论，从零重读整个仓库（重点 `src/Prism/` 全部新改路径 + 交互面），产出新问题清
单；修复其中风险等级为中、高的项；修复后再次从零复检，直至无中/高问题；完整提交。

## 关键决策记录（与方向规划/设想文档的差异）

| 点 | 方向规划/设想 | 本计划 | 理由 |
|---|---|---|---|
| 拖拽结束后的隐藏 | 未提及（1.6 验收要求"松手后恢复正常"） | `DragOutFinished` 复核非激活即按策略隐藏 | 嵌套循环期间激活变更被 isDragging 挡下且不会重发，不补判则拖后残留可见 |
| 「存」按钮时点 | 阶段二规格含"存为工作集" | 随阶段三落地 | 工作集存储阶段二尚不存在，按钮无处可存 |
| 工作集入口形态 | 设想文档为右侧弹层 | 条带下方可展开第二行 | WPF Popup 交互内容有焦点/失焦隐藏守卫纠缠；展开行零新守卫，风险最低 |
| 备注编辑 | "可加备注" | 创建时填写 + tooltip 展示 | 已覆盖备注的核心价值（文件清单说不清的状态/上下文）；独立编辑器留待真实需求 |
| chip 打开方式 | 未指定 | `Process.Start` UseShellExecute，不经 broker | 一行零依赖；暂存区取用不是搜索挑选，不参与 frecency |
| 删除工作集后其标记条目 | 未指定 | 改为未标记、不删条目 | 最小惊扰；删除条目有显式「清」/× |

## 关联文档

- `PRISM-DRAG-STAGING-PLAN-2026-08-21.md` — 方向性规划（本文件的任务 1-3 对其阶段一/二/三）
- `E:\下载\拖拽与暂存区.md`、`E:\下载\暂存区与工作集探索.md` — 设想探索（本文档已吸收）
