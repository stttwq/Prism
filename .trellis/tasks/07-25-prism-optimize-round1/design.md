# Design — Prism 第一轮优化

## 总体边界

- 后端（src/prism-core，Rust）：新增 `usn_watch.rs` 模块 + `index.rs` 增量更新接口 + `ipc.rs` 进度字段。
- 前端（src/Prism，WPF）：ResultList 差量刷新 + 右键菜单 + more 行交互 + 索引进度文案。
- IPC 契约变更向后兼容：`results` 响应新增可选字段，老前端忽略即可。

## R1 USN 实时监听（usn_watch.rs）

### 数据流
```
build_full_index() 时逐卷记录 NextUsn 位点
  └→ 每 NTFS 卷 spawn 一个 tokio 任务（spawn_blocking 内循环）
       FSCTL_READ_USN_JOURNAL（BytesToWaitFor>0 阻塞式长轮询, timeout 数秒）
       解析 USN_RECORD_V2/V3 → 过滤 reason:
         FILE_CREATE / RENAME_NEW_NAME → upsert(frn, parent_frn, name)
         FILE_DELETE / RENAME_OLD_NAME → remove(frn)
         (忽略纯属性/内容变更 reason)
       批量攒 ~500ms 或 N 条 → 写锁 SharedIndex 应用增量
```

### index.rs 增量接口
- `FileIndex::upsert(dir, name, kind)` / `remove(dir, name)`（已核实：条目**不存 FRN**，dir 是预拼完整父路径串，池只增不减）。
- 因此需每卷常驻「目录 FRN 表」（仅目录，个位数 MB）解析 USN 事件路径，随缓存升 v4 持久化（含 journal_id/next_usn，重启后重放追平停机窗口）；目录改名/删除不做子树重写，标记 dirty 走 10s 去抖全量重建兜底（文件级事件纯增量）。细则见 backend-spec.md §4–§5。
- 噪声目录黑名单同 build 时逻辑复用（同一个 `is_noise` 判断函数）。

### 降级与自愈
- 打不开卷句柄 / `FSCTL_READ_USN_JOURNAL` 拒绝（非管理员）：该卷记 `watch=false`；任一卷失败不影响其它卷。
- 全部卷 watch 失败 → 保留定时全量重建（`index_refresh_secs`，缺省 300→60）。
- 任一卷 watch 成功 → 该卷不再需要定时重建；实现从简：只要全部卷 watch 成功就把定时重建间隔拉长为 0（关闭）或 3600 兜底，混合场景保持 60s。
- `ERROR_JOURNAL_ENTRY_DELETED` / journal id 变化：触发一次 `build_full_index` 换表并重置位点。
- 关停：进程退出即任务终止，无需优雅关闭。

### 权衡
- 不用 ReadDirectoryChangesW（B3）本轮：USN 失败即回退定时重建已满足 AC2，B3 留待需要时再加。
- 批量应用而非逐条：避免写锁高频争用；500ms 批仍远小于 5s 验收线。

## R2 索引进度（ipc.rs + 前端）

- `SharedIndex` 旁挂 `AtomicU64 scanned_count` + `total_estimate`（上次缓存条数，无缓存则 0）。`build_full_index` 枚举循环内递增。
- `results` 响应新增可选字段 `index_progress: {scanned, total_estimate}`（仅 `is_indexing=true` 时带）。
- 前端 `SearchViewModel` 轮询已存在，把 StatusMessage 换成「正在首次构建全盘索引… 已扫描 N 万项」或「N% 」（有 total_estimate 时）。
- `load_cache` 已有耗时日志行，任务第一步实测记录，若 >2s 则加载期也显示提示（同一机制，`is_indexing` 已覆盖）。

## R3 右键菜单（前端）

- `ResultList` 加 `MouseRightButtonUp`：命中行先选中，再向 `SearchWindow` 抛 `ContextMenuRequested(SearchResult, position)` 事件。
- `SearchWindow` 调用现有 `_pipe.GetActionsAsync(executeId)` 组装 WPF `ContextMenu`（跳过 SectionHeader 或渲染为 Separator+标题），点击项走现有 `RunActionAsync` + 成功后 Hide 逻辑（与 ActionPanel 相同代码路径，抽公共方法 `RunActionOnTargetAsync`）。
- 主题：ContextMenu 样式挂 Tokens 资源（DynamicResource），深浅色跟随。
- kind=web/more：不弹（web 可选仅"打开"，从简：不弹）。

## R4 差量刷新（前端）

- `AppState.Results` 改为持有 `ObservableCollection<SearchResult>`（或 ResultList 内部维护 OC，Items setter 做 diff——**选后者**，改动面小：VM/AppState 契约不动）。
- ResultList.Items setter：按 (Kind, ExecuteId, Title) 做键控 diff：
  - 逐位置比较，相同键的行原位保留（必要时更新 MatchSpans 装饰，由现有 DecorateVisibleItems 覆盖）；
  - 不同则 `oc[i]=new`、多删少补。
- ItemsSource 只在首次赋 OC，一次都不重设 → 容器复用（Recycling 已开）。
- 图标：行未变时 icon.Tag 相同 → 不清 Source，不闪。
- 高度：条数不变时跳过 UpdateListHeight 的重新赋值。

## R5 more 行实装（前端）

- `SearchViewModel` 用单一常量定义首屏 limit=8；呼出窗口或查询变化时都重置为 8，避免长列表占满交互区。
- `ApplySearchResponse`：仅当 `resp.Items.Count >= max` 时追加 More 行。
- ResultList 加 `MouseLeftButtonUp`（单击）：命中行若 `Kind=="more"` 直接 `ItemInvoked`；其他行维持单击选中、双击执行。
- ShowMore 后（limit=1000）返回 <1000 条 → 不再追加 more 行，自然消失。

## 兼容与回滚

- IPC 新字段全部可选（serde default / C# 可空），双向老新版本互通。
- USN watch 整体在独立模块，出问题可用配置 `usn_watch: false`（config.rs 新开关，缺省 true）一键关闭回到 1.0.0 行为。
- 索引缓存格式不变（v3），无迁移。
