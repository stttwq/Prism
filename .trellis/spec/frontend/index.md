# Frontend Development Guidelines

> Best practices for frontend development in this project.

---

## Overview

The "frontend" in this project is **C# / .NET 8 / WPF** (`src/Prism`), not a web
stack. It talks to the `prism-core` broker over a single named pipe. Several of
these files were generated from a web-oriented template; the table below states
which ones carry real project content and which are still unfilled boilerplate.
Do not treat template prompts about React, hooks, CSS, or props as project
conventions.

---

## Guidelines Index

| Guide | Description | Status |
|-------|-------------|--------|
| [Quality Guidelines](./quality-guidelines.md) | Forbidden/required patterns for the WPF UI, pipe I/O, tray, icons | **Filled — authoritative** |
| [Type Safety](./type-safety.md) | Typed action targets, versioned settings schema, unknown-kind handling | **Filled — authoritative** |
| [State Management](./state-management.md) | `SearchViewModel` state, prefix cache validity, polling | **Partly filled** — the "Prism Search State" section is authoritative; surrounding template sections are not |
| [Directory Structure](./directory-structure.md) | Module organization and file layout | Unfilled boilerplate — see `src/Prism` layout below |
| [Component Guidelines](./component-guidelines.md) | XAML control patterns | Unfilled boilerplate |
| [Hook Guidelines](./hook-guidelines.md) | Not applicable to WPF | **Not applicable** — no hooks in this stack |

### Actual `src/Prism` layout

```text
src/Prism/
├── Models/       AppState, Settings, SearchResult, HostContext, ActionItem
├── ViewModels/   SearchViewModel, SettingsViewModel, HostScopeController,
│                 SearchWindowFocusPolicy
├── Views:
│   ├── Windows/  SearchWindow, SettingsWindow
│   └── Controls/ SearchHeader, ResultList, ActionPanel, PinButton, HotkeyRecorderBox
├── Services/     PipeClient, IndexerGenerationClient, SearchAbstractions,
│                 HotkeyService, TrayService, ThemeWatcher, IconCache,
│                 SettingsStore, AutoStartService, RootValidation,
│                 HostAdapter + ExplorerHostAdapter / DirectoryOpusHostAdapter /
│                 HostAdapterCatalog / HostProcessGuard, ShellBrowserInterop,
│                 NativeWindowQuery, DispatcherDebounceTimer
├── Themes/       light/dark resource dictionaries
└── Assets/

src/Prism.Tests/  xUnit; 84 tests covering SearchViewModel, SettingsStore,
                  HostScopeController, host adapters, adapter catalog
```

Testability rule that shaped the layout: anything `SearchViewModel` depends on is
behind an interface (`ISearchClient`, `IDebounceTimerFactory`, `ISearchScheduler`,
`IIndexGenerationClient`, `IHostAdapter`, `IRootValidator`) so tests run without a
Dispatcher or a live pipe.

---

## How to Fill These Guidelines

For each guideline file:

1. Document your project's **actual conventions** (not ideals)
2. Include **code examples** from your codebase
3. List **forbidden patterns** and why
4. Add **common mistakes** your team has made

The goal is to help AI assistants and new team members understand how YOUR project works.

---

**Language**: All documentation should be written in **English**.
