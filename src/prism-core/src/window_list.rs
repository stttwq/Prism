//! G5: bounded top-level window enumeration, identity, and snapshot tokens.
//!
//! Two rules shape this module:
//!
//! 1. **HWNDs do not leave the broker.** Search results carry an opaque token that is
//!    only valid inside the snapshot that produced it. `resolve` is the single place a
//!    token turns back into a handle, and it re-verifies identity while doing so.
//! 2. **Filtering is pure.** Win32 enumeration fills `RawWindow`, and `is_switchable`
//!    decides on that data alone, so every filter rule is unit-testable on non-Windows
//!    CI where there are no windows to enumerate.

use std::sync::RwLock;

/// Hard cap on enumerated windows. A normal session has tens; hitting this means
/// something pathological, so truncate and log rather than grow without bound.
pub const MAX_WINDOWS: usize = 512;

/// Token layout: `generation << INDEX_BITS | index`. Decimal-encoded, so it still
/// satisfies the existing `TargetKind::Window` "must be numeric" validation.
const INDEX_BITS: u32 = 10;
const INDEX_MASK: u64 = (1 << INDEX_BITS) - 1;

/// Raw per-window facts collected from Win32, before any policy is applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawWindow {
    pub handle: u64,
    pub pid: u32,
    pub title: String,
    pub app_name: String,
    pub app_path: String,
    /// Win32 window class. Used only to drop UWP inner core windows — see [`CORE_WINDOW_CLASS`].
    pub class_name: String,
    pub is_visible: bool,
    pub is_tool_window: bool,
    /// Raw `DWM_CLOAKED_*` bits from `DWMWA_CLOAKED`; 0 when the window is not cloaked.
    /// Kept as bits rather than a bool because `DWM_CLOAKED_SHELL` alone cannot tell a
    /// suspended UWP app on this desktop from a window on another virtual desktop —
    /// see [`is_switchable`].
    pub cloaked: u32,
    /// True only when `IVirtualDesktopManager` positively reports the window lives on
    /// another virtual desktop. Defaults to false so a failed query never hides a window
    /// that is really here.
    pub is_on_other_desktop: bool,
    pub has_owner: bool,
    pub is_minimized: bool,
}

/// A window that passed filtering, as exposed to the rest of the broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowEntry {
    pub handle: u64,
    pub pid: u32,
    pub title: String,
    pub app_name: String,
    pub app_path: String,
    pub is_minimized: bool,
}

impl WindowEntry {
    /// Stable history key: application identity + normalized title. Deliberately not the
    /// HWND — handles are recycled by Windows, so a persisted handle would eventually
    /// point at an unrelated window.
    pub fn history_key(&self) -> String {
        format!(
            "{}|{}",
            self.app_name.to_lowercase(),
            normalize_title(&self.title)
        )
    }
}

/// Collapses whitespace and case so a title whose suffix churns (`* file.txt - Editor`)
/// still matches its history entry.
pub fn normalize_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `DWM_CLOAKED_*` bits, redeclared so the pure filter and its tests compile on
/// non-Windows CI. `cloaked_bits_match_win32` asserts these against the real constants.
pub const CLOAKED_APP: u32 = 1;
pub const CLOAKED_SHELL: u32 = 2;
pub const CLOAKED_INHERITED: u32 = 4;

/// A UWP app has two top-level windows: an `ApplicationFrameWindow` host (what Alt-Tab
/// shows and what activation must target) and this inner core window. Both carry the same
/// title, so keeping both would list 设置 twice. Excluding the inner one also drops
/// `TextInputHost "Windows 输入体验"`, which is a bare `CoreWindow` with no frame host and
/// which Alt-Tab never offers.
pub const CORE_WINDOW_CLASS: &str = "Windows.UI.Core.CoreWindow";

/// The single filter policy. `self_pids` is Prism's own processes so it never offers to
/// switch to itself.
///
/// Rejects: invisible, empty-title, tool windows, windows on other virtual desktops,
/// app-cloaked windows, UWP inner core windows, owned/subordinate windows, and Prism's own
/// windows.
///
/// **Cloaking is deliberately not a single rule.** `DWMWA_CLOAKED` returns the same
/// `DWM_CLOAKED_SHELL` for a suspended UWP app on this desktop — which Alt-Tab shows and
/// so must stay switchable — and for a window parked on another virtual desktop, which
/// must not. Treating "cloaked at all" as unswitchable dropped every suspended UWP app.
/// So only `DWM_CLOAKED_APP` (the app hid itself) rejects here, and the other-desktop
/// case is decided by `IVirtualDesktopManager` instead. `DWM_CLOAKED_INHERITED` needs no
/// rule of its own: it only appears on windows whose owner is cloaked, and an owned
/// window is already rejected.
pub fn is_switchable(window: &RawWindow, self_pids: &[u32]) -> bool {
    window.handle != 0
        && window.is_visible
        && !window.title.trim().is_empty()
        && !window.is_tool_window
        && !window.is_on_other_desktop
        && window.cloaked & CLOAKED_APP == 0
        && window.class_name != CORE_WINDOW_CLASS
        && !window.has_owner
        && !self_pids.contains(&window.pid)
}

/// Applies the filter and the bound. Returns the kept entries plus whether the cap
/// truncated the list, so the caller can log it.
pub fn select(raw: Vec<RawWindow>, self_pids: &[u32]) -> (Vec<WindowEntry>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    for window in raw {
        if !is_switchable(&window, self_pids) {
            continue;
        }
        if kept.len() >= MAX_WINDOWS {
            truncated = true;
            break;
        }
        kept.push(WindowEntry {
            handle: window.handle,
            pid: window.pid,
            title: window.title,
            app_name: window.app_name,
            app_path: window.app_path,
            is_minimized: window.is_minimized,
        });
    }
    (kept, truncated)
}

/// Why a token could not be turned back into a live window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// Not decimal, or decodes to a slot the snapshot never had.
    Malformed,
    /// Token came from an earlier enumeration. Stale UI, not a live target.
    StaleGeneration,
    /// The window is gone since enumeration.
    WindowGone,
    /// The handle is alive but is now a different window — classic HWND reuse.
    IdentityChanged,
}

impl ResolveError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Malformed => "window target is not a valid enumeration token",
            Self::StaleGeneration => "window list is stale, please search again",
            Self::WindowGone => "the window no longer exists",
            Self::IdentityChanged => "the window handle now belongs to a different window",
        }
    }
}

/// Live re-probe of a handle, injected so `resolve` is testable without real windows.
pub trait WindowProbe {
    /// Current facts for `handle`, or `None` if it is not a live window.
    fn probe(&self, handle: u64) -> Option<RawWindow>;
}

/// Holds only the most recent enumeration. Replaced wholesale each time, so there is no
/// long-lived window list and no unbounded growth across repeated queries.
pub struct WindowSnapshotStore {
    inner: RwLock<Snapshot>,
}

#[derive(Default)]
struct Snapshot {
    generation: u64,
    entries: Vec<WindowEntry>,
}

impl Default for WindowSnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowSnapshotStore {
    pub fn new() -> Self {
        Self {
            // Generation 0 is reserved as "nothing enumerated yet" so a token minted
            // before the first publish can never validate.
            inner: RwLock::new(Snapshot::default()),
        }
    }

    /// Replaces the snapshot and returns tokens paired with their entries, in order.
    pub fn publish(&self, entries: Vec<WindowEntry>) -> Vec<(String, WindowEntry)> {
        let mut guard = match self.inner.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.generation = guard.generation.saturating_add(1);
        guard.entries = entries;
        guard
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (encode_token(guard.generation, index), entry.clone()))
            .collect()
    }

    pub fn generation(&self) -> u64 {
        self.inner.read().map(|guard| guard.generation).unwrap_or(0)
    }

    /// Turns a token back into a verified live window. This is the only place a handle
    /// crosses back out, and it re-checks pid plus title fingerprint to catch the case
    /// where Windows recycled the handle between enumeration and activation.
    pub fn resolve(
        &self,
        token: &str,
        probe: &dyn WindowProbe,
    ) -> Result<WindowEntry, ResolveError> {
        let value: u64 = token.trim().parse().map_err(|_| ResolveError::Malformed)?;
        let (generation, index) = decode_token(value);
        let entry = {
            let guard = self.inner.read().map_err(|_| ResolveError::Malformed)?;
            if generation == 0 || generation != guard.generation {
                return Err(ResolveError::StaleGeneration);
            }
            guard
                .entries
                .get(index)
                .cloned()
                .ok_or(ResolveError::Malformed)?
        };

        let live = probe.probe(entry.handle).ok_or(ResolveError::WindowGone)?;
        if !live.is_visible {
            return Err(ResolveError::WindowGone);
        }
        if live.pid != entry.pid {
            return Err(ResolveError::IdentityChanged);
        }
        // The title may legitimately change while the window stays the same (tab switch,
        // dirty marker), so only the app identity is required to hold.
        if !live.app_name.is_empty() && !live.app_name.eq_ignore_ascii_case(&entry.app_name) {
            return Err(ResolveError::IdentityChanged);
        }
        Ok(WindowEntry {
            title: if live.title.trim().is_empty() {
                entry.title.clone()
            } else {
                live.title
            },
            is_minimized: live.is_minimized,
            ..entry
        })
    }
}

fn encode_token(generation: u64, index: usize) -> String {
    ((generation << INDEX_BITS) | (index as u64 & INDEX_MASK)).to_string()
}

fn decode_token(value: u64) -> (u64, usize) {
    (value >> INDEX_BITS, (value & INDEX_MASK) as usize)
}

#[cfg(windows)]
mod platform {
    use super::RawWindow;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, MAX_PATH, TRUE};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Shell::IVirtualDesktopManager;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindow, GetWindowLongW, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, GWL_EXSTYLE,
        GW_OWNER, WS_EX_TOOLWINDOW,
    };

    /// The other-desktop oracle, created once per enumeration.
    ///
    /// `DWMWA_CLOAKED` cannot separate "suspended UWP on this desktop" from "on another
    /// virtual desktop", so this answers the second question directly. Every failure path
    /// reports `false` (= "on this desktop"): showing a window the user cannot switch to
    /// is a far milder bug than silently hiding the window they asked for, which is the
    /// bug this type exists to fix.
    pub struct DesktopOracle {
        manager: Option<IVirtualDesktopManager>,
        need_uninit: bool,
    }

    impl DesktopOracle {
        pub fn new() -> Self {
            use windows::Win32::System::Com::{
                CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
            };
            use windows::Win32::UI::Shell::VirtualDesktopManager;

            // Failure is fine and expected: the calling thread may already be in an
            // apartment. Only uninitialize what we actually initialized.
            let need_uninit = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
            let manager =
                unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_INPROC_SERVER) }
                    .ok();
            if manager.is_none() {
                crate::log(
                    "IVirtualDesktopManager unavailable; windows on other virtual desktops \
                     will be listed",
                );
            }
            Self {
                manager,
                need_uninit,
            }
        }

        /// True only on a positive "not on the current desktop" answer.
        pub fn is_on_other_desktop(&self, hwnd: HWND) -> bool {
            let Some(manager) = self.manager.as_ref() else {
                return false;
            };
            // Returns E_INVALIDARG for windows the shell does not track (and the HWND may
            // die mid-enumeration); either way, do not hide it.
            unsafe { manager.IsWindowOnCurrentVirtualDesktop(hwnd) }
                .map(|on_current| !on_current.as_bool())
                .unwrap_or(false)
        }
    }

    impl Drop for DesktopOracle {
        fn drop(&mut self) {
            // Release the interface before leaving the apartment it was created in.
            self.manager = None;
            if self.need_uninit {
                unsafe { windows::Win32::System::Com::CoUninitialize() };
            }
        }
    }

    /// Enumerate every top-level window with the raw facts the filter needs.
    pub fn enumerate() -> Vec<RawWindow> {
        let mut sink = (Vec::new(), DesktopOracle::new());
        let ptr = &mut sink as *mut (Vec<RawWindow>, DesktopOracle) as isize;
        // EnumWindows can fail if a callback returns FALSE; we always return TRUE, so an
        // error here means the system refused to enumerate. Partial results are fine.
        let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(ptr)) };
        sink.0
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let (windows, oracle) = &mut *(lparam.0 as *mut (Vec<RawWindow>, DesktopOracle));
        windows.push(collect(hwnd, Some(oracle)));
        TRUE
    }

    /// Facts for one handle. `oracle` is `None` on the activation-time re-probe, which
    /// only needs identity and visibility — `resolve` never re-applies the desktop filter,
    /// because a window the user picked stays a valid target even if they have since
    /// switched desktops.
    pub fn collect(hwnd: HWND, oracle: Option<&DesktopOracle>) -> RawWindow {
        if hwnd.0.is_null() || unsafe { !IsWindow(hwnd).as_bool() } {
            return RawWindow::default();
        }
        let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
        let mut cloaked_bits: u32 = 0;
        let cloaked = if unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked_bits as *mut u32 as *mut _,
                std::mem::size_of::<u32>() as u32,
            )
        }
        .is_ok()
        {
            cloaked_bits
        } else {
            0
        };
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let app_path = process_path(pid).unwrap_or_default();
        let app_name = std::path::Path::new(&app_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        RawWindow {
            handle: hwnd.0 as u64,
            pid,
            title: window_title(hwnd),
            app_name,
            app_path,
            class_name: window_class(hwnd),
            is_visible: unsafe { IsWindowVisible(hwnd).as_bool() },
            is_tool_window: ex_style & WS_EX_TOOLWINDOW.0 != 0,
            cloaked,
            is_on_other_desktop: oracle
                .map(|oracle| oracle.is_on_other_desktop(hwnd))
                .unwrap_or(false),
            has_owner: !unsafe { GetWindow(hwnd, GW_OWNER) }
                .unwrap_or_default()
                .0
                .is_null(),
            is_minimized: unsafe { IsIconic(hwnd).as_bool() },
        }
    }

    fn window_title(hwnd: HWND) -> String {
        let length = unsafe { GetWindowTextLengthW(hwnd) };
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let written = unsafe { GetWindowTextW(hwnd, &mut buffer) };
        if written <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buffer[..written as usize])
    }

    /// Win32 caps class names at 256 chars, so one fixed buffer always suffices.
    fn window_class(hwnd: HWND) -> String {
        let mut buffer = [0u16; 257];
        let written = unsafe { GetClassNameW(hwnd, &mut buffer) };
        if written <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buffer[..written as usize])
    }

    fn process_path(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buffer = vec![0u16; MAX_PATH as usize];
        let mut size = buffer.len() as u32;
        let result = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buffer.as_mut_ptr()),
                &mut size,
            )
        };
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
        result.ok()?;
        Some(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

/// Live handle re-probe backed by Win32. Non-Windows builds always report "gone", which
/// keeps the broker compiling and testable on CI Linux.
pub struct SystemWindowProbe;

impl WindowProbe for SystemWindowProbe {
    #[cfg(windows)]
    fn probe(&self, handle: u64) -> Option<RawWindow> {
        use windows::Win32::Foundation::HWND;
        if handle == 0 {
            return None;
        }
        let raw = platform::collect(HWND(handle as *mut _), None);
        (raw.handle != 0).then_some(raw)
    }

    #[cfg(not(windows))]
    fn probe(&self, _handle: u64) -> Option<RawWindow> {
        None
    }
}

/// Enumerate, filter, and publish in one step. Returns tokens paired with entries.
pub fn enumerate_and_publish(
    store: &WindowSnapshotStore,
    self_pids: &[u32],
) -> Vec<(String, WindowEntry)> {
    #[cfg(windows)]
    let raw = platform::enumerate();
    #[cfg(not(windows))]
    let raw: Vec<RawWindow> = Vec::new();

    let (entries, truncated) = select(raw, self_pids);
    if truncated {
        crate::log(format!(
            "window enumeration hit the {MAX_WINDOWS} cap, list truncated"
        ));
    }
    store.publish(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SELF_PID: u32 = 4242;

    fn switchable() -> RawWindow {
        RawWindow {
            handle: 0x1234,
            pid: 100,
            title: "Untitled - Notepad".into(),
            app_name: "notepad".into(),
            app_path: r"C:\Windows\notepad.exe".into(),
            class_name: "Notepad".into(),
            is_visible: true,
            is_tool_window: false,
            cloaked: 0,
            is_on_other_desktop: false,
            has_owner: false,
            is_minimized: false,
        }
    }

    #[derive(Default)]
    struct FakeProbe(HashMap<u64, RawWindow>);

    impl FakeProbe {
        fn with(handle: u64, window: RawWindow) -> Self {
            let mut map = HashMap::new();
            map.insert(handle, window);
            Self(map)
        }
    }

    impl WindowProbe for FakeProbe {
        fn probe(&self, handle: u64) -> Option<RawWindow> {
            self.0.get(&handle).cloned()
        }
    }

    #[test]
    fn baseline_window_is_switchable() {
        assert!(is_switchable(&switchable(), &[SELF_PID]));
    }

    #[test]
    fn invisible_window_is_filtered() {
        let window = RawWindow {
            is_visible: false,
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn untitled_and_whitespace_only_windows_are_filtered() {
        for title in ["", "   ", "\t"] {
            let window = RawWindow {
                title: title.into(),
                ..switchable()
            };
            assert!(!is_switchable(&window, &[SELF_PID]), "title {title:?}");
        }
    }

    #[test]
    fn tool_window_is_filtered() {
        let window = RawWindow {
            is_tool_window: true,
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn app_cloaked_window_is_filtered() {
        // DWM_CLOAKED_APP means the app hid the window itself — genuinely not switchable.
        let window = RawWindow {
            cloaked: CLOAKED_APP,
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn window_on_another_virtual_desktop_is_filtered() {
        // Shell-cloaked *and* positively reported elsewhere by IVirtualDesktopManager.
        let window = RawWindow {
            cloaked: CLOAKED_SHELL,
            is_on_other_desktop: true,
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    /// The bug this module was fixed for: a suspended UWP app (Settings, Calculator) is
    /// shell-cloaked while sitting on the current desktop. Alt-Tab lists it, so Prism must
    /// too. Before the fix, `is_cloaked` was one bool and this window was dropped.
    #[test]
    fn suspended_uwp_on_this_desktop_stays_switchable() {
        let window = RawWindow {
            cloaked: CLOAKED_SHELL,
            is_on_other_desktop: false,
            app_name: "ApplicationFrameHost".into(),
            class_name: "ApplicationFrameWindow".into(),
            title: "设置".into(),
            ..switchable()
        };
        assert!(
            is_switchable(&window, &[SELF_PID]),
            "suspended UWP apps must stay switchable — Alt-Tab shows them"
        );
    }

    /// A UWP app exposes both an `ApplicationFrameWindow` and an inner `CoreWindow` with the
    /// same title. Keeping both listed 设置 twice, so only the frame host survives.
    #[test]
    fn uwp_inner_core_window_is_filtered() {
        let window = RawWindow {
            cloaked: CLOAKED_SHELL,
            app_name: "SystemSettings".into(),
            class_name: CORE_WINDOW_CLASS.into(),
            title: "设置".into(),
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    /// `TextInputHost "Windows 输入体验"` is a bare `CoreWindow` with no frame host. Alt-Tab
    /// never offers it, and un-hiding suspended UWP apps must not drag it in.
    #[test]
    fn text_input_host_is_not_offered() {
        let window = RawWindow {
            cloaked: CLOAKED_SHELL,
            app_name: "TextInputHost".into(),
            class_name: CORE_WINDOW_CLASS.into(),
            title: "Windows 输入体验".into(),
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    /// Only the exact class is excluded; a normal app whose class merely resembles it stays.
    #[test]
    fn a_normal_window_with_a_similar_class_is_kept() {
        for class in [
            "Windows.UI.Core.CoreWindowHost",
            "CoreWindow",
            "Chrome_WidgetWin_1",
        ] {
            let window = RawWindow {
                class_name: class.into(),
                ..switchable()
            };
            assert!(is_switchable(&window, &[SELF_PID]), "class {class:?}");
        }
    }

    #[test]
    fn a_failed_desktop_query_shows_the_window_rather_than_hiding_it() {
        // DesktopOracle reports false when COM or the query fails. A shell-cloaked window
        // must then still be offered: over-showing beats silently losing the target.
        let window = RawWindow {
            cloaked: CLOAKED_SHELL,
            is_on_other_desktop: false,
            ..switchable()
        };
        assert!(is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn app_cloaked_is_rejected_even_on_the_current_desktop() {
        let window = RawWindow {
            cloaked: CLOAKED_APP | CLOAKED_SHELL,
            is_on_other_desktop: false,
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    /// The redeclared bits must match Win32, or the filter reads the wrong flag.
    #[test]
    #[cfg(windows)]
    fn cloaked_bits_match_win32() {
        use windows::Win32::Graphics::Dwm::{
            DWM_CLOAKED_APP, DWM_CLOAKED_INHERITED, DWM_CLOAKED_SHELL,
        };
        assert_eq!(CLOAKED_APP, DWM_CLOAKED_APP);
        assert_eq!(CLOAKED_SHELL, DWM_CLOAKED_SHELL);
        assert_eq!(CLOAKED_INHERITED, DWM_CLOAKED_INHERITED);
    }

    #[test]
    fn owned_window_is_filtered() {
        let window = RawWindow {
            has_owner: true,
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn prism_own_windows_are_filtered() {
        let window = RawWindow {
            pid: SELF_PID,
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn null_handle_is_filtered() {
        let window = RawWindow {
            handle: 0,
            ..switchable()
        };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn minimized_window_stays_switchable() {
        let window = RawWindow {
            is_minimized: true,
            ..switchable()
        };
        assert!(is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn select_enforces_the_cap_and_reports_truncation() {
        let raw: Vec<RawWindow> = (0..MAX_WINDOWS + 10)
            .map(|i| RawWindow {
                handle: i as u64 + 1,
                ..switchable()
            })
            .collect();
        let (entries, truncated) = select(raw, &[SELF_PID]);
        assert_eq!(entries.len(), MAX_WINDOWS);
        assert!(truncated);
    }

    #[test]
    fn select_does_not_report_truncation_under_the_cap() {
        let (entries, truncated) = select(vec![switchable()], &[SELF_PID]);
        assert_eq!(entries.len(), 1);
        assert!(!truncated);
    }

    #[test]
    fn tokens_are_numeric_so_existing_target_validation_holds() {
        let store = WindowSnapshotStore::new();
        let published = store.publish(vec![entry()]);
        assert!(published[0].0.parse::<u64>().is_ok());
    }

    fn entry() -> WindowEntry {
        WindowEntry {
            handle: 0x1234,
            pid: 100,
            title: "Untitled - Notepad".into(),
            app_name: "notepad".into(),
            app_path: r"C:\Windows\notepad.exe".into(),
            is_minimized: false,
        }
    }

    #[test]
    fn resolve_returns_the_live_window() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        let probe = FakeProbe::with(0x1234, switchable());
        let resolved = store.resolve(&token, &probe).expect("resolves");
        assert_eq!(resolved.handle, 0x1234);
        assert_eq!(resolved.pid, 100);
    }

    #[test]
    fn token_from_a_previous_enumeration_is_rejected() {
        let store = WindowSnapshotStore::new();
        let stale = store.publish(vec![entry()])[0].0.clone();
        store.publish(vec![entry()]);
        let probe = FakeProbe::with(0x1234, switchable());
        assert_eq!(
            store.resolve(&stale, &probe),
            Err(ResolveError::StaleGeneration)
        );
    }

    #[test]
    fn token_minted_before_any_enumeration_cannot_validate() {
        let store = WindowSnapshotStore::new();
        let probe = FakeProbe::default();
        assert_eq!(
            store.resolve("0", &probe),
            Err(ResolveError::StaleGeneration)
        );
    }

    #[test]
    fn non_numeric_token_is_malformed() {
        let store = WindowSnapshotStore::new();
        store.publish(vec![entry()]);
        let probe = FakeProbe::default();
        assert_eq!(
            store.resolve(r"C:\notepad.exe", &probe),
            Err(ResolveError::Malformed)
        );
    }

    #[test]
    fn token_pointing_past_the_snapshot_is_malformed() {
        let store = WindowSnapshotStore::new();
        store.publish(vec![entry()]);
        let probe = FakeProbe::default();
        let bogus = encode_token(store.generation(), 7);
        assert_eq!(store.resolve(&bogus, &probe), Err(ResolveError::Malformed));
    }

    #[test]
    fn closed_window_resolves_to_window_gone() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        let probe = FakeProbe::default();
        assert_eq!(store.resolve(&token, &probe), Err(ResolveError::WindowGone));
    }

    #[test]
    fn window_hidden_since_enumeration_resolves_to_window_gone() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        let probe = FakeProbe::with(
            0x1234,
            RawWindow {
                is_visible: false,
                ..switchable()
            },
        );
        assert_eq!(store.resolve(&token, &probe), Err(ResolveError::WindowGone));
    }

    #[test]
    fn recycled_handle_with_a_new_pid_is_rejected() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        // Same handle, different process: Windows reused the HWND.
        let probe = FakeProbe::with(
            0x1234,
            RawWindow {
                pid: 999,
                ..switchable()
            },
        );
        assert_eq!(
            store.resolve(&token, &probe),
            Err(ResolveError::IdentityChanged)
        );
    }

    #[test]
    fn recycled_handle_with_a_new_app_is_rejected() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        let probe = FakeProbe::with(
            0x1234,
            RawWindow {
                app_name: "calc".into(),
                ..switchable()
            },
        );
        assert_eq!(
            store.resolve(&token, &probe),
            Err(ResolveError::IdentityChanged)
        );
    }

    #[test]
    fn title_change_alone_does_not_invalidate_the_target() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        let probe = FakeProbe::with(
            0x1234,
            RawWindow {
                title: "notes.txt - Notepad".into(),
                ..switchable()
            },
        );
        let resolved = store
            .resolve(&token, &probe)
            .expect("still the same window");
        assert_eq!(resolved.title, "notes.txt - Notepad");
    }

    #[test]
    fn resolve_reports_current_minimized_state() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        let probe = FakeProbe::with(
            0x1234,
            RawWindow {
                is_minimized: true,
                ..switchable()
            },
        );
        assert!(
            store
                .resolve(&token, &probe)
                .expect("resolves")
                .is_minimized
        );
    }

    #[test]
    fn history_key_is_stable_across_title_churn() {
        let clean = WindowEntry {
            title: "report.docx - Word".into(),
            ..entry()
        };
        let dirty = WindowEntry {
            title: "report.docx  -  Word".into(),
            ..entry()
        };
        assert_eq!(clean.history_key(), dirty.history_key());
    }

    #[test]
    fn history_key_separates_different_apps() {
        let a = entry();
        let b = WindowEntry {
            app_name: "wordpad".into(),
            ..entry()
        };
        assert_ne!(a.history_key(), b.history_key());
    }

    /// 步骤 8「多窗口同应用」：两个 Notepad 必须是两个可独立选中的条目，且历史键不同 ——
    /// 否则切到其中一个会把另一个也标记为「最近用过」，最近列表就永远指错窗口。
    #[test]
    fn two_windows_of_the_same_app_stay_distinct() {
        let raw = vec![
            RawWindow {
                handle: 0x11,
                title: "a.txt - Notepad".into(),
                ..switchable()
            },
            RawWindow {
                handle: 0x22,
                title: "b.txt - Notepad".into(),
                ..switchable()
            },
        ];
        let (entries, _) = select(raw, &[SELF_PID]);
        assert_eq!(entries.len(), 2, "same-app windows must not be collapsed");
        assert_ne!(entries[0].handle, entries[1].handle);
        assert_ne!(
            entries[0].history_key(),
            entries[1].history_key(),
            "same app, different documents — history must not conflate them"
        );
    }

    /// 同应用同标题（两个未命名 Notepad）确实会共享历史键。这是刻意的：标题是唯一稳定的
    /// 区分信号，而 HWND 不能持久化。此测试把这条边界钉住，避免被当成 bug「修」掉。
    ///
    /// **这条是文档而不是测试**（按 state-management.md 的规矩，得说清楚）：把 `history_key`
    /// 里的标题整个删掉，它依然通过——因为「相等」正是它断言的东西。真正锁住标题参与计算的
    /// 是上面那条 `two_windows_of_the_same_app_stay_distinct`，同一个 mutation 下只有它转红。
    #[test]
    fn same_app_same_title_deliberately_shares_one_history_key() {
        let raw = vec![
            RawWindow {
                handle: 0x11,
                ..switchable()
            },
            RawWindow {
                handle: 0x22,
                ..switchable()
            },
        ];
        let (entries, _) = select(raw, &[SELF_PID]);
        assert_eq!(entries.len(), 2, "both are still separately selectable");
        assert_eq!(
            entries[0].history_key(),
            entries[1].history_key(),
            "identical app+title is indistinguishable without persisting an HWND"
        );
    }

    #[test]
    fn history_key_never_contains_the_handle() {
        assert!(!entry().history_key().contains("1234"));
    }

    /// Live-desktop probe for the Win32 half of this module.
    ///
    /// Every other test here feeds hand-built `RawWindow` fixtures to the pure filter, so
    /// `platform::enumerate` / `platform::collect` — EnumWindows, the DWM cloaked query,
    /// QueryFullProcessImageNameW — are otherwise compiled but never executed. This is the
    /// only test that runs them.
    ///
    /// `#[ignore]` because it asserts against whatever the developer's desktop happens to
    /// be, which is not a stable fixture. Run explicitly:
    ///
    /// ```text
    /// cargo test --manifest-path src/prism-core/Cargo.toml \
    ///   window_list::tests::live_enumeration_probe -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "live desktop probe; run explicitly with --ignored"]
    fn live_enumeration_probe() {
        let store = WindowSnapshotStore::new();
        let self_pids = [std::process::id()];
        let published = enumerate_and_publish(&store, &self_pids);

        println!("enumerated {} switchable window(s)", published.len());
        for (token, entry) in &published {
            // Titles carry user data, so print a bounded prefix only.
            let title: String = entry.title.chars().take(24).collect();
            println!(
                "  token={token:<8} pid={:<6} app={:<20} min={} title={title:?}",
                entry.pid, entry.app_name, entry.is_minimized
            );
        }

        // Invariants that must hold against a real desktop, not a fixture.
        for (token, entry) in &published {
            assert!(
                token.parse::<u64>().is_ok(),
                "token must stay numeric for TargetKind::Window validation"
            );
            assert!(
                !entry.title.trim().is_empty(),
                "untitled window leaked through"
            );
            assert_ne!(entry.pid, std::process::id(), "own window leaked through");
            assert_ne!(entry.handle, 0, "null handle leaked through");
        }
        assert!(published.len() <= MAX_WINDOWS, "cap not enforced");

        let tokens: std::collections::HashSet<_> =
            published.iter().map(|(token, _)| token.clone()).collect();
        assert_eq!(tokens.len(), published.len(), "tokens must be unique");

        // A desktop running a test has at least one switchable window; zero means the
        // filter is rejecting everything, which is the failure this probe exists to catch.
        #[cfg(windows)]
        assert!(
            !published.is_empty(),
            "no switchable windows found on a live desktop — filter is over-rejecting"
        );

        // Round-trip one token through the real probe to prove resolve works end to end.
        #[cfg(windows)]
        if let Some((token, entry)) = published.first() {
            let resolved = store
                .resolve(token, &SystemWindowProbe)
                .expect("freshly minted token must resolve against a live window");
            assert_eq!(resolved.handle, entry.handle);
            assert_eq!(resolved.pid, entry.pid);
            println!("resolve round-trip ok for token={token}");
        }
    }

    /// Rejection breakdown for the live desktop. Exists because "enumerated N windows" can
    /// look like a pass while the filter is quietly dropping windows the user expects to
    /// switch to. Prints why each candidate was rejected so over-rejection is visible.
    ///
    /// ```text
    /// cargo test --manifest-path src/prism-core/Cargo.toml \
    ///   window_list::tests::live_rejection_breakdown -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "live desktop probe; run explicitly with --ignored"]
    fn live_rejection_breakdown() {
        #[cfg(not(windows))]
        println!("not windows; nothing to enumerate");

        #[cfg(windows)]
        {
            let raw = platform::enumerate();
            let self_pid = std::process::id();
            println!("EnumWindows returned {} top-level window(s)", raw.len());

            let mut counts: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            // Windows with a title but rejected: the interesting set. An untitled window is
            // almost always genuine plumbing, but a *titled* rejection may be a real window
            // the user wanted.
            let mut titled_rejects: Vec<(String, String)> = Vec::new();

            for window in &raw {
                let reason = if window.handle == 0 {
                    "null_handle"
                } else if !window.is_visible {
                    "invisible"
                } else if window.title.trim().is_empty() {
                    "untitled"
                } else if window.is_tool_window {
                    "tool_window"
                } else if window.is_on_other_desktop {
                    "other_desktop"
                } else if window.cloaked & CLOAKED_APP != 0 {
                    "cloaked_app"
                } else if window.class_name == CORE_WINDOW_CLASS {
                    "uwp_core_window"
                } else if window.has_owner {
                    "has_owner"
                } else if window.pid == self_pid {
                    "own_process"
                } else {
                    "KEPT"
                };
                *counts.entry(reason).or_default() += 1;
                if reason != "KEPT" && !window.title.trim().is_empty() {
                    let title: String = window.title.chars().take(36).collect();
                    titled_rejects
                        .push((reason.to_string(), format!("{} {title:?}", window.app_name)));
                }
            }

            for (reason, count) in &counts {
                println!("  {reason:<14} {count}");
            }
            println!("titled-but-rejected ({}):", titled_rejects.len());
            for (reason, what) in &titled_rejects {
                println!("  {reason:<14} {what}");
            }

            // The suspended-UWP fix, measured rather than asserted: these are shell-cloaked
            // windows on this desktop that the old single-bool filter dropped.
            let rescued: Vec<&RawWindow> = raw
                .iter()
                .filter(|w| {
                    w.cloaked & CLOAKED_SHELL != 0
                        && !w.is_on_other_desktop
                        && is_switchable(w, &[self_pid])
                })
                .collect();
            println!("shell-cloaked but kept ({}):", rescued.len());
            for window in &rescued {
                let title: String = window.title.chars().take(36).collect();
                println!(
                    "  cloaked=0x{:x} {} {title:?}",
                    window.cloaked, window.app_name
                );
            }
        }
    }

    #[test]
    fn publishing_replaces_rather_than_accumulates() {
        let store = WindowSnapshotStore::new();
        for _ in 0..100 {
            let published = store.publish(vec![entry(), entry()]);
            assert_eq!(published.len(), 2);
        }
        assert_eq!(store.generation(), 100);
    }
}
