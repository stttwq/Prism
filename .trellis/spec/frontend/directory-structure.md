# Directory Structure

> How frontend code is organized in `src/Prism` (C# / .NET 8 / WPF).

---

## Overview

This is a WPF tray application, not a web app. It is organized MVVM-ish: `Models`
hold protocol and settings shapes, `ViewModels` hold state machines, `Windows` and
`Controls` hold XAML, and `Services` hold everything that touches the outside
world (named pipes, Win32, COM, registry, filesystem).

The organizing constraint is **testability without a Dispatcher or a live pipe**.
Anything `SearchViewModel` depends on sits behind an interface so `src/Prism.Tests`
can substitute it. New outside-world dependencies follow the same rule.

---

## Directory Layout

```text
src/Prism/
├── App.xaml(.cs)        startup, DI wiring of production service implementations
├── GlobalUsings.cs      WPF-vs-WinForms type preference (avoid per-file aliases)
├── Models/
│   ├── AppState.cs          observable UI state
│   ├── Settings.cs          persisted settings + defaults (schema version 1)
│   ├── SearchResult.cs      wire shape; raw kind string → SearchResultKind
│   ├── ActionItem.cs        action panel entry
│   ├── HostContext.cs       host kind, root, capabilities
│   └── WebEngineEditItem.cs settings-grid editing shape
├── ViewModels/
│   ├── SearchViewModel.cs        query → debounce → search → results/actions
│   ├── HostScopeController.cs    root scope, Ctrl+G toggle, scope label text
│   ├── SearchWindowFocusPolicy.cs  explicit deactivate/hide policy
│   └── SettingsViewModel.cs
├── Windows/             SearchWindow, SettingsWindow
├── Controls/            SearchHeader, ResultList, ActionPanel, PinButton,
│                        HotkeyRecorderBox  (all UserControl)
├── Services/
│   ├── PipeClient.cs               broker pipe; paired request/response lines
│   ├── IndexerGenerationClient.cs  generation notifications
│   ├── SearchAbstractions.cs       ISearchClient, IDebounceTimer(+Factory),
│   │                               ISearchScheduler
│   ├── DispatcherDebounceTimer.cs  production IDebounceTimer
│   ├── HostAdapter.cs              IHostAdapter contract + DisabledHostAdapter
│   ├── ExplorerHostAdapter.cs      Shell COM path, IExplorerShellAccess
│   ├── DirectoryOpusHostAdapter.cs dopusrt.exe, IDirectoryOpusRuntime
│   ├── HostAdapterCatalog.cs       host detection dispatch
│   ├── HostProcessGuard.cs         elevation / process identity checks
│   ├── ShellBrowserInterop.cs      IShellBrowser/IShellView, active-tab Z-order
│   ├── NativeWindowQuery.cs        foreground HWND, window class queries
│   ├── RootValidation.cs           IRootValidator, IHostWindowProbe
│   ├── SettingsStore.cs            data dir resolution + atomic write
│   ├── AutoStartService.cs         HKCU Run key (quoted paths)
│   ├── HotkeyService.cs            double-Ctrl / custom combo, WH_KEYBOARD_LL
│   ├── TrayService.cs              NotifyIcon lifecycle
│   ├── ThemeWatcher.cs             follow system light/dark
│   └── IconCache.cs                bounded FIFO icon cache
├── Themes/              Styles.xaml, Tokens.Light.xaml, Tokens.Dark.xaml
└── Assets/              packaged icons

src/Prism.Tests/         xUnit, 84 tests:
                         SearchViewModelTests, SettingsStoreTests,
                         HostScopeControllerTests, ExplorerHostAdapterTests,
                         DirectoryOpusHostAdapterTests, HostAdapterCatalogTests
```

---

## Module Organization

- **New outside-world dependency → new interface in `Services`.** Production
  implementation registered in `App.xaml.cs`; tests supply a fake. Interfaces used
  only by one adapter live next to that adapter (`IExplorerShellAccess` in
  `ExplorerHostAdapter.cs`); interfaces shared by the search pipeline live in
  `SearchAbstractions.cs`.
- **New host integration → new `IHostAdapter` implementation** plus registration in
  `HostAdapterCatalog`, plus a settings toggle defaulting to `false`, plus rows in
  the compat matrix. Never enable a host adapter by default before its matrix is
  signed.
- **State machines belong in `ViewModels`, not in code-behind.** Window code-behind
  handles input routing and Win32 window concerns and delegates decisions
  (`SearchWindowFocusPolicy`, `HostScopeController`).
- **Controls are `UserControl` with imperative setters** (`SetMode`) and events
  raised upward to `SearchWindow`; they do not reach into view models directly.
- **Theming goes through `DynamicResource`** against `Tokens.*.xaml`; no hardcoded
  brushes in controls, so live light/dark switching works.

---

## Naming Conventions

- One public type per file, filename equals type name.
- Interfaces are `I`-prefixed; the production implementation of `IFoo` is either
  `Foo` or `FooAdapter`/`DispatcherFooTimer` when it wraps a platform primitive.
- Test files are `<TypeUnderTest>Tests.cs`.
- XAML `x:Name` uses PascalCase; event handlers are `On<Event>`.
- Namespaces follow folders (`Prism.Services`, `Prism.ViewModels`, `Prism.Controls`).

---

## Examples

- Interface extraction done right: `SearchAbstractions.cs` — `SearchViewModel`
  never news up a `DispatcherTimer` or a pipe, so its tests need no UI thread.
- Host adapter shape: `DirectoryOpusHostAdapter` isolates the external process
  behind `IDirectoryOpusRuntime`, so tests cover XML parsing and failure paths
  without Opus installed.
- Non-obvious platform knowledge belongs in comments at the call site:
  `ShellBrowserInterop` records why active-tab detection uses
  `ShellTabWindowClass` sibling Z-order rather than `IsWindowVisible`.
