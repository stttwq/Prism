# Progress

## 2026-08-13: G8 Implementation

### Completed
- Settings: added `SuggestionsEnabled` (default false) and `FaviconGrants` (Dictionary<string, FaviconGrant>)
- `ISuggestionService` interface + `SuggestionItem` record
- `WebModeDetector`: pure function web keyword detection (long-keyword-first, case-insensitive, built-in vs custom)
- `SuggestionService` with injectable `IHttpTransport`: Bing/百度/Google adapters with structured JSON parsing, 800ms timeout, max 5 suggestions, dedup, length bound
- `SystemHttpTransport`: singleton HttpClient, 3 redirect limit, 64KB response cap, 900ms total timeout
- `FaviconCache`: origin-normalized cache, MIME/format/pixel validation, versioned metadata, LRU eviction (64 entries, 256KB, 128×128), atomic write
- `SearchViewModel.RunWebSearchAsync`: synchronous direct result, async suggestion append with `_suggestionSeq` request identity, `CancelSuggestions()` on query change/reset
- `App.xaml.cs`: inject `SuggestionService`, call `UpdateWebSettings` on startup and settings save
- `prism.iss`: favicon cache directory added to `[UninstallDelete]`
- Spec updates: frontend/state-management.md (G8 Web Mode contract), frontend/quality-guidelines.md (G8 review checklist)
- Tests: 30 new G8 tests (WebModeDetector 10, SuggestionAdapter 12, FaviconCache 8)

### Quality Gates
- C# tests: 131 passed, 5 skipped (live), 0 failed
- Rust tests: 261 passed, 14 skipped (live), 0 failed
- Rust Clippy: 0 warnings
- WPF Release build: 0 warnings, 0 errors

### Deferred (out of scope per PRD)
- Built-in engine icon asset packaging (bundled .png/.ico resources) — favicon infrastructure is complete; built-in icons can be added as pack resources in a follow-up
- Settings UI for suggestions toggle and favicon grant flow — data model is ready; UI to be added
- Real-network suggestion compatibility check — requires manual verification per design.md review gates
