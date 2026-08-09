# Directory Structure

> How backend code is organized in `src/prism-core`.

---

## Overview

One Rust crate, two shipped binaries, one shared library (`lib.rs`). The split
that matters is not by layer but by **which process may link a module**: the
LocalSystem indexer service must not pull in Shell, clipboard, network, or
user-settings code. That is a security property verified during host-integration
acceptance (compat-matrix S2), not a style preference.

---

## Directory Layout

```text
src/prism-core/
├── Cargo.toml          two shipped [[bin]] targets + pinyin-sidecar-prototype
├── build.rs            injects PRISM_BUILD_STAMP / PRISM_BUILD_PROFILE
├── src/
│   ├── lib.rs          module list, pipe names, INDEXER_PROTOCOL, build_id()
│   ├── main.rs                      → prism-core.exe (broker entry)
│   ├── prism-indexer-service.rs     → prism-indexer-service.exe (service entry)
│   │
│   │   ── indexer-side (LocalSystem) ──
│   ├── ntfs.rs             MFT enumeration, USN journal read/replay
│   ├── hierarchy.rs        NodeSlot (12B) FRN parent-chain index, name pool,
│   │                       candidate generation, global Top-K, path_for
│   ├── index_cache.rs      index-v5.bin envelope, machine_data_dir()
│   ├── indexer_runtime.rs  per-volume build/publish, USN watchers, generation,
│   │                       accept loop, spawn_blocking discipline
│   ├── pinyin_sidecar.rs   pinyin-v1.bin build/load/fault handling
│   ├── pinyin.rs           syllable matching (initials + full pinyin)
│   ├── root_scope.rs       root normalization, bounds, ancestor validation
│   │
│   │   ── shared ──
│   ├── indexer_ipc.rs      IndexerRequest/Response, SearchFilter, IndexerStatus,
│   │                       BuildProgress, request validation and bounds
│   ├── indexer_client.rs   broker→indexer client, retry/timeout
│   ├── logging.rs          per-process JSONL, redaction, panic hook
│   ├── persistence.rs      schema-version constants, VersionedEnvelope,
│   │                       VersionedData trait (user-data contracts)
│   │
│   │   ── broker-side (ordinary user) ──
│   ├── ipc.rs              broker protocol (Hello/search/execute/actions),
│   │                       result merging, is_truncated, empty-query results
│   ├── apps.rs             Start Menu inventory, .lnk resolution (COM MTA)
│   ├── actions.rs          action list per target kind
│   ├── shell.rs            ShellExecutor: dedicated STA worker owning COM
│   ├── history.rs          history-v1.json, atomic replace, schema version
│   ├── websearch.rs        web keyword engines, URL building
│   └── config.rs           settings + data dir resolution (portable vs LocalAppData)
└── tools/
    └── pinyin-sidecar-prototype.rs   design-gate prototype, not shipped
```

---

## Module Ownership

| Module group | Linked by | Hard rule |
| --- | --- | --- |
| `ntfs`, `hierarchy`, `index_cache`, `indexer_runtime`, `pinyin_sidecar`, `pinyin`, `root_scope` | indexer service | Read-only with respect to user data; no Shell/COM/network |
| `shell`, `apps`, `actions`, `history`, `websearch`, `config` | broker only | Must not become reachable from the service entry point |
| `indexer_ipc`, `indexer_client`, `logging`, `persistence` | both | Keep free of process-specific assumptions |

The service entry point references only `logging`, `index_cache`,
`indexer_runtime`, and the crate-level `log` helper. Adding a `shell` dependency
to any indexer-side module breaks the privilege boundary and must fail review.

---

## Persistence (no database)

There is no ORM, SQL, or migration system. All state is files.

| Data | Owner | Location | Recovery |
| --- | --- | --- | --- |
| `index-v5.bin` | indexer | `%ProgramData%\Prism` (machine-level) | Magic/version mismatch → explainable full rebuild |
| `pinyin-v1.bin` | indexer | `%ProgramData%\Prism` (machine-level) | Missing/corrupt/version mismatch → disable pinyin only; literal search unaffected |
| `history-v1.json` | broker | user data dir | Corrupt → empty history, retain diagnostic |
| `settings.json` | broker / WPF | user data dir | Missing fields → safe defaults; future schema → refuse |

User data dir is `<exe dir>\data` when writable (portable/custom install),
otherwise `%LocalAppData%\Prism`. The broker (`config::resolve_data_dir`) and the
frontend (`SettingsStore.ResolveDataDir`) implement the same policy and must stay
in sync. Schema versions are centralized in `persistence.rs`, not redefined per
feature.

Any stage that adds a persistent file must update the install/uninstall manifest
in `dist/prism.iss` in the **same** stage. Machine-level derived caches are
removed on uninstall (`{commonappdata}\Prism`); user data is retained.

---

## Naming Conventions

- Module files are snake_case, named after the domain noun rather than the layer
  (`hierarchy.rs`, not `index_service.rs`).
- Protocol types live in the `*_ipc.rs` module for their protocol and carry
  explicit serde wire names; the wire format is lowercase snake_case.
- Cache and sidecar filenames embed their format version (`index-v5.bin`,
  `pinyin-v1.bin`, `history-v1.json`) so an incompatible read is detectable both
  by name and by envelope.

---

## Examples

- Privilege boundary done right: `indexer_runtime.rs` answers a `Search` request
  entirely from index state and never resolves a Shell verb.
- Blocking discipline: `Search` and `Status` both hand index work to
  `spawn_blocking`; `wait_generation` uses the read-only `generation()` instead of
  the allocating `status()`. See the "Forbidden Patterns" section of
  [`quality-guidelines.md`](./quality-guidelines.md).
