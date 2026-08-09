# Hook Guidelines — Not Applicable

This file came from a web/React-oriented template. **This project has no hooks.**
The frontend is C# / .NET 8 / WPF (`src/Prism`); there is no React, no
`useSomething`, and no React Query / SWR data fetching.

Do not fill this file in with invented conventions, and do not treat the former
template prompts ("What custom hooks do you have?", "How do you handle data
fetching?") as project requirements.

## Where the equivalent concerns actually live

| Web concept | Prism equivalent | Documented in |
| --- | --- | --- |
| Custom hooks / shared stateful logic | View models plus injected service interfaces (`ISearchClient`, `IDebounceTimerFactory`, `ISearchScheduler`, `IIndexGenerationClient`) | [`index.md`](./index.md), [`state-management.md`](./state-management.md) |
| Data fetching | Named-pipe request/response via `PipeClient`; one written request always consumes its paired response line | [`quality-guidelines.md`](./quality-guidelines.md) |
| Caching / revalidation | Prefix cache gated on `is_truncated`, `is_indexing`, index generation, and scope; generation change invalidates it | [`state-management.md`](./state-management.md) |
| Debounced input | `DispatcherDebounceTimer` behind `IDebounceTimerFactory`; handler bound once, controlled by Restart/Stop | [`quality-guidelines.md`](./quality-guidelines.md) |
