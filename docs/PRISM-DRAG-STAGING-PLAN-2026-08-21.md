# Prism 拖拽与暂存区实施规划

> **文档性质:实施计划。** 由两份设想探索（`PRISM-STAGING-DRAG-EXPLORATION-2026-08-21.md`、
> `PRISM-STAGING-WORKSET-EXPLORATION-2026-08-21.md`）收敛而来，按"影响面最小"重排优先级与技术选型。
>
> 日期：2026-08-21
> 前置约定：先做拖拽并确保好用，再做暂存区，工作集后置。三者最终都要。

## 0. 影响面判定

拖拽、暂存区、工作集三件事全部落在 `src/Prism/`（WPF 前端）。broker 不动、IPC 协议不动、
`prism-core` 不动、索引与服务不动。

因此弃用设想文档里"工作集挂 history 旁标注表"的方案——那会拉出 Rust 侧 + 协议改动，
与"影响降到最低"直接冲突。工作集改为纯前端落盘，名字召回靠前端合成行注入
（`SearchResult.More` 已是前端合成行的先例）。

## 1. 阶段一：拖出（结果列表 → 任意应用）

改动 4 处，其中 3 处是新增文件/新增成员，只有 1.1 触及既有逻辑。

### 1.1 失焦闸（唯一一处改既有纯逻辑）

`src/Prism/ViewModels/SearchWindowFocusPolicy.cs` 的 `ShouldHide()` 增加第 6 个参数
`isDragging`，语义与其余守卫一致（任一为真即不隐藏）。

选这个插入点的原因：两条隐藏路径——`SearchWindow.OnDeactivated` 与 Win32 前台钩子
`OnForegroundChanged`——都已经过这同一个闸，改一个纯函数即同时覆盖两条，不需要在两处
分别加判断。这也是仓库既有的守卫模式（`_ignoreDeactivate` / `_contextMenuOpen` /
`_contextMenuActionPending` / `IsPinned` / `_hiding`）。

`SearchWindow` 侧新增 `_isDragging` 字段，两处调用点补上实参。

**锚测试**：`isDragging = true` 时，其余五个参数的任意组合都不隐藏。

### 1.2 拖源

挂在 `src/Prism/Controls/ResultList.xaml.cs`（约 50 行）：

- `PreviewMouseLeftButtonDown`：记录起点坐标与命中行索引。**不置 `e.Handled`**，
  否则 ListBox 的单击选中会坏。
- `MouseMove`：左键仍按下，且位移超过 `SystemParameters.MinimumHorizontalDragDistance`
  或 `MinimumVerticalDragDistance` 时起拖；未超过按点击处理。
- 行解析走既有 `ItemAt(index)`，不用 `container.DataContext`——同键行在
  `_displayItems` 中永不替换，容器可能仍持有该行的较早实例（见 `ResultList.xaml.cs`
  `SynchronizeDisplayItems` 注释）。

### 1.3 载荷与语义

- `DataObject` + `SetFileDropList(StringCollection)`，`DragDropEffects.Copy`，
  不放行 `Move`——搜索结果不是文件所有者，不允许目标程序搬走原文件。
- 放行条件：`Kind ∈ {file, folder, app}`，`ExecuteId` 非空，且路径实际存在。
- 不放行 `web`（ExecuteId 是 URL）、`window`（ExecuteId 是窗口枚举 token，不是路径，
  `ResultList` 的图标装饰已有同类守卫）、`more`（ExecuteId 为空）。

### 1.4 `DoDragDrop` 的三条纪律

`DoDragDrop` 是阻塞式嵌套消息循环，期间 WPF 继续派发消息：搜索 debounce 定时器、
索引 generation 变更、pipe 响应都照跑，`Results.Items` 可能在拖拽途中整个被换掉。

1. **调用前快照路径。** 快照之后拖出去的一定是用户抓的那一项，与列表后续变化无关。
2. **`try/catch` 包 `COMException`。** 目标程序行为异常时 OLE 会抛，不能让它冒到
   应用未处理异常。
3. **`finally` 里清 `_isDragging`。** 漏清则窗口从此永不失焦隐藏——比拖拽本身的缺陷
   更难排查。

嵌套循环期间列表被换掉只会造成视觉抖动（`ScrollIntoView`）。先不加抑制；实测确认
抖动可感知时，再在 `ApplyState` 的动画分支上加 `_isDragging` 抑制。

### 1.5 明确不做

- 拖入（搜索框是打字的地方，拖文件进来的价值不足）
- 多选拖拽（复杂度由暂存区吸收：逐个扔进去，凑齐再逐个拖出）
- 球状聚合等入场动画（Windows 自带半透明拖影已足够；拖拽讲究一气呵成）

### 1.6 阶段一验收（缺一不算好用）

1. 拖到资源管理器 → 复制成功
2. 拖到邮件客户端 → 成为附件
3. 拖到聊天窗口 → 发出文件
4. 拖到一半、鼠标离开 Prism 窗口 → 窗口不消失
5. 松手后 → 失焦隐藏逻辑恢复正常（拖到别的应用上松手，Prism 正常隐藏）
6. 零回归：单击选中、双击打开、右键动作菜单、`more` 行点击（走
   `PreviewMouseLeftButtonUp`）行为全部与改动前一致
7. `dotnet test` 全绿（当前 295 通过 / 0 失败）+ 新锚测试
8. `scripts/ui-shot.ps1` 真机渲染无异常、日志无新增错误

### 1.7 已核事实与已知限制

- `src/Prism/app.manifest` 的 `requestedExecutionLevel` 是 `asInvoker`，不提权，
  正常安装使用下无 UIPI 问题。
- 用户手动以管理员身份运行 Prism 时，拖向非提权的资源管理器会被 Windows UIPI
  静默阻止。这是系统机制，不做补偿，写入文档即可。

## 2. 阶段二：暂存区

### 2.1 存储：独立 `staging.json`，不写入 `settings.json`

`SettingsViewModel` 的模式是"打开设置页时载入快照、按保存时全量写盘"。暂存区由搜索窗
侧写、设置项由设置窗侧写；若两者同处一个文件，用户在设置页打开期间往暂存区加的条目
会被设置页的保存整体覆盖。

新增 `src/Prism/Services/StagingStore.cs`，独立文件，复用 `SettingsStore.Save` 的原子写
模式（写 `.tmp` 再 `File.Move(overwrite: true)`）与损坏时回落默认值的纪律。

进 `Settings` 的只有两项：
- `StagingCapacity`（默认 5，`Validate` 给上界）
- `StagingAddHotkey`（见 2.4）

### 2.2 状态：独立于 `AppState`

`SearchWindow.ReleaseIdleMemory` 与 `SearchViewModel.ResetForShow` 每次隐藏都会清空
`AppState.Results`。暂存区必须活过呼出周期，因此单独一个 `StagingState`
（`ObservableCollection` + 变更通知），不塞进 `AppState`。

### 2.3 UI：新增 `Controls/StagingStrip.xaml`

插在 `SearchWindow.xaml` 的 `Header` 与 `Divider` 之间。窗口是 `SizeToContent="Height"`，
条带高度变化自动跟随。

**不接入 `AnimatePanelHeight`。** 那条路径只管 `PanelHost`（结果/动作面板互斥切换），
是逐帧栅格化的敏感区（动画期间会临时移除 `DropShadowEffect`），不把条带拉进来。

条带内容按设想文档：空态高度 0；有文件时一行高度（等于一行搜索结果高度）；左侧数字为
当前文件数；文件名横排、截断、悬停 tooltip 显示完整路径；属于工作集的带角标；单字按钮
清空 / 存为工作集；背景半透明，视觉权重低于搜索框与结果列表。

排序按扔进去的先后，新文件接最右，不分组不重排。

### 2.4 加入快捷键：可配置，复用现有录键机制

裸 `+` 不可用：`SearchHeader.OnQueryPreviewKeyDown` 只上抛导航键与带修饰键的组合，
裸 `+` 是正常输入字符，吞掉会破坏输入框打字。

按决定做成可配置项，复用既有基础设施：
- `Settings` 增 `StagingAddHotkey`（字符串组合键，如 `"Ctrl+D"`）
- 校验复用 `ActionHotkeyTable.Parse` / `IsReserved` / `Canonicalize`（保留键：方向键、
  Enter、Esc、Ctrl+G、Ctrl+数字）
- 设置页"快速访问"tab 增一行，复用 `HotkeyRecorderBox`
- `SettingsStore.Validate` 增一条撞键检查：不得与 `ActionHotkeys` 中任一绑定冲突
- `SearchWindow.OnHeaderKeyDown` 在动作快捷键匹配之后、导航键之前查这一个绑定

**不塞进 `ActionHotkeys` 字典。** `ActionHotkeyCatalog` 是 broker 动作面板的前端静态镜像，
有锚测试防止两边漂移；"加入暂存区"是纯前端动作，不是 broker 动作，混进去会破坏那条锚。

默认值建议 `Ctrl+D`（已核不在保留键内，与 Ctrl+G / Ctrl+数字无冲突）。

### 2.5 其余行为

- 逐个拖出：复用阶段一的拖源，暂存区条带作为第二个拖源接入点
- 拖出不移除引用：暂存区是永久拿取口，拖一百次文件还在
- LRU 淘汰：满了挤掉最早的未标记项，工作集标记项挤不掉（纯逻辑，单测锚）
- 清空：只清未标记项
- 存的是路径引用，不是文件本身；原文件被删则该条目下次拖出失效，这是路径引用的固有限制

### 2.6 一处修正设想

设想文档写"暂存区不随 Prism 主窗口失焦隐藏"。暂存区在主窗口内，做不到也不需要——
拖拽期间由 1.1 的 `isDragging` 闸保护即可，不需要为它开独立窗口。

### 2.7 阶段二验收

1. 加入 / 拖出 / 点击打开 / 清空四条操作均正常
2. 关闭 Prism 再启动，暂存区内容仍在
3. 超出上限时挤掉最早未标记项，标记项不被挤
4. 设置页改容量与快捷键后立即生效，撞键被拦在落盘之前
5. 设置页保存不会覆盖暂存区内容（2.1 的核心动机，需显式验证）
6. 空态不占地方，有文件时不挤压结果列表可视行数
7. `dotnet test` 全绿 + 新锚测试；`ui-shot.ps1` 真机验证

## 3. 阶段三：工作集（后置）

- 落盘：与暂存区同一个 store 文件，加一张"名字 → 路径列表"表
- 名字召回：前端本地注入合成行，零协议改动（`SearchResult.More` 是先例）
- 整组拖出：复用阶段一拖源
- 载入时的保留规则、工作集间可重叠、可加备注：按设想文档，不再重述

阶段三在阶段二落地并实际用过一段时间后再开，避免提前规划过期。

## 4. 执行纪律

- 每阶段独立提交，可独立回滚。阶段一的改动面是一个纯函数参数 + 新增代码，
  单独回滚不影响任何现有功能。
- 阶段一不满足 1.6 全部验收项，不进阶段二——拖拽做不靠谱，暂存区的全部设计都是
  空中楼阁。
- 每阶段门：`dotnet test` 全绿 + 新锚测试 → `scripts/ui-shot.ps1` 真机验证 → 提交。

## 5. 关联文档

- `docs/PRISM-STAGING-DRAG-EXPLORATION-2026-08-21.md` — 拖拽与暂存区设想
- `docs/PRISM-STAGING-WORKSET-EXPLORATION-2026-08-21.md` — 暂存区与工作集设想
- `docs/PRISM-DRAG-DROP-EXPLORATION-2026-08-21.md` — 拖拽 WPF API 调研
- `docs/PRISM-ACTION-HOTKEYS-PLAN-2026-08-21.md` — 动作快捷键（本规划 2.4 复用其机制）
- `docs/PRISM-FUTURE-DIRECTIONS-2026-08-20.md` — 完整设想清单
