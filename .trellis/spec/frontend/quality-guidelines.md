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

---

## Required Patterns

- Debounce input (~50ms); bump `_searchSeq` on query clear / show reset so in-flight polls die.
- Discard search responses unless `seq`, current `Query`, and `resp.Query` all match.
- While `is_indexing`, show status and poll (~0.5s, ~15s cap); do not start nested polls from poll callbacks.
- Icons: existing path → no `USEFILEATTRIBUTES`; missing path → attributes fallback.

---

## Testing Requirements

- Manual: type/backspace quickly (`clash` ↔ `cla`) — list and highlight match current text.
- Manual: empty query Esc / click-away hides window.
- Manual: app name (e.g. Chrome) appears as first `app` row and launches on Enter.
- `dotnet build src/Prism` clean (close running `Prism.exe` first if MSB3027 file lock).

---

## Code Review Checklist

- [ ] Pipe client never abandons a half-finished request/response pair
- [ ] Indexing poll continues when apps returned but files still loading
- [ ] Focus/hide works in Idle and Results
- [ ] No silent swallow of execute/reveal errors without status text
