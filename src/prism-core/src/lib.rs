//! Shared Prism backend library used by the user broker and indexer service.

pub mod actions;
pub mod apps;
pub mod config;
pub mod hierarchy;
pub mod index;
pub mod index_cache;
pub mod indexer_client;
pub mod indexer_ipc;
pub mod indexer_runtime;
pub mod ipc;
pub mod ntfs;
pub mod websearch;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PIPE_NAME: &str = r"\\.\pipe\prism-core";
pub const INDEXER_PIPE_NAME: &str = r"\\.\pipe\prism-indexer-v1";
pub const INDEXER_PROTOCOL: u32 = 1;

pub fn log(msg: impl AsRef<str>) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    eprintln!("[prism {ms}] {}", msg.as_ref());
}
