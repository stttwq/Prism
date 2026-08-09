# Database Guidelines — Not Applicable

This file came from a web-service template. **This project has no database.** No
ORM, no SQL, no migration runner, no connection pool. Do not add one to satisfy
this file, and do not treat the former template prompts ("What ORM do you use?",
"How are migrations managed?") as project requirements.

Persisted state is a small set of files, each versioned by name and by envelope.
The authoritative table lives in
[`directory-structure.md`](./directory-structure.md#persistence-no-database).

## The equivalents you probably came here looking for

| Database concept | Prism equivalent |
| --- | --- |
| Schema migration | Format version in the filename plus `persistence.rs` schema constants. An incompatible version is **not** migrated: machine-level caches (`index-v5.bin`, `pinyin-v1.bin`) are rebuilt from the filesystem; user data falls back to safe defaults or empty state. |
| Transaction / atomicity | Write to `<file>.tmp`, validate, then atomic replace. Never partially overwrite a live file. |
| Query layer | `hierarchy.rs` in-memory candidate scan plus global Top-K, reached only through the read-only `indexer_ipc` protocol. |
| Corruption handling | Fail open: corrupt history → empty history; corrupt sidecar → pinyin disabled, literal search unaffected; corrupt index cache → full rebuild. Search availability must never depend on a persisted file being intact. |
| Backup / retention | History caps at 500 entries with 90-day eviction. Derived caches are disposable and are deleted on uninstall. |

## Rules that still apply

- A new persistent file requires, in the same stage: a schema version, an atomic
  write path, a corruption fallback, and an entry in `dist/prism.iss` install and
  uninstall manifests.
- Machine-level files belong to the indexer under `%ProgramData%\Prism`; user-level
  files belong to the broker/frontend under the resolved user data dir. The
  LocalSystem service must not read a specific user's data directory.
