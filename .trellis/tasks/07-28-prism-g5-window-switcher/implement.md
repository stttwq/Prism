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

## 步骤 7 / 8 的未完成项（2026-08-11）

代码已完整落地并通过四道质量门（Rust 194、C# 101、clippy、Release build 全绿），
但下面两项属于**实机验收**，静态测试无法收口，所以 7、8 保持未勾选：

- **真实前台切换未在实机验证。** 单测覆盖的是接缝内分支：resolve 失败、激活被拒、
  最小化状态、失败不写历史。`SetForegroundWindow` 真正是否夺到前台，只有实机能证。
- **激活被拒路径需要 A/B。** 按 `verify-before-claiming-fixed`，这类偶发路径要拿
  未修版本做对照；跑一次干净不构成证据。
- **长时间反复查询的内存表现未测。** 枚举有 512 上限且 snapshot 整表替换（
  `publishing_replaces_rather_than_accumulates` 覆盖了逻辑层面不累积），但
  `Measure-ProcessMemory.ps1` 的 ≤100MB 同步采样门未跑。

已用 mutation 反验过的两条断言：把「先激活再隐藏」对调 → 两条测试转红（有效）；
把前缀缓存 guard 删掉 → 测试仍通过（无效，已改写为只断言可观察行为并留注释）。

## Open Question Carried Into Phase 2

`ResolveWindow` + `RecordWindowSwitch` 是两次往返。若实机发现两次往返之间的延迟足以让前台
权限失效，退路是 WPF 在同一次 UI 线程回调内完成激活、记历史改为 fire-and-forget。先按两次
往返实现并实测。
