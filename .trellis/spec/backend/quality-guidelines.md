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

## Memory Acceptance (≤100MB hard gate)

Task Manager's "内存(专用工作集)" column = **Private Working Set** = `Get-Process <name> | % PrivateMemorySize64`. The ≤100MB gate (design.md) is the **sum of `Prism.exe` + `prism-core.exe`** private working sets, at idle, **after the index is loaded** (pipe `search` returns `is_indexing:false`).

Procedure:
1. **Rebuild both release binaries first** — release artifacts go stale silently: a commit made after the last build is NOT reflected in the exe. `cargo build --release` + `dotnet build src/Prism -c Release`, then **manually copy** `prism-core.exe` beside `Prism.exe` in the publish dir (the csproj has no target that does this).
2. Launch `Prism.exe` (it spawns `prism-core.exe`); confirm `is_indexing:false` via a pipe search before measuring.
3. Sum `PrivateMemorySize64` across both processes vs 100MB. (`WorkingSet64` runs ~10MB higher — private WS is the Task-Manager-matching figure; report both.)

Baseline (2026-07-25, dev machine, full index loaded, idle): total **~38MB private WS** / ~50MB WS — backend ~22MB, frontend ~16MB. Comfortably under the 70/30 split.

---

## Indexing: Known Gaps (post-step-11 review)

Two user-observed issues found after the step-11 install acceptance. Deferred to a follow-up task (no code change yet — recorded here so the next task picks them up).

### Gap A — first-install index-build wait

**Symptom (user-reported, 2026-07-25):** On first install + launch (no disk cache yet), the frontend connects but search returns nothing or partial results until the backend finishes the full-build scan. Users may mistake this for a broken app.

**Mechanism (`index.rs` build_or_load):**
1. Frontend pulls up the backend and connects the named pipe — timeout 10 s, normally **millisecond-level** handshake (`PipeClient.ConnectAsync`, frontend `is_indexing` echo).
2. Backend `load_cache` runs on `spawn_blocking`:
   - **cache present** (restart case): deserialize `data\index.bin` — measured **seconds** (the `加载耗时 {}ms` log line exists but the actual value was not captured in step-10 acceptance; the `~秒级常驻` claim is qualitative). After load completes, `is_indexing=false` and search is fully usable.
   - **cache absent** (first install): falls through to `build_full_index()` — full MFT enumeration across all NTFS volumes. **Dozens of seconds to minutes** depending on file count / MFT permission. During this window `is_indexing=true` and results are partial.

**Why "seconds after restart" is the steady state but "first install" stalls:** cache presence is the difference. Reboot → cache loads in seconds. First install → no cache → full build.

**Improvement routes (light → heavy):**
- **A1 (frontend signal, low cost):** surface the existing `is_indexing` from `results` in the UI — "Prism 正在首次构建全盘索引…请稍候" instead of a bare empty result. No backend change. Already the IPC contract (`results` includes `is_indexing`).
- **A2 (progress):** backend pushes periodic `index_progress{scanned,total_estimate}`; frontend shows a progress bar. `total_estimate` from last scan count stored in cache.
- **A3 (MFT/USN fast path):** real USN Journal path at fast-build — cut first build from minutes to seconds (also GAP B's answer).

**Open measurement:** capture `load_cache` ms on this machine (the log line is there) before deciding whether A1's UI wording is even needed (<2s ⇒ perhaps not).

### Gap B — new-file detection latency (no real-time USN watch)

**Symptom (user-reported, 2026-07-25):** Files created/renamed/deleted after index build do **not** appear promptly in search — up to a **~5-minute** blind window.

**Mechanism:** `build_or_load` only does a **one-shot MFT enumeration** via `FSCTL_ENUM_USN_DATA` to build the index fast; there is **no USN Journal subscription / `FSCTL_READ_USN_JOURNAL` long-poll**. New-file awareness comes solely from `index_refresh_secs` (default **300**, from `Config::default`) triggering **full re-enumeration + whole-index swap** (`build_full_index`) on that interval.

- Worst case latency ≈ `index_refresh_secs`; average ≈ half that.
- Each refresh is a redundant full enumeration (USN could read just the delta). Wasted work on top of the latency.

**Gap vs design.md:** design.md "数据流 / 重要权衡" flags planned real-time increment via USN Journal (needs admin; refuse ⇒ degrade to directory scan). **Step 3 actually shipped only the MFT-enumeration half — the realtime-increment half was not implemented.** This is the documented feature debt.

**Improvement routes (light → heavy):**
- **B1 (tunable, trivial):** lower `index_refresh_secs` default (300 → 60 or 30). Trades periodic full-enum disk/CPU churn for a shorter blind window. Patches the symptom, not the cause.
- **B2 (USN Journal realtime watch, the real fix, matches design.md):** new backend module — spawn a tokio task holding the USN journal's `Min/MaxUsn`, `FSCTL_READ_USN_JOURNAL` long-poll, apply create/rename/delete events as **incremental** updates to the shared index (**not** full swap). New files visible in **seconds**. Requires admin to read the journal; refuse ⇒ fall back to current periodic full rebuild.
- **B3 (ReadDirectoryChangesW fallback):** per-root watch without admin, for the degrade path under B2.

### Recommended next-task shape

One follow-up task covers both gaps coherently because A3 and B2 are the same USN-Journal work:
- **B2** (USN realtime watch) is the centrepiece — fixes the ~5-min blind window *and* gives A3 its fast first-build path.
- **A1/A2** (frontend `is_indexing` surfacing + progress) layered on top for the residual first-install window that even USN fast-build leaves.
- **B1** optional stop-gap if B2 is deferred.
- **Open measurement** (capture `load_cache` ms) should be the first step of that task — it tells us whether A1 wording is needed at all.

---

## Installer / Packaging (step 11)

Executable layout after build lives in **`dist/`** at repo root (the installer source — Inno Setup pulls from there):

1. **Frontend single-file, framework-dependent** — do not self-contain (bloats to ~150MB+). From repo root:
   ```
   dotnet publish src/Prism -c Release -r win-x64 --self-contained false -p:PublishSingleFile=true
   ```
   Output: `src/Prism/bin/Release/net8.0-windows/win-x64/publish/Prism.exe` (~300KB, single file, no `Prism.dll` beside it). Copy to `dist/Prism.exe`.
2. **Backend** — `cargo build --release --manifest-path src/prism-core/Cargo.toml`; copy `src/prism-core/target/release/prism-core.exe` to `dist/`.
3. `dist/prism.ico` (also the setup/uninstall icon).
4. `dist/prism.iss` — Inno Setup **Chinese** wizard. Run `ISCC.exe dist\prism.iss` → `dist\PrismSetup-<ver>.exe`.

Installer conventions baked into the ISS (do not break without reason):
- **Self-start registry** writes `HKCU\...\Run\Prism` = `"<dir>\Prism.exe"` — **same value name and quoting** as `Prism.Services.AutoStartService` so the settings page and installer agree. `PrivilegesRequired=lowest` + `PrivilegesRequiredOverridesAllowed=dialog` → installing under Program Files auto-prompts UAC; the exe itself stays `asInvoker` (MFT ascent handled by backend).
- **Do NOT pre-create `data`** in the ISS. Data-dir resolution happens at Prism first launch (`config::resolve_data_dir`): install dir `\data` if writable (e.g. `D:\工具\Prism`), else `%LocalAppData%\Prism` (Program Files case). `[UninstallDelete]` removes `{app}\data` only; user-data-folder content survives uninstall.
- `CloseApplications=force` + an `[UninstallRun]` `taskkill Prism.exe`/`prism-core.exe` before deleting — tray-resident process would otherwise lock the exe.

Install acceptance (prd.md R7): run the produced setup twice — once into a **Chinese-named dir** (`D:\工具\Prism`; verify index appears under the install dir `\data`) and once into **Program Files** (UAC; verify index lands in `%LocalAppData%\Prism` and the settings page shows that path). This double-install is the gate for step 11; it requires Inno Setup + the real setup exe, so it runs locally, not in the dev session.

---

## Code Review Checklist

- [ ] Pipe write always paired with a full line read
- [ ] Heavy work off the async pipe path
- [ ] New result kinds covered in frontend parse / icons (`web` skips shell path icons)
- [ ] No debug-only logging that floods stderr in hot paths
- [ ] Merge order: web (if keyword) > apps > files; without keyword apps > files
