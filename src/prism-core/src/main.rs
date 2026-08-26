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
    // L 批次（FRESH-AUDIT-3-2026-08-20）：250ms 定时冲刷节流脏数据，
    // 把强杀场景的丢失窗口压回 ≤ 2× 节流间隔。
    history.start_periodic_flush();
    let preferences = std::sync::Arc::new(ipc::BrokerPreferences::with_zip_program(
        cfg.pinyin_enabled,
        cfg.zip_program,
    ));
    // 别名系统（2026-08-21 设想）：用户数据，同 history 放数据目录。
    let aliases = std::sync::Arc::new(prism_core::alias::AliasStore::load(&data_dir));
    // K0：命令系统存储（目录 + 使用记录），用户数据，同 history/aliases 放数据目录。
    let commands = std::sync::Arc::new(prism_core::commands::CommandStore::load(&data_dir));

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

    // 全量审查（2026-08-22）：broker 启动时把拼音偏好推给 indexer 服务，并周期
    // 重推。此前只有前端 UpdatePreferences 会下发——broker 崩溃被拉起 / indexer
    // 服务独立重启（SCM 侧标志回默认 true）后，用户关掉的拼音结果会重新混入，
    // 且无人再推。指数退避重试直到送达；成功后每小时重推一次对齐漂移（幂等，
    // 受 indexer 侧 G5 每秒一次限速约束）。每次推送读共享态当前值，不盖用户
    // 刚改的偏好。
    {
        let preferences = preferences.clone();
        tokio::spawn(async move {
            let mut delay = std::time::Duration::from_secs(1);
            loop {
                let enabled = preferences.pinyin_enabled();
                match prism_core::indexer_client::set_pinyin_enabled(enabled).await {
                    Ok(()) => {
                        delay = std::time::Duration::from_secs(3600);
                    }
                    Err(error) => {
                        log(format!(
                            "pinyin preference push to indexer failed, retry in {}s: {error}",
                            delay.as_secs()
                        ));
                        delay = (delay * 2).min(std::time::Duration::from_secs(60));
                    }
                }
                tokio::time::sleep(delay).await;
            }
        });
    }

    log(format!("broker pipe listening at {PIPE_NAME}"));
    if let Err(error) = ipc::serve(
        PIPE_NAME,
        apps,
        engines,
        shell,
        history,
        preferences,
        aliases,
        commands,
    )
    .await
    {
        log(format!("broker pipe failed: {error}"));
        std::process::exit(1);
    }
}
