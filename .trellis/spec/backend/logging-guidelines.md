# Logging Guidelines

## Scenario: Fail-Open Process Logs

### 1. Scope / Trigger

Broker and indexer diagnostics must not leak search payloads or gate availability.

### 2. Signatures

- `logging::init(process, directory)` selects `<process>.jsonl`.
- `logging::event(level, event, elapsed_ms, generation)` writes JSONL.
- `logging::redacted_id(message)` returns a non-reversible stable id.
- `logging::install_panic_hook()` records a panic before the process aborts.

### 3. Contracts

- Broker uses `broker.jsonl`; indexer uses `indexer.jsonl`.
- Fields: timestamp, level, event, elapsed, generation.
- Rotate at 2 MiB to `.jsonl.1`; never log full query/path/URL/title.
- Both entry points install the panic hook immediately after `init`. Because the
  release profile uses `panic = "abort"`, a panic without the hook leaves no
  record at all. The panic *location* comes from source and is logged verbatim so
  a crash can be placed; the panic *message* may embed a path and is hashed.
  Event shape: `panic at <file>:<line> message_<hash>` at level `error`.

### 4. Validation & Error Matrix

| Condition | Required behavior |
| --- | --- |
| Directory/open failure | Disable file log; continue |
| Write/flush failure | Disable writer; continue |
| Rotation failure | Disable writer; continue |
| Service startup failure | Redacted fallback id |

### 5. Good / Base / Bad Cases

- Good: `search_complete` with elapsed/generation.
- Base: internal text becomes `message_<hash>`.
- Bad: event fields contain query, `execute_id`, path, title, or URL.

### 6. Tests Required

- Sensitive tokens absent from default record.
- Unwritable directory and forced rotation failure do not panic.
- Broker and indexer use distinct names.
- Panic event keeps the source location and hashes the message; a missing
  location or payload still produces a well-formed record.

### 7. Wrong vs Correct

```rust
// Wrong
log(format!("opened {path}"));
// Correct
logging::event("info", "shell_open_complete", None, None);
```
