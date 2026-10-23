//! Расположение файлов приложения (FR-13.1).
//!
//! Переносная версия: если рядом с программой лежит файл `portable.txt`, настройки, кэш,
//! журналы и история хранятся в папке `data` рядом с ней — программа работает с флешки и не
//! оставляет следов в профиле пользователя.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const APP: &str = "converter";
/// Файл-признак переносной версии рядом с программой.
pub const PORTABLE_MARKER: &str = "portable.txt";

/// Папка данных переносной версии для программы из `exe_dir`, если там есть `portable.txt`.
fn portable_root_in(exe_dir: &Path) -> Option<PathBuf> {
    exe_dir
        .join(PORTABLE_MARKER)
        .is_file()
        .then(|| exe_dir.join("data"))
}

/// Папка данных переносной версии; `None` — обычная установка.
pub fn portable_root() -> Option<PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        portable_root_in(exe.parent()?)
    })
    .clone()
}

fn fallback() -> PathBuf {
    std::env::temp_dir().join(APP)
}

/// `%APPDATA%\converter` / `$XDG_CONFIG_HOME/converter`.
pub fn config_dir() -> PathBuf {
    if let Some(root) = portable_root() {
        return root.join("config");
    }
    dirs::config_dir().map_or_else(fallback, |d| d.join(APP))
}

/// `%LOCALAPPDATA%\converter\cache` / `$XDG_CACHE_HOME/converter`.
pub fn cache_dir() -> PathBuf {
    if let Some(root) = portable_root() {
        return root.join("cache");
    }
    if cfg!(windows) {
        dirs::data_local_dir().map_or_else(fallback, |d| d.join(APP).join("cache"))
    } else {
        dirs::cache_dir().map_or_else(fallback, |d| d.join(APP))
    }
}

/// `%LOCALAPPDATA%\converter\logs` / `$XDG_STATE_HOME/converter`.
pub fn state_dir() -> PathBuf {
    if let Some(root) = portable_root() {
        return root.join("state");
    }
    if cfg!(windows) {
        dirs::data_local_dir().map_or_else(fallback, |d| d.join(APP).join("logs"))
    } else {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .map_or_else(fallback, |d| d.join(APP))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_only_with_marker_next_to_program() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(portable_root_in(dir.path()), None);
        std::fs::write(dir.path().join(PORTABLE_MARKER), "").unwrap();
        assert_eq!(portable_root_in(dir.path()), Some(dir.path().join("data")));
    }
}
