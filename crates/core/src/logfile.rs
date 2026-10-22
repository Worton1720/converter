//! Журнал на диске с ротацией (FR-13.1): `converter.log` + до 4 старых файлов по 10 МБ.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::events::{Event, JobOutcome, LogLevel};
use crate::paths;

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const KEEP_OLD: usize = 4;

pub struct LogFile {
    dir: PathBuf,
    file: Mutex<Option<File>>,
}

impl LogFile {
    pub fn open_default() -> LogFile {
        Self::open(paths::state_dir().join("logs"))
    }

    pub fn open(dir: PathBuf) -> LogFile {
        let _ = std::fs::create_dir_all(&dir);
        LogFile {
            file: Mutex::new(Self::open_current(&dir)),
            dir,
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("converter.log")
    }

    fn open_current(dir: &Path) -> Option<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("converter.log"))
            .ok()
    }

    /// `converter.log` → `.1` → … → `.4`, самый старый удаляется.
    fn rotate(&self) {
        let name = |n: usize| self.dir.join(format!("converter.log.{n}"));
        let _ = std::fs::remove_file(name(KEEP_OLD));
        for n in (1..KEEP_OLD).rev() {
            let _ = std::fs::rename(name(n), name(n + 1));
        }
        let _ = std::fs::rename(self.path(), name(1));
    }

    pub fn write(&self, level: &str, message: &str) {
        let mut guard = self.file.lock().unwrap();
        let too_big = guard
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|m| m.len() >= MAX_BYTES);
        if too_big {
            *guard = None;
            self.rotate();
        }
        if guard.is_none() {
            *guard = Self::open_current(&self.dir);
        }
        if let Some(f) = guard.as_mut() {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            // `2026:10:22 02:23:40` → `2026-10-22 02:23:40 UTC`.
            let stamp = crate::plan::civil_from_unix(secs).replacen(':', "-", 2);
            let _ = writeln!(f, "{stamp} UTC\t{level}\t{}", message.replace('\n', " ⏎ "));
        }
    }

    /// Записывает событие ядра, если оно интересно для разбора проблем.
    pub fn event(&self, e: &Event) {
        match e {
            Event::JobStarted {
                source, operation, ..
            } => self.write(
                "INFO",
                &format!("начато: {} ({operation})", source.display()),
            ),
            Event::JobFinished {
                source, outcome, ..
            } => {
                let (level, text) = match outcome {
                    JobOutcome::Done {
                        output, operation, ..
                    } => (
                        "INFO",
                        format!(
                            "готово: {} → {} ({operation})",
                            source.display(),
                            output.display()
                        ),
                    ),
                    JobOutcome::Skipped { reason } => {
                        ("INFO", format!("пропущено: {}: {reason}", source.display()))
                    }
                    JobOutcome::Failed { error } => {
                        ("ERROR", format!("ошибка: {}: {error}", source.display()))
                    }
                    JobOutcome::Cancelled => ("INFO", format!("отменено: {}", source.display())),
                };
                self.write(level, &text);
            }
            Event::Log { level, message, .. } => {
                let level = match level {
                    LogLevel::Info => "INFO",
                    LogLevel::Warn => "WARN",
                    LogLevel::Error => "ERROR",
                };
                self.write(level, message);
            }
            Event::PausedNoSpace { path } => {
                self.write("WARN", &format!("нет места: {}", path.display()))
            }
            Event::RunFinished(s) => self.write(
                "INFO",
                &format!(
                    "итог: готово {}, пропущено {}, ошибок {}, отменено {}",
                    s.done, s.skipped, s.failed, s.cancelled
                ),
            ),
            Event::JobProgress { .. } | Event::Governor(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_keeps_limited_history() {
        let dir = tempfile::tempdir().unwrap();
        let log = LogFile::open(dir.path().to_path_buf());
        let chunk = "x".repeat(1024 * 1024);
        for _ in 0..(MAX_BYTES as usize / chunk.len() + 1) * (KEEP_OLD + 2) {
            log.write("INFO", &chunk);
        }
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), KEEP_OLD + 1, "{names:?}");
        let total: u64 = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.metadata().unwrap().len())
            .sum();
        assert!(total <= (MAX_BYTES + 2 * 1024 * 1024) * (KEEP_OLD as u64 + 1));
    }
}
