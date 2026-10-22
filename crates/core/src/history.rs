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
