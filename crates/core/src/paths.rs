//! Расположение файлов приложения (FR-13.1).

use std::path::PathBuf;

const APP: &str = "converter";

fn fallback() -> PathBuf {
    std::env::temp_dir().join(APP)
}

/// `%APPDATA%\converter` / `$XDG_CONFIG_HOME/converter`.
pub fn config_dir() -> PathBuf {
    dirs::config_dir().map_or_else(fallback, |d| d.join(APP))
}

/// `%LOCALAPPDATA%\converter\cache` / `$XDG_CACHE_HOME/converter`.
pub fn cache_dir() -> PathBuf {
    if cfg!(windows) {
        dirs::data_local_dir().map_or_else(fallback, |d| d.join(APP).join("cache"))
    } else {
        dirs::cache_dir().map_or_else(fallback, |d| d.join(APP))
    }
}

/// `%LOCALAPPDATA%\converter\logs` / `$XDG_STATE_HOME/converter`.
pub fn state_dir() -> PathBuf {
    if cfg!(windows) {
        dirs::data_local_dir().map_or_else(fallback, |d| d.join(APP).join("logs"))
    } else {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .map_or_else(fallback, |d| d.join(APP))
    }
}
