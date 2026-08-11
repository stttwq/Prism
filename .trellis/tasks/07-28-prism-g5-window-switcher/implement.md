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
7. [ ] **激活**：新增可注入的窗口激活服务，复核 → 必要时 `SW_RESTORE` → 复用 `ForceActivate`
       路径 → `GetForegroundWindow` 验证。**先激活再隐藏**。成功才隐藏并发
       `RecordWindowSwitch`；失败保留 UI、不写成功历史、不结束进程。
8. [ ] **机器测试**：多窗口同应用、标题变化、关闭竞态、句柄复用、最小化恢复、激活被拒、
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

## 仍未完成（2026-08-11）

- **激活被拒路径未做 A/B。** 上面证明的是「能切成功」；Windows 拒绝时是否正确降级，
  仍需按 `verify-before-claiming-fixed` 拿未修版本做对照，跑一次干净不算证据。
- **端到端未走过一次。** 没有装 Prism、按 `>`、看列表、按 Enter。协议两端与 UI 状态
  机都只在单测里对过，没有一次真实的 pipe 往返。
- **≤100MB 同步采样门未跑**（`Measure-ProcessMemory.ps1`）。
- **cloaked 过滤会漏掉挂起的 UWP。** 实机看到 `SystemSettings "设置"` 与
  `ApplicationFrameHost "设置"` 被判 cloaked 而排除，但 Alt-Tab 会显示它。
  `DWMWA_CLOAKED` 对「本桌面挂起的 UWP」和「其他虚拟桌面的窗口」返回同一个
  `DWM_CLOAKED_SHELL`，当前代码无法区分。行为与 design.md 写的一致，但这是个
  真实的可用性缺口，需要单独决策（可能要配合 `IVirtualDesktopManager`）。

已用 mutation 反验过的断言：把「先激活再隐藏」对调 → 两条测试转红（有效）；
把前缀缓存 guard 删掉 → 测试仍通过（无效，已改写为只断言可观察行为并加对照组）。

## Open Question Carried Into Phase 2

`ResolveWindow` + `RecordWindowSwitch` 是两次往返。若实机发现两次往返之间的延迟足以让前台
权限失效，退路是 WPF 在同一次 UI 线程回调内完成激活、记历史改为 fire-and-forget。先按两次
往返实现并实测。
