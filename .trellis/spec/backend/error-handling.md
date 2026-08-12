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

---

## Scenario: Window Activation Is An Explicit Exception To The Shell Boundary

### 1. Scope / Trigger

New cross-layer IPC contract (G5 window switcher). The rule above — "all user-session
Shell, clipboard, shortcut-resolution, and COM work belongs to the broker" — does **not**
extend to window activation, and the reason is a Windows constraint, not a preference.

`SetForegroundWindow` only takes effect when called from the foreground process (or one
that just received input). When the user presses Enter, the foreground process is
`Prism.exe`; `prism-core.exe` is a background process, so its foreground calls are
silently downgraded to a taskbar flash. A broker-side activation therefore *cannot* work,
and its failure mode is "nothing visibly happened" rather than an error.

Ownership split:

| Responsibility | Process |
| --- | --- |
| Enumerate, filter, collect window identity | broker (`EnumWindows` needs no foreground rights) |
| Title/app-name matching, pinyin, history ranking | broker (owns pinyin + `HistoryStore`) |
| Snapshot and opaque-token lifetime | broker (HWNDs do not cross the boundary) |
| Pre-activation identity re-check | **both** — broker at resolve, WPF again before activating |
| Foreground activation, restoring a minimized window | WPF (only process with foreground rights) |
| Writing switch history | broker, and only after WPF reports success |

### 2. Signatures

- `Request::ResolveWindow { target: ActionTarget }` → `Response::WindowHandle { handle: u64, pid: u32, title: String, is_minimized: bool }`
- `Request::RecordWindowSwitch { target: ActionTarget }` → `Response::Status`
- `Request::Search { .., mode: Option<SearchMode> }` where `SearchMode = all | window | unknown`
- `window_list::WindowSnapshotStore::resolve(&str, &dyn WindowProbe) -> Result<WindowEntry, ResolveError>`
- `window_list::is_switchable(&RawWindow, self_pids: &[u32]) -> bool`

Window targets must never reach `ShellExecutor`. `shell.rs` keeps returning
`unsupported` ("window activation is not implemented") for `TargetKind::Window` — that is
the intended terminal state, not a gap to fill.

### 3. Contracts

- `target.value` for a window is a **snapshot token**, not an HWND: decimal
  `generation << 10 | index`. Decimal encoding keeps the pre-existing
  `TargetKind::Window` "must be numeric" validation byte-compatible.
- Tokens are valid only inside the enumeration that minted them. Each `publish` replaces
  the snapshot wholesale and bumps the generation; generation `0` means "nothing
  enumerated yet" and can never validate.
- `mode` absent ⇒ `all`. A global-search payload therefore stays byte-identical to the
  pre-G5 format, so an older broker is unaffected. An unrecognized mode string decodes to
  `Unknown` and is treated as `all` rather than failing the request.
- History key is `"<app_name lowercased>|<whitespace-collapsed lowercased title>"`.
  **Never the HWND** — Windows recycles handles, so a persisted handle eventually names an
  unrelated window.
- Enumeration is bounded at `MAX_WINDOWS = 512`; overflow truncates and logs.
- Window responses carry no `index_generation` and always `is_indexing: false`. Window
  availability is unrelated to index readiness.

### 4. Validation & Error Matrix

| Condition | Result |
| --- | --- |
| Non-decimal token, or index outside the snapshot | `target_invalid` (`ResolveError::Malformed`) |
| Token from an earlier enumeration | `conflict` (`StaleGeneration`) |
| Window gone, or no longer visible | `conflict` (`WindowGone`) |
| Handle alive but pid or app differs | `conflict` (`IdentityChanged`) |
| `resolve_window` / `record_window_switch` given a non-window target | `target_invalid` |
| Windows refuses the foreground change | WPF reports failure; UI retained; **no success history** |
| Title changed but pid and app match | Success — titles legitimately churn (tab switch, dirty marker) |

### 5. Good / Base / Bad Cases

- Good: `{"type":"search","query":"记事","mode":"window"}` → window rows → `resolve_window`
  → WPF activates → `record_window_switch`.
- Base: a payload with no `mode` behaves exactly as before G5.
- Bad: activating from the broker; persisting an HWND as identity; routing a window target
  through `ShellExecutor`; writing history before WPF confirms success.

### 6. Tests Required

- One test per filter rule (invisible, untitled, tool window, owned, own-process,
  UWP inner `CoreWindow`), asserting `is_switchable` is false for each in isolation.
- Cloaking needs **four** tests, not one, because `DWM_CLOAKED_SHELL` is ambiguous:
  `DWM_CLOAKED_APP` → rejected; shell-cloaked + on another desktop → rejected;
  shell-cloaked on the current desktop (suspended UWP) → **kept**, this is the regression
  guard; and shell-cloaked with a failed desktop query → kept, since failing open is
  required. Assert the redeclared `CLOAKED_*` bits equal the Win32 constants too, or the
  filter silently reads the wrong flag.
- `resolve` races: closed window → `WindowGone`; same handle with a new pid → `IdentityChanged`;
  previous-generation token → `StaleGeneration`; title-only change → still resolves.
- `record_window_switch` writes history under the app+title key, and **writes nothing** for a
  dead window or a stale token.
- Ranking: literal beats pinyin; app-name hit yields no title highlight; history breaks
  same-tier ties; Chinese titles highlight on UTF-16 offsets.
- Enumeration cap: over-cap input truncates and reports truncation; under-cap does not.

### 7. Wrong vs Correct

#### Wrong

```rust
// Broker tries to take the foreground itself. Windows downgrades this to a taskbar
// flash, so the user sees nothing and no error is reported anywhere.
Request::Execute { target } if target.kind == "window" => {
    unsafe { SetForegroundWindow(HWND(target.value.parse()?)) };
}
```

#### Correct

```rust
// Broker re-verifies and hands the handle to the foreground process.
Request::ResolveWindow { target } => {
    resolve_window(&target, windows, &SystemWindowProbe)
}
// WPF activates while it is still foreground, then reports back so history is only
// written for a switch that actually happened.
Request::RecordWindowSwitch { target } => {
    record_window_switch(&target, windows, &SystemWindowProbe, history)
}
```
