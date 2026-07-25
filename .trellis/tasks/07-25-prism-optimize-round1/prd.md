# PRD — Prism 第一轮优化：USN 实时索引 + 前端体验修缮

## 目标与用户价值

初版（1.0.0）验收后发现的最痛体验缺口一次收敛：新文件搜不到（最坏 ~5 分钟盲区）、首装建索引期间像"坏了"、右键无菜单、删字母时列表抖动、"显示更多结果"点击无效。本轮完成后，文件变更秒级可搜、索引状态可见、鼠标交互补齐、列表刷新平滑。

## 背景（已确认事实）

- **Gap B（核心）**：`index.rs build_or_load` 只做一次性 MFT 枚举（`FSCTL_ENUM_USN_DATA`），无 USN Journal 实时订阅；新文件靠 `index_refresh_secs=300` 定时全量重建感知（`index.rs:630-650`）。design.md 规划的 USN 实时增量未落地，是记录在案的功能债（spec backend/quality-guidelines.md「Indexing: Known Gaps」）。
- **Gap A**：首装无缓存 → `build_full_index()` 全盘枚举分钟级，期间结果不全。前端已有 `is_indexing` 轮询与「索引加载中…」文案（`SearchViewModel.ApplySearchResponse`），但无进度信息；重启走 `load_cache`，实际毫秒数从未测过（日志有「加载耗时 {}ms」行）。
- **右键菜单未开发**：`ResultList.xaml` 只有 `MouseDoubleClick`，无 ContextMenu / 右键处理；动作能力已存在（→ 键 ActionPanel + 后端 get_actions/run_action）。
- **删字母列表抖动**：每次搜索响应整表替换 `_state.Results`（`SearchViewModel.ApplySearchResponse` → `ResultList.Items` setter 重设 `ItemsSource`），容器重建、图标先置 null 再异步加载、高度重算，视觉上"颤一下"。
- **"显示更多"点击无效**：`ShowMoreAsync`（limit 100→1000 重搜）只在 Enter/双击时触发；单击 "more" 行仅选中不执行。且 "more" 行在任何有结果时恒显示（`ApplySearchResponse`），结果不足 limit 时点了也无变化。
- 后端 `max` 参数透传正常（`ipc.rs`，缺省 100）。

## 需求

### R1 USN Journal 实时增量监听（Gap B 根治，B2）
- 后端每 NTFS 卷一个监听任务：记录枚举时的 USN 位点，`FSCTL_READ_USN_JOURNAL` 长轮询，将 create/rename/delete 事件**增量**应用到共享索引（非整表换）。
- 新建/重命名/删除文件后 ≤5 秒可搜到/搜不到。
- 无权限读 Journal 时降级：保留现有定时全量重建路径（不阻塞启动、不崩溃），并把 `index_refresh_secs` 缺省下调（300→60，B1 兜底）。
- Journal wrap / 溢出（`ERROR_JOURNAL_ENTRY_DELETED`）时触发一次全量重建自愈。

### R2 首建/加载状态可见（Gap A，A1+A2）
- 首步实测：抓取本机 `load_cache` 毫秒数并记录（决定重启场景是否也需提示）。
- 首次全量构建期间：后端在 `results` 响应中带进度（已扫条数/上次总数估计）；前端显示「正在首次构建全盘索引…（N 万条）」样式的可感知进度，替代干等。

### R3 结果行右键菜单
- 右键任一 file/folder 结果弹出 ContextMenu，动作集与 ActionPanel 一致（打开、打开所在文件夹、复制、剪切、复制路径），复用后端 get_actions/run_action，不新增动作实现。
- web / more 行右键不弹菜单（或仅"打开"）。深浅色主题下菜单样式与 Tokens 一致。

### R4 列表刷新平滑（去抖动）
- 搜索响应到达时对现有列表做**差量更新**（原位增删改），未变化的行不重建容器、图标不闪空。
- 删除一个字母时肉眼无整表闪烁/跳动；选中项保持逻辑不变（仍按 ExecuteId 保持）。

### R5 "显示更多结果"实装
- 单击 "more" 行即触发加载更多（与 Enter/双击一致）。
- 仅当结果可能还有更多时显示 "more" 行（返回条数达到当前 limit 才显示）；加载后若无新增则行消失。

## 验收标准

- AC1：新建文件后 5 秒内可搜到；删除后 5 秒内消失（管理员权限下实测）。
- AC2：无管理员权限启动不崩溃，搜索可用，新文件最坏 ~60 秒可见（降级路径）。
- AC3：首装（删掉 data 目录）启动后立刻搜索，UI 显示构建进度提示而非空白；构建完成后提示消失。
- AC4：`load_cache` 实测毫秒数记入任务 research/ 或 spec。
- AC5：右键 file/folder 结果出菜单，五个动作全部可用且与 ActionPanel 行为一致；深浅色主题均正常。
- AC6：输入 "abc" 再退格成 "ab"，结果列表更新时无整表闪动、共有行图标不闪空。
- AC7：单击"显示更多结果"加载到最多 1000 条；结果不足 limit 时不显示该行。
- AC8：`cargo test` / `cargo clippy` / `dotnet build` 全绿；空闲内存仍满足后端 ≤70MB / 前端 ≤30MB 预算（USN 监听常驻不显著抬高基线）。

## 不做（Out of Scope）

- A3 之外的首建提速（USN fast-build 顺带收益即可，不单独优化 MFT 枚举）。
- 托盘「重建索引」占位实装（下一轮）。
- 安装包 .NET 运行时自检/自带（下一轮）。
- 内容搜索、网络盘、非 NTFS 卷监听。

## 关键决策

- 范围按 spec「Recommended next-task shape」：B2 为核心 + A1/A2 + B1 兜底；用户 2026-07-25 批准，并追加 R3/R4/R5 三个前端问题。
- 右键菜单复用现有动作后端，不做独立 shell context menu（IContextMenu 集成成本高，本轮不需要）。
