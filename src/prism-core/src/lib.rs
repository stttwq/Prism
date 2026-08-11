//! Shared Prism backend library used by the user broker and indexer service.

pub mod actions;
pub mod apps;
pub mod config;
pub mod hierarchy;
pub mod history;
pub mod index_cache;
pub mod indexer_client;
pub mod indexer_ipc;
pub mod indexer_runtime;
pub mod ipc;
pub mod logging;
pub mod ntfs;
pub mod persistence;
pub mod pinyin;
pub mod pinyin_sidecar;
pub mod root_scope;
pub mod shell;
pub mod websearch;
pub mod window_list;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 构建指纹：源码树最新 mtime + 构建 profile，由 `build.rs` 注入。
///
/// `VERSION` 取自 Cargo.toml，改代码不会变，所以回答不了「跑的是不是我刚编的那份」。
/// 装完可用 `scripts/prism-build.ps1 -VerifyOnly` 比对运行中进程与源码是否一致。
pub const BUILD_STAMP: &str = env!("PRISM_BUILD_STAMP");
pub const BUILD_PROFILE: &str = env!("PRISM_BUILD_PROFILE");

/// 形如 `0.1.0+release.1754400000`，用于日志与握手回传。
pub fn build_id() -> String {
    format!("{VERSION}+{BUILD_PROFILE}.{BUILD_STAMP}")
}

pub const PIPE_NAME: &str = r"\\.\pipe\prism-core";
pub const INDEXER_PIPE_NAME: &str = r"\\.\pipe\prism-indexer-v1";
pub const INDEXER_PROTOCOL: u32 = 1;

pub fn log(msg: impl AsRef<str>) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let event_id = logging::redacted_id(msg.as_ref());
    eprintln!("[prism {ms}] {event_id}");
    logging::redacted_message(msg.as_ref());
}
