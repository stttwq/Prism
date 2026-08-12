# Implementation Plan

分层提交，每层可独立回滚。顺序：协议 → broker 枚举 → 复核 → 排序 → 空输入 → WPF mode → 激活 → 机器测试 → 质量门。

1. [x] **协议层**：`Request::Search` 加 `mode`（缺失 = `all`）；`SearchResultKind` 加 `Window`；
       新增 `ResolveWindow` / `RecordWindowSwitch` 与 `Response::WindowHandle`。补旧读者兼容
       测试（不带 `mode` 的 payload 逐字节等价于今天的行为）。
2. [x] **枚举层**：新建 `src/prism-core/src/window_list.rs`，实现过滤规则、有界 snapshot、
       带世代号的 opaque token。过滤规则每条一个单元测试；HWND 不出 broker 边界。
3. [x] **复核层**：`ResolveWindow` 解析 token 时核对 HWND/PID/可见性/标题指纹，覆盖句柄复用与
       窗口已关闭两条竞态。
4. [x] **排序层**：标题与应用名分别接入 `pinyin::match_name`、`rank_title`、`match_spans` 与
       `history.score`，取更强来源，复用 G2 等级与同级历史加权。
5. [x] **空输入**：把 `ipc.rs::search_service` 里 G5 预留分支换成「历史 ∩ 当前枚举」的最近窗口。
6. [x] **WPF mode**：`>` 首字符解析为 Window mode，写进 `SearchContext.Mode`，接上
       `PipeClient.SearchPayload`（该字段今天存在但没上线）；`SearchResult.ResultKind` 加
       `Window` 映射；补 `>` 退出与空输入状态。
7. [x] **激活**：新增可注入的窗口激活服务，复核 → 必要时 `SW_RESTORE` → 复用 `ForceActivate`
       路径 → `GetForegroundWindow` 验证。**先激活再隐藏**。成功才隐藏并发
       `RecordWindowSwitch`；失败保留 UI、不写成功历史、不结束进程。
8. [x] **机器测试**：多窗口同应用、标题变化、关闭竞态、句柄复用、最小化恢复、激活被拒、
       长时间反复查询无内存增长。
9. [x] **质量门 + spec**：跑下面全套命令；把「窗口激活是 broker-owned Shell 边界的显式例外」
       与窗口枚举/状态机约定写回 spec（Phase 3.3）。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test src/Prism.Tests/Prism.Tests.csproj -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

## Review Gates And Rollback

- 前台激活必须在 Windows 11 x64 实机通过，不允许用注入规避系统限制。
- 步骤 7 是唯一无法靠单元测试收口的部分：接缝内的分支可测，真实前台切换必须实机验收。
  按 `verify-before-claiming-fixed`，激活被拒这类偶发路径要拿未修版本做 A/B，不靠「跑一次没错」。
- 协议、provider、WPF mode 分开提交；任一层可关掉窗口模式而不影响文件搜索。
- 无持续内存增长、历史过滤测试通过后方可归档。

## 实机探针结果（2026-08-11）

发现原先「四道门全绿」掩盖了一个更大的空洞：两条 `cfg(windows)` 路径**从未执行过**
——所有窗口测试都注入 fake。补了两组实机探针（默认 ignore/skip，不进常规 gate）。

**枚举（`window_list::tests::live_enumeration_probe`）—— 已验证真实执行。**
`EnumWindows` 返回 227 个顶层窗口 → 保留 2 个（Terminal、Firefox，与桌面实况一致；
Opus 常驻托盘所以只有隐藏窗口）。token 数值化、自身进程被排除、resolve 往返成功。

`live_rejection_breakdown` 的分类结果证明过滤规则没有过度拒绝：
205 个 invisible 全是 `Default IME` / `MSCTFIME UI` / `DDE Server Window` /
`GDI+ Window` 这类管道窗口，`Program Manager`（桌面）被 tool_window 正确拦下。

**激活（`WindowActivatorLiveTests`）—— 已验证真实夺到前台。**
第一次跑是假绿：探针选中的窗口本来就在前台，`TryActivate` 走早返回分支，
`AttachThreadInput` 回退根本没执行。加 `excludeForeground` 后重跑：
`before=0x2C0980 → after=0x40079C`，前台确实从非前台调用方切走了。
死句柄与 pid 不匹配两条也验证了：拒绝且不动前台。

## 仍未完成（2026-08-11）→ 已于 2026-08-12 全部收口

- ~~**激活被拒路径未做 A/B。**~~ 已补，见「步骤 8 收口」的激活被拒一节。
- ~~**端到端未走过一次。**~~ 已装到 D 盘临时目录并跑通 `scripts/g5-pipe-probe.ps1`（20/20），
  用户另行手测确认 UI 正常。
- ~~**≤100MB 同步采样门未跑**（`Measure-ProcessMemory.ps1`）。~~ 那个脚本**从来不存在**，
  只在本文件里被引用过。而且 PRD 的原话是「无持续内存增长」，不是「≤100MB」——
  是**趋势**而不是阈值。改用 `scripts/g5-memory-soak.ps1`，见下。
- **cloaked 过滤漏掉挂起的 UWP —— 已修（2026-08-12）。** 见下节。

已用 mutation 反验过的断言：把「先激活再隐藏」对调 → 两条测试转红（有效）；
把前缀缓存 guard 删掉 → 测试仍通过（无效，已改写为只断言可观察行为并加对照组）。

## 挂起 UWP 修复（2026-08-12）

**根因不是「cloaked 判错」，是「cloaked 被当成一条规则」。** `DWMWA_CLOAKED` 对两种完全
不同的情况返回同一个 `DWM_CLOAKED_SHELL`：本桌面上挂起的 UWP（Alt-Tab 显示，必须可切）
和停在其他虚拟桌面的窗口（必须排除）。原来 `is_cloaked: bool` 把两者压成一个值，于是
所有挂起的 UWP 被丢掉。

改法：`RawWindow.cloaked` 存原始 bits；只有 `DWM_CLOAKED_APP` 在过滤里排除；
「是否在其他虚拟桌面」改由 `IVirtualDesktopManager` 直接回答。该接口在已启用的
`Win32_UI_Shell` feature 里，**没有新增依赖**。COM 每次枚举初始化一次（复用
`apps.rs` 的 `LnkResolver` 模式），不是每窗口一次。

**失败一律按「在本桌面」处理。** COM 起不来、`CoCreateInstance` 失败、
`IsWindowOnCurrentVirtualDesktop` 返回 `E_INVALIDARG`（shell 不跟踪的窗口，或
HWND 在枚举中途死掉）——全部报 false。多列一个切不过去的窗口是轻得多的 bug，
静默弄丢用户要找的窗口才是这次要修的那个。

### 测量先于修改，测量改变了修法

按 `verify-before-claiming-fixed`，先给探针加了「shell-cloaked 但保留」的输出再看实机，
结果第一版修法立刻暴露两个新问题：

```
shell-cloaked but kept (3):
  cloaked=0x2 SystemSettings       "设置"            <- 内层 CoreWindow，重复项
  cloaked=0x2 ApplicationFrameHost "设置"            <- Alt-Tab 显示的就是这个
  cloaked=0x2 TextInputHost        "Windows 输入体验" <- Alt-Tab 从不显示
```

单靠 cloak bits 分不开，用 `GetClassNameW` 才能：外壳是 `ApplicationFrameWindow`，
内层与输入法都是 `Windows.UI.Core.CoreWindow`。一条规则同时解决重复和误列：
排除 `CoreWindow`。**要点是这条规则不是想出来的，是量出来的**——只跑单测的话，
会交付一个把「设置」列两遍、还多一个切不过去的输入法窗口的版本。

### A/B（未修版本做对照）

| | 旧策略 `cloaked == 0` | 新策略 |
|---|---|---|
| 实机枚举 | **2** 个，无「设置」 | **3** 个，含「设置」 |
| 保留的 shell-cloaked | 0 | 1（仅 `ApplicationFrameHost`） |
| `suspended_uwp_..._stays_switchable` | **FAILED** | 通过 |

回归测试在未修版本上转红，所以是可证伪的，不是「跑一次干净」。

### 列出来必须真能切

新增 C# 探针 `ActivatesASuspendedUwpWindow`：shell-cloaked 窗口正是
`SetForegroundWindow` 最可能拒绝的地方，列进候选却切不过去等于没修。实测
`cloaked=0x2`，`before=0x1206C6` → `after=0x3073E`，真的拿到了前台。

四道门：Rust 202 passed（+8）/ clippy 干净 / C# 101 passed, 4 skipped / build 0 警告 0 错误。

### 已知边界

- `TextInputHost` 这类裸 `CoreWindow` 一并被排除。若将来有应用只有 CoreWindow
  而没有 frame host 且需要被切，这条规则会漏掉它 —— 目前实机上没有这种情况。
- 虚拟桌面判定依赖 `IVirtualDesktopManager` 可用；不可用时会列出其他桌面的窗口，
  并记一条日志。

## 步骤 8 收口（2026-08-12）

七项逐条对账。**已被覆盖的四项**先说清楚，免得重复造探针：

| 项 | 覆盖处 | 性质 |
|---|---|---|
| 标题变化 | `resolve` 的标题指纹复核（步骤 3） | 单测 |
| 关闭竞态 | `window_..._resolves_to_window_gone` + 实机死句柄探针 | 单测 + 实机 |
| 句柄复用 | `PidMismatchIsRejectedEvenWhenTheHandleIsAlive` | 实机 |
| 激活被拒（ViewModel 层） | `RejectedActivationKeepsTheUiAndWritesNoSuccessHistory` 等三条 | 单测 |

剩下三项是真缺口，本次补齐。

### 多窗口同应用

同应用多窗口靠 `history_key`（`app_name` 小写 + 规范化标题）区分，此前**一条测试都没有**。
补两条，一条正向一条钉边界：

- `two_windows_of_the_same_app_stay_distinct`：两个 Notepad 开不同文档 → 两个条目、
  两个 HWND、两个不同的 history key。若压成一个键，切到 a.txt 会把 b.txt 也标记成
  「最近用过」，最近列表从此指错窗口。
- `same_app_same_title_deliberately_shares_one_history_key`：两个**未命名** Notepad 确实
  共享一个键。这是刻意的——标题是唯一稳定的区分信号，HWND 不能持久化。写成测试是为了
  防止后人把它当 bug「修」掉。

Mutation 反验（把 `history_key` 里的标题整个删掉）：`two_windows_..._stay_distinct` 转红，
而**原有三条 `history_key` 测试全部照绿**——标题从键里消失，它们一条都没察觉。
同一个 mutation 下 `same_app_same_title_...` 也照绿（「相等」正是它断言的东西），
所以按 state-management.md 的规矩，已在它的注释里写明「这条是文档不是测试」。

### 最小化恢复：`SW_RESTORE` 第一次真的执行

此前所有实机探针都挑正常显示的窗口，`IsIconic` 为假 → `ShowWindow` 那行**从未运行过**。
新增 `RestoresAMinimizedWindowBeforeActivating`：主动最小化目标再切回来。

实机：`IsWindowVisible while minimized = True`（这条也断言了——最小化的窗口若不算 visible，
`TryActivate` 第 32 行的可见性检查会先把它拒掉，那 restore 分支就是永远到不了的死代码），
`before=0x6F04E6 → after=0x1904FC`，`iconic: True → False`。

**A/B 挖出一个真问题。** 删掉 `SW_RESTORE` 重跑：

```
iconic   = True          <- 窗口还是最小化的
returned = True          <- 但激活「成功」了
```

Windows 把一个仍然最小化的窗口设成了前台，`GetForegroundWindow() == handle` 成立。
**只按现有契约（返回值 == 前台实况）断言的话，这个版本是绿的**——用户按下 Enter，
什么也没发生，测试却说切成功了。是那条额外的 `Assert.False(stillIconic)` 抓住的。
探针里多写一条断言的成本，和交付一个「切了但没显示」的版本的成本，不在一个量级。

### 激活被拒：拿掉 `AttachThreadInput` 兜底做对照

之前证明的「拒绝」都是**我们自己的**复核在拒（死句柄、pid 不匹配）。**Windows 自己拒绝**
从未被观察过，这正是 `verify-before-claiming-fixed` 点名的那类偶发路径。

删掉 `AttachThreadInput` 兜底后实机重跑，非前台调用方的 `SetForegroundWindow` 真的被拒：

| | 兜底完整 | 删掉兜底 |
|---|---|---|
| `before → after` | `0x1904FC → 0x6F04E6`（切走了） | `0x6F04E6 → 0x6F04E6`（**没动**） |
| `TryActivate` 返回 | True | **False** |

两件事同时成立：兜底不是装饰（没它就真切不过去），且被拒时返回 False 而不是谎报成功。
ViewModel 层拿到 False 之后的行为（保留 UI、不写成功历史、不结束进程）已由上表那三条单测覆盖。

### 长时间反复查询无内存增长

`scripts/g5-memory-soak.ps1`（新建，替代那个不存在的 `Measure-ProcessMemory.ps1`）。
先跑 20 次预热再定基线，否则会把一次性的堆增长当成泄漏；查询在
`e / a / 空 / zzzz-no-match / o / set` 之间轮换，命中数不同、路径不同，有 token 就顺手 resolve。

| 查询数 | 增长 | 稳态 |
|---|---|---|
| 400 | 244KB | ~26MB |
| 1200 | 424KB | ~26MB |

**判据是形状，不是端点。** 查询数 ×3，增长只 ×1.7；线性泄漏应当预测约 730KB。
且序列在震荡（第 1150 个采样点比 1100 低），是平台期而不是斜坡。这印证了
`WindowSnapshotStore` 的设计主张：只持有最新一次枚举，整体替换。

顺带纠正一条记在本文件里的错账：门槛原文是「无持续内存增长」，一直被我读成「≤100MB」。
单个采样点无法否证「无持续增长」——一个每次查询漏 2MB 的 broker，在第一个采样点上也很好看。

四道门：Rust 204 passed（+2）/ clippy 干净 / C# 101 passed, 5 skipped / build 0 警告 0 错误。

## Open Question Carried Into Phase 2

`ResolveWindow` + `RecordWindowSwitch` 是两次往返。若实机发现两次往返之间的延迟足以让前台
权限失效，退路是 WPF 在同一次 UI 线程回调内完成激活、记历史改为 fire-and-forget。先按两次
往返实现并实测。
