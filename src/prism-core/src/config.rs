//! 后端配置读写 + 数据目录探测。
//!
//! 与前端 `SettingsStore.cs` 共用同一份 `settings.json`：
//! - 数据目录策略：优先「安装目录\data」，不可写退回 `%LocalAppData%\Prism`
//!   （design.md 数据目录策略 / prd.md R7）。
//! - settings.json 损坏时恢复默认，不 panic（design.md 回滚策略）。
//! - 中文路径全程 Unicode（PathBuf/OsString），JSON 用 UTF-8。
//!
//! 后端本步只读取自己关心的字段（索引刷新间隔等），未知字段忽略，
//! 以免与前端写入的字段（快捷键 / 网页引擎）冲突。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 设置文件名，与前端 `SettingsStore.SettingsFileName` 一致。
const SETTINGS_FILE_NAME: &str = "settings.json";

/// 后端配置。字段是前端 `Settings` 的子集 + 后端私有项；
/// `#[serde(default)]` 保证前端写的额外字段不影响反序列化，缺字段回落默认。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 无 USN 权限时的定时全量刷新间隔（秒），默认 300（5 分钟）。
    pub index_refresh_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            index_refresh_secs: 300,
        }
    }
}

impl Config {
    /// 从数据目录加载配置；文件缺失或损坏时返回默认值（绝不 panic）。
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SETTINGS_FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}

/// 探测数据目录，策略与前端 `SettingsStore.ResolveDataDir` 保持一致：
/// 1. 「可执行文件所在目录\data」可写则用之（便携/自定义安装）；
/// 2. 否则退回 `%LocalAppData%\Prism`（装在 Program Files 等只读位置时）。
///
/// 返回目录已创建完成。全程用 `PathBuf`，中文路径无损。
pub fn resolve_data_dir() -> PathBuf {
    if let Some(install_data) = install_data_dir() {
        if is_writable(&install_data) {
            return install_data;
        }
    }

    let local = local_appdata_prism();
    // 尽力创建；即使失败也返回该路径，调用方后续 I/O 会自然报错并降级。
    let _ = std::fs::create_dir_all(&local);
    local
}

/// 「可执行文件所在目录\data」。取不到可执行路径时返回 None。
fn install_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join("data"))
}

/// `%LocalAppData%\Prism`；环境变量缺失时退回临时目录下的 Prism。
fn local_appdata_prism() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("Prism");
    }
    std::env::temp_dir().join("Prism")
}

/// 探测目录是否可写：创建目录 + 写删探针文件。任何一步失败即视为不可写。
fn is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write_probe");
    if std::fs::write(&probe, b"").is_err() {
        return false;
    }
    let _ = std::fs::remove_file(&probe);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_refresh_is_five_minutes() {
        assert_eq!(Config::default().index_refresh_secs, 300);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = std::env::temp_dir().join("prism_cfg_missing_test");
        let _ = std::fs::create_dir_all(&dir);
        let cfg = Config::load(&dir);
        assert_eq!(cfg.index_refresh_secs, 300);
    }

    #[test]
    fn load_ignores_frontend_only_fields() {
        // 前端写的额外字段（hotkeyMode 等）不应导致后端解析失败。
        let dir = std::env::temp_dir().join("prism_cfg_extra_test");
        let _ = std::fs::create_dir_all(&dir);
        let json = r#"{"hotkeyMode":"DoubleCtrl","comboHotkey":"Alt+Space","autoStart":false,"webEngines":[]}"#;
        std::fs::write(dir.join(SETTINGS_FILE_NAME), json).unwrap();
        let cfg = Config::load(&dir);
        assert_eq!(cfg.index_refresh_secs, 300);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_corrupt_file_recovers_default() {
        let dir = std::env::temp_dir().join("prism_cfg_corrupt_test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join(SETTINGS_FILE_NAME), b"{ not valid json ][").unwrap();
        let cfg = Config::load(&dir);
        assert_eq!(cfg.index_refresh_secs, 300);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
