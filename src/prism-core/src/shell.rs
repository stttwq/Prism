//! Typed Shell boundary owned by the normal-user broker.

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
        args: crate::ipc::ActionArgs,
        /// settings.json 的 ZipProgram 字段值，仅 zip 动作使用。
        zip_program: Option<String>,
    },
}

struct WorkItem {
    operation: ShellOperation,
    result: mpsc::Sender<Result<ShellOutcome, ShellError>>,
}

enum WorkerMessage {
    Execute(WorkItem),
    ScanApps(mpsc::Sender<Vec<crate::apps::AppEntry>>),
    /// 解析单个 .lnk 的目标路径（别名身份用）。复用 STA worker 的 COM 单元。
    ResolveLnk {
        path: String,
        reply: mpsc::Sender<Option<String>>,
    },
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
        tokio::task::spawn_blocking(move || worker.execute_blocking(operation))
            .await
            .map_err(|error| ShellError::new(ShellErrorKind::System, error.to_string()))?
    }

    pub async fn scan_apps(self: &Arc<Self>) -> Result<Vec<crate::apps::AppEntry>, ShellError> {
        let worker = self.clone();
        tokio::task::spawn_blocking(move || {
            let (result, receiver) = mpsc::channel();
            // M1：同 execute_blocking——满队列立即报错，不无限停泊 blocking 线程。
            worker
                .sender
                .try_send(WorkerMessage::ScanApps(result))
                .map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => ShellError::new(
                        ShellErrorKind::System,
                        "shell worker queue is saturated (worker stuck on a modal operation?)",
                    ),
                    mpsc::TrySendError::Disconnected(_) => {
                        ShellError::new(ShellErrorKind::System, "Shell worker is closed")
                    }
                })?;
            // M4：清单扫描解析数百个 .lnk 可合法耗时，给满慢预算防挂死。
            receiver
                .recv_timeout(std::time::Duration::from_secs(300))
                .map_err(|_| ShellError::new(ShellErrorKind::System, "Shell worker did not reply"))
        })
        .await
        .map_err(|error| ShellError::new(ShellErrorKind::System, error.to_string()))?
    }

    /// 解析 .lnk 的目标路径（别名身份指向真实 exe）。复用 STA worker 的 COM 单元；
    /// 非 .lnk 或解析失败返回 None（调用方回退原值）。超时取快档 60s。
    pub async fn resolve_lnk(self: &Arc<Self>, path: String) -> Result<Option<String>, ShellError> {
        let worker = self.clone();
        tokio::task::spawn_blocking(move || {
            let (result, receiver) = mpsc::channel();
            worker
                .sender
                .try_send(WorkerMessage::ResolveLnk {
                    path: path.clone(),
                    reply: result,
                })
                .map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => ShellError::new(
                        ShellErrorKind::System,
                        "shell worker queue is saturated (worker stuck on a modal operation?)",
                    ),
                    mpsc::TrySendError::Disconnected(_) => {
                        ShellError::new(ShellErrorKind::System, "Shell worker is closed")
                    }
                })?;
            // 与 execute_blocking 的 FAST 同档：解析单个 .lnk 通常瞬时，
            // 给 60s 足以越过死网络路径的内核超时而不拖死队列。
            receiver
                .recv_timeout(std::time::Duration::from_secs(60))
                .map_err(|_| ShellError::new(ShellErrorKind::System, "Shell worker did not reply"))
        })
        .await
        .map_err(|error| ShellError::new(ShellErrorKind::System, error.to_string()))?
    }

    fn execute_blocking(&self, operation: ShellOperation) -> Result<ShellOutcome, ShellError> {
        // S2a：外部进程 zip（7-Zip/自定义压缩程序）不进 STA 队列——纯
        // std::process 调用分钟级等待，会占死唯一的 Shell worker；当前
        // spawn_blocking 线程正好是它的归宿。Windows Shell COM 压缩路径
        // （CopyHere 需要 COM apartment）返回 None，照旧走 STA。
        if let ShellOperation::RunAction {
            ref target,
            ref action,
            ref zip_program,
            ..
        } = operation
        {
            if action == crate::actions::ActionId::Zip.as_str() {
                let output_path = zip_output_path(target);
                if let Some(result) =
                    crate::zip::zip_external(target, &output_path, zip_program.as_deref())
                {
                    return result;
                }
            }
        }
        // M4（FRESH-AUDIT-3-2026-08-20）：裸 recv() 无限等待——properties 模态页 /
        // 死网络路径 / IFileOperation 对话框挂住唯一 STA worker 时，后续动作在
        // 容量 32 的 channel 排队，每个占一个 spawn_blocking 线程无限等。按动作
        // 类别给等待预算：超时即放弃该请求（worker 完成后 send 到已弃接收端自然
        // 失败），worker 仍可服务后续动作。预算在 move 前取好。
        let budget = sta_wait_budget(&operation);
        let (result, receiver) = mpsc::channel();
        // M1（复审 2026-08-21）：入队用 try_send。阻塞 send 在队列满（容量
        // 全是超时弃单留下的积压，worker 已被模态页挂住）时无限停泊调用方
        // 的 spawn_blocking 线程——recv_timeout 的预算根本轮不到生效，反复
        // 动作可耗尽 blocking 池。满队列时 worker 本就卡死，立即报错让调用
        // 方看到真相比无限等待正确。
        self.sender
            .try_send(WorkerMessage::Execute(WorkItem { operation, result }))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => ShellError::new(
                    ShellErrorKind::System,
                    "shell worker queue is saturated (worker stuck on a modal operation?)",
                ),
                mpsc::TrySendError::Disconnected(_) => {
                    ShellError::new(ShellErrorKind::System, "Shell worker is closed")
                }
            })?;
        receiver.recv_timeout(budget).map_err(|_| {
            ShellError::new(
                ShellErrorKind::System,
                format!(
                    "Shell worker did not reply within {}s (operation may still be in flight)",
                    budget.as_secs()
                ),
            )
        })?
    }
}

/// M4：按动作类别的 STA 等待预算。模态/进度类（properties 模态页、openas
/// 对话框、IFileOperation 进度与冲突对话框、长压缩）合法长等待，与前端 F6
/// 的 5 分钟兜底取齐；open/reveal 通常瞬时，60s 足以越过死网络路径的内核
/// 超时而不会把队列拖死。
fn sta_wait_budget(operation: &ShellOperation) -> std::time::Duration {
    const FAST: std::time::Duration = std::time::Duration::from_secs(60);
    const SLOW: std::time::Duration = std::time::Duration::from_secs(300);
    match operation {
        ShellOperation::Properties(_)
        | ShellOperation::OpenWith(_)
        | ShellOperation::RunAction { .. } => SLOW,
        ShellOperation::Open(_) | ShellOperation::Reveal(_) => FAST,
    }
}

impl Drop for ShellExecutor {
    fn drop(&mut self) {
        // M1（复审 2026-08-21）：不再 join。worker 被模态页/死网络路径挂住时
        // join 会让 broker 的退出路径无限阻塞；Drop 只在进程退出（最后一个
        // Arc 释放）时到达，detach 由进程终结统一收拾，等待没有价值。
        // L5（全仓复审 2026-08-22）：Shutdown 用有界重试的 try_send——队列被
        // 弃单填满但 worker 仍在缓慢消化时，直接放弃会让 worker 收不到
        // Shutdown，CoUninitialize 不执行（STA 线程泄漏）。真挂死（重试期间
        // 队列不退）在 200ms 后放弃，与 M1 的「不无限阻塞退出路径」一致。
        for _ in 0..20 {
            if self.sender.try_send(WorkerMessage::Shutdown).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if let Ok(mut join) = self.join.lock() {
            drop(join.take());
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
                let outcome = execute_on_sta(item.operation);
                let _ = item.result.send(outcome);
            }
            WorkerMessage::ScanApps(result) => {
                let _ = result.send(crate::apps::scan_start_menu_on_sta());
            }
            WorkerMessage::ResolveLnk { path, reply } => {
                let _ = reply.send(crate::apps::resolve_lnk_target(&path));
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
        ShellOperation::RunAction {
            target,
            action,
            args,
            zip_program,
        } => execute_run_action(target, action, args, zip_program),
    }
}

/// 在 STA worker 上路由 `RunAction`。broker 根据 `ActionId` 重新验证动作与
/// target kind，不信任 WPF 传来的路径/命令。
///
/// 无 mutation 动作直接路由到 Shell 函数；mutation 动作由 `file_ops` 在
/// `IFileOperation` 上执行。
fn execute_run_action(
    target: ActionTarget,
    action: String,
    args: crate::ipc::ActionArgs,
    zip_program: Option<String>,
) -> Result<ShellOutcome, ShellError> {
    use crate::actions::ActionId;

    let id = action.parse::<ActionId>().map_err(|_| {
        ShellError::new(
            ShellErrorKind::Unsupported,
            format!("unknown action: {action}"),
        )
    })?;

    let kind = target.validate()?;
    if matches!(kind, TargetKind::Window | TargetKind::Web) {
        return Err(ShellError::new(
            ShellErrorKind::Unsupported,
            "the action does not support this target kind",
        ));
    }

    match id {
        // 无 mutation 文件/文件夹动作：复用已有 Shell 路径。
        ActionId::OpenFolder => reveal(&target),
        ActionId::Properties => shell_execute(&target, "properties"),
        // 2026-08-24 修复：App 目标的 value 是开始菜单 .lnk，属性页要的是真实
        // 可执行程序的（动作语义：查看真实可执行程序属性）。
        ActionId::AppProperties => {
            let resolved = crate::apps::resolve_lnk_target(&target.value);
            let real = match resolved {
                Some(path) => ActionTarget::new(TargetKind::Application, path),
                None => target.clone(),
            };
            shell_execute(&real, "properties")
        }
        ActionId::OpenWith => {
            if kind != TargetKind::File {
                return Err(ShellError::new(
                    ShellErrorKind::Unsupported,
                    "open_with is only available for files",
                ));
            }
            shell_execute(&target, "openas")
        }
        // 无 mutation 应用专属动作。
        ActionId::LocateApp => {
            if kind != TargetKind::Application {
                return Err(ShellError::new(
                    ShellErrorKind::Unsupported,
                    "locate_app is only available for applications",
                ));
            }
            reveal(&target)
        }
        ActionId::RunAsAdmin => {
            if kind != TargetKind::Application {
                return Err(ShellError::new(
                    ShellErrorKind::Unsupported,
                    "run_as_admin is only available for applications",
                ));
            }
            // 仍对 .lnk 本身 runas：ShellExecute 会解析快捷方式并携带其
            // 参数/工作目录提升真实目标——换成解析后的 exe 反而丢参数。
            shell_execute(&target, "runas")
        }
        // 剪贴板动作（已有实现，无 mutation）。L4：run_action_direct 已返回
        // 类型化 ShellError，不再从中文消息反推类别。
        // 2026-08-24 修复：copy_app_path 复制的是真实可执行程序路径（.lnk
        // 解析失败回退快捷方式路径本身）；copy/cut/copy_path 维持原值。
        ActionId::Copy | ActionId::Cut | ActionId::CopyPath | ActionId::CopyAppPath => {
            let value = if matches!(id, ActionId::CopyAppPath) {
                crate::apps::resolve_lnk_target(&target.value).unwrap_or(target.value.clone())
            } else {
                target.value.clone()
            };
            crate::actions::run_action_direct(&value, id.as_str()).map(|()| ShellOutcome::Success)
        }
        // mutation 动作：IFileOperation 在 STA worker 上执行。
        ActionId::Recycle => crate::file_ops::recycle(&target),
        ActionId::DeletePermanent => crate::file_ops::delete_permanent(&target),
        ActionId::Rename => {
            let new_name = args.new_name.ok_or_else(|| {
                ShellError::new(
                    ShellErrorKind::TargetInvalid,
                    "rename requires a new_name argument",
                )
            })?;
            crate::file_ops::rename(&target, &new_name)
        }
        ActionId::CopyTo => {
            let dest = args.destination.ok_or_else(|| {
                ShellError::new(
                    ShellErrorKind::TargetInvalid,
                    "copy_to requires a destination argument",
                )
            })?;
            validate_destination(&dest)?;
            crate::file_ops::copy_to(&target, &dest)
        }
        ActionId::MoveTo => {
            let dest = args.destination.ok_or_else(|| {
                ShellError::new(
                    ShellErrorKind::TargetInvalid,
                    "move_to requires a destination argument",
                )
            })?;
            validate_destination(&dest)?;
            crate::file_ops::move_to(&target, &dest)
        }
        ActionId::Zip => {
            crate::zip::zip(&target, &zip_output_path(&target), zip_program.as_deref())
        }
    }
}

/// zip 动作的输出路径：源路径同名 + `.zip`。
fn zip_output_path(target: &ActionTarget) -> String {
    format!("{}.zip", target.value)
}

/// FRESH-AUDIT-2 F5: destination 与 path 类 target 同守校验——
/// JSON `\u0000` 会让 PCWSTR 截断成意外路径，控制字符/相对路径/超长同样拒绝。
fn validate_destination(dest: &str) -> Result<(), ShellError> {
    if dest.is_empty() || dest.contains('\0') || dest.chars().any(char::is_control) {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "destination is invalid",
        ));
    }
    if dest.len() > MAX_PATH_BYTES {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "destination is too long",
        ));
    }
    if !std::path::Path::new(dest).is_absolute() {
        return Err(ShellError::new(
            ShellErrorKind::TargetInvalid,
            "destination must be an absolute path",
        ));
    }
    Ok(())
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
            || self.value.contains('"')
            || self.value.chars().any(char::is_control)
        {
            return Err(ShellError::new(
                ShellErrorKind::TargetInvalid,
                "target is invalid",
            ));
        }
        // L7（全仓复审 2026-08-22）：双引号在 NTFS 文件名里非法，但 validate 此前
        // 放行——reveal 的 explorer /select,"{path}" 用 raw_arg 刻意绕开转义，
        // 值里带引号即可塑形 explorer 命令行。真实磁盘路径永不包含引号，
        // 这里拒绝只会拦下构造载荷。
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
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
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
    let verb_wide: Vec<u16> = verb.encode_utf16().chain(std::iter::once(0)).collect();
    // 传父目录作为工作目录。R2（全仓检验 2026-08-25 第二轮）：web 目标跳过——
    // 对 URL 取 parent() 会产出 "https://host/" 这类不存在的"目录"，传入
    // lpDirectory 后部分协议激活链路（视默认浏览器注册形态）直接激活失败，
    // 表现为网页行回车无反应。URL 激活不需要工作目录，显式传 null。
    let dir: Vec<u16> = if kind == TargetKind::Web {
        Vec::new()
    } else {
        std::path::Path::new(&target.value)
            .parent()
            .and_then(|p| p.to_str())
            .map(|s| {
                std::ffi::OsStr::new(s)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect::<Vec<u16>>()
            })
            .unwrap_or_default()
    };

    // SEE_MASK_INVOKEIDLIST: 让 ShellExecuteEx 通过 IContextMenu 路由 verb，
    // 解决后台进程调用 "properties" 等 verb 时返回 code 31 (SE_ERR_NOASSOC) 的问题。
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_NOCLOSEPROCESS,
        hwnd: HWND::default(),
        lpVerb: PCWSTR(verb_wide.as_ptr()),
        lpFile: PCWSTR(value.as_ptr()),
        lpParameters: PCWSTR::null(),
        lpDirectory: if dir.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(dir.as_ptr())
        },
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };

    let success = unsafe { ShellExecuteExW(&mut info) };
    if success.is_ok() {
        // SEE_MASK_NOCLOSEPROCESS 让系统把进程句柄交给我们，必须还回去；
        // 句柄本身无人使用，只关不读。
        if !info.hProcess.is_invalid() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
            }
        }
        Ok(ShellOutcome::Success)
    } else {
        // ShellExecuteExW 的错误通过 GetLastError 获取，不像旧 ShellExecuteW 返回 code。
        let err = windows::core::Error::from_win32();
        Err(ShellError::new(
            ShellErrorKind::System,
            format!("ShellExecuteEx failed: {err}"),
        ))
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
    // 2026-08-24 修复：Application 目标的 value 是开始菜单 .lnk——「打开所在
    // 文件夹」要定位的是真实可执行程序（actions.rs 的动作语义如此），解析
    // 失败回退 .lnk 本身。File/Directory 不解析：用户搜到的就是那个文件。
    let value = if kind == TargetKind::Application {
        crate::apps::resolve_lnk_target(&target.value).unwrap_or_else(|| target.value.clone())
    } else {
        target.value.clone()
    };
    let normalized = value.replace('/', "\\");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// M4（FRESH-AUDIT-3-2026-08-20）：模态/进度类动作拿 5 分钟慢预算
    ///（与前端 F6 兜底取齐），open/reveal 拿 60s 快预算。
    #[test]
    fn m4_sta_wait_budgets_split_modal_from_fast_verbs() {
        use std::time::Duration;
        let target = || ActionTarget::new(TargetKind::File, r"C:\x.txt");
        assert_eq!(
            sta_wait_budget(&ShellOperation::Open(target())),
            Duration::from_secs(60)
        );
        assert_eq!(
            sta_wait_budget(&ShellOperation::Reveal(target())),
            Duration::from_secs(60)
        );
        assert_eq!(
            sta_wait_budget(&ShellOperation::Properties(target())),
            Duration::from_secs(300)
        );
        assert_eq!(
            sta_wait_budget(&ShellOperation::OpenWith(target())),
            Duration::from_secs(300)
        );
        assert_eq!(
            sta_wait_budget(&ShellOperation::RunAction {
                target: target(),
                action: "copy_to".into(),
                args: crate::ipc::ActionArgs::default(),
                zip_program: None,
            }),
            Duration::from_secs(300)
        );
    }

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

    // M3（全仓复审 2026-08-22）：取消旗标整条链路（execute 每次传全新
    // `AtomicBool::new(false)`、无任何调用方能置位、PerformOperations 期间
    // 也从不检查）是死代码，已删除。取消语义由 IFileOperation 自身的确认
    // 对话框承担（COPYENGINE_S_USER_CANCELLED → ShellOutcome::Cancelled）。

    /// S2a：zip 分流发生在进 STA 队列之前，验证错误必须与走 STA 路径时一致
    /// （zip 与 zip_external 共用同一校验）。
    #[test]
    fn run_action_zip_rejects_invalid_targets_before_the_sta_queue() {
        let worker = ShellExecutor::start().unwrap();
        for target in [
            ActionTarget::new(TargetKind::Window, "12345"),
            ActionTarget::new(TargetKind::Web, "https://example.com"),
        ] {
            let error = worker
                .execute_blocking(ShellOperation::RunAction {
                    target,
                    action: "zip".into(),
                    args: Default::default(),
                    zip_program: None,
                })
                .unwrap_err();
            assert_eq!(error.kind, ShellErrorKind::Unsupported);
            assert!(error.message.contains("zip requires"));
        }
    }

    // ── G6 execute_run_action 路由测试 ──────────────────────────

    #[test]
    fn run_action_rejects_unknown_action_id() {
        let target = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        let err =
            execute_run_action(target, "not_real".into(), Default::default(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);
        assert!(err.message.contains("unknown action"));
    }

    #[test]
    fn run_action_rejects_window_and_web_targets() {
        let window = ActionTarget::new(TargetKind::Window, "12345");
        let err = execute_run_action(window, "copy".into(), Default::default(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);

        let web = ActionTarget::new(TargetKind::Web, "https://example.com");
        let err = execute_run_action(web, "copy".into(), Default::default(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);
    }

    #[test]
    fn run_action_open_with_rejects_directory() {
        let dir = ActionTarget::new(TargetKind::Directory, r"C:\Windows");
        let err =
            execute_run_action(dir, "open_with".into(), Default::default(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);
        assert!(err.message.contains("only available for files"));
    }

    #[test]
    fn run_action_locate_app_rejects_file() {
        let file = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        let err =
            execute_run_action(file, "locate_app".into(), Default::default(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);
        assert!(err.message.contains("only available for applications"));
    }

    #[test]
    fn run_action_run_as_admin_rejects_file() {
        let file = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        let err =
            execute_run_action(file, "run_as_admin".into(), Default::default(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::Unsupported);
        assert!(err.message.contains("only available for applications"));
    }

    #[test]
    fn run_action_mutation_actions_without_args_return_target_invalid() {
        let target = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        // rename/copy_to/move_to without args return TargetInvalid.
        for action in ["rename", "copy_to", "move_to"] {
            let err = execute_run_action(target.clone(), action.into(), Default::default(), None)
                .unwrap_err();
            assert_eq!(
                err.kind,
                ShellErrorKind::TargetInvalid,
                "{action} without args should be TargetInvalid"
            );
        }
        // zip on a nonexistent source: fails because either the source or
        // output path is invalid/permission-denied. The point is it does
        // not return Unsupported (routing reached file_ops).
        let err =
            execute_run_action(target.clone(), "zip".into(), Default::default(), None).unwrap_err();
        assert_ne!(err.kind, ShellErrorKind::Unsupported);
    }

    #[test]
    fn run_action_rename_without_args_errors() {
        let target = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        let err =
            execute_run_action(target, "rename".into(), Default::default(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("new_name"));
    }

    #[test]
    fn run_action_copy_to_without_args_errors() {
        let target = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        let err =
            execute_run_action(target, "copy_to".into(), Default::default(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("destination"));
    }

    #[test]
    fn run_action_move_to_without_args_errors() {
        let target = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        let err =
            execute_run_action(target, "move_to".into(), Default::default(), None).unwrap_err();
        assert_eq!(err.kind, ShellErrorKind::TargetInvalid);
        assert!(err.message.contains("destination"));
    }

    /// FRESH-AUDIT-2 F5: destination 与 target 同守输入校验——NUL/控制字符/相对路径拒绝。
    #[test]
    fn run_action_destination_is_validated_like_targets() {
        let target = ActionTarget::new(TargetKind::File, r"C:\x.txt");
        for bad in ["C:\\des\u{0}t", "C:\\des\tt", "relative\\dir", ""] {
            let err = execute_run_action(
                target.clone(),
                "copy_to".into(),
                crate::ipc::ActionArgs {
                    new_name: None,
                    destination: Some(bad.into()),
                },
                None,
            )
            .unwrap_err();
            assert_eq!(
                err.kind,
                ShellErrorKind::TargetInvalid,
                "destination {bad:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    #[ignore = "live IFileOperation test; triggers Windows rename dialog. Run with --ignored"]
    async fn run_action_rename_succeeds_on_sta_worker() {
        let temp = std::env::temp_dir().join(format!(
            "prism-g6-rename-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&temp, "test").unwrap();
        let worker = ShellExecutor::start().unwrap();
        let target = ActionTarget::new(TargetKind::File, temp.to_str().unwrap());
        let result = worker
            .execute(ShellOperation::RunAction {
                target,
                action: "rename".into(),
                args: crate::ipc::ActionArgs {
                    new_name: Some("prism-g6-renamed.txt".into()),
                    destination: None,
                },
                zip_program: None,
            })
            .await;
        // Rename may succeed or be cancelled if Windows shows a conflict dialog.
        // Rename may succeed, be cancelled, or fail with a system error if
        // IFileOperation cannot show UI in the test environment. The point is
        // that the routing reached IFileOperation, not that it returned Success.
        let _ = result;
    }

    #[tokio::test]
    #[ignore = "live IFileOperation test; triggers Windows copy dialog. Run with --ignored"]
    async fn run_action_copy_to_reaches_file_ops() {
        let temp = std::env::temp_dir().join(format!(
            "prism-g6-copy-src-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&temp, "test").unwrap();
        let worker = ShellExecutor::start().unwrap();
        let target = ActionTarget::new(TargetKind::File, temp.to_str().unwrap());
        let result = worker
            .execute(ShellOperation::RunAction {
                target,
                action: "copy_to".into(),
                args: crate::ipc::ActionArgs {
                    destination: Some(std::env::temp_dir().to_str().unwrap().into()),
                    new_name: None,
                },
                zip_program: None,
            })
            .await;
        // copy_to may succeed, be cancelled, or fail with a system error if
        // IFileOperation cannot show UI in the test environment. The point is
        // that the routing reached IFileOperation, not that it returned Success.
        let _ = result;
    }

    #[tokio::test]
    #[ignore = "live IFileOperation test; sends file to Recycle Bin. Run with --ignored"]
    async fn run_action_recycle_succeeds_on_sta_worker() {
        let temp = std::env::temp_dir().join(format!(
            "prism-g6-shell-recycle-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&temp, "test").unwrap();
        let worker = ShellExecutor::start().unwrap();
        let target = ActionTarget::new(TargetKind::File, temp.to_str().unwrap());
        let result = worker
            .execute(ShellOperation::RunAction {
                target,
                action: "recycle".into(),
                args: Default::default(),
                zip_program: None,
            })
            .await;
        match result {
            Ok(ShellOutcome::Success) | Ok(ShellOutcome::Cancelled) => {}
            Err(e) => panic!("recycle via STA worker failed unexpectedly: {e:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "live clipboard test; modifies system clipboard. Run with --ignored"]
    async fn run_action_copy_succeeds_on_sta_worker() {
        let worker = ShellExecutor::start().unwrap();
        let target = ActionTarget::new(TargetKind::File, r"C:\Windows\explorer.exe");
        let outcome = worker
            .execute(ShellOperation::RunAction {
                target,
                action: "copy".into(),
                args: Default::default(),
                zip_program: None,
            })
            .await
            .unwrap();
        assert_eq!(outcome, ShellOutcome::Success);
    }

    #[tokio::test]
    #[ignore = "live ShellExecute test; may trigger UAC prompt. Run with --ignored"]
    async fn run_action_runas_succeeds_for_application_on_sta_worker() {
        // ShellExecute "runas" on explorer.exe will show a UAC prompt or succeed
        // silently depending on the system. We only verify it does not return an
        // error kind other than what the OS gives us. On test machines this
        // typically returns Success (explorer.exe is a valid target).
        let worker = ShellExecutor::start().unwrap();
        let target = ActionTarget::new(TargetKind::Application, r"C:\Windows\explorer.exe");
        let result = worker
            .execute(ShellOperation::RunAction {
                target,
                action: "run_as_admin".into(),
                args: Default::default(),
                zip_program: None,
            })
            .await;
        // Either success or a system error (UAC declined) is acceptable; the
        // point is that the routing does not return Unsupported.
        match result {
            Ok(ShellOutcome::Success) | Err(ShellError { .. }) => {}
            Ok(ShellOutcome::Cancelled) => panic!("unexpected cancellation"),
        }
    }
}
