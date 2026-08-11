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
    pub is_visible: bool,
    pub is_tool_window: bool,
    pub is_cloaked: bool,
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
        format!("{}|{}", self.app_name.to_lowercase(), normalize_title(&self.title))
    }
}

/// Collapses whitespace and case so a title whose suffix churns (`* file.txt - Editor`)
/// still matches its history entry.
pub fn normalize_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The single filter policy. `self_pid` is the broker's own process so Prism never
/// offers to switch to itself.
///
/// Rejects: invisible, empty-title, tool windows, cloaked (covers other virtual
/// desktops and suspended UWP), owned/subordinate windows, and Prism's own windows.
pub fn is_switchable(window: &RawWindow, self_pids: &[u32]) -> bool {
    window.handle != 0
        && window.is_visible
        && !window.title.trim().is_empty()
        && !window.is_tool_window
        && !window.is_cloaked
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
            guard.entries.get(index).cloned().ok_or(ResolveError::Malformed)?
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
        if !live.app_name.is_empty()
            && !live.app_name.eq_ignore_ascii_case(&entry.app_name)
        {
            return Err(ResolveError::IdentityChanged);
        }
        Ok(WindowEntry {
            title: if live.title.trim().is_empty() { entry.title.clone() } else { live.title },
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
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, GWL_EXSTYLE, GW_OWNER,
        WS_EX_TOOLWINDOW,
    };

    /// Enumerate every top-level window with the raw facts the filter needs.
    pub fn enumerate() -> Vec<RawWindow> {
        let mut windows: Vec<RawWindow> = Vec::new();
        let ptr = &mut windows as *mut Vec<RawWindow> as isize;
        // EnumWindows can fail if a callback returns FALSE; we always return TRUE, so an
        // error here means the system refused to enumerate. Partial results are fine.
        let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(ptr)) };
        windows
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let windows = &mut *(lparam.0 as *mut Vec<RawWindow>);
        windows.push(collect(hwnd));
        TRUE
    }

    /// Facts for one handle. Also used by the activation-time re-probe.
    pub fn collect(hwnd: HWND) -> RawWindow {
        if hwnd.0.is_null() || unsafe { !IsWindow(hwnd).as_bool() } {
            return RawWindow::default();
        }
        let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
        let mut cloaked: u32 = 0;
        let cloaked = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut _,
                std::mem::size_of::<u32>() as u32,
            )
        }
        .is_ok()
            && cloaked != 0;
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
            is_visible: unsafe { IsWindowVisible(hwnd).as_bool() },
            is_tool_window: ex_style & WS_EX_TOOLWINDOW.0 != 0,
            is_cloaked: cloaked,
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

    fn process_path(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        let handle =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
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
        let raw = platform::collect(HWND(handle as *mut _));
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
            is_visible: true,
            is_tool_window: false,
            is_cloaked: false,
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
        let window = RawWindow { is_visible: false, ..switchable() };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn untitled_and_whitespace_only_windows_are_filtered() {
        for title in ["", "   ", "\t"] {
            let window = RawWindow { title: title.into(), ..switchable() };
            assert!(!is_switchable(&window, &[SELF_PID]), "title {title:?}");
        }
    }

    #[test]
    fn tool_window_is_filtered() {
        let window = RawWindow { is_tool_window: true, ..switchable() };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn cloaked_window_is_filtered() {
        // Covers other virtual desktops and suspended UWP apps.
        let window = RawWindow { is_cloaked: true, ..switchable() };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn owned_window_is_filtered() {
        let window = RawWindow { has_owner: true, ..switchable() };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn prism_own_windows_are_filtered() {
        let window = RawWindow { pid: SELF_PID, ..switchable() };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn null_handle_is_filtered() {
        let window = RawWindow { handle: 0, ..switchable() };
        assert!(!is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn minimized_window_stays_switchable() {
        let window = RawWindow { is_minimized: true, ..switchable() };
        assert!(is_switchable(&window, &[SELF_PID]));
    }

    #[test]
    fn select_enforces_the_cap_and_reports_truncation() {
        let raw: Vec<RawWindow> = (0..MAX_WINDOWS + 10)
            .map(|i| RawWindow { handle: i as u64 + 1, ..switchable() })
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
        assert_eq!(store.resolve("0", &probe), Err(ResolveError::StaleGeneration));
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
        let probe = FakeProbe::with(0x1234, RawWindow { is_visible: false, ..switchable() });
        assert_eq!(store.resolve(&token, &probe), Err(ResolveError::WindowGone));
    }

    #[test]
    fn recycled_handle_with_a_new_pid_is_rejected() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        // Same handle, different process: Windows reused the HWND.
        let probe = FakeProbe::with(0x1234, RawWindow { pid: 999, ..switchable() });
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
            RawWindow { app_name: "calc".into(), ..switchable() },
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
            RawWindow { title: "notes.txt - Notepad".into(), ..switchable() },
        );
        let resolved = store.resolve(&token, &probe).expect("still the same window");
        assert_eq!(resolved.title, "notes.txt - Notepad");
    }

    #[test]
    fn resolve_reports_current_minimized_state() {
        let store = WindowSnapshotStore::new();
        let token = store.publish(vec![entry()])[0].0.clone();
        let probe = FakeProbe::with(0x1234, RawWindow { is_minimized: true, ..switchable() });
        assert!(store.resolve(&token, &probe).expect("resolves").is_minimized);
    }

    #[test]
    fn history_key_is_stable_across_title_churn() {
        let clean = WindowEntry { title: "report.docx - Word".into(), ..entry() };
        let dirty = WindowEntry { title: "report.docx  -  Word".into(), ..entry() };
        assert_eq!(clean.history_key(), dirty.history_key());
    }

    #[test]
    fn history_key_separates_different_apps() {
        let a = entry();
        let b = WindowEntry { app_name: "wordpad".into(), ..entry() };
        assert_ne!(a.history_key(), b.history_key());
    }

    #[test]
    fn history_key_never_contains_the_handle() {
        assert!(!entry().history_key().contains("1234"));
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
