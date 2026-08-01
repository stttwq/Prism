# G2 Installed Acceptance

Final installed-state evidence was captured on 2026-08-01 under
`artifacts/bench/g2-20260801-final/`. The earlier
`artifacts/bench/g2-20260801-installed/` run is superseded and is retained only
as diagnostic history.

## Release Identity

| Artifact | SHA-256 |
| --- | --- |
| `dist/Prism.exe` | `30D3E99955990F034272CC9CD65BE6C8B9B57A03CD0DA5330C39F8D554CECF6D` |
| `dist/prism-core.exe` | `99BC90C9BC9E96404FC400800EBC5AB86C3AA20C08A9330DDD56BBEA460E57C0` |
| `dist/prism-indexer-service.exe` | `1650BB91BAD6C79ED00E285933686CA05D6B31806ACFDB4B9544EDF579DEE784` |
| `dist/PrismSetup-1.0.0.exe` | `E3C3907DC28CD219C6E1E8447FFE346596BE54A8AA79AC6F424D6B99A30FB055` |

## Search Gate

`search-enabled/search-summary.json` contains 420 warm samples. The worst
`max=8` P95 was 82.133 ms and the worst `max=1000` P95 was 97.039 ms. These
pass the 100 ms and 300 ms limits respectively. The same G0 query set covers
ASCII exact, prefix, and contains cases; no unexplained ASCII regression was
observed.

## Memory Gate

The synchronized three-process Private Working Set was 57,913,344 bytes at
both P50 and max with pinyin enabled, and 57,208,832 bytes with pinyin
disabled. The enabled-minus-disabled delta was 704,512 bytes (0.67 MiB).
This passes both the 10 MiB optional-feature delta and 100 MiB total gates.

Evidence:

- `memory-enabled/memory-summary.json`
- `memory-disabled/memory-summary.json`
- corresponding `memory-samples.jsonl` raw samples

## Fault Isolation

- `fault-corrupt-final.json`: clean response with `pinyin_status=corrupt`;
  returned matches are literal only.
- `fault-version-mismatch.json`: clean response with
  `pinyin_status=version_mismatch`; returned matches are literal only.
- `Invoke-PinyinFaultFixtureLogged.ps1` and `微信-G2-literal.txt` are the final
  fixture wrapper and Unicode literal input.

Earlier files without the `-final` suffix are superseded diagnostics and are
not acceptance evidence.

## Uninstall Gate

`uninstall-residue-final2.json` is authoritative. Silent uninstall returned
zero, `%ProgramData%\Prism` and `C:\Program Files\Prism` were absent, no
installed binaries or Prism processes remained, and the `PrismIndexer`
service was absent (`passed=true`). Prism intentionally remains uninstalled
after this acceptance run.

The installer fix force-terminates the frontend and broker process trees before
file removal and uses `dirifempty` for `{app}`, preserving unrelated user files
instead of recursively deleting the install directory.

## Result

All G2 installed acceptance gates pass. Source quality gates are rerun after
the task/spec documentation update before final handoff.
