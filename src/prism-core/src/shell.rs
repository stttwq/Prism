//! Typed Shell boundary owned by the normal-user broker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

const SHELL_QUEUE_CAPACITY: usize = 32;
const MAX_PATH_BYTES: usize = 32 * 1024;
const MAX_WEB_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionTarget {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    File,
    Directory,
    Application,
    Window,
    Web,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShellErrorKind {
    AccessDenied,
    TargetInvalid,
    Conflict,
    ElevationRequired,
    Unsupported,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellError {
    pub kind: ShellErrorKind,
    pub message: String,
}

impl ShellError {
    pub(crate) fn new(kind: ShellErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellOutcome {
    Success,
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum ShellOperation {
    Open(ActionTarget),
    Reveal(ActionTarget),
    Properties(ActionTarget),
    OpenWith(ActionTarget),
    RunAction {
        target: ActionTarget,
        action: String,
    },
}

struct WorkItem {
    operation: ShellOperation,
    cancelled: Arc<AtomicBool>,
    result: mpsc::Sender<Result<ShellOutcome, ShellError>>,
}

enum WorkerMessage {
    Execute(WorkItem),
    ScanApps(mpsc::Sender<Vec<crate::apps::AppEntry>>),
    Shutdown,
}

pub struct ShellExecutor {
    sender: SyncSender<WorkerMessage>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ShellExecutor {
    pub fn start() -> Result<Arc<Self>, ShellError> {
        let (sender, receiver) = mpsc::sync_channel(SHELL_QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("prism-shell-sta".into())
            .spawn(move || worker_loop(receiver, ready_tx))
            .map_err(|error| ShellError::new(ShellErrorKind::System, error.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Arc::new(Self {
                sender,
                join: Mutex::new(Some(join)),
            })),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(ShellError::new(
                    ShellErrorKind::System,
                    "Shell worker stopped during startup",
                ))
            }
        }
    }

    pub async fn execute(
        self: &Arc<Self>,
        operation: ShellOperation,
    ) -> Result<ShellOutcome, ShellError> {
        let worker = self.clone();
        tokio::task::spawn_blocking(move || {
            worker.execute_blocking(operation, Arc::new(AtomicBool::new(false)))
        })
        .await
        .map_err(|error| ShellError::new(ShellErrorKind::System, error.to_string()))?
    }

    pub async fn scan_apps(self: &Arc<Self>) -> Result<Vec<crate::apps::AppEntry>, ShellError> {
        let worker = self.clone();
        tokio::task::spawn_blocking(move || {
            let (result, receiver) = mpsc::channel();
            worker
                .sender
                .send(WorkerMessage::ScanApps(result))
                .map_err(|_| ShellError::new(ShellErrorKind::System, "Shell worker is closed"))?;
            receiver
                .recv()
                .map_err(|_| ShellError::new(ShellErrorKind::System, "Shell worker did not reply"))
        })
        .await
        .map_err(|error| ShellError::new(ShellErrorKind::System, error.to_string()))?
    }

    fn execute_blocking(
        &self,
        operation: ShellOperation,
        cancelled: Arc<AtomicBool>,
    ) -> Result<ShellOutcome, ShellError> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(ShellOutcome::Cancelled);
        }
        let (result, receiver) = mpsc::channel();
        self.sender
            .send(WorkerMessage::Execute(WorkItem {
                operation,
                cancelled,
                result,
            }))
            .map_err(|_| ShellError::new(ShellErrorKind::System, "Shell worker is closed"))?;
        receiver
            .recv()
            .map_err(|_| ShellError::new(ShellErrorKind::System, "Shell worker did not reply"))?
    }
}

impl Drop for ShellExecutor {
    fn drop(&mut self) {
        let _ = self.sender.send(WorkerMessage::Shutdown);
        if let Ok(mut join) = self.join.lock() {
            if let Some(handle) = join.take() {
                let _ = handle.join();
            }
        }
    }
}

fn worker_loop(receiver: mpsc::Receiver<WorkerMessage>, ready: SyncSender<Result<(), ShellError>>) {
    let apartment = match ComApartment::initialize_sta() {
        Ok(apartment) => apartment,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    while let Ok(message) = receiver.recv() {
        match message {
            WorkerMessage::Execute(item) => {
                let outcome = if item.cancelled.load(Ordering::Acquire) {
                    Ok(ShellOutcome::Cancelled)
                } else {
                    execute_on_sta(item.operation)
                };
                let _ = item.result.send(outcome);
            }
            WorkerMessage::ScanApps(result) => {
                let _ = result.send(crate::apps::scan_start_menu_on_sta());
            }
            WorkerMessage::Shutdown => break,
        }
    }
    drop(apartment);
}

fn execute_on_sta(operation: ShellOperation) -> Result<ShellOutcome, ShellError> {
    match operation {
        ShellOperation::Open(target) => shell_execute(&target, "open"),
        ShellOperation::Reveal(target) => reveal(&target),
        ShellOperation::Properties(target) => shell_execute(&target, "properties"),
        ShellOperation::OpenWith(target) => shell_execute(&target, "openas"),
        ShellOperation::RunAction { target, action } => {
            let kind = target.validate()?;
            if !matches!(
                kind,
                TargetKind::File | TargetKind::Directory | TargetKind::Application
            ) {
                return Err(ShellError::new(
                    ShellErrorKind::Unsupported,
                    "the action does not support this target kind",
                ));
            }
            crate::actions::run_action_direct(&target.value, &action)
                .map(|()| ShellOutcome::Success)
                .map_err(|message| ShellError::new(classify_message(&message), message))
        }
    }
}

impl ActionTarget {
    pub fn new(kind: TargetKind, value: impl Into<String>) -> Self {
        Self {
            kind: kind.as_str().into(),
            value: value.into(),
        }
    }

    pub fn validate(&self) -> Result<TargetKind, ShellError> {
        let kind = TargetKind::parse(&self.kind).ok_or_else(|| {
            ShellError::new(
                ShellErrorKind::Unsupported,
                format!("unsupported target kind: {}", self.kind),
            )
        })?;
        if self.value.is_empty()
            || self.value.contains('\0')
            || self.value.chars().any(char::is_control)
        {
            return Err(ShellError::new(
                ShellErrorKind::TargetInvalid,
                "target is invalid",
            ));
        }
        let maximum = if kind == TargetKind::Web {
            MAX_WEB_BYTES
        } else {
            MAX_PATH_BYTES
        };
        if self.value.len() > maximum {
            return Err(ShellError::new(
                ShellErrorKind::TargetInvalid,
                "target is too long",
            ));
        }
        match kind {
            TargetKind::Web if !crate::websearch::is_http_url(&self.value) => Err(ShellError::new(
                ShellErrorKind::TargetInvalid,
                "web targets must use http or https",
            )),
            TargetKind::File | TargetKind::Directory | TargetKind::Application
                if !std::path::Path::new(&self.value).is_absolute() =>
            {
                Err(ShellError::new(
                    ShellErrorKind::TargetInvalid,
                    "path targets must be absolute",
                ))
            }
            TargetKind::Window if self.value.parse::<u64>().is_err() => Err(ShellError::new(
                ShellErrorKind::TargetInvalid,
                "window targets must contain a numeric handle",
            )),
            _ => Ok(kind),
        }
    }
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Application => "application",
            Self::Window => "window",
            Self::Web => "web",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "directory" => Some(Self::Directory),
            "application" => Some(Self::Application),
            "window" => Some(Self::Window),
            "web" => Some(Self::Web),
            _ => None,
        }
    }
}

#[cfg(windows)]
struct ComApartment;

#[cfg(windows)]
impl ComApartment {
    fn initialize_sta() -> Result<Self, ShellError> {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .map(|| Self)
            .map_err(|error| ShellError::new(ShellErrorKind::System, error.to_string()))
    }
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
}

#[cfg(not(windows))]
struct ComApartment;

#[cfg(not(windows))]
impl ComApartment {
    fn initialize_sta() -> Result<Self, ShellError> {
        Ok(Self)
    }
}

#[cfg(windows)]
fn shell_execute(target: &ActionTarget, verb: &str) -> Result<ShellOutcome, ShellError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let kind = target.validate()?;
    if kind == TargetKind::Window {
        return Err(ShellError::new(
            ShellErrorKind::Unsupported,
            "window activation is not implemented",
        ));
    }
    let value: Vec<u16> = std::ffi::OsStr::new(&target.value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = verb.encode_utf16().chain(std::iter::once(0)).collect();
    let code = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(value.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    }
    .0 as isize;
    if code > 32 {
        Ok(ShellOutcome::Success)
    } else {
        Err(shell_execute_error(code))
    }
}

#[cfg(not(windows))]
fn shell_execute(target: &ActionTarget, _verb: &str) -> Result<ShellOutcome, ShellError> {
    target.validate()?;
    Err(ShellError::new(
        ShellErrorKind::Unsupported,
        "Shell is only available on Windows",
    ))
}

#[cfg(windows)]
fn reveal(target: &ActionTarget) -> Result<ShellOutcome, ShellError> {
    use std::os::windows::process::CommandExt;

    let kind = target.validate()?;
    if !matches!(
        kind,
        TargetKind::File | TargetKind::Directory | TargetKind::Application
    ) {
        return Err(ShellError::new(
            ShellErrorKind::Unsupported,
            "target cannot be revealed",
        ));
    }
    let normalized = target.value.replace('/', "\\");
    let arg = format!("/select,\"{normalized}\"");
    std::process::Command::new("explorer")
        .raw_arg(arg)
        .spawn()
        .map(|_| ShellOutcome::Success)
        .map_err(|error| ShellError::new(classify_io_error(&error), error.to_string()))
}

#[cfg(not(windows))]
fn reveal(target: &ActionTarget) -> Result<ShellOutcome, ShellError> {
    target.validate()?;
    Err(ShellError::new(
        ShellErrorKind::Unsupported,
        "Shell is only available on Windows",
    ))
}

#[cfg(windows)]
fn shell_execute_error(code: isize) -> ShellError {
    let kind = match code {
        5 => ShellErrorKind::AccessDenied,
        2 | 3 => ShellErrorKind::TargetInvalid,
        32 => ShellErrorKind::Conflict,
        _ => ShellErrorKind::System,
    };
    ShellError::new(kind, format!("ShellExecute failed with code {code}"))
}

fn classify_io_error(error: &std::io::Error) -> ShellErrorKind {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => ShellErrorKind::AccessDenied,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput => {
            ShellErrorKind::TargetInvalid
        }
        std::io::ErrorKind::AlreadyExists => ShellErrorKind::Conflict,
        _ => ShellErrorKind::System,
    }
}

fn classify_message(message: &str) -> ShellErrorKind {
    if message.contains("权限") || message.contains("拒绝") {
        ShellErrorKind::AccessDenied
    } else if message.contains("路径") || message.contains("目标") {
        ShellErrorKind::TargetInvalid
    } else if message.contains("未知") || message.contains("未实现") {
        ShellErrorKind::Unsupported
    } else {
        ShellErrorKind::System
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_targets_validate_and_unknown_kinds_are_safe() {
        assert_eq!(
            ActionTarget::new(TargetKind::File, r"C:\x.txt")
                .validate()
                .unwrap(),
            TargetKind::File
        );
        assert_eq!(
            ActionTarget::new(TargetKind::Web, "https://example.com")
                .validate()
                .unwrap(),
            TargetKind::Web
        );
        let unknown = ActionTarget {
            kind: "future".into(),
            value: "x".into(),
        };
        assert_eq!(
            unknown.validate().unwrap_err().kind,
            ShellErrorKind::Unsupported
        );
    }

    #[test]
    fn incompatible_kind_and_payload_are_rejected() {
        let target = ActionTarget::new(TargetKind::File, "https://example.com");
        assert_eq!(
            target.validate().unwrap_err().kind,
            ShellErrorKind::TargetInvalid
        );
        let target = ActionTarget::new(TargetKind::Window, "not-a-handle");
        assert_eq!(
            target.validate().unwrap_err().kind,
            ShellErrorKind::TargetInvalid
        );
    }

    #[test]
    fn worker_initializes_and_shuts_down_cleanly() {
        let worker = ShellExecutor::start().unwrap();
        drop(worker);
    }

    #[tokio::test]
    async fn app_scan_is_dispatched_through_the_sta_worker() {
        let worker = ShellExecutor::start().unwrap();
        worker.scan_apps().await.unwrap();
    }

    #[test]
    fn cancellation_before_queueing_is_not_an_error() {
        let worker = ShellExecutor::start().unwrap();
        let cancelled = Arc::new(AtomicBool::new(true));
        let outcome = worker
            .execute_blocking(
                ShellOperation::Open(ActionTarget::new(TargetKind::File, r"C:\x")),
                cancelled,
            )
            .unwrap();
        assert_eq!(outcome, ShellOutcome::Cancelled);
    }
}
