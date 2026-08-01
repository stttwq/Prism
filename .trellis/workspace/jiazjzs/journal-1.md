# Journal - jiazjzs (Part 1)

> AI development session journal
> Started: 2026-07-23

---



## Session 1: Prism steps 1–5 checkpoint: search, apps, git init

**Date**: 2026-07-24
**Task**: Prism steps 1–5 checkpoint: search, apps, git init
**Branch**: `main`

### Summary

完成 Prism implement 步骤 1–5（骨架/热键/索引/搜索管道/开始菜单应用置顶），修复空搜 is_indexing 轮询、管道配对与 Idle 焦点问题；写入前后端 quality-guidelines；清理 target/bin/obj 后 git init 并提交检查点。任务 07-23-prism-planning 保持 in_progress（步骤 6–11 未做，未 archive）。

### Main Changes

- `ResultList` now applies stable-key collection diffs without resetting `ItemsSource`.
- Existing result rows no longer gain a transient searching-status row during replacement queries.
- Same-target panel height animations and identical status assignments are layout no-ops.
- Frontend quality guidelines now capture the stable-refresh contract and manual regression case.

### Git Commits

| Hash | Message |
|------|---------|
| `fce47ea` | (see git log) |
| `9982d2a` | (see git log) |
| `46b0d17` | (see git log) |
| `b7675dd` | (see git log) |

### Testing

- `dotnet build src/Prism -c Release --no-restore` (0 warnings, 0 errors)
- `dotnet test src/Prism -c Release --no-restore`
- Targeted `dotnet format --verify-no-changes`
- Manual `abc` to `ab` backspace regression confirmed by the user

### Status

[OK] **R4 completed; parent task remains in progress**

### Next Steps

- Continue the remaining `prism-optimize-round1` requirements (R1/R2/R3) in a future session.


## Session 2: Prism step 6: web shortcut search (Bing-first)

**Date**: 2026-07-24
**Task**: Prism step 6: web shortcut search (Bing-first)
**Branch**: `main`

### Summary

完成 implement 步骤 6 网页快捷搜索：websearch 解析 bi/b/g 与 settings.json 自定义引擎（必应优先），IPC 搜索 web 置顶、execute 默认浏览器打开 URL；前端 web 行跳过系统图标；同步前后端 quality-guidelines。cargo test 45 全绿、clippy/dotnet build 通过。任务 07-23-prism-planning 保持 in_progress（步骤 7–11 未做，未 archive）。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `e993e9e` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 3: Prism step 7: tray icon and registry autostart

**Date**: 2026-07-24
**Task**: Prism step 7: tray icon and registry autostart
**Branch**: `main`

### Summary

完成 implement 步骤 7 托盘与自启：NotifyIcon（打开设置/重建索引占位/退出）、HKCU Run 自启（路径加引号）、最小设置窗（开机自启+数据目录）、WPF+WinForms GlobalUsings 与图标生命周期；dotnet build 通过。任务 07-23-prism-planning 保持 in_progress（步骤 8–11 未做，未 archive）。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `8073045` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 4: Prism step 8 settings UI and memory budget

**Date**: 2026-07-25
**Task**: Prism step 8 settings UI and memory budget
**Branch**: `main`

### Summary

完成 implement 步骤 8 中文设置页（快捷键/网页引擎/自启/数据目录、HotkeyRecorderBox、reload_engines 热重载）；索引 v3 父目录/文件名 intern + 噪声目录黑名单；前端 IconCache 上限/扩展名键/懒创建搜索窗/EmptyWorkingSet；release 空闲合计约 42MB WS / 37MB 私有。cargo test 57 绿、clippy/dotnet build 通过。任务 07-23-prism-planning 保持 in_progress（步骤 9–11 未做，未 archive）。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `5d8d0fb` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 5: Prism step 9 UI polish: theme pin actions

**Date**: 2026-07-25
**Task**: Prism step 9 UI polish: theme pin actions
**Branch**: `main`

### Summary

完成 implement 步骤 9 界面精修：Tokens.Dark + ThemeWatcher 跟随系统深浅色、PinButton 固定窗口、ActionPanel + 后端 open_folder/copy/cut/copy_path（剪贴板 CF_HDROP/CF_UNICODETEXT）、圆角裁剪/细滚动条/展开动画/展示更多蓝图标；trellis-check 修复动作失败可见 ActionStatus、→ 仅 file/folder 吞键、Pin 主题刷新、剪贴板失败 GlobalFree；cargo test 63 绿、clippy/dotnet build 通过。任务 07-23-prism-planning 保持 in_progress（步骤 10–11 未做，未 archive）。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `9430bec` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 6: Prism step 10: memory acceptance (≤100MB verified)

**Date**: 2026-07-25
**Task**: Prism step 10: memory acceptance (≤100MB verified)
**Branch**: `main`

### Summary

完成 implement 步骤 10 内存验收：发现发布二进制静默过期（不含 step 9 改动），rebuild 双 release 并手动复制 prism-core.exe 至发布目录；命名管道确认 is_indexing:false（索引已从 18MB v3 缓存加载常驻）；实测前后端合计私有工作集约 38MB / 工作集约 50MB，远低于 100MB（后端约 22MB≤70、前端约 16MB≤30）。验收方法+实测基线沉淀入 backend quality-guidelines。任务 07-23-prism-planning 保持 in_progress（步骤 11 未做，未 archive）。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `faddfb0` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete



## Session 7: Prism step 11: first installer package (PrismSetup-1.0.0.exe, double-install accepted)

**Date**: 2026-07-25
**Task**: Prism step 11: first installer package (PrismSetup-1.0.0.exe, double-install accepted)
**Branch**: `main`

### Summary

完成 implement 第 11 步并产出 Prism 第一个软件包：dist 安装源（前端单文件 frame-dependent 发布 + 后端 release + ico）与 Inno Setup 中文安装脚本 prism.iss；用户本地用 Inno Setup 7（装在 D:\LS\Setup7）编译生成 PrismSetup-1.0.0.exe，并完成中文目录 D:\工具\Prism 与 Program Files 双套安装验收——前者索引落安装目录\data、后者退回 %LocalAppData%\Prism，全部功能正常。README 补完中文使用说明，backend quality-guidelines 新增 Installer/Packaging 节。任务 07-23-prism-planning 已 archive。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `9ead91d` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 8: Prism post-step-11: indexing known gaps recorded (first-install wait + new-file ~5min latency)

**Date**: 2026-07-25
**Task**: Prism post-step-11: indexing known gaps recorded (first-install wait + new-file ~5min latency)
**Branch**: `main`

### Summary

第11步首个安装包与双套验收完成后，用户复盘点出两个索引相关缺口，未改代码、沉入 backend quality-guidelines 新增「Indexing: Known Gaps」节：Gap A 首次装无缓存→全量建索引期间（分钟级）搜不全、重启有缓存 load_cache 秒级（真实 ms 未测，日志已有「加载耗时」行可抓）；Gap B 新增文件靠 index_refresh_secs=300 全量重建感知、最坏~5min 盲区，根因是 design.md 规划的 USN Journal 实时监听未落地（第3步只做了 MFT 一次性枚举那半）。给出改善路线 A1/A2/A3 与 B1/B2/B3，注明后续任务形状（B2 USN 实时 watch 为核心，先抓 load_cache ms 决定 A1 文案）。无代码改动。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `1592277` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 9: Prism 第一轮优化：AC4 与 R5

**Date**: 2026-07-26
**Task**: Prism 第一轮优化：AC4 与 R5
**Branch**: `main`

### Summary

完成 AC4：实测 616223 条索引缓存 load_cache 正式 5 次为 83–87ms，均值 84.8ms，并记录到任务 research。完成 R5：仅在结果达到 limit 时显示 more 行，单击即可加载更多；根据验收反馈将首屏调整为 8 条、第 9 行显示更多，展开上限保持 1000。用户已手测确认全部场景正常；Release 构建 0 warning/0 error，测试产物已清理。当前任务保留 in_progress，后续继续 R4/R3/R2/R1。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `199bb99` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 10: R4 差量刷新与抖动修复

**Date**: 2026-07-26
**Task**: R4 差量刷新与抖动修复
**Branch**: `main`

### Summary

实现 ResultList 稳定键差量更新，消除搜索状态行与重复高度动画导致的退格跳动；用户手测确认修复，并将布局稳定约束写入前端规范。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `fb5b7a7` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 11: Prism R3 结果右键菜单

**Date**: 2026-07-26
**Task**: Prism R3 结果右键菜单
**Branch**: `main`

### Summary

完成 Prism 搜索结果右键菜单：app/file/folder 复用现有 actions/run_action，适配深浅主题与失焦保护；用户手测通过，构建、63 项 Rust 测试及 Clippy 全绿。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `f74090b` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 12: R2 后端索引构建进度字段

**Date**: 2026-07-26
**Task**: R2 后端索引构建进度字段
**Branch**: `main`

### Summary

实现后端共享索引构建进度、MFT/walkdir 扫描计数与 results.index_progress 可选字段；补齐 IPC 序列化测试，并完成真实冷启动验证。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `154ee16` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 13: 完成 R1 Everything 式 USN 实时索引

**Date**: 2026-07-27
**Task**: 完成 R1 Everything 式 USN 实时索引
**Branch**: `main`

### Summary

实装 LocalSystem 索引服务、FRN 层级索引、MFT/USN 实时监听与停机回放、v5 缓存、只读 IPC、WPF generation 刷新和 Inno 服务安装；修复阻塞 watcher 导致的 SCM 停止卡死，并完成延迟、内存、协议、安装及构建验收。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `0296463` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 14: 完成 Prism G0 可复现基线

**Date**: 2026-07-29
**Task**: 完成 Prism G0 可复现基线
**Branch**: `feature`

### Summary

完成 G0 搜索、全量扫描与三进程内存基线；修复基准脚本自扰动和内存硬门问题，更新三进程 Memory Acceptance，并在验收机恢复健康的默认 z 服务。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `19408bb` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 15: 完成 Prism G1 搜索正确性与协议演进

**Date**: 2026-07-30
**Task**: 完成 Prism G1 搜索正确性与协议演进
**Branch**: `feature`

### Summary

完成全局 Top-K、USN 补放稳健性、IPC handshake/truncation/generation/filter 契约、WPF 可注入搜索状态与测试地基。Rust 95 tests/clippy/format、C# 7 tests、WPF Release 0 warnings/errors 通过；正式搜索 max=8/1000、三进程内存和 scan-floor 验收通过。G1 已归档。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `974dee9` | (see git log) |
| `3993200` | (see git log) |
| `d84581a` | (see git log) |
| `f5eed7f` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 16: 完成 Prism G9 首建可用性与进度

**Date**: 2026-07-31
**Task**: 完成 Prism G9 首建可用性与进度
**Branch**: `feature`

### Summary

完成系统卷优先的逐卷首建发布与即时 USN watcher，新增可选进度和前端安全轮询，修复取消、重试计数及缓存保存完成时序；三卷机器验收、稳态延迟和三进程内存门均通过。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `bc01766` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 17: 完成 G3 工程地基与权限边界

**Date**: 2026-08-01
**Task**: 完成 G3 工程地基与权限边界
**Branch**: `feature`

### Summary

完成 Shell/COM STA worker、typed target、版本化持久化、Top-K 前用户排除、脱敏滚动日志、旧链清理与安装卸载验收；Rust 91 项和 C# 14 项测试及全部质量门通过。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `23dbad4` | (see git log) |
| `2fdf8d3` | (see git log) |
| `9aee7d5` | (see git log) |
| `a8c000a` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 18: 完成 Prism G2 历史与拼音

**Date**: 2026-08-02
**Task**: 完成 Prism G2 历史与拼音
**Branch**: `feature`

### Summary

完成版本化历史、拼音 matcher/sidecar、WPF 设置与卸载修复；112 Rust tests、15 C# tests、Clippy 和 Release build 通过；安装态搜索、内存、故障降级与严格卸载验收通过；已推送 origin/feature。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `b950d89` | (see git log) |
| `52d0445` | (see git log) |
| `4f8900d` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete
