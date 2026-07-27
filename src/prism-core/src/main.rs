//! User-session broker entry point.

use prism_core::{apps, config, ipc, log, PIPE_NAME, VERSION};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    log(format!("prism-core starting, version {VERSION}"));

    let data_dir = config::resolve_data_dir();
    let cfg = config::Config::load(&data_dir);
    let apps: apps::SharedApps = std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));
    let engines = std::sync::Arc::new(std::sync::RwLock::new(cfg.web_engines));

    {
        let apps = apps.clone();
        tokio::spawn(async move { apps::load(apps).await });
    }

    log(format!("broker pipe listening at {PIPE_NAME}"));
    if let Err(error) = ipc::serve(PIPE_NAME, apps, engines).await {
        log(format!("broker pipe failed: {error}"));
        std::process::exit(1);
    }
}
