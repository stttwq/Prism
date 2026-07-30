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

- Search ranking when query has **no** web-engine keyword: apps, files, and folders
  compete in one global order: exact name, prefix, contains position, centralized
  score, then stable name/path/kind ties. Do not reserve slots by kind.
- When the query matches a web-engine keyword (`bi`/`b`/`g` or custom from `settings.json` `WebEngines`): insert one **`kind=web`** hit **first**, then apps, then files. Longer keywords win (`bi` before `b`).
- `execute` for `kind=web`: `execute_id` is an `http(s)` URL — open via ShellExecute **without** path validation (absolute-path checks reject URLs). File/app execute still validates paths.
- `results` JSON must include **`is_indexing`** so the UI can poll until the file index is ready even if apps already returned.
- The indexer scans every eligible name across every volume into a bounded heap
  sized by the request `max`. Only heap finalists may call `path_for`; never stop
  on the first `max` MFT records or concatenate per-volume truncations.
- Broker and indexer search requests accept optional `filters` (at most 32
  field/value entries) as a reserved read-only channel. Missing and empty are
  equivalent; G1 does not apply filter semantics.
- A ready result returns a nonzero `index_generation` and an accurate
  `is_truncated`. Index building/partial readiness is reported by `is_indexing`,
  not `is_truncated`.
- Broker connections start with `hello { protocol }`; incompatible protocol
  numbers return an explicit error. Legacy `ping` remains available.
- MFT snapshot catch-up must collect the complete bounded USN replay window and
  defer create/rename records whose parent is not present yet. Retry deferred
  records after later parent events; once the high-water mark is reached, skip
  only records that are still unreachable and advance the checkpoint. Live USN
  batches remain strict: an unresolved parent rolls the batch back and queues a
  serialized rebuild instead of silently losing a current file.
- Path validation for file/app `execute`/`reveal`: reject empty, NUL, and relative paths.
- Match spans: UTF-16 code unit offsets (WPF `string` indexing).
- Web engines load at backend start from shared `settings.json`; empty/missing list falls back to defaults (Bing-first: `bi`, `b`, `g`).
- `reload_engines` IPC replaces the in-memory list immediately (RwLock); empty list falls back to defaults. Accept PascalCase engine fields from the frontend.
- `actions` / `run_action`: path validation same as execute/reveal (absolute only). First-version actions are exactly four: `open_folder` (explorer /select), `copy`/`cut` (CF_HDROP + Preferred DropEffect), `copy_path` (CF_UNICODETEXT). Do not ship placeholder "快捷菜单" rows that cannot run. On clipboard `SetClipboardData` failure, `GlobalFree` the unowned `HGLOBAL` (system only takes ownership after success).

---

## Testing Requirements

- Unit tests for apps search scoring (exact > prefix > contains) and Chinese names.
- IPC tests: empty index `is_indexing=true`; ready empty index `is_indexing=false`; apps appear with `kind=app`.
- Ranking tests: late-volume winners enter `max=8`, `max=1000` has no hidden
  intermediate cap, stable ties do not depend on record scan order, and path
  construction count is bounded by returned finalists.
- USN replay tests: child-before-parent within the catch-up window resolves to a
  valid path; a stale orphan is skipped only in snapshot catch-up; the same
  unresolved parent in a live batch still rolls back all earlier mutations.
- Web-search tests: Chinese query URL-encoding; `bi` vs `b`; custom engine list; `execute` https not rejected as relative path; web row sorts before apps when keyword matches.
- `reload_engines` tests: replace list affects subsequent search; empty list falls back to defaults; PascalCase fields parse.
- Actions tests: list_actions rejects relative; basics present; unknown run_action errors; IPC `actions` / `run_action` wiring.
- `cargo test --manifest-path src/prism-core/Cargo.toml` must stay green before calling a step done.

---

## Memory Acceptance (≤100MB hard gate)

### 1. Scope / Trigger

Apply this gate to an idle Release build after the LocalSystem indexer reports
`ready=true`, `building=false`, and `degraded=false`. The gate covers all three
resident product processes: `Prism.exe`, `prism-core.exe`, and
`prism-indexer-service.exe`.

### 2. Signatures

Run the synchronized sampler from a normal-user shell:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\bench\Measure-ProcessMemory.ps1 `
  -OutputDirectory <explicit-output-directory> `
  -ReleaseDirectory <three-binary-release-directory> `
  -RunId <run-id>
```

The installed `PrismIndexer` service binary must have the same SHA-256 as
`<three-binary-release-directory>\prism-indexer-service.exe`.

### 3. Contracts

- Hard-gate metric: the synchronized sum of
  `Win32_PerfFormattedData_PerfProc_Process.WorkingSetPrivate`, mapped by PID,
  for the three processes above. The maximum sampled sum must be at most
  100 MiB (`100 * 1024 * 1024` bytes).
- `WorkingSet64` and `PrivateMemorySize64` are diagnostics only.
  `PrivateMemorySize64` is committed private bytes and is not Private Working
  Set; it must never be substituted for `WorkingSetPrivate`.
- Every sample records all three PIDs, per-process values, the synchronized
  total, and a healthy indexer status. PIDs must remain unchanged throughout
  the series.
- A generation may advance between samples because the service applies live
  USN events. Each memory record owns the healthy status and generation read at
  that sample point; cross-sample generation equality is not required.
- G0 baseline, Windows build 22631.5696, five one-second samples, commit
  `0bcb42f0862dd1badf942f786a3eeefbe5049cc7`:

| Effective profile | Frontend P50 | Broker P50 | Indexer P50 | Total P50 | Total max |
| --- | ---: | ---: | ---: | ---: | ---: |
| committed `opt-level="z"` | 1.676 MiB | 2.008 MiB | 27.453 MiB | 31.137 MiB | 31.145 MiB |
| isolated `opt-level=3` decision input | 3.594 MiB | 2.020 MiB | 45.516 MiB | 51.129 MiB | 51.301 MiB |

The `z` row is the current acceptance baseline. The `3` row is a G1 decision
input, not a profile change or a new target.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Missing, duplicate, exited, or changed PID | Fail the run; publish no success summary. |
| Service PID differs from `PrismIndexer` SCM PID | Fail the run. |
| Frontend/broker path or installed service hash differs from the Release set | Fail the run. |
| Indexer is not ready, is building/degraded, or reports generation zero | Fail the run. |
| Fewer than three process records in any sample | Fail the run. |
| Maximum synchronized Private Working Set exceeds 100 MiB | Fail acceptance and retain raw evidence. |

### 5. Good / Base / Bad Cases

- Good: five synchronized samples contain stable PIDs, a healthy indexer, and a
  maximum three-process `WorkingSetPrivate` sum below 100 MiB.
- Base: generation advances between samples while every sample remains healthy;
  retain all samples and report the generation list.
- Bad: sum `PrivateMemorySize64`, omit the service, mix Debug/stale binaries, or
  combine values captured at different times.

### 6. Tests Required

- `tools\bench\Test-Bench.ps1` passes, including buffered JSONL publication.
- PowerShell parsing succeeds for every `tools/bench/*.ps1` and `*.psm1` file.
- Raw JSONL count equals the declared sample count; every record contains three
  process samples; recomputing each total from its process records matches the
  stored total exactly.
- The environment and service checks prove Release hashes and SCM PID identity
  before accepting a measurement.

### 7. Wrong vs Correct

```powershell
# Wrong: committed private bytes, only two processes, nonsynchronized reads.
(Get-Process Prism, prism-core | Measure-Object PrivateMemorySize64 -Sum).Sum

# Correct: one CIM formatted-performance snapshot, match WorkingSetPrivate by
# IDProcess, then sum Prism + prism-core + prism-indexer-service for that sample.
Get-CimInstance Win32_PerfFormattedData_PerfProc_Process |
  Select-Object IDProcess, WorkingSetPrivate
```

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

## R1 USN Service Contract (supersedes Gap B above)

### 1. Scope / Trigger

The LocalSystem indexer owns synchronous NTFS/USN calls from `spawn_blocking`; SCM stop must not leave the service in `RUNNING` after the cache checkpoint completes.

### 2. Signatures

- `indexer_runtime::Shutdown::request()` sets the stop flag and wakes the Tokio runtime.
- `indexer_runtime::run(Arc<Shutdown>) -> Result<(), String>` checkpoints and aborts the pipe task before returning.
- The service control callback reports `StopPending`; the service reports `Stopped` after bounded runtime teardown.

### 3. Contracts

- A watcher checks stop/epoch before reading USN and again before taking the live-index write lock.
- Runtime teardown uses `shutdown_timeout(2s)` so a blocking OS call cannot hold SCM in `RUNNING` indefinitely.
- Healthy operation uses `FSCTL_READ_USN_JOURNAL` incremental replay; periodic full-volume scans are forbidden.

### 4. Validation & Error Matrix

| Condition | Required behavior |
| --- | --- |
| SCM Stop | Report `STOP_PENDING`, wake runtime, checkpoint, then report `STOPPED`. |
| Stop while a watcher waits on USN | Do not apply a post-stop batch; bounded teardown permits process exit. |
| Journal id/checkpoint invalid | Queue one serialized rebuild; never run concurrent full rebuilds. |
| Pipe client disconnect | Close only that connection; keep the accept loop alive. |

### 5. Good/Base/Bad Cases

- Good: stop completes under 10 seconds and restart replays stop-window changes without changing the cache timestamp.
- Base: no changes arrive while stopped; restart loads the cache and resumes watchers.
- Bad: service remains `RUNNING` after checkpoint or silently starts a periodic full scan.

### 6. Tests Required

- Unit: shutdown requested before waiting is observed; an in-flight waiter is woken.
- Protocol: named-pipe roundtrip passes after restart.
- Machine: clean stop duration, stop-window create/rename/delete replay, USN latency P95/max, and Private Working Set sample.

### 7. Wrong vs Correct

```rust
// Wrong: runtime drop waits indefinitely for a blocking watcher.
runtime.block_on(run(stop));

// Correct: bound runtime teardown after the service has been asked to stop.
let result = runtime.block_on(run(stop));
runtime.shutdown_timeout(Duration::from_secs(2));
```

> **Warning**: `PrivateMemorySize64` is committed private bytes, not Task Manager's
> Private Working Set. Use `WorkingSetPrivate` for the AC9 process-memory gate.

The historical Gap B text above describes the pre-R1 implementation. R1 resolves it with the USN watcher and measured P95 32.1 ms / max 177.2 ms over 120 operations.

## Code Review Checklist

- [ ] Pipe write always paired with a full line read
- [ ] Heavy work off the async pipe path
- [ ] MFT catch-up handles child-before-parent and stale orphan records without a rebuild loop
- [ ] New result kinds covered in frontend parse / icons (`web` skips shell path icons)
- [ ] No debug-only logging that floods stderr in hot paths
- [ ] Merge order: web (if keyword) > apps > files; without keyword apps > files
