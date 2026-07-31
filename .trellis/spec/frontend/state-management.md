# State Management

> How state is managed in this project.

---

## Overview

<!--
Document your project's state management conventions here.

Questions to answer:
- What state management solution do you use?
- How is local vs global state decided?
- How do you handle server state?
- What are the patterns for derived state?
-->

(To be filled by the team)

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

## State Categories

<!-- Local state, global state, server state, URL state -->

(To be filled by the team)

---

## When to Use Global State

<!-- Criteria for promoting state to global -->

(To be filled by the team)

---

## Server State

<!-- How server data is cached and synchronized -->

(To be filled by the team)

---

## Common Mistakes

<!-- State management mistakes your team has made -->

(To be filled by the team)
