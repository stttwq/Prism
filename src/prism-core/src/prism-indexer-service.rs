use std::ffi::OsString;
use std::io::Write;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::{define_windows_service, service_dispatcher};

const SERVICE_NAME: &str = "PrismIndexer";

define_windows_service!(ffi_service_main, service_main);

fn main() {
    prism_core::logging::init("indexer", &prism_core::index_cache::machine_data_dir());
    // Under `panic = "abort"` a panicking worker takes the whole service down with no
    // log line; the SCM restart then looks like a spontaneous reboot.
    prism_core::logging::install_panic_hook();
    let result = if std::env::args_os().any(|arg| arg == "--console") {
        run_console()
    } else {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|error| error.to_string())
    };
    if let Err(error) = result {
        log_service_error("indexer service failed", &error);
        std::process::exit(1);
    }
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        log_service_error("SCM service main failed", &error);
    }
}

fn run_service() -> Result<(), String> {
    let stop = prism_core::indexer_runtime::Shutdown::new();
    let handler_stop = stop.clone();
    let status_slot = Arc::new(OnceLock::<ServiceStatusHandle>::new());
    let handler_status = status_slot.clone();
    let status = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop => {
            if let Some(status) = handler_status.get() {
                let _ = status.set_service_status(service_status(
                    ServiceState::StopPending,
                    ServiceControlAccept::empty(),
                    ServiceExitCode::Win32(0),
                    1,
                    Duration::from_secs(30),
                ));
            }
            handler_stop.request();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .map_err(|error| error.to_string())?;
    let _ = status_slot.set(status);
    status
        .set_service_status(service_status(
            ServiceState::Running,
            ServiceControlAccept::STOP,
            ServiceExitCode::Win32(0),
            0,
            Duration::default(),
        ))
        .map_err(|error| error.to_string())?;
    let result = run_runtime(stop);
    let exit_code = if result.is_ok() {
        ServiceExitCode::Win32(0)
    } else {
        ServiceExitCode::ServiceSpecific(1)
    };
    status
        .set_service_status(service_status(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            exit_code,
            0,
            Duration::default(),
        ))
        .map_err(|error| error.to_string())?;
    result
}

fn service_status(
    current_state: ServiceState,
    controls_accepted: ServiceControlAccept,
    exit_code: ServiceExitCode,
    checkpoint: u32,
    wait_hint: Duration,
) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state,
        controls_accepted,
        exit_code,
        checkpoint,
        wait_hint,
        process_id: None,
    }
}

fn run_console() -> Result<(), String> {
    let stop = prism_core::indexer_runtime::Shutdown::new();
    run_runtime(stop)
}

fn run_runtime(stop: Arc<prism_core::indexer_runtime::Shutdown>) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let result = runtime.block_on(prism_core::indexer_runtime::run(stop));
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

fn log_service_error(context: &str, error: &str) {
    let message = format!("{context}: {error}");
    let sanitized = prism_core::logging::sanitize(&message);
    prism_core::log(&message);
    prism_core::logging::event_detail("error", "indexer_service_failure", &sanitized, None, None);
    let data_dir = prism_core::index_cache::machine_data_dir();
    if std::fs::create_dir_all(&data_dir).is_ok() {
        let path = data_dir.join("service.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{sanitized}");
        }
    }
}
