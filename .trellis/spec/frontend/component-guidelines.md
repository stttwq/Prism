# Control Guidelines

> How XAML controls are built in `src/Prism`. ("Components" in the original
> template meant React components; this project has WPF `UserControl`s.)

---

## Overview

Five controls compose the search window: `SearchHeader`, `ResultList`,
`ActionPanel`, `PinButton`, `HotkeyRecorderBox`. All are `UserControl`. They are
deliberately **dumb**: they render what they are told and raise events upward.
`SearchWindow` owns input routing; `SearchViewModel` and `HostScopeController` own
decisions. A control must not reach into a view model or a service.

This keeps the state machine testable without instantiating XAML, which is why the
84 frontend tests cover view models and services rather than controls.

---

## Control Structure

A control is a `.xaml` + `.xaml.cs` pair in `Controls/`, namespace
`Prism.Controls`, one control per file, filename equals type name.

The code-behind, in order:

1. XML doc comment stating the control's role and its visible modes.
2. Constructor: `InitializeComponent()` then set the initial mode explicitly
   (`SetMode(PanelMode.Idle)`) rather than relying on XAML defaults.
3. `public event` declarations for everything the parent must decide.
4. Imperative setters the parent calls (`SetMode`, status/text setters).
5. `On<Event>` handlers that translate WPF events into the control's own events.

---

## Parent Communication

- **Down: imperative setters.** The parent calls `SetMode(...)` or assigns a
  display property. Prefer a single setter that puts the control into a coherent
  mode over several independent flags.
- **Up: `public event`.** Every control raises events; none call back into view
  models. Keyboard keys the parent needs to interpret (arrows, `Enter`, `Esc`,
  `←`/`→`, `Ctrl+N`) are forwarded up rather than handled locally, so key
  semantics live in one place.
- Setters must be **idempotent and layout-neutral when the value is unchanged**.
  Assigning the same `StatusMessage` twice must not trigger re-layout, and an
  in-flight height animation must not restart when the target is unchanged — one
  search response arrives as a burst of `PropertyChanged` notifications.

---

## Styling Patterns

- Visual values come from `Themes/Tokens.Light.xaml` / `Tokens.Dark.xaml` via
  **`DynamicResource`**, never hardcoded brushes — `ThemeWatcher` swaps token
  dictionaries at runtime, and `StaticResource` would freeze the old theme.
- Shared styles live in `Themes/Styles.xaml`; controls reference styles by key
  (`ResultListBoxStyle`) instead of setting properties inline.
- Long lists set `VirtualizingPanel.IsVirtualizing="True"` with
  `VirtualizationMode="Recycling"`; the result list can hold 1000 rows.
- Keep result-panel geometry stable while replacing a non-empty result set: do not
  insert a transient status row when rows are already visible.

---

## Accessibility

- Every action reachable by mouse must be reachable by keyboard; the window is
  keyboard-first (double-Ctrl to summon, arrows to select, `Ctrl+1..9` direct open,
  `Ctrl+G` scope toggle).
- Contrast comes from theme tokens rather than per-control colors, so both light
  and dark themes stay legible.
- Text must not be the only carrier of state where a screen reader would miss it;
  the scope label is both readable text and a clickable control with the same
  effect as `Ctrl+G`.
- Full WCAG conformance has not been validated: that needs manual testing with
  assistive technology and expert review, neither of which has been done here.

---

## Common Mistakes

- **Assuming `Activate()` succeeds.** For a tray-launched borderless window it can
  fail; `ForceActivate` plus window-level `PreviewKeyDown` is required, and Esc /
  deactivate handling must not depend on activation having worked.
- **Restarting animations on every notification.** Guard on unchanged target.
- **Touching UI from a tray callback.** `NotifyIcon` and its menu can fire off the
  UI thread; marshal with `Dispatcher.BeginInvoke` first.
- **Passing `ExecuteId` to shell icon APIs for `kind=web`/`kind=more`.** For web
  results it is a URL, not a path. `RevealSelected` must skip both kinds.
- **Leaking icon bitmaps.** `IconCache` is a bounded FIFO; clear it when the window
  hides and drop result bitmaps so the tray-resident working set shrinks.
- **Adding the More row from item count.** Only `is_truncated` justifies it;
  `items.Count == max` does not.
