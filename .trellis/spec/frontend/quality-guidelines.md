# Quality Guidelines

> Code quality standards for Prism WPF frontend (`src/Prism`).

---

## Overview

C# / .NET 8 / WPF search UI talks to `prism-core` over a single named pipe. UI must stay responsive; search correctness depends on sequence numbers and complete pipe I/O.

---

## Forbidden Patterns

- **Passing `CancellationToken` into pipe read after the request line was written.** Cancel only after the matching response line is consumed (`PipeClient.SendAsync`).
- **Treating non-empty results as “index ready”.** When `is_indexing=true`, keep polling so file hits can join apps that already showed up.
- **Assuming `Activate()` always succeeds** for a tray-less / terminal-launched borderless window. Idle Esc/deactivate can fail without `ForceActivate` / window-level `PreviewKeyDown`.
- **Loading `.lnk` icons with `SHGFI_USEFILEATTRIBUTES` only.** Real paths must let the shell resolve the shortcut icon.
- **Leaving `NotifyIcon` alive after exit.** Always `Visible=false` + `Dispose` in `OnExit` / tray service dispose; otherwise the ghost icon stays until hover.
- **Writing unquoted paths into HKCU Run.** Paths with spaces or Chinese must be `"C:\…\Prism.exe"` form.

---

## Required Patterns

- Debounce input (~50ms); bump `_searchSeq` on query clear / show reset so in-flight polls die.
- Discard search responses unless `seq`, current `Query`, and `resp.Query` all match.
- Keep result-panel geometry stable while replacing a non-empty result set: do not add the transient `搜索中…` status row when rows are already visible. Empty-result searches may show it; `is_indexing` progress must remain visible.
- Treat one search response as a burst of `AppState.PropertyChanged` notifications. `AnimatePanelHeight(target)` must not restart an in-flight animation when `target` is unchanged, and identical `ResultList.StatusMessage` assignments must be layout no-ops.
- While `is_indexing`, show status and poll (~0.5s, ~15s cap); do not start nested polls from poll callbacks.
- Apply every matching poll response, including an empty `items` array, because
  it may carry newer volume progress or the terminal `is_indexing=false` state.
  At the polling cap, keep the last real progress text visible until a later
  search replaces it.
- Never seed or locally narrow the prefix cache from `is_indexing=true` results;
  partial-volume completeness is independent of `is_truncated`.
- Icons: existing path → no `USEFILEATTRIBUTES`; missing path → attributes fallback.
- `kind=web` / `kind=more`: do **not** pass `ExecuteId` to shell icon APIs (`ExecuteId` for web is a URL).
- `RevealSelected` must skip `kind` of `more` and `web` (no folder to reveal).
- Tray app uses `ShutdownMode.OnExplicitShutdown`; only tray「退出」/ explicit `Shutdown()` ends the process.
- On startup, apply `settings.AutoStart` to the registry so Run key matches settings after path changes.
- Settings save must re-`Apply` hotkeys immediately (`HotkeyService.Apply`) and push engines via pipe `reload_engines` when connected.
- Hotkey recorder output must use names `HotkeyService.ParseCombo` accepts (`Key` enum names: `Space`, `D1`, `OemComma`, …); require at least one modifier for global combos.
- WPF + WinForms coexistence: prefer WPF types via `GlobalUsings.cs`; do not scatter per-file aliases.
- Tray `NotifyIcon` / menu callbacks may fire off the WPF UI thread — marshal with `Dispatcher.BeginInvoke` before touching windows or view-models.
- Own the tray `Icon` instance (clone pack resource / system icon) and dispose it after `NotifyIcon`.
- `IconCache` must cap entries (FIFO ~128); key by extension for ordinary files, full path only for `.lnk`/`.exe`; dirs use a shared `dir:` key. Avoid `Directory.Exists` on the UI decoration path for every ordinary file — only probe FS for extension-less paths.
- `Clear()` when the search window hides.
- After hide: drop result list + icon bitmaps and idle `GC.Collect(Optimized)` so tray-resident WS shrinks; never collect on the input/search hot path.
- Prefer workstation GC (`ServerGarbageCollection=false`) for the WPF tray process.
- Create `SearchWindow` lazily on first show; after hide / settings close / backend connect idle, call `EmptyWorkingSet` so task-manager WS reflects tray-resident footprint (does not reduce private bytes).
- Theme: only `ThemeWatcher` writes `AppState.Theme` and swaps the Tokens.* resource dictionary; ResultList must `InvalidateThemeBrushes()` on theme change so MatchSpans colors update.
- Pin: `IsPinned=true` blocks deactivate-hide; Esc still hides. PinButton sits outside the card (margin -12) and must not be clipped by the card `RectangleGeometry`.
- Actions: enter only for `kind` file/folder via →; ← / Esc leaves Actions back to Results (restore prior query). Pipe `actions` / `run_action` pair like other requests (never abandon mid-flight).
- Visual tokens live in `Themes/Tokens.*.xaml`; controls must use `DynamicResource` keys from frontend-spec §5 — no hardcoded hex in SearchWindow/ResultList/ActionPanel.

---

## Testing Requirements

- Manual: with a full first page visible, type `abc` and backspace to `ab`; assert the status row does not appear between responses, the panel height stays constant, shared row icons do not clear, and no same-target height animation restarts.

- Manual: type/backspace quickly (`clash` ↔ `cla`) — list and highlight match current text.
- Manual: empty query Esc / click-away hides window.
- Manual: app name (e.g. Chrome) appears as first `app` row and launches on Enter.
- Manual: `bi 天气` shows a top `web` row; Enter opens default browser to Bing; Ctrl+Enter does nothing harmful for web.
- Manual: tray icon visible; left-click toggles search; right-click 打开设置 / 重建索引 / 退出 work; 退出 removes tray icon.
- Manual: settings「开机时自动启动」toggles `HKCU\…\Run\Prism` and persists in settings.json.
- Manual: settings 改快捷键为组合键后立即生效；改网页引擎保存后无需重启即可用新关键词。
- Manual: 设置页「数据目录」显示实际可写路径。
- Manual: 系统切换深/浅色后搜索窗背景与文字随之切换。
- Manual: 右上角固定按钮点击后失焦不隐藏；再点取消；Esc 仍隐藏。
- Manual: 选中文件按 → 出现动作列表（打开所在文件夹/复制/剪切/复制路径）；← 或 Esc 返回结果；回车执行后隐藏。
- Manual: 窗口圆角不裁切阴影、列表展开有短动画、"展示更多"行有蓝底图标。
- `dotnet build src/Prism` clean (close running `Prism.exe` first if MSB3027 file lock).

---

## Code Review Checklist

- [ ] Pipe client never abandons a half-finished request/response pair
- [ ] Indexing poll continues when apps returned but files still loading
- [ ] Progress-only and empty-item poll responses refresh status; polling cap retains the last real progress
- [ ] Partial-index responses never seed the prefix cache, regardless of generation or truncation fields
- [ ] Focus/hide works in Idle and Results
- [ ] No silent swallow of execute/reveal/run_action errors without status text
- [ ] NotifyIcon disposed on exit; Run key path is quoted
- [ ] Theme change refreshes ResultList brushes + PinButton background
- [ ] → only handled for file/folder selection; Action errors show under ActionStatus
- [ ] G8: web mode keyword produces direct result immediately; suggestions default off; late/cancelled/timeout responses do not overwrite newer results; web mode results never seed the prefix cache; favicon authorization independent of suggestions
