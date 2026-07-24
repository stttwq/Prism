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
- While `is_indexing`, show status and poll (~0.5s, ~15s cap); do not start nested polls from poll callbacks.
- Icons: existing path → no `USEFILEATTRIBUTES`; missing path → attributes fallback.
- `kind=web` / `kind=more`: do **not** pass `ExecuteId` to shell icon APIs (`ExecuteId` for web is a URL).
- `RevealSelected` must skip `kind` of `more` and `web` (no folder to reveal).
- Tray app uses `ShutdownMode.OnExplicitShutdown`; only tray「退出」/ explicit `Shutdown()` ends the process.
- On startup, apply `settings.AutoStart` to the registry so Run key matches settings after path changes.
- WPF + WinForms coexistence: prefer WPF types via `GlobalUsings.cs`; do not scatter per-file aliases.
- Tray `NotifyIcon` / menu callbacks may fire off the WPF UI thread — marshal with `Dispatcher.BeginInvoke` before touching windows or view-models.
- Own the tray `Icon` instance (clone pack resource / system icon) and dispose it after `NotifyIcon`.

---

## Testing Requirements

- Manual: type/backspace quickly (`clash` ↔ `cla`) — list and highlight match current text.
- Manual: empty query Esc / click-away hides window.
- Manual: app name (e.g. Chrome) appears as first `app` row and launches on Enter.
- Manual: `bi 天气` shows a top `web` row; Enter opens default browser to Bing; Ctrl+Enter does nothing harmful for web.
- Manual: tray icon visible; left-click toggles search; right-click 打开设置 / 重建索引 / 退出 work; 退出 removes tray icon.
- Manual: settings「开机时自动启动」toggles `HKCU\…\Run\Prism` and persists in settings.json.
- `dotnet build src/Prism` clean (close running `Prism.exe` first if MSB3027 file lock).

---

## Code Review Checklist

- [ ] Pipe client never abandons a half-finished request/response pair
- [ ] Indexing poll continues when apps returned but files still loading
- [ ] Focus/hide works in Idle and Results
- [ ] No silent swallow of execute/reveal errors without status text
- [ ] NotifyIcon disposed on exit; Run key path is quoted
