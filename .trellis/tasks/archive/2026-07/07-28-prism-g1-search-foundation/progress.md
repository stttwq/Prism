# G1 Progress

## Implementation

- Global bounded Top-K ranks apps, files, and folders under one stable contract.
- File paths are constructed only for final heap candidates.
- Broker/indexer protocol adds an explicit handshake, stable result kinds,
  truncation, generation, optional match/work metadata, and bounded reserved filters.
- WPF search dependencies are injectable; complete-result prefix caching is
  invalidated by deletion, generation, mode, root, filters, and sort version.
- Added a command-line C# test project for search state, cache, cancellation,
  focus, pin, actions, generation, and unknown-kind behavior.

## Acceptance

| Check | Result |
| --- | --- |
| Rust tests | 95 passed |
| Rust Clippy | Passed with `-D warnings` |
| Rust format | Passed |
| C# tests | 7 passed |
| WPF Release build | 0 warnings, 0 errors |
| Benchmark self-tests | Passed |
| Scan-floor tests / Clippy | 5 passed / passed |
| `git diff --check` | Passed |

Formal machine evidence is under `artifacts/bench/g1-20260730-fixed/`:

- Search: 420 warm samples. Slowest `max=8` P95 was 87.770 ms (limit 100 ms);
  slowest `max=1000` P95 was 88.117 ms (limit 200 ms).
- Memory: maximum synchronized three-process Private Working Set was
  53,268,480 bytes (limit 104,857,600 bytes).
- Scan floor: 2,439,315 nodes, 655,828 eligible names, P95 42.189 ms,
  max 43.734 ms, and zero path constructions.

## Acceptance Bug And Prevention

The first formal run failed with `broken parent chain at record 47212`. The MFT
snapshot catch-up implicitly assumed parent USN records always preceded child
records inside each read batch. The fix collects the complete bounded catch-up
window, defers missing-parent create/rename records, retries them after later
parent events, and skips only records still unreachable at the high-water mark.
Live incremental batches remain strict and roll back before requesting a rebuild.
Regression coverage includes child-before-parent and stale-orphan replay.
