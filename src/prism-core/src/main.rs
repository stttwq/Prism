//! Prism 后端入口（prism-core.exe）

mod apps;
mod config;
mod index;
mod ipc;
mod websearch;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PIPE_NAME: &str = r"\\.\pipe\prism-core";

// multi_thread：索引全量扫描走 spawn_blocking，不能堵在 current_thread 上，
// 否则命名管道收发会一起卡死，前端表现为"搜不到"。
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    log("prism-core 启动，版本 ".to_owned() + VERSION);

    let data_dir = config::resolve_data_dir();
    let cfg = config::Config::load(&data_dir);
    log(format!("数据目录：{}", data_dir.display()));
    log(format!(
        "网页引擎 {} 个：{}",
        cfg.web_engines.len(),
        cfg.web_engines
            .iter()
            .map(|e| e.keyword.as_str())
            .collect::<Vec<_>>()
            .join(",")
    ));

    let shared: index::SharedIndex = std::sync::Arc::new(std::sync::RwLock::new(None));
    let apps: apps::SharedApps = std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));
    // 引擎可热重载：设置页保存后发 reload_engines，无需重启后端。
    let engines = std::sync::Arc::new(std::sync::RwLock::new(cfg.web_engines));

    // 异步构建/加载索引，不阻塞管道服务启动。
    {
        let shared2 = shared.clone();
        let data_dir2 = data_dir.clone();
        let refresh = cfg.index_refresh_secs;
        tokio::spawn(async move {
            index::build_or_load(data_dir2, shared2, refresh).await;
        });
    }

    // 程序清单（开始菜单）与索引并行扫描，通常秒级完成。
    {
        let apps2 = apps.clone();
        tokio::spawn(async move {
            apps::load(apps2).await;
        });
    }

    log(format!("命名管道服务监听：{PIPE_NAME}"));
    if let Err(e) = ipc::serve(PIPE_NAME, shared, apps, engines).await {
        log(format!("管道服务异常退出：{e}"));
        std::process::exit(1);
    }
}

pub fn log(msg: impl AsRef<str>) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    eprintln!("[prism-core {ms}] {}", msg.as_ref());
}
