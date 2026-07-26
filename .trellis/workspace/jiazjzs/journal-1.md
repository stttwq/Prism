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

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `fce47ea` | (see git log) |
| `9982d2a` | (see git log) |
| `46b0d17` | (see git log) |
| `b7675dd` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


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
