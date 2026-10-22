//! Имена результатов: шаблон (FR-4.2), папка (FR-4.1) и резервирование имён до
//! запуска задач (FR-4.3), чтобы параллельные файлы не получили одно имя.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::fs_ops::with_suffix;
use crate::options::{ConflictPolicy, OutputDir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    Path(PathBuf),
    /// Файл уже существует, а политика — «пропустить».
    Skip(PathBuf),
}

pub struct OutputPlanner {
    output: OutputDir,
    template: String,
    conflict: ConflictPolicy,
    reserved: HashSet<String>,
    counter: usize,
}

/// Ключ сравнения путей: на Windows ФС нечувствительна к регистру.
fn key(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    }
}

const RESERVED_WINDOWS: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Заменяет недопустимые символы и зарезервированные имена Windows (на всех ОС,
/// чтобы результат можно было перенести на любой диск).
pub fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    if s.is_empty() {
        s.push('_');
    }
    let base = s.split('.').next().unwrap_or_default().to_ascii_uppercase();
    if RESERVED_WINDOWS.contains(&base.as_str()) {
        s.insert(0, '_');
    }
    s
}

/// `2024:05:17 10:20:30` → `2024-05-17_10-20-30`.
fn format_date(date: &str) -> String {
    date.replacen(' ', "_", 1).replace(':', "-")
}

impl OutputPlanner {
    pub fn new(output: OutputDir, template: String, conflict: ConflictPolicy) -> Self {
        Self {
            output,
            template,
            conflict,
            reserved: HashSet::new(),
            counter: 0,
        }
    }

    pub fn needs_date(&self) -> bool {
        self.template.contains("{date}")
    }

    /// `root` — корень, от которого считается структура подпапок (для `IntoKeepTree`).
    /// `date` — дата съёмки; если её нет, берётся mtime.
    pub fn plan(
        &mut self,
        source: &Path,
        root: Option<&Path>,
        ext: &str,
        date: Option<&str>,
    ) -> Planned {
        self.counter += 1;
        let stem = source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let src_ext = source
            .extension()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let date = date
            .map(str::to_string)
            .unwrap_or_else(|| mtime_date(source));
        let name = self
            .template
            .replace("{name}", &stem)
            .replace("{ext}", &src_ext)
            .replace("{n}", &self.counter.to_string())
            .replace("{date}", &format_date(&date));
        let file_name = format!("{}.{ext}", sanitize(&name));

        let source_dir = source.parent().unwrap_or(Path::new("."));
        let dir = match &self.output {
            OutputDir::BesideSource => source_dir.to_path_buf(),
            OutputDir::Into(d) => d.clone(),
            OutputDir::IntoKeepTree(d) => {
                let rel = root
                    .and_then(|r| source_dir.strip_prefix(r).ok())
                    .unwrap_or(Path::new(""));
                d.join(rel)
            }
        };
        let wanted = dir.join(file_name);

        let is_source = key(&wanted) == key(source);
        let reserved = self.reserved.contains(&key(&wanted));
        let exists = wanted.exists();
        let target = match self.conflict {
            // Перезаписывать можно чужой старый файл, но не исходник и не соседний результат.
            ConflictPolicy::Overwrite if !is_source && !reserved => wanted,
            ConflictPolicy::Skip if exists && !is_source => return Planned::Skip(wanted),
            _ if !is_source && !reserved && !exists => wanted,
            _ => (1..)
                .map(|n| with_suffix(&wanted, n))
                .find(|p| !self.reserved.contains(&key(p)) && !p.exists() && key(p) != key(source))
                .expect("бесконечный перебор суффиксов"),
        };
        self.reserved.insert(key(&target));
        Planned::Path(target)
    }
}

fn mtime_date(path: &Path) -> String {
    let secs = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    civil_from_unix(secs)
}

/// UTC-дата без внешних зависимостей (алгоритм Говарда Хиннанта).
pub(crate) fn civil_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}:{m:02}:{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_sources_get_distinct_names() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("IMG_1.HEIC");
        let b = dir.path().join("IMG_1.PNG");
        std::fs::write(&a, b"").unwrap();
        std::fs::write(&b, b"").unwrap();
        let mut p = OutputPlanner::new(
            OutputDir::BesideSource,
            "{name}".into(),
            ConflictPolicy::Suffix,
        );
        assert_eq!(
            p.plan(&a, None, "jpg", None),
            Planned::Path(dir.path().join("IMG_1.jpg"))
        );
        assert_eq!(
            p.plan(&b, None, "jpg", None),
            Planned::Path(dir.path().join("IMG_1_1.jpg"))
        );
    }

    #[test]
    fn never_targets_the_source_itself() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("photo.jpg");
        std::fs::write(&src, b"").unwrap();
        for policy in [
            ConflictPolicy::Suffix,
            ConflictPolicy::Overwrite,
            ConflictPolicy::Skip,
        ] {
            let mut p = OutputPlanner::new(OutputDir::BesideSource, "{name}".into(), policy);
            assert_eq!(
                p.plan(&src, None, "jpg", None),
                Planned::Path(dir.path().join("photo_1.jpg")),
                "{policy:?}"
            );
        }
    }

    #[test]
    fn skip_and_overwrite_existing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.heic");
        let existing = dir.path().join("a.jpg");
        std::fs::write(&src, b"").unwrap();
        std::fs::write(&existing, b"").unwrap();
        let mut p = OutputPlanner::new(
            OutputDir::BesideSource,
            "{name}".into(),
            ConflictPolicy::Skip,
        );
        assert_eq!(
            p.plan(&src, None, "jpg", None),
            Planned::Skip(existing.clone())
        );
        let mut p = OutputPlanner::new(
            OutputDir::BesideSource,
            "{name}".into(),
            ConflictPolicy::Overwrite,
        );
        assert_eq!(p.plan(&src, None, "jpg", None), Planned::Path(existing));
    }

    #[test]
    fn keep_tree_and_template() {
        let mut p = OutputPlanner::new(
            OutputDir::IntoKeepTree(PathBuf::from("/out")),
            "{date}_{name}".into(),
            ConflictPolicy::Suffix,
        );
        let got = p.plan(
            Path::new("/phone/DCIM/100APPLE/IMG_7.HEIC"),
            Some(Path::new("/phone")),
            "jpg",
            Some("2024:05:17 10:20:30"),
        );
        assert_eq!(
            got,
            Planned::Path(PathBuf::from(
                "/out/DCIM/100APPLE/2024-05-17_10-20-30_IMG_7.jpg"
            ))
        );
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize("a:b*c?"), "a_b_c_");
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("nul.tar"), "_nul.tar");
        assert_eq!(sanitize("name. "), "name");
        assert_eq!(sanitize("Я-люблю"), "Я-люблю");
    }

    #[test]
    fn civil_date() {
        assert_eq!(civil_from_unix(0), "1970:01:01 00:00:00");
        assert_eq!(civil_from_unix(1_715_941_230), "2024:05:17 10:20:30");
    }
}
