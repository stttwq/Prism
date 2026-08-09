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
