# Error Handling

## Scenario: Broker-Owned Shell Boundary

### 1. Scope / Trigger

All user-session Shell, clipboard, shortcut-resolution, and COM work belongs to
the normal-user broker. The LocalSystem indexer remains read-only.

### 2. Signatures

- `ActionTarget { kind, value }`: `file|directory|application|window|web`.
- `ShellExecutor::execute(ShellOperation) -> Result<ShellOutcome, ShellError>`.
- Error kinds: `access_denied`, `target_invalid`, `conflict`,
  `elevation_required`, `unsupported`, and `system`.

### 3. Contracts

- New results contain typed `target`; `execute_id` is legacy-reader compatibility.
- A bounded queue feeds one `prism-shell-sta` thread that owns COM lifecycle.
- Indexer IPC exposes only hello, status, bounded search, and generation wait.

### 4. Validation & Error Matrix

| Condition | Result |
| --- | --- |
| Unknown kind | `unsupported`, no execution |
| Empty/control/relative path | `target_invalid` |
| Non-http(s) web target | `target_invalid` |
| Permission denial | `access_denied` |
| User cancellation | status with `cancelled=true` |
| Worker failure | `system` |

### 5. Good / Base / Bad Cases

- Good: WPF sends `{target:{kind:"web",value:"https://..."}}`.
- Base: legacy `id` converts once at the broker edge.
- Bad: infer URL/path throughout handlers or initialize COM on Tokio workers.

### 6. Tests Required

- Typed/legacy decoding, target serialization, unknown/incompatible rejection.
- STA startup, pre-queue cancellation, clean shutdown.
- Indexer decoder rejection for write/action commands.

### 7. Wrong vs Correct

```rust
// Wrong
execute_id(id);
// Correct
shell.execute(ShellOperation::Open(target)).await;
```
