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
- **Forcing a full disk rebuild when a valid `index.bin` cache loads.** Cache-first; refresh on timer. Bump cache version when `IndexEntry` / pool layout changes so old files are discarded, not mis-parsed.
- **Storing full paths without interning parent directories.** Use dir+name intern (cache v3): sibling files share one parent string; identical filenames share one name string. Reconstruct full path only when returning search hits.
- **Storing filename + lowercased filename as separate pool strings.** Match case-insensitively without a stored lower copy.
- **Indexing high-noise trees** (`node_modules`, `.git`, `WinSxS`, `Windows\Installer`, temp/package caches) — they dominate entry count and memory.

---

## Required Patterns

- Search merge order when query has **no** web-engine keyword: **apps (`kind=app`) first**, then files/folders. Respect `max`; leave room for files when `max` is large.
- When the query matches a web-engine keyword (`bi`/`b`/`g` or custom from `settings.json` `WebEngines`): insert one **`kind=web`** hit **first**, then apps, then files. Longer keywords win (`bi` before `b`).
- `execute` for `kind=web`: `execute_id` is an `http(s)` URL — open via ShellExecute **without** path validation (absolute-path checks reject URLs). File/app execute still validates paths.
- `results` JSON must include **`is_indexing`** so the UI can poll until the file index is ready even if apps already returned.
- Path validation for file/app `execute`/`reveal`: reject empty, NUL, and relative paths.
- Match spans: UTF-16 code unit offsets (WPF `string` indexing).
- Web engines load at backend start from shared `settings.json`; empty/missing list falls back to defaults (Bing-first: `bi`, `b`, `g`).
- `reload_engines` IPC replaces the in-memory list immediately (RwLock); empty list falls back to defaults. Accept PascalCase engine fields from the frontend.
- `actions` / `run_action`: path validation same as execute/reveal (absolute only). First-version actions are exactly four: `open_folder` (explorer /select), `copy`/`cut` (CF_HDROP + Preferred DropEffect), `copy_path` (CF_UNICODETEXT). Do not ship placeholder "快捷菜单" rows that cannot run. On clipboard `SetClipboardData` failure, `GlobalFree` the unowned `HGLOBAL` (system only takes ownership after success).

---

## Testing Requirements

- Unit tests for apps search scoring (exact > prefix > contains) and Chinese names.
- IPC tests: empty index `is_indexing=true`; ready empty index `is_indexing=false`; apps appear with `kind=app`.
- Web-search tests: Chinese query URL-encoding; `bi` vs `b`; custom engine list; `execute` https not rejected as relative path; web row sorts before apps when keyword matches.
- `reload_engines` tests: replace list affects subsequent search; empty list falls back to defaults; PascalCase fields parse.
- Actions tests: list_actions rejects relative; basics present; unknown run_action errors; IPC `actions` / `run_action` wiring.
- `cargo test --manifest-path src/prism-core/Cargo.toml` must stay green before calling a step done.

---

## Code Review Checklist

- [ ] Pipe write always paired with a full line read
- [ ] Heavy work off the async pipe path
- [ ] New result kinds covered in frontend parse / icons (`web` skips shell path icons)
- [ ] No debug-only logging that floods stderr in hot paths
- [ ] Merge order: web (if keyword) > apps > files; without keyword apps > files
