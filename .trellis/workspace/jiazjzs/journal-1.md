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

## Session 7: Prism step 11: installer artifacts (iss + dist, compile/install pending local)

**Date**: 2026-07-25
**Task**: Prism step 11: 安装包脚本与产物（编译/安装验收待本地）
**Branch**: `main`

### Summary

完成 implement 步骤 11 的脚本与产物准备（按用户选定交付范围：只产出脚本+产物+说明，本地用 ISCC 编译与双套安装验收）：
- 前端单文件发布（framework-dependent，PublishSingleFile）→ dist/Prism.exe（299KB）。
- 后端 release 产物 → dist/prism-core.exe（502KB）。
- dist/prism.ico。
- dist/prism.iss：Inno Setup 中文向导（选目录/开始菜单/桌面（默认不勾）/开机自启勾选）；HKCU Run\Prism 自启值名与前端 AutoStartService 一致；Unicode 全程；PrivilegesRequired=lowest+overridesAllowed=dialog（装 Program Files 时 UAC 自动提示）；CloseApplications=force + 卸载 taskkill 退出常驻进程；中文目录与 Program Files 两套数据目录策略由 Prism 首启自检承载（安装包不预建 data）。
- README.md 补完中文使用说明（安装/使用/设置自启/数据位置/卸载/内存验收）。

### Main Changes

- dist/: 新增 Prism.exe、prism-core.exe、prism.ico、prism.iss（安装源 + Inno 脚本）
- README.md: 由「使用说明待补充」改为完整中文使用说明
- 发布命令：`dotnet publish src/Prism -c Release -r win-x64 --self-contained false -p:PublishSingleFile=true`；后端 `cargo build --release --manifest-path src/prism-core/Cargo.toml`

### Git Commits

| Hash | Message |
|------|---------|
| (未提交，待本地编译验收后一并提交) | - |

### Testing

- 产物齐全校验：dist/ 四文件齐备；前端单文件发布成功（Prism.exe 299KB 单文件，无 Prism.dll 伴生）。
- 数据目录探测逻辑前后端已实现（config.rs resolve_data_dir / SettingsStore.ResolveDataDir），第10步内存验收已确认可降级 LocalAppData。
- 安装包编译（ISCC）与「中文目录 D:\工具\Prism」「Program Files」两套安装验收待本地执行（Inno Setup 当前未装）。

### Status

[OK] **步骤 11 脚本/产物部分完成**

### Next Steps

- 本地安装 Inno Setup 6 → 跑 `ISCC.exe dist\prism.iss` 生成 PrismSetup-1.0.0.exe
- 在 D:\工具\Prism 安装验收：索引出现在安装目录 data；功能正常
- 在 Program Files（UAC）安装验收：索引自动存 %LocalAppData%\Prism，设置页显示实际位置；功能正常
- 两套验收 OK 后提交 git 并归档任务 07-23-prism-planning
