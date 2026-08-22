//! Shared Prism backend library used by the user broker and indexer service.

pub mod actions;
pub mod alias;
pub mod apps;
pub mod config;
pub mod file_ops;
pub mod fs_util;
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
pub mod zip;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 构建指纹：源码树最新 mtime + 构建 profile，由 `build.rs` 注入。
///
/// `VERSION` 取自 Cargo.toml，改代码不会变，所以回答不了「跑的是不是我刚编的那份」。
/// M20（全仓复审 2026-08-22）：-VerifyOnly 只比磁盘文件哈希，不比运行中进程
/// 的 stamp——「运行中的进程是否是刚编的那份」目前只能靠 hello 握手回传的
/// `build_id` 人工比对（见 build.rs 头注释的两个限定）。
pub const BUILD_STAMP: &str = env!("PRISM_BUILD_STAMP");
pub const BUILD_PROFILE: &str = env!("PRISM_BUILD_PROFILE");

/// 形如 `0.1.0+release.1754400000`，用于日志与握手回传。
pub fn build_id() -> String {
    format!("{VERSION}+{BUILD_PROFILE}.{BUILD_STAMP}")
}

pub const PIPE_NAME: &str = r"\\.\pipe\prism-core";
pub const INDEXER_PIPE_NAME: &str = r"\\.\pipe\prism-indexer-v1";
pub const INDEXER_PROTOCOL: u32 = 2;

pub fn log(msg: impl AsRef<str>) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let event_id = logging::redacted_id(msg.as_ref());
    let sanitized = logging::sanitize(msg.as_ref());
    eprintln!("[prism {ms}] {sanitized}");
    logging::event_detail("info", &event_id, &sanitized, None, None);
}
