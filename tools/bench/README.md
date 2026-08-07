# Prism G0 baseline tools

These tools measure the existing three-process Release build without changing
search behavior, product configuration, the index, or user files. Every result
directory is explicit. Product data directories under `%ProgramData%\Prism`,
`%LocalAppData%\Prism`, and `%AppData%\Prism` are rejected as outputs.
The selected Release directory is also rejected, covering portable installs
whose product `data` directory lives beside the executables.

## Prerequisites

Run a clean Release build and assemble one directory containing all three
binaries:

```powershell
cargo build --release --manifest-path src/prism-core/Cargo.toml
dotnet publish src/Prism -c Release -r win-x64 --self-contained false -p:PublishSingleFile=true
```

The checked `dist` directory is an example layout, but formal results must use
fresh artifacts from the commit recorded in `environment.json`. The indexer
service must already be installed, running, healthy, and ready. Service control
is deliberately separate and is the only step that may require elevation:

```powershell
# Elevated shell, only when the service is stopped:
Start-Service PrismIndexer
```

Do not run the query driver elevated. It connects to the ordinary-user broker.
No GUI clicks or manual stopwatch timing are involved.

## Search baseline

For an isolated cold broker start plus the first-ready and stable warm phases:

```powershell
$run = 'artifacts\bench\g0-20260729-z'
powershell -NoProfile -ExecutionPolicy Bypass -File tools\bench\Invoke-SearchBaseline.ps1 `
  -OutputDirectory $run `
  -ReleaseDirectory dist `
  -SecuritySoftwareNotes 'State product and scan status here' `
  -BackgroundIoNotes 'State known background I/O here' `
  -EffectiveOptLevel z `
  -StartBroker
```

The fixed `queries.json` fixture covers ASCII, Chinese, exact, prefix,
contains, rare, no-hit, and all-indexed-volume/cross-volume cases. Each query
runs 30 times for both `max=8` and `max=1000` by default. The raw JSONL keeps
query ids and counts, never result paths or titles. A protocol error, unhealthy
index, response mismatch, disappeared process, or timeout aborts the run and no
success summary is written.

`-EffectiveOptLevel` is mandatory (`z` or `3`) because an isolated Cargo
profile override cannot be recovered from an already-built executable. The
environment manifest records the committed and declared effective values
separately and verifies that the installed indexer service binary has the same
SHA-256 hash as the supplied Release directory.

`search-summary.json` uses nearest-rank percentiles: rank
`ceil(percentile * sample_count)`, one-based. Samples are never discarded.

## Three-process memory

Launch the WPF Release build normally so exactly one frontend and one broker
are running, then sample from a normal-user shell:

```powershell
Start-Process (Resolve-Path 'dist\Prism.exe')
powershell -NoProfile -ExecutionPolicy Bypass -File tools\bench\Measure-ProcessMemory.ps1 `
  -OutputDirectory 'artifacts\bench\g0-20260729-z' `
  -ReleaseDirectory dist `
  -RunId 'g0-20260729-z'
```

The hard-gate metric is the synchronized sum of Performance Counter
`Working Set - Private` for `Prism.exe`, `prism-core.exe`, and
`prism-indexer-service.exe`. `WorkingSet64` and `PrivateMemorySize64` are also
reported, but `PrivateMemorySize64` is private committed bytes, not Private
Working Set. The sampler verifies the LocalSystem process PID against the
`PrismIndexer` service.

## Full-scan floor

`scan-floor` loads the existing v5 cache read-only and scans every slot on every
volume without early termination or path construction. It requires at least 30
iterations and writes raw samples separately from the summary. Debug binaries
are rejected. The requested profile label must match the compile-time Cargo
profile override (`3` requires `CARGO_PROFILE_RELEASE_OPT_LEVEL=3`):

```powershell
cargo build --release --manifest-path tools\bench\scan-floor\Cargo.toml
& tools\bench\scan-floor\target\release\prism-g0-scan-floor.exe `
  --cache-directory "$env:ProgramData\Prism" `
  --output-directory 'artifacts\bench\g0-20260729-z\scan-floor' `
  --query 'prism' --iterations 30 --run-id 'g0-20260729-z' `
  --opt-level-label z
```

This number is a G1 decision input, not a performance conclusion. If P95 is at
least 60 ms, the G1 record must flag parallel-search investigation and schedule
re-estimation.

## `opt-level` comparison

Do not edit the committed product `Cargo.toml`. Cargo profile environment
overrides produce isolated builds:

```powershell
$env:CARGO_TARGET_DIR = (Resolve-Path '.').Path + '\target\g0-z'
$env:CARGO_PROFILE_RELEASE_OPT_LEVEL = 'z'
cargo build --release --manifest-path src/prism-core/Cargo.toml

$env:CARGO_TARGET_DIR = (Resolve-Path '.').Path + '\target\g0-3'
$env:CARGO_PROFILE_RELEASE_OPT_LEVEL = '3'
cargo build --release --manifest-path src/prism-core/Cargo.toml
Remove-Item Env:CARGO_TARGET_DIR, Env:CARGO_PROFILE_RELEASE_OPT_LEVEL
```

Place each alternate `prism-core.exe` in a separate three-binary Release
directory, then run the same query fixture, iteration count, machine state, and
run notes with `-StartBroker`. Build and run `scan-floor` the same way for the
full-scan comparison. Keep both raw datasets; do not remove outliers.

## Root-scoped queries (G4)

`queries.json` carries five root-scoped fixtures alongside the global ones. A
fixture with a `root` field makes the driver add `root` to the search request;
fixtures without one stay byte-identical to the pre-G4 wire format, so old runs
remain comparable. Each sample records `root` and `root_rejection`, and each
aggregate additionally reports `workload` percentiles.

`workload.path_constructions` is the metric that answers the G4 PRD question
("can the parent-chain approach meet P95, or is an ancestor cache required?").
Latency alone cannot: it hides whether a fast query did little work or a slow one
walked many ancestors. A non-empty `root_rejections` on an aggregate means the
indexer refused that root and answered globally — such a row must never be read
as evidence that scoping is fast.

### Paired comparison (not a baseline)

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\bench\Invoke-RootScopeComparison.ps1 `
  -OutputDirectory artifacts\bench\g4-root-YYYYMMDD `
  -Iterations 40 `
  -Notes 'State machine conditions here'
```

`Invoke-SearchBaseline.ps1` aborts when the index generation moves, because
absolute numbers are only comparable across runs on a quiet machine. That is
correct and must not be relaxed. `Invoke-RootScopeComparison.ps1` answers a
narrower question — *is the scoped arm slower than the global arm* — by running
both arms interleaved inside each iteration, so background I/O hits them equally.
It tolerates churn and records generation movement instead of hiding it.

It is **not** a substitute for the formal baseline: its absolute latencies are
inflated and cross-run comparison is invalid. Check `sample_count` against
`errors` before drawing any conclusion; a run where most requests failed proves
nothing. See `08-07-prism-ipc-resilience` for the pipe-busy failures observed
under a USN flood.

## Protocol counters

The G1 broker/indexer protocol exposes generation, truncation, scanned node
count, eligible name-candidate count, matching-name count, Top-K admission
count, path-construction count, volume count, indexer memory, result count,
response bytes, and readiness. The script records older responses with absent
optional fields as `g1_pending`; it never infers truncation from result count.
Runtime cache version, total node count, and name-pool capacity remain pending.

## Checks

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\bench\Test-Bench.ps1
cargo test --manifest-path tools/bench/scan-floor/Cargo.toml
```
