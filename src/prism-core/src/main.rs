//! User-session broker entry point.

use prism_core::{apps, config, history, ipc, log, logging, shell, PIPE_NAME, VERSION};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let data_dir = config::resolve_data_dir();
    logging::init("broker", &data_dir);
    // `panic = "abort"` kills the process without unwinding, so without this hook a
    // panic leaves no trace at all: the frontend silently relaunches the broker and
    // the crash is unattributable afterwards.
    logging::install_panic_hook();
    log(format!("prism-core starting, version {VERSION}"));
    let cfg = config::Config::load(&data_dir);
    let apps: apps::SharedApps = std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));
    let engines = std::sync::Arc::new(std::sync::RwLock::new(cfg.web_engines));
    let history = std::sync::Arc::new(history::HistoryStore::load(&data_dir, cfg.history_enabled));
    let preferences = std::sync::Arc::new(ipc::BrokerPreferences::with_zip_program(
        cfg.pinyin_enabled,
        cfg.zip_program,
    ));

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
        // 开始菜单被安装器短暂锁住等瞬时故障会导致应用搜索永久为空；
        // 30 秒退避重试（最多 5 次）兜住首次扫描的窗口期。
        tokio::spawn(async move {
            for attempt in 1..=5 {
                if apps::load(apps.clone(), shell.clone()).await {
                    return;
                }
                crate::log(format!("app catalog scan retry {attempt}/5 in 30s"));
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });
    }

    log(format!("broker pipe listening at {PIPE_NAME}"));
    if let Err(error) = ipc::serve(PIPE_NAME, apps, engines, shell, history, preferences).await {
        log(format!("broker pipe failed: {error}"));
        std::process::exit(1);
    }
}
