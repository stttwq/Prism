# G5 窗口切换器

## Goal

增加 `>` 前缀窗口模式，让用户搜索并切换当前可切换的可见顶层窗口，并用本地历史改善同等级排序；不扩展为窗口控制或进程管理工具。

## Requirements

- 强依赖 G2，复用 G1 排名/kind/测试地基和 G2 历史/拼音。
- 输入 `>` 进入窗口模式；匹配窗口标题和应用名，支持字面、全拼、首字母及同级历史加权。
- 只枚举当前用户会话中可切换的可见顶层窗口；排除 Prism 自身、无标题窗口、工具窗口、不可激活后台窗口和已销毁句柄。
- 窗口列表按请求实时枚举，不长期缓存 HWND；结果使用稳定 `window` kind 和仅本次枚举有效的 opaque target。
- execute 前重新验证 HWND、PID、可见性和目标身份，防止句柄复用或窗口关闭竞态。
- 成功切换后记录窗口历史并隐藏 Prism；失败显示可恢复错误，不写成功历史、不结束目标进程。
- 窗口历史本地持久化，但展示时必须与当前枚举求交集；空输入只显示仍存在的最近窗口。
- 拼音高亮映射窗口标题汉字；应用名与标题的匹配来源可解释。
- 只提供搜索和切换，不提供关闭、最小化、最大化、置顶、分屏、跨屏、结束进程或多选。

## Acceptance Criteria

- [x] `>`、`>query`、退出窗口模式和空输入最近窗口有状态测试。
- [x] 多窗口同应用、标题变化、窗口关闭、句柄复用、最小化恢复和激活失败有自动化或机器测试。
- [x] Prism 自身、工具窗口、无标题和不可切换窗口被稳定过滤。
- [x] 字面/拼音/历史排名遵守 G2 等级，中文高亮正确。
- [x] 持久历史不会展示已关闭窗口，也不持久化可复用 HWND 作为稳定身份。
- [x] 成功切换隐藏 Prism，失败保留可操作 UI 且不写成功历史。
- [x] 长时间反复查询不会因常驻窗口列表或事件订阅造成持续内存增长。
- [x] Rust tests、Clippy、C# tests 和 WPF Release build 全部通过。

### 验收证据（2026-08-12）

逐条对应到具名测试，避免「感觉做完了」就打勾。详细过程见 `implement.md`。

| 验收条 | 证据 |
|---|---|
| 1 状态测试 | `WindowPrefixSendsWindowModeAndStripsThePrefix`、`BareWindowPrefixListsRecentWindowsInsteadOfGoingIdle`、`LeavingWindowModeReturnsToGlobalSearch`、`NoMatchingWindowIsDistinctFromNoRecentWindows` |
| 2 多窗口/标题/关闭/复用/最小化/激活失败 | `two_windows_of_the_same_app_stay_distinct`、`title_change_alone_does_not_invalidate_the_target`、`closed_window_resolves_to_window_gone`、`recycled_handle_with_a_new_pid_is_rejected`、`RestoresAMinimizedWindowBeforeActivating`（实机）、`RejectedActivationKeepsTheUiAndWritesNoSuccessHistory` |
| 3 过滤 | `prism_own_windows_are_filtered`、`tool_window_is_filtered`、`untitled_and_whitespace_only_windows_are_filtered`、`owned_window_is_filtered`、`invisible_window_is_filtered`、`null_handle_is_filtered`、`uwp_inner_core_window_is_filtered`；实机 `live_rejection_breakdown` 分类了 205 个被拒窗口 |
| 4 排名/高亮 | `literal_title_match_beats_pinyin_match`、`chinese_title_highlight_uses_utf16_offsets`、`history_breaks_ties_within_the_same_match_tier`、`pinyin_disabled_drops_pinyin_only_hits` |
| 5 历史不复活已关闭窗口 | `empty_query_lists_only_windows_that_have_history`、`empty_query_hides_a_remembered_window_once_it_is_closed`、`history_key_never_contains_the_handle` |
| 6 成功隐藏/失败保留 | `SuccessfulSwitchActivatesThenHidesAndRecordsHistory`、`RejectedActivationKeepsTheUiAndWritesNoSuccessHistory`、`FailedHistoryWriteDoesNotTurnASuccessfulSwitchIntoAFailure` |
| 7 无持续内存增长 | `scripts/g5-memory-soak.ps1`：400 次 +244KB / 1200 次 +424KB，×3 查询只换 ×1.7 增长且序列震荡 |
| 8 四道门 | Rust 206 passed / clippy 干净 / C# 101 passed, 5 skipped / build 0 警告 0 错误 |
| 端到端 | `scripts/g5-pipe-probe.ps1` 21/21，真实 pipe 往返（用户另行手测确认 UI 正常） |

第 5 条原先**无法被断言**：`window_search` 里直接调 `enumerate_and_publish`，测试进不去真实桌面。
已把排名部分抽成 `rank_window_list`，「历史 ∩ 当前枚举」这条约定才有了落点。
Mutation 反验：删掉 `history_score == 0` 那道门 → `empty_query_lists_only_...` 转红。

## Out Of Scope

- 关闭、最小化、最大化、置顶、分屏、跨屏和进程结束；
- 虚拟桌面管理；
- 提权窗口控制；
- 长期持久化 HWND。

## Dependencies

强依赖 G2。
