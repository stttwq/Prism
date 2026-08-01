//! User-session broker entry point.

use prism_core::{apps, config, ipc, log, logging, shell, PIPE_NAME, VERSION};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let data_dir = config::resolve_data_dir();
    logging::init("broker", &data_dir);
    log(format!("prism-core starting, version {VERSION}"));
    let cfg = config::Config::load(&data_dir);
    let apps: apps::SharedApps = std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));
    let engines = std::sync::Arc::new(std::sync::RwLock::new(cfg.web_engines));

    let shell = match shell::ShellExecutor::start() {
        Ok(shell) => shell,
        Err(error) => {
            log(format!("Shell worker failed: {}", error.message));
            std::process::exit(1);
        }
    };

    {
        let apps = apps.clone();
        let shell = shell.clone();
        tokio::spawn(async move { apps::load(apps, shell).await });
    }

    log(format!("broker pipe listening at {PIPE_NAME}"));
    if let Err(error) = ipc::serve(PIPE_NAME, apps, engines, shell).await {
        log(format!("broker pipe failed: {error}"));
        std::process::exit(1);
    }
}
