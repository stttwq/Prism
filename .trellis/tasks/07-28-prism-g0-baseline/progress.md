# Implementation progress

## 2026-07-29 tooling implementation

- Implemented the fixed query fixture, ordinary-user broker driver, deterministic
  JSONL aggregation, environment manifest, and fail-fast response pairing under
  `tools/bench/`.
- Implemented synchronized three-process memory sampling. The gate metric is
  Performance Counter `Working Set - Private`; `PrivateMemorySize64` is retained
  only as the separately named private-bytes diagnostic.
- Implemented a read-only full-node scan-floor tool and documented isolated Cargo
  profile overrides for `opt-level = "z"` and `opt-level = "3"`.
- Fields unavailable from the current protocol are represented as
  `g1_pending`, not inferred.

### Verification completed

| Date | Command | Result |
| --- | --- | --- |
| 2026-07-29 | `powershell -NoProfile -ExecutionPolicy Bypass -File tools\bench\Test-Bench.ps1` | Passed. |
| 2026-07-29 | `cargo test --manifest-path tools/bench/scan-floor/Cargo.toml` | Passed: 5 tests after final review. |
| 2026-07-29 | `cargo clippy --manifest-path tools/bench/scan-floor/Cargo.toml --all-targets -- -D warnings` | Passed with no warnings. |
| 2026-07-29 | PowerShell parser over all `tools/bench/*.ps1` and `*.psm1` | Passed: 5 files parsed without errors. |
| 2026-07-29 | `cargo test --manifest-path src/prism-core/Cargo.toml` | Passed: 86 tests. |
| 2026-07-29 | `cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings` | Passed with no warnings. |
| 2026-07-29 | `dotnet build src/Prism/Prism.csproj -c Release` | Passed: 0 warnings, 0 errors. |
| 2026-07-29 | Search-driver smoke run against local named pipes | Correctly aborted without a success summary because the installed indexer reported `broken parent chain at record 14577`. This is existing machine/index state, not produced by G0 tooling. |
| 2026-07-29 | Initial read-only `scan-floor` smoke | Superseded diagnostic only: the first matcher allocated a lowercase string for every ASCII name and did not represent the product scan path. Its output was removed. |

### Remaining machine measurements

- Through a separately authorized deployment/repair workflow, make the running
  service binary match commit `0bcb42f` and restore the acceptance-machine
  indexer to `ready=true`, `building=false`, `degraded=false`. G0 itself must
  not replace the service binary or rebuild/delete the product cache.
- Run cold/first-ready/warm search twice for both profile builds and verify raw
  sample counts and generation identity.
- Launch the matching WPF Release build and capture the synchronized
  three-process memory samples.
- Only after those values exist, update the owner-reviewed Memory Acceptance spec
  with the three-process measured value and old-versus-new metric explanation.

## 2026-07-29 formal measurement attempt

- Rebuilt `prism-core.exe` and `prism-indexer-service.exe` from commit
  `0bcb42f0862dd1badf942f786a3eeefbe5049cc7` with committed
  `opt-level = "z"`; built an isolated `opt-level = "3"` product backend via
  `CARGO_PROFILE_RELEASE_OPT_LEVEL=3` without changing `Cargo.toml`.
- Published the WPF Release single-file executable. Publish completed; the
  sandboxed restore emitted `NU1900` because NuGet vulnerability metadata was
  unreachable. This did not prevent artifact generation.
- Corrected the memory sampler review finding: a single locale-neutral CIM
  formatted-performance snapshot reads both `IDProcess` and
  `WorkingSetPrivate`, then maps each value by PID. It does not assume the
  counter instance name equals the executable name.
- The first formal scan comparison was superseded during review because it did
  not prove the effective build profile and did not mirror the product's
  allocation-free ASCII matcher. Those intermediate datasets were removed;
  only the final matcher-aligned datasets below are acceptance evidence.

- Fresh `z` and `3` brokers were each run outside the sandbox as an ordinary
  user. Both reached the installed indexer and aborted before formal sampling
  with the same existing machine-state error:
  `broken parent chain at record 14577`.
- The two failed-attempt `environment.json` files record the committed profile
  (`z`) only; they predate the review fix that requires a separately declared
  effective profile. They are diagnostic records, not evidence of a completed
  `z`/`3` search comparison. A successful rerun must use
  `-EffectiveOptLevel z` or `-EffectiveOptLevel 3`, and the runner now verifies
  that the installed indexer service hash matches the supplied Release set.
- No successful search summary or memory summary was produced. Per G0 safety
  constraints, the run did not delete/rebuild the product cache or change the
  installed service. The three-process Memory Acceptance spec remains unchanged
  until a healthy `ready=true`, `building=false`, `degraded=false` run supplies
  owner-reviewable measurements.
- The final read-only service identity check also rejected the current
  installation because its `PrismIndexer` binary SHA-256 does not match the
  freshly built commit `0bcb42f` Release set. Replacing the installed binary is
  outside G0's elevation boundary, which permits service state read/control but
  not arbitrary installation changes.

## 2026-07-29 review corrections

- Search collection now rejects degraded readiness, missing or changing
  generations, incomplete query/max matrices, duplicate iterations, and groups
  with fewer than 30 samples.
- Search and memory collection now verify the running indexer service binary
  against the Release set. Memory collection also rejects PID or generation
  changes during the synchronized series.
- Success summaries are published only after validation; scan-floor uses an
  atomic rename, and PowerShell removes a partially written summary on error.
- Scan-floor output validation resolves existing ancestors before comparing
  paths, preventing case, `..`, and junction aliases from writing into the
  product cache.
- Scan-floor now rejects Debug builds and validates the profile label against
  compile-time `CARGO_PROFILE_RELEASE_OPT_LEVEL` evidence.
- PowerShell output validation also rejects the selected Release directory,
  covering portable installs whose product data lives beside the binaries.
- The scan-floor matcher now mirrors the product's allocation-free ASCII path
  and Unicode fallback without changing the product module's public API.

### Product-matcher-aligned final scan rerun

The reviewer rebuilt and reran both profiles from commit
`0bcb42f0862dd1badf942f786a3eeefbe5049cc7`. Both runs loaded generation
`302734` read-only, scanned 2,439,315 slots and 643,786 eligible names per
sample, performed no path construction or early termination, and retained all
30 samples. Raw nearest-rank recomputation exactly matches each summary.

| Profile evidence | Samples | P50 | P95 | Max |
| --- | ---: | ---: | ---: | ---: |
| committed scan-floor Release profile `z` | 30 | 39.531 ms | 50.252 ms | 57.906 ms |
| `CARGO_PROFILE_RELEASE_OPT_LEVEL=3` | 30 | 37.851 ms | 53.064 ms | 55.643 ms |

The final datasets are under `research/scan-z-final/` and
`research/scan-3-final/`. Both P95 values are below the 60 ms review threshold,
so G0 does not trigger a G1 parallel-search investigation. These measurements
remain decision inputs, not optimization conclusions.

## Final quality gate

| Command / check | Result |
| --- | --- |
| `powershell -NoProfile -ExecutionPolicy Bypass -File tools\bench\Test-Bench.ps1` | Passed. |
| PowerShell parser over five benchmark scripts/modules | Passed. |
| `cargo test --manifest-path tools/bench/scan-floor/Cargo.toml` | Passed: 5 tests. |
| scan-floor Clippy with `-D warnings` | Passed. |
| `cargo test --manifest-path src/prism-core/Cargo.toml` | Passed: 86 tests. |
| product Clippy with `-D warnings` | Passed. |
| `dotnet build src/Prism/Prism.csproj -c Release --no-restore` | Passed: 0 warnings, 0 errors. |
| Final research JSON/JSONL parse and percentile recomputation | Passed: 64 records; both summaries matched raw nearest-rank values. |
| `git diff --check` | Passed; only existing CRLF conversion notices were emitted. |

## 2026-07-29 service repair and formal acceptance measurements

- The separately authorized repair workflow backed up the previously installed
  service and cache under
  `C:\Users\jia\AppData\Local\Temp\PrismG0Repair-0bcb42f-a1`, deployed the
  commit-matched service, and restored a healthy
  `ready=true`, `building=false`, `degraded=false` index without deleting or
  rebuilding the cache. The acceptance machine was left running the committed
  `opt-level="z"` service hash
  `1FC91280684AFF23C109A96275321BFA05357349ED5F4154D194DE87CF83B8A1`.
- Formal search datasets are `research/search-z-final-1e/`,
  `research/search-z-final-2/`, `research/search-3-final-1/`, and
  `research/search-3-final-2/`. Every run contains 422 raw records: two
  first-ready samples and 420 warm samples (seven queries, two max values, 30
  iterations). Raw nearest-rank recomputation matches every aggregate.
- Worst query-group P95 values across the two reruns were 36.324 ms (`z`,
  `max=8`), 80.026 ms (`z`, `max=1000`), 45.313 ms (`3`, `max=8`), and
  81.791 ms (`3`, `max=1000`). Both profiles remain G1 decision inputs; the
  committed profile stays `z`.
- Live USN replay makes one generation for an entire 30-60 second run
  unrealistic on an active Windows machine. The protocol guarantees each
  result and generation atomically under the index read lock. The runner now
  requires a five-second stable window before sampling, rejects zero/missing
  generations and unhealthy status, records every sample generation, and
  publishes the distinct/min/max generation summary. It no longer treats valid
  generation advances between independent samples as data corruption.
- Formal synchronized memory datasets are `research/memory-z-final/` and
  `research/memory-3-final/`, each with five records and exact three-process
  PID/hash checks. The committed `z` Private Working Set total was 31.137 MiB
  P50 / 31.145 MiB max; the isolated `3` comparison was 51.129 MiB P50 /
  51.301 MiB max. Both are below the unchanged 100 MiB hard gate.
- Two measurement bugs were fixed during formal execution: case-insensitive
  PowerShell scope made `$Fixture` shadow the query-set document, and writing
  one JSONL line per sample caused the indexer to observe the benchmark's own
  output writes. The scripts now use an explicit `$fixtureDocument` and buffer
  validated JSONL records until sampling finishes.

## 2026-07-29 final reviewer pass

- Fixed the memory sampler's hard-gate enforcement. It previously calculated
  the maximum three-process Private Working Set but would still publish a
  success summary above 100 MiB. It now retains the buffered raw JSONL, rejects
  an over-limit run, and publishes no summary. The summary records the limit
  and pass state for accepted runs; `Test-Bench.ps1` covers the inclusive limit
  and one-byte-over failure cases.
- Recomputed all formal evidence from raw JSONL: 60 scan records, 10 memory
  records, and 1,688 search records. Counts, nearest-rank percentiles, scan
  counters, three-process memory sums, healthy per-sample status, and all search
  aggregate matrices match the checked summaries.
- Reconfirmed the complete quality gate after the fix: benchmark PowerShell
  tests and parser passed; scan-floor passed 5 tests and Clippy; the product
  passed 86 tests and Clippy; WPF Release built with 0 warnings and 0 errors;
  scoped `git diff --check` passed with only existing line-ending notices.
