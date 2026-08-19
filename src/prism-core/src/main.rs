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
        // S3（FRESH-AUDIT-2026-08-19）: 开始菜单被安装器短暂锁住等瞬时故障会导致
        // 应用搜索为空。前 5 次 30 秒重试兜住首扫窗口期，之后指数退避到 1 小时，
        // 永不放弃——成功即停。
        tokio::spawn(async move {
            let mut attempt = 1u32;
            loop {
                if apps::load(apps.clone(), shell.clone()).await {
                    return;
                }
                let delay = apps::app_scan_retry_delay(attempt);
                crate::log(format!(
                    "app catalog scan retry {attempt} in {}s",
                    delay.as_secs()
                ));
                attempt += 1;
                tokio::time::sleep(delay).await;
            }
        });
    }

    log(format!("broker pipe listening at {PIPE_NAME}"));
    if let Err(error) = ipc::serve(PIPE_NAME, apps, engines, shell, history, preferences).await {
        log(format!("broker pipe failed: {error}"));
        std::process::exit(1);
    }
}
