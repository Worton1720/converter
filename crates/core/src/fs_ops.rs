//! Безопасные файловые операции: атомарная запись через временный файл с журналом
//! (FR-4.4), перенос даты (FR-4.5), удаление исходников (FR-5.1–5.4, FR-11.1).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, IoContext, Result};
use crate::events::SourceAction;
use crate::options::SourcePolicy;
use crate::paths;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

fn journal_dir() -> PathBuf {
    paths::state_dir().join("pending")
}

/// Временный файл рядом с результатом: `.<имя>.cvtmp-<pid>-<n>.<ext>`.
/// Путь пишется в журнал, чтобы после падения убрать только свои файлы.
/// Если файл не зафиксирован через [`TempFile::commit`], он удаляется при `Drop`.
pub struct TempFile {
    path: PathBuf,
    journal: PathBuf,
    committed: bool,
}

impl TempFile {
    pub fn new(final_path: &Path) -> Result<TempFile> {
        let dir = final_path
            .parent()
            .ok_or_else(|| Error::Invalid(format!("нет папки у {}", final_path.display())))?;
        std::fs::create_dir_all(dir).at(dir)?;
        let stem = final_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = final_path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = format!(
            "{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let path = dir.join(format!(".{stem}.cvtmp-{id}.{ext}"));

        let jdir = journal_dir();
        std::fs::create_dir_all(&jdir).at(&jdir)?;
        let journal = jdir.join(format!("{id}.path"));
        std::fs::write(&journal, path.to_string_lossy().as_bytes()).at(&journal)?;
        Ok(TempFile {
            path,
            journal,
            committed: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Переименовывает во `final_path`. Если там уже появился файл, а перезапись
    /// не разрешена, берётся следующее свободное имя `_N`. Возвращает итоговый путь.
    pub fn commit(mut self, final_path: &Path, overwrite: bool) -> Result<PathBuf> {
        let mut target = final_path.to_path_buf();
        if !overwrite {
            let mut n = 1;
            while target.exists() {
                target = with_suffix(final_path, n);
                n += 1;
            }
        }
        std::fs::rename(&self.path, &target).at(&target)?;
        self.committed = true;
        let _ = std::fs::remove_file(&self.journal);
        Ok(target)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(&self.journal);
        }
    }
}

/// `photo.jpg` → `photo_3.jpg`.
pub fn with_suffix(path: &Path, n: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(ext) => format!("{stem}_{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}_{n}"),
    };
    path.with_file_name(name)
}

/// Удаляет временные файлы, оставшиеся после падения. Файлы живых процессов не трогаются.
pub fn cleanup_stale_temps() -> usize {
    let Ok(entries) = std::fs::read_dir(journal_dir()) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let pid: Option<u32> = name.split('-').next().and_then(|p| p.parse().ok());
        if pid.is_some_and(process_alive) {
            continue;
        }
        if let Ok(target) = std::fs::read_to_string(entry.path()) {
            let target = PathBuf::from(target);
            // Защита: удаляем только файлы с нашей меткой.
            let ours = target
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains(".cvtmp-"));
            if ours && std::fs::remove_file(&target).is_ok() {
                removed += 1;
            }
        }
        let _ = std::fs::remove_file(entry.path());
    }
    removed
}

fn process_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    let mut system = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).is_some()
}

pub fn copy_mtime(from: &Path, to: &Path) -> Result<()> {
    let mtime = std::fs::metadata(from)
        .and_then(|m| m.modified())
        .at(from)?;
    let file = std::fs::OpenOptions::new().write(true).open(to).at(to)?;
    file.set_modified(mtime).at(to)
}

pub fn trash(path: &Path) -> Result<()> {
    trash::delete(path).map_err(|e| Error::TrashUnavailable(e.to_string()))
}

/// Обработка исходника после успешной конвертации. Корзина недоступна —
/// файл остаётся, решение за пользователем (FR-5.4).
pub fn dispose_source(path: &Path, policy: SourcePolicy) -> SourceAction {
    match policy {
        SourcePolicy::Keep => SourceAction::Kept,
        SourcePolicy::Trash => match trash(path) {
            Ok(()) => SourceAction::Trashed,
            Err(e) => SourceAction::KeptBecause(e.to_string()),
        },
        SourcePolicy::Delete => match std::fs::remove_file(path) {
            Ok(()) => SourceAction::Deleted,
            Err(e) => SourceAction::KeptBecause(e.to_string()),
        },
    }
}

/// Точка монтирования и свободное место тома, где лежит (или появится) `path`.
pub fn volume_of(path: &Path, disks: &sysinfo::Disks) -> Option<(PathBuf, u64)> {
    // Папки результата может ещё не быть — берём ближайшего существующего предка.
    let existing = path.ancestors().find(|p| p.exists())?;
    let path = std::fs::canonicalize(existing).ok()?;
    disks
        .list()
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| (d.mount_point().to_path_buf(), d.available_space()))
}

/// Свободное место на томе, где лежит `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    volume_of(path, &sysinfo::Disks::new_with_refreshed_list()).map(|(_, free)| free)
}

pub fn is_no_space(e: &Error) -> bool {
    match e {
        Error::Io { source, .. } => source.kind() == std::io::ErrorKind::StorageFull,
        Error::Ffmpeg { stderr, .. } => crate::ffmpeg::is_no_space(stderr),
        Error::Encode(msg) => msg.contains("No space left") || msg.contains("not enough space"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_file_is_removed_unless_committed() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("out.jpg");
        let t = TempFile::new(&final_path).unwrap();
        std::fs::write(t.path(), b"x").unwrap();
        let tmp = t.path().to_path_buf();
        assert!(
            tmp.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".out.cvtmp-")
        );
        drop(t);
        assert!(!tmp.exists());

        let t = TempFile::new(&final_path).unwrap();
        std::fs::write(t.path(), b"x").unwrap();
        assert_eq!(t.commit(&final_path, false).unwrap(), final_path);

        // Имя занято — commit без перезаписи выбирает следующее.
        let t = TempFile::new(&final_path).unwrap();
        std::fs::write(t.path(), b"y").unwrap();
        assert_eq!(
            t.commit(&final_path, false).unwrap(),
            dir.path().join("out_1.jpg")
        );
        assert_eq!(std::fs::read(&final_path).unwrap(), b"x");
    }

    #[test]
    fn suffix_keeps_extension() {
        assert_eq!(
            with_suffix(Path::new("/a/b.c.jpg"), 2),
            PathBuf::from("/a/b.c_2.jpg")
        );
        assert_eq!(
            with_suffix(Path::new("/a/noext"), 1),
            PathBuf::from("/a/noext_1")
        );
    }
}
