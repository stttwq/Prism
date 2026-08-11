# Design

## Process Ownership (修正稿)

原稿把激活放在 broker 的「有界 window service」里。这与 Windows 前台规则冲突：
`SetForegroundWindow` 只对前台进程（或刚收到输入的进程）生效，按下 Enter 时前台进程是
WPF，broker 是后台进程，它的前台调用会被系统静默降级为任务栏闪烁。仓库里已验证可用的
激活路径也在 WPF（`src/Prism/Windows/SearchWindow.xaml.cs` 的 `ForceActivate`，含
`AttachThreadInput` 兜底）。因此按下面的边界划分：

| 职责 | 归属 | 理由 |
| --- | --- | --- |
| 枚举顶层窗口、过滤、采集身份 | broker | `EnumWindows` 不需要前台权限；`windows` crate 已启用 `Win32_UI_WindowsAndMessaging` / `Win32_System_Threading` |
| 标题/应用名匹配、拼音、历史排序 | broker | 拼音与 `HistoryStore` 都在 broker |
| snapshot 与 opaque token 生命周期 | broker | HWND 不外泄，token 只在本次 snapshot 有效 |
| execute 前身份复核 | broker + WPF 两次 | broker 解析 token 时核对；WPF 激活前用 `INativeWindowQuery` 再核对，压掉「解析→激活」之间的关闭/句柄复用窗口 |
| 最终前台激活、最小化恢复 | WPF | 唯一持有前台权限的进程 |
| 成功后写历史 | broker | 只在 WPF 回报成功后写，满足 PRD「失败不写成功历史」 |

`.trellis/spec/backend/error-handling.md` 的「所有用户会话 Shell/COM 工作归 broker」仍然
成立：窗口激活不是 Shell verb 也不是 COM 调用，而是受前台规则约束的 Win32 UI 调用，属于
显式例外，Phase 3.3 要把这条例外写回 spec。`shell.rs` 里 `TargetKind::Window` 现有的
`unsupported`（"window activation is not implemented"）保留：窗口目标永远不进
`ShellExecutor`。

## Enumeration And Identity

broker 按请求枚举顶层窗口，逐个采集 HWND、PID、进程启动身份、标题、应用名与可切换标志。
过滤规则（每条都要有单元测试）：

- 不可见（`IsWindowVisible` 为假）、无标题、`WS_EX_TOOLWINDOW`、cloaked（`DWMWA_CLOAKED`，
  覆盖其他虚拟桌面与已挂起的 UWP）、owner 非空的从属窗口；
- Prism 自身进程的全部窗口；
- 枚举上限有界（默认 512），超限截断并记日志，不无界增长。

返回给 WPF 的 `target` 仍是既有的 `{kind:"window", value:"<数字>"}`——`value` 是本次
snapshot 的 opaque token，不是 HWND。`shell.rs` 现有的「window 目标必须是数字」校验因此
逐字节保持有效，而 HWND 不出 broker 边界（除激活那一次显式解析）。broker 只保留最近一次
snapshot（有界，随下次枚举整表替换），token 携带 snapshot 世代号，跨世代 token 一律拒绝。

窗口历史用「应用稳定身份 + 规范化标题指纹」，不保存 HWND。展示最近窗口时先实时枚举，再把
历史与当前候选求交集，已关闭的窗口不出现。

## Protocol

新增字段与消息全部对旧读者兼容（新增 optional 字段 / 新增 variant）：

- `Request::Search` 增加 `mode: Option<String>`（缺失 = `"all"`）。客户端侧
  `SearchContext.Mode` 已存在但从未写进 payload（`PipeClient.SearchPayload`），本阶段接上。
- `SearchResultKind` 增加 `Window`（Rust `snake_case` → `"window"`；C# 侧
  `SearchResult.ResultKind` 加映射，旧值落 `Unknown` 的兜底不变）。
- `Request::ResolveWindow { target }` → `Response::WindowHandle { handle, pid, title }`
  或 `Response::Error`。broker 在这里做第一次复核。
- `Request::RecordWindowSwitch { target }`：仅在 WPF 激活成功后发送，broker 据此写历史。

两次往返是有意的：把「解析 + 复核」与「已成功、可写历史」分开，才能在失败路径上不写成功
历史。`ResolveWindow` 与 `RecordWindowSwitch` 的失败都不结束目标进程。

## Mode And Ranking

WPF 把 `>` 解析为显式 Window mode，不与文件/应用/网页混排；`>` 只在查询首字符时生效。
broker 分别匹配标题与应用名，取更强来源并返回 spans/source，排序复用 G2 的
`Literal > FullPinyin > Initials` 与同级历史加权（`hierarchy::MatchKind` / `MatchMetadata`）。
空输入（仅 `>`）走 `ipc.rs::search_service` 里 G5 预留的分支——那里现在的注释是
「Non-host empty input (recent windows) is deferred to G5; keep an empty Results reply」，
本阶段把它换成「与当前枚举求交后的最近窗口」。

## Activation

WPF 在仍是前台进程时执行：用 `INativeWindowQuery` 复核 handle/pid/可见性与标题指纹 →
必要时 `ShowWindow(SW_RESTORE)` → 复用 `ForceActivate` 的前台路径 → 用
`GetForegroundWindow` 验证结果 → 成功才隐藏自身并发 `RecordWindowSwitch`。

顺序上必须先激活、再隐藏：先隐藏会丢掉前台权限，`SetForegroundWindow` 随即失效。

被系统前台限制拒绝时返回明确失败、保留 UI，不注入、不模拟任意输入、不结束进程。激活逻辑放在
可注入的服务后面（沿用 `INativeWindowQuery` 的接缝风格），让 `Prism.Tests` 能覆盖复核失败、
恢复最小化、激活被拒三条路径而不依赖真实窗口。

## Compatibility And Rollback

`window` 是可选稳定 kind，旧 UI 映射 `Unknown`。功能整体挂在 `>` 解析与 window provider 上；
回滚时移除 provider 与 `>` 入口即可，历史里的 window 条目可保留但不展示。协议、provider、
WPF mode 分层提交，任一层关闭窗口模式都不影响文件搜索。
