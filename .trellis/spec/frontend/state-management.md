# State Management

> How state is managed in `src/Prism` (C# / .NET 8 / WPF).

---

## Overview

No state-management library. Plain `INotifyPropertyChanged` — `AppState` is a
sealed class implementing it by hand, deliberately **without** a CommunityToolkit
dependency. One `AppState` instance is created in `App.xaml.cs` and passed down; it
is not a static singleton and not a service-locator lookup.

State is split three ways:

| Kind | Owner | Lifetime |
| --- | --- | --- |
| UI state (`PanelMode` Idle/Results/Actions, query, results, selected index, theme) | `AppState` | Process; reset on hide |
| Search-pipeline state (in-flight sequence, debounce, prefix cache, generation) | `SearchViewModel` | Per query burst |
| Scope state (host kind, root, scope label) | `HostScopeController` | Per summon; never carried across summons |
| Persisted state (settings, history) | `SettingsStore` / broker | Disk, schema-versioned |

Two rules that are easy to get wrong:

- **Never reuse the previous summon's host root.** If host detection fails, fall
  back to global and clear `root`. A stale root silently searches the wrong tree,
  which is the one G4 failure mode that produces confidently wrong results rather
  than an obvious error.
- **Derived state is computed, not stored.** The scope label text is derived from
  `HostContext`; caching it invites showing a path that no longer applies.

The sections below are the committed contracts. They are authoritative — do not
relax them without a corresponding test change.

## Prism Search State

- `SearchViewModel` depends on `ISearchClient`, `IDebounceTimerFactory`, and
  `ISearchScheduler`; production dispatcher timers register their callback once
  and expose restart/stop behavior.
- A response may seed local prefix filtering only when it is not indexing, has
  no index error, is not truncated, carries an index generation, and contains
  no web result. Query deletion or replacement and generation notification
  invalidate that cache.
- `is_indexing=true` means file results are incomplete even when the response is
  already searchable. Such responses never seed the prefix cache; volume
  publication advances generation and the next poll replaces the visible set.
- Poll responses update indexing state even when `Items` is empty. The optional
  volume progress is retained after the bounded polling window expires so the
  UI does not regress to an indefinite generic waiting message.
- A written pipe request always consumes its paired line response. UI
  cancellation uses a monotonically increasing search sequence to discard late
  responses without desynchronizing the transport.
- The frontend adds the More row only when `is_truncated` is true. Item count
  equal to `max` is not sufficient evidence of truncation.
- Protocol kinds are retained as raw strings and projected to
  `SearchResultKind`; unknown strings map to `Unknown` without throwing.

## G7 Filter Cache Identity

- The WPF does **not** parse `ext:` / `path:` syntax. The raw query text is sent
  unchanged to the broker, which parses it and echoes the original query back in
  `Response::Results.query`. The frontend's `QueryMatchesResponse` and prefix-cache
  `StartsWith` checks therefore operate on the original text and need no changes.
- Different filter queries (`"report ext:pdf"` vs `"report ext:md"`) have different
  text, so the prefix cache never crosses filter boundaries. A cached response for
  `"report ext:pdf"` cannot serve `"report ext:md"` because the latter is not a
  prefix of the former.
- `SearchContext.Filters` (carrying G3 exclude_path entries) is already part of
  `SearchContext.IsEquivalentTo`, which gates the prefix cache. G7's parsed filters
  live in the query text, not in `SearchContext.Filters`, so no new cache-key
  dimension is needed.
- Late responses from an old filter query are discarded by the existing
  `_searchSeq` + `QueryMatchesResponse` mechanism — a stale broker echo that does
  not match the current input box is rejected before `ApplySearchResponse`.

## Window Mode (G5)

### 1. Scope / Trigger

Cross-layer contract: the `>` prefix selects an exclusive search mode whose results are
volatile, and activation is a foreground-bound Win32 call the broker cannot make.

### 2. Signatures

- `SearchContext.Mode` — `SearchContext.AllMode` (`"all"`) | `SearchContext.WindowMode` (`"window"`), with `IsWindowMode`
- `ISearchClient.ResolveWindowAsync(ActionTarget) -> Task<WindowHandleInfo>`
- `ISearchClient.RecordWindowSwitchAsync(ActionTarget) -> Task`
- `IWindowActivator.TryActivate(WindowHandleInfo) -> bool`
- `WindowHandleInfo(IntPtr Handle, uint Pid, string Title, bool IsMinimized)`

### 3. Contracts

- **Mode is derived from the query text, never stored as ambient state.** `_searchContext`
  keeps root/filters only; the mode is recomputed per request from the box. Storing it
  invites the mode and the visible text drifting apart.
- `>` counts **only at index 0**. `a>b` is an ordinary file search — a `>` inside a
  filename must not change modes.
- The broker never sees the `>`. It echoes the stripped query, so every comparison against
  `resp.Query` uses the stripped form while staleness checks against `_state.Query` use the
  raw form. Mixing these up silently drops every window response as "stale".
- A bare `>` is a real query (recent windows), not empty input. It must not fall through to
  the Idle branch.
- Window mode clears `Root` and `Filters`: windows are not scoped to a directory.
- Window results **never seed the prefix cache** — tokens expire with the next enumeration.
- Only an explicitly non-default mode is written to the wire, keeping global-search payloads
  byte-identical to the pre-G5 format.

### 4. Validation & Error Matrix

| Condition | UI outcome |
| --- | --- |
| `resolve_window` fails (closed / recycled / stale) | `切换失败：…`; activation never attempted; panel retained |
| `TryActivate` returns false | `无法切换到该窗口`; not hidden; no history |
| Activation succeeds | Hide, then record |
| `record_window_switch` throws | Stay silent — the switch already happened |
| No `IWindowActivator` injected | `窗口切换不可用`; degrade visibly, never throw |
| Empty result, bare `>` | `没有最近使用过的窗口` |
| Empty result, typed query | `没有匹配的窗口` |

### 5. Good / Base / Bad Cases

- Good: `>记事` → window rows → Enter → activate → hide → record.
- Base: no `>` → unchanged global behavior, no `mode` on the wire.
- Bad: hiding before activating; caching window rows; treating a bare `>` as empty input.

### 6. Tests Required

- `>query` sends `mode=window` with the prefix stripped; global search sends no mode.
- `a>b` stays in `all` mode.
- Bare `>` reaches `Results`, not `Idle`.
- **Ordering**: assert activation happened before the hide callback fired. Verify by
  mutation — reversing the two must fail the test, or it is proving nothing.
- Rejected activation: not hidden, no record call, panel retained.
- Failed history write does not surface as a failed switch.
- Missing activator reports unavailable rather than throwing.

### 7. Wrong vs Correct

#### Wrong

```csharp
// Hiding first surrenders foreground rights, so SetForegroundWindow degrades to a
// taskbar flash. Reads as "Enter did nothing" and reports no error.
HideRequested?.Invoke();
_activator.TryActivate(window);
```

#### Correct

```csharp
// Activate while still foreground; hide only after it actually worked.
if (!_activator.TryActivate(window))
{
    _state.StatusMessage = "无法切换到该窗口";
    return;
}
HideRequested?.Invoke();
await _pipe.RecordWindowSwitchAsync(item.ExecutionTarget);
```

---

## Backend State (there is no "server")

The broker is the only source of search results; there is no HTTP layer and no
generic response cache. Synchronization is by **index generation**, not by TTL:

- `index_generation` on a response identifies the index snapshot it came from.
- `IndexerGenerationClient` notifies on change; a change invalidates the prefix
  cache and re-issues the visible query.
- During first build, each volume publishing advances the generation, so partial
  results are replaced rather than expiring on a timer.
- The frontend must never infer freshness from elapsed time.

## Common Mistakes

These have all actually happened in this codebase:

- **Seeding the prefix cache from an incomplete response.** `is_indexing=true` and
  `is_truncated=true` are *independent* reasons a result set is incomplete;
  checking only one lets the frontend locally filter a partial set and drop hits.
- **Inferring truncation from `items.Count == max`.** A search that legitimately has
  exactly `max` hits is not truncated. Only trust `is_truncated`.
- **Cancelling a pipe read after the request line was written.** That desynchronizes
  the transport for every later request. Cancel by discarding the response via the
  search sequence number, after the paired line is consumed.
- **Dropping poll responses with empty `Items`.** They can carry newer volume
  progress or the terminal `is_indexing=false`, and ignoring them strands the UI on
  a stale progress message.
- **Letting a late response overwrite a newer query's results.** Every response must
  match on sequence, current `Query`, and `resp.Query` before being applied.
- **Reusing the last known host root after a failed detection.** See Overview.
- **Hiding the search window before activating the target window.** Losing foreground
  status makes `SetForegroundWindow` degrade to a taskbar flash, so the switch appears to do
  nothing and no error is raised at any layer. Activate first, hide second.
- **Comparing a mode-prefixed query against the broker's echo.** The broker echoes the
  stripped query, so comparing it to the raw `>foo` discards every response as stale and the
  panel just sits on 搜索中…. Staleness compares against `_state.Query` (raw); echo
  comparison uses the stripped form.
- **Treating `GetForegroundWindow() == handle` as proof the switch is visible.** A window can
  be foreground *and still minimized*: with `SW_RESTORE` removed, Windows set a minimized
  window as foreground and the activator reported success, so the user pressed Enter and saw
  nothing change. Restore before activating, and assert `!IsIconic` too — the foreground
  identity check alone is green for a switch the user cannot see.
- **Writing a test whose assertion is already guaranteed by something else.** The
  "window results never seed the prefix cache" test passed with the guard deleted, because a
  null `IndexGeneration` and a prefix mismatch each blocked caching independently. Mutate the
  code you think you are testing; if the test still passes, it is documentation, not a test —
  and it should say so.
