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


## Session 19: G4 宿主联动收尾：矩阵签署、三个实机缺陷、构建链路统一

**Date**: 2026-08-09
**Task**: G4 宿主联动收尾：矩阵签署、三个实机缺陷、构建链路统一
**Branch**: `feature`

### Summary

G4 矩阵全部通过并归档；修复 E1/E3/O1 三个实机缺陷；统一构建安装脚本根治旧二进制问题；panic 落盘已确证，ERROR_PIPE_BUSY 因果链未证明。

### Main Changes

## 本轮做完的事

**G4 收尾并归档。** 兼容矩阵 E1–E15 / O1–O11 / S1–S5 全部通过并签署
（OS 22631、Opus 13.23.0.0）。S1–S5 是逐条查代码核实的，不是勾选了事：
宿主路径的 P/Invoke 全是只读查询；索引服务只引用 `index_cache` /
`indexer_runtime` / `log` / `logging`，不含 shell 模块，LocalSystem 侧没有执行
shell 的能力；协议与宿主相关的字段只有 `root: Option<String>`。

两个 adapter 的默认值按产品决策**保持 false**，签署只表示矩阵通过。

**修掉三个实机缺陷。** 都不是设计问题，而是判据选错或格式假设错，共同点是
单测全绿但前提是假的：

- **E1**（范围标签正确但结果是全局）：broker 跑的是 `target\debug` 里 8月5日的
  旧二进制，而所有改动都编到了 release。代码本身早就是对的。
- **E3**（多标签 Explorer 认不出活动标签）：`IsWindowVisible` 判不出来——实测
  所有标签的视图窗口恒为 `visible=true`、`cloaked=0`、矩形相同。改用视图所属
  `ShellTabWindowClass` 的兄弟 Z 序，活动标签恒在最前，实机 18 次切换验证。
- **O1**（Opus 无法识别目录）：`dopusrt /info paths` 输出的是 XML，解析器却在
  按行找盘符开头的路径。原单测喂的是我们自己编的纯文本格式，Opus 从不产出
  那种格式。

**统一构建安装链路。** 「跑的不是你改的代码」是本轮最大的时间黑洞，三个独立
成因：前端 `LocateBackend` 写死 debug 优先且只判存在性；cargo/dotnet 的静默
no-op（`Finished in 0.37s` + 退出码 0）；服务安装手工、文件锁导致 `Copy-Item`
静默失败。新增 `scripts/prism-build.ps1` 串起构建到安装，装完用 SHA-256 逐个
比对，并校验产物不早于源码最新改动；握手回传 `build_id` 让进程能自报身份。

**IPC 韧性（部分）。** panic 无日志已确证修复：`panic = "abort"` 且无 hook，
崩溃不留任何痕迹——探针实测现在能拿到
`panic at tools/panic-probe.rs:23 message_e395…`，位置明文、路径已哈希。
`ERROR_PIPE_BUSY` 的三处运行时阻塞已修（Status 走 `spawn_blocking`、
新增只读 `generation()`、监听池扩到 4），但 **A/B 对照否证了因果链**：
未修复的旧二进制在 5831 世代/秒下同样零失败。任务保持 `in_progress`。

## 判断失误与纠正

- **删掉了唯一一份干净基准数据。** 第一轮 root 基准实际采集成功，只是聚合有
  bug，我清目录重采而没有对已有原始 JSONL 重跑聚合。之后机器环境变了
  （Windows 更新、重装系统、索引从 171MB 掉到 66MB），再也采不到。
- **采信 5 天前未核实的记忆。** 依据一条过期记忆在测试文档里写了「不要重新
  编译」，直接导致用户用旧服务测 E1 而失败。记忆里关于文件是否存在的断言，
  引用前必须核实。
- **没先问环境就自动化。** Opus 接管了默认文件管理器，我用 `explorer.exe`
  反复自动化测 E3，打开的一直是 Opus Lister，白跑好几轮。
- **写了一个会死锁的测试。** 为缺陷 A 补回归测试时用 `worker_threads = 1`，
  服务端与客户端争同一线程，`cargo test` 直接挂住。已删除。缺陷 A 因此**没有**
  回归测试兜底，只有代码论证。

## 沉淀

- `docs/排查踩坑记录.md`（314 行）：五节，环境陷阱 + 验证方法上的错误 +
  我犯的流程错误 + 下次开工检查清单。无法从磁盘复核的数字标注了来源。
- Forbidden Patterns 从「不得阻塞**扫描**」放宽到「不得取 index/pinyin/history
  锁」——原措辞只提扫描，`Status` 大概正因此漏掉。另加「abort 模式必须装
  panic hook」。
- 保留 6 个可复用探针 + TabProbe 源码（三份已提交文档引用它们，此前整个目录
  没被跟踪，clone 下来会找不到文件）。提交前修掉硬编码的仓库绝对路径。

## 遗留

- `08-09-prism-g4-root-baseline`（P3，新建）：承接 G4 步骤 8 未采到的正式基准。
  补采前提是索引 ≥150MB、机器安静。
- `08-07-prism-ipc-resilience`（P1，open）：`ERROR_PIPE_BUSY` 因果链未证明。
- broker 的 `ipc.rs` 还有两处同类违规未改（`run_shell` → `history.record()`
  持写锁 + 同步写文件；`ClearHistory` 同步删文件），严重度低于 Status，
  超出本轮范围。
- `dist/` 里的产物是 8月1日的，出新版本时需要更新。


### Git Commits

| Hash | Message |
|------|---------|
| `18a4c0e` | (see git log) |
| `c40edea` | (see git log) |
| `06dc747` | (see git log) |
| `3475ff2` | (see git log) |
| `b284bf9` | (see git log) |
| `3dee8f9` | (see git log) |
| `0733ad3` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 20: G5 窗口切换器：枚举/协议/前台激活落地，激活归属修正

**Date**: 2026-08-11
**Task**: G5 窗口切换器：枚举/协议/前台激活落地，激活归属修正
**Branch**: `feature`

### Summary

按规范推进 G5（G4 补采按要求留到后面）。Phase 1 已有产物，先按实际代码校准：design.md 原稿把激活放在 broker，与 Windows 前台规则冲突——SetForegroundWindow 只对前台进程生效，broker 是后台进程，调用会被静默降级成任务栏闪烁；已验证可用的 ForceActivate 本来就在 WPF。改为 broker 枚举/排序/持 token 与历史、WPF 负责激活，并把该例外写回 backend/error-handling。另修 implement.jsonl 里的代码文件与 G3 已删的 search.rs。实现：新增 window_list（六条过滤规则各有单测、有界 snapshot、generation<<10|index 的十进制 token 使既有数字校验仍成立、HWND 不出边界）；ipc 加 mode（缺失=all，旧前端逐字节兼容）、Window kind、ResolveWindow/RecordWindowSwitch、复用 G2 拼音与历史等级、空输入改为历史∩当前枚举；WPF 加 > 前缀解析（只认首字符）、Win32WindowActivator 在 App.xaml.cs 装配、先激活再隐藏。用 mutation 反验断言有效性：对调激活/隐藏顺序→两条测试转红（有效）；删掉前缀缓存 guard→测试仍通过（无效，已改写为只断言可观察行为并加对照组）。该教训已并入 verify-before-claiming-fixed 记忆。四道门全绿：Rust 194、C# 101、clippy -D warnings、Release build 0 警告。步骤 7/8 保持未完成：真实前台切换、激活被拒的 A/B、≤100MB 采样门都需实机，非代码缺口，任务不归档。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `3e72402` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 21: G5 窗口切换器收口：挂起 UWP 修复、步骤 8 机器测试与 PRD 验收证据

**Date**: 2026-08-12
**Task**: G5 窗口切换器收口：挂起 UWP 修复、步骤 8 机器测试与 PRD 验收证据
**Branch**: `feature`

### Summary

修挂起 UWP 被 cloaked 过滤丢掉的缺陷：DWMWA_CLOAKED 对「本桌面挂起的 UWP」和「其他虚拟桌面」返回同一个 SHELL 位，原来压成一个 bool。改为存原始 bits，只排除 CLOAKED_APP，虚拟桌面交给 IVirtualDesktopManager。测量先于修改救了这次修法——探针输出暴露第一版会把「设置」列两遍并多列一个切不过去的输入法窗口，加 CoreWindow 规则解决。收口步骤 8：补同应用多窗口测试（mutation 反验只有新增那条转红，原有三条 history_key 测试对标题从键里消失毫无察觉）；补最小化恢复实机探针，SW_RESTORE 第一次真的执行——A/B 挖出真问题，删掉它之后 Windows 照样把仍然最小化的窗口设成前台并返回 true，只断言前台身份的测试是绿的；激活被拒拿掉 AttachThreadInput 兜底做对照，Windows 真的拒了。内存门改测趋势不测阈值（原文是「无持续内存增长」，此前一直被误记为「≤100MB」），400 次 +276KB、1200 次 +424KB 是平台期。逐条找 PRD 验收证据时发现空输入的「历史 ∩ 当前枚举」无法被断言，抽出 rank_window_list 才有落点。四道门：Rust 206 / clippy 干净 / C# 101 passed 5 skipped / build 0 警告 0 错误；端到端 pipe probe 21/21。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `31370b7` | (see git log) |
| `4527156` | (see git log) |
| `7459914` | (see git log) |
| `457d96a` | (see git log) |
| `47cb02b` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 22: G6 完整内置动作

**Date**: 2026-08-13
**Task**: G6 完整内置动作
**Branch**: `feature`

### Summary

G6 完整内置动作：ActionId 封闭枚举 allowlist（File=12/Directory=11/Application=4），IFileOperation 回收站/永久删除/复制到/移动到/重命名，ZIP adapter 三级回退（自定义路径>7-Zip 探测>Windows Shell COM），IPC ActionArgs 协议扩展，前端 rename 编辑态+copy_to/move_to 文件夹选择器，mutation 后 generation 超时提示，历史候选磁盘存在性检查修复旧路径不消失，属性 ShellExecuteExW+INVOKEIDLIST 修复 code 31，机器测试矩阵（重名/长路径/中文/只读/源消失/复制到自身/移动到子目录）

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `fddaef2` | (see git log) |
| `3aa15bd` | (see git log) |
| `2670ef1` | (see git log) |
| `5d71d65` | (see git log) |
| `f832857` | (see git log) |
| `c4d3478` | (see git log) |
| `7b9e4d9` | (see git log) |
| `bc6935e` | (see git log) |
| `d177439` | (see git log) |
| `ef52ae7` | (see git log) |
| `387758c` | (see git log) |
| `668fffb` | (see git log) |
| `0aa283d` | (see git log) |
| `7b2655e` | (see git log) |
| `d6fcc2f` | (see git log) |
| `2d8a2ba` | (see git log) |
| `d3281b4` | (see git log) |
| `1c15af4` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 23: G7 ext:/path: 查询过滤：broker 解析 + Top-K 前过滤 + 实机验收

**Date**: 2026-08-13
**Task**: G7 ext:/path: 查询过滤：broker 解析 + Top-K 前过滤 + 实机验收
**Branch**: `feature`

### Summary

实现 G7 ext:/path: 查询过滤。broker 侧 parse_query 单次 FSM 解析 ext:/path: token（逗号 OR、引号值、前导点规范化、大小写不敏感），未识别/不完整/未闭合引号回退为普通文本。解析出的 filters 复用 G1 预留、G3 已使用的 filters 通道（不新增协议通道）。indexer 在 search_volumes 和 pinyin_sidecar 的 Top-K 堆前应用 ext（低成本 name-only，目录排除）和 path（高成本完整路径子串匹配）过滤。有过滤器时 broker 跳过 apps/web/window。空 name_query + 过滤器不再短路（修复 find_case_insensitive 空 query 的 windows(0) panic）。实机：ext:pdf→8 items、ext:txt,pdf OR、path:substring、ext+path AND、bi ext:pdf 抑制 web、max=1000→46 pdf、path_constructions=28285、内存 86MB≤100MB。261 Rust tests + 14 ignored / clippy clean / 101 C# tests + 5 skipped / WPF Release 0 warnings 0 errors。G7 已归档。

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `caa8ce9` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete
