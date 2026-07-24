# Quality Guidelines

> Code quality standards for Prism backend (`prism-core`) and IPC boundary.

---

## Overview

Rust backend + named-pipe JSON protocol. Prefer small modules, no panics on the request path, and keep pipe request/response strictly paired.

---

## Forbidden Patterns

- **Cancel mid-read on the named pipe after a request was written.** Leaving an unread response desyncs the next request (UI shows wrong query highlights / stale lists). Cancellation may only drop the *business* result after the line is fully read.
- **Blocking index/app scan on the async pipe runtime thread.** Use `spawn_blocking` + multi-thread Tokio; otherwise search returns empty / hangs.
- **Per-`.lnk` `CoInitialize`/`CoCreateInstance` in a tight scan loop.** Reuse one COM `IShellLink` for the whole Start Menu scan.
- **Forcing a full disk rebuild when a valid `index.bin` cache loads.** Cache-first; refresh on timer.

---

## Required Patterns

- Search merge order: **apps (`kind=app`) first**, then files/folders. Respect `max`; leave room for files when `max` is large.
- `results` JSON must include **`is_indexing`** so the UI can poll until the file index is ready even if apps already returned.
- Path validation for `execute`/`reveal`: reject empty, NUL, and relative paths.
- Match spans: UTF-16 code unit offsets (WPF `string` indexing).

---

## Testing Requirements

- Unit tests for apps search scoring (exact > prefix > contains) and Chinese names.
- IPC tests: empty index `is_indexing=true`; ready empty index `is_indexing=false`; apps appear with `kind=app`.
- `cargo test --manifest-path src/prism-core/Cargo.toml` must stay green before calling a step done.

---

## Code Review Checklist

- [ ] Pipe write always paired with a full line read
- [ ] Heavy work off the async pipe path
- [ ] New result kinds covered in frontend parse / icons
- [ ] No debug-only logging that floods stderr in hot paths
- [ ] Spec/design order (apps > files) still holds after merge changes
