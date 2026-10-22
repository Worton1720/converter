//! История действий: что программа сделала с файлами. На неё опираются «Вернуть как было»
//! (по записи видно, что вернуть из корзины и что убрать) и продолжение после сбоя
//! (незакрытая запись преобразования — список того, что не успели сделать).
//!
//! Хранится в `history.json` в папке состояния, последние [`KEEP`] записей.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::events::SourceAction;
use crate::options::ConvertOptions;
use crate::paths;

/// Сколько записей хранить.
pub const KEEP: usize = 100;

/// Что стало с исходным файлом.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fate {
    Kept,
    Trashed,
    /// Удалён навсегда — вернуть нельзя.
    Deleted,
    MovedTo(PathBuf),
}

impl From<&SourceAction> for Fate {
    fn from(a: &SourceAction) -> Self {
        match a {
            SourceAction::Trashed => Fate::Trashed,
            SourceAction::Deleted => Fate::Deleted,
            SourceAction::Kept | SourceAction::KeptBecause(_) => Fate::Kept,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub source: PathBuf,
    /// Созданный файл (у преобразования).
    pub output: Option<PathBuf>,
    pub fate: Fate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Action {
    Convert {
        options: ConvertOptions,
        /// Ещё не сделанные файлы: при сбое — что доделать.
        pending: Vec<PathBuf>,
        skipped: usize,
        failed: usize,
    },
    Dupes {
        freed: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Миллисекунды от эпохи на момент начала — и время, и ключ записи.
    pub id: u64,
    /// `None` — запись не закрыта: работа идёт или программа упала посреди неё.
    pub finished_ms: Option<u64>,
    pub action: Action,
    pub changes: Vec<Change>,
    /// Действие отменено кнопкой «Вернуть как было».
    #[serde(default)]
    pub undone: bool,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct History {
    pub entries: Vec<Entry>,
}

impl History {
    pub fn path() -> PathBuf {
        paths::state_dir().join("history.json")
    }

    /// Битый файл — пустая история: она не должна мешать запуску.
    pub fn load_from(path: &Path) -> History {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn load() -> History {
        Self::load_from(&Self::path())
    }

    /// Через временный файл: оборванная запись не должна портить историю.
    pub fn save_to(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    pub fn save(&self) {
        self.save_to(&Self::path());
    }

    /// Новая запись в конец; старые сверх [`KEEP`] забываются.
    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
        let extra = self.entries.len().saturating_sub(KEEP);
        self.entries.drain(..extra);
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }
}

impl Entry {
    /// Начало преобразования: все файлы пока в `pending`.
    pub fn convert(options: ConvertOptions, paths: Vec<PathBuf>) -> Entry {
        Entry {
            id: now_ms(),
            finished_ms: None,
            action: Action::Convert {
                options,
                pending: paths,
                skipped: 0,
                failed: 0,
            },
            changes: Vec::new(),
            undone: false,
        }
    }

    /// Файл обработан: убрать из `pending`; если что-то сделано — записать изменение.
    pub fn file_done(
        &mut self,
        source: &Path,
        change: Option<Change>,
        skipped: bool,
        failed: bool,
    ) {
        if let Action::Convert {
            pending,
            skipped: s,
            failed: f,
            ..
        } = &mut self.action
        {
            pending.retain(|p| p != source);
            *s += usize::from(skipped);
            *f += usize::from(failed);
        }
        self.changes.extend(change);
    }

    pub fn finish(&mut self) {
        self.finished_ms = Some(now_ms());
    }
}

// ---------- «Вернуть как было» ----------

/// Итог отмены: сколько вернулось и что не получилось (простыми словами).
#[derive(Debug, Default, Serialize)]
pub struct UndoReport {
    /// Исходники возвращены (из корзины или из другой папки).
    pub restored: usize,
    /// Созданные файлы убраны в корзину.
    pub removed: usize,
    pub problems: Vec<(PathBuf, String)>,
}

/// Возвращает файлы как было до действия. Порядок важен для сохранности: сначала исходник
/// возвращается на место, и только если это удалось (или он и не уходил), его результат
/// убирается в корзину — иначе человек остался бы без обеих копий.
pub fn undo(entry: &Entry) -> UndoReport {
    let mut report = UndoReport::default();
    // Корзина целиком читается один раз; у одного пути берётся последнее удаление.
    let mut trashed: Option<Vec<trash::TrashItem>> = None;
    for c in &entry.changes {
        let source_back = match &c.fate {
            Fate::Kept => true,
            Fate::Deleted => {
                report.problems.push((
                    c.source.clone(),
                    "файл был удалён навсегда — вернуть нельзя".into(),
                ));
                false
            }
            Fate::MovedTo(at) => match move_back(at, &c.source) {
                Ok(()) => {
                    report.restored += 1;
                    true
                }
                Err(e) => {
                    report.problems.push((c.source.clone(), e));
                    false
                }
            },
            Fate::Trashed => {
                let items =
                    trashed.get_or_insert_with(|| trash::os_limited::list().unwrap_or_default());
                match restore_from_trash(items, &c.source) {
                    Ok(()) => {
                        report.restored += 1;
                        true
                    }
                    Err(e) => {
                        report.problems.push((c.source.clone(), e));
                        false
                    }
                }
            }
        };
        if let Some(out) = c.output.as_ref().filter(|_| source_back) {
            if !out.exists() {
                continue; // уже убран вручную
            }
            match crate::fs_ops::trash(out) {
                Ok(()) => report.removed += 1,
                Err(e) => report.problems.push((out.clone(), e.to_string())),
            }
        }
    }
    report
}

fn move_back(from: &Path, to: &Path) -> std::result::Result<(), String> {
    if to.exists() {
        return Err("на прежнем месте уже есть файл с таким именем".into());
    }
    if !from.exists() {
        return Err("файла уже нет там, куда он был перемещён".into());
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if std::fs::rename(from, to).is_err() {
        // Другой диск: копия с проверкой размера, затем удаление.
        std::fs::copy(from, to).map_err(|e| e.to_string())?;
        let same = std::fs::metadata(from)
            .ok()
            .zip(std::fs::metadata(to).ok())
            .is_some_and(|(a, b)| a.len() == b.len());
        if !same {
            let _ = std::fs::remove_file(to);
            return Err("копия не совпала по размеру".into());
        }
        std::fs::remove_file(from).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn restore_from_trash(
    items: &mut Vec<trash::TrashItem>,
    original: &Path,
) -> std::result::Result<(), String> {
    if original.exists() {
        return Err("на прежнем месте уже есть файл с таким именем".into());
    }
    let Some(i) = items
        .iter()
        .enumerate()
        .filter(|(_, t)| t.original_path() == original)
        .max_by_key(|(_, t)| t.time_deleted)
        .map(|(i, _)| i)
    else {
        return Err("файла нет в корзине — возможно, её уже очистили".into());
    };
    let item = items.remove(i);
    trash::os_limited::restore_all([item]).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_convert_run_and_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let mut h = History::default();
        let mut e = Entry::convert(
            ConvertOptions::default(),
            vec!["/a.heic".into(), "/b.mov".into()],
        );
        let id = e.id;
        e.file_done(
            Path::new("/a.heic"),
            Some(Change {
                source: "/a.heic".into(),
                output: Some("/a.jpg".into()),
                fate: Fate::Trashed,
            }),
            false,
            false,
        );
        h.push(e);
        h.save_to(&path);

        // Программа «упала»: запись не закрыта, второй файл не сделан.
        let mut back = History::load_from(&path);
        let e = back.get_mut(id).unwrap();
        assert_eq!(e.finished_ms, None);
        let Action::Convert { pending, .. } = &e.action else {
            panic!()
        };
        assert_eq!(pending, &[PathBuf::from("/b.mov")]);
        assert_eq!(e.changes[0].fate, Fate::Trashed);
        e.file_done(Path::new("/b.mov"), None, false, true);
        e.finish();
        let Action::Convert {
            pending, failed, ..
        } = &e.action
        else {
            panic!()
        };
        assert!(pending.is_empty());
        assert_eq!(*failed, 1);
    }

    #[test]
    fn undo_moves_back_and_reports_what_cannot_return() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let original = d.join("Фото/копия.jpg");
        let moved = d.join("Лишнее/копия.jpg");
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::write(&moved, b"jpeg").unwrap();
        let entry = Entry {
            id: 1,
            finished_ms: Some(2),
            action: Action::Dupes { freed: 4 },
            changes: vec![
                Change {
                    source: original.clone(),
                    output: None,
                    fate: Fate::MovedTo(moved.clone()),
                },
                Change {
                    source: d.join("удалённый.jpg"),
                    output: None,
                    fate: Fate::Deleted,
                },
            ],
            undone: false,
        };
        let r = undo(&entry);
        assert_eq!(r.restored, 1);
        assert!(original.exists() && !moved.exists());
        assert_eq!(r.problems.len(), 1);
        assert!(r.problems[0].1.contains("навсегда"));

        // Повторная отмена: на месте уже есть файл — не перезаписываем.
        std::fs::write(&moved, b"other").unwrap();
        let r = undo(&entry);
        assert_eq!(r.restored, 0);
        assert_eq!(std::fs::read(&original).unwrap(), b"jpeg");
    }

    #[test]
    fn keeps_last_entries_and_tolerates_broken_file() {
        let mut h = History::default();
        for i in 0..KEEP + 5 {
            h.push(Entry {
                id: i as u64,
                finished_ms: Some(1),
                action: Action::Dupes { freed: 0 },
                changes: Vec::new(),
                undone: false,
            });
        }
        assert_eq!(h.entries.len(), KEEP);
        assert_eq!(h.entries[0].id, 5);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::write(&path, b"{ broken").unwrap();
        assert!(History::load_from(&path).entries.is_empty());
    }
}
