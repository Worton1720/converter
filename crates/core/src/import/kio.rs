//! Android на KDE: KIO через `kioclient` (kio-fuse не монтирует `mtp:/`).

use std::collections::HashMap;
use std::sync::Condvar;
use std::sync::atomic::AtomicUsize;

use serde::Deserialize;

use super::*;

/// Списки папок и сведения о файлах по MTP — по 0,3 с на запуск `kioclient`, поэтому
/// параллельно (больше 8 не быстрее: телефон отвечает по очереди).
const PARALLEL: usize = 8;

fn run(args: &[&str], limit: Duration, cancel: Option<&AtomicBool>) -> Option<String> {
    let mut all = vec!["--noninteractive", "--platform", "offscreen"];
    all.extend_from_slice(args);
    run_with_timeout("kioclient", &all, limit, cancel)
}

/// Имена в папке; `None` — телефон не отвечает или `kioclient` нет (не KDE).
pub(super) fn ls(url: &str) -> Option<Vec<String>> {
    let out = run(&["ls", url], Duration::from_secs(60), None)?;
    Some(
        out.lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty() && *l != "." && *l != "..")
            .map(str::to_string)
            .collect(),
    )
}

/// Адрес вложенного элемента. `%`, `#` и `?` в имени иначе ломают адрес.
pub(super) fn child(url: &str, name: &str) -> String {
    let name = name
        .replace('%', "%25")
        .replace('#', "%23")
        .replace('?', "%3F");
    format!("{}/{name}", url.trim_end_matches('/'))
}

pub(super) struct Stat {
    pub dir: bool,
    pub size: u64,
    pub month: String,
}

fn stat(url: &str) -> Option<Stat> {
    let out = run(&["stat", url], Duration::from_secs(30), None)?;
    parse_stat(&out)
}

pub(super) fn parse_stat(out: &str) -> Option<Stat> {
    let field = |key: &str| out.lines().find_map(|l| l.strip_prefix(key).map(str::trim));
    Some(Stat {
        // Восьмеричный тип st_mode, с ведущим нулём или без: `0040000` — папка.
        dir: u32::from_str_radix(field("FILE_TYPE")?, 8).ok()? & 0o170000 == 0o040000,
        size: field("SIZE").and_then(|v| v.parse().ok()).unwrap_or(0),
        month: field("MODIFICATION_TIME")
            .and_then(month_of)
            .unwrap_or_else(|| "Без даты".into()),
    })
}

/// «Sun Oct 18 15:15:22 2026» → «2026-10». Русские сокращения — на случай другого языка.
pub(super) fn month_of(time: &str) -> Option<String> {
    const EN: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    const RU: [&str; 12] = [
        "янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
    ];
    let words: Vec<String> = time.split_whitespace().map(str::to_lowercase).collect();
    let year = words
        .iter()
        .find(|w| w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()))?;
    let month = words.iter().find_map(|w| {
        let head: String = w.chars().take(3).collect();
        EN.iter().position(|m| *m == head).or_else(|| {
            RU.iter()
                .position(|m| *m == head || (*m == "мая" && head == "май"))
        })
    })?;
    Some(format!("{year}-{:02}", month + 1))
}

/// Android-телефоны, которые KDE видит по MTP: устройство → накопители. Накопитель,
/// который не открывается или пуст, — телефон ещё не разрешил доступ.
pub(super) fn mtp_sources() -> Vec<Source> {
    let Some(devices) = ls("mtp:/") else {
        return Vec::new();
    };
    devices
        .into_iter()
        .map(|device| {
            let base = child("mtp:/", &device);
            let roots = ls(&base)
                .unwrap_or_default()
                .into_iter()
                .map(|storage| child(&base, &storage))
                .filter(|storage| ls(storage).is_some_and(|names| !names.is_empty()))
                .map(Location::Kio)
                .collect();
            Source {
                name: device,
                kind: SourceKind::Phone,
                roots,
            }
        })
        .collect()
}

/// Стоит ли спрашивать сведения: фото, видео или, похоже, папка. `kioclient ls` не
/// отличает папки от файлов, а у папок бывают точки в имени (`com.whatsapp`): у файла
/// расширение — до четырёх букв и цифр.
pub(super) fn worth_stat(name: &str) -> bool {
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        None => true,
        Some(ext) => {
            is_media(name) || ext.len() > 4 || !ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
    }
}

/// Что известно о файлах телефона с прошлого поиска (настройка «Запоминать, что есть на
/// телефоне»): размер и месяц фото и видео, какие имена — папки и скрыты ли они
/// (`.nomedia`). Повторный поиск читает списки папок, но о знакомом телефон не спрашивает.
/// Файл, изменённый на телефоне под тем же именем, так не заметить — у Android правка
/// снимка сохраняется под новым именем.
#[derive(Default, Serialize, Deserialize)]
struct Memory {
    /// Адрес → (размер, «ГГГГ-ММ»).
    files: HashMap<String, (u64, String)>,
    /// Адрес папки → скрыта ли.
    dirs: HashMap<String, bool>,
}

/// Память прошлого поиска и то, что увидено в этом.
pub(super) struct Remembered {
    path: Option<PathBuf>,
    old: Memory,
    seen: Mutex<Memory>,
}

impl Remembered {
    /// `None` — не запоминать: ничего не читается и не пишется.
    pub(super) fn open(path: Option<&Path>) -> Self {
        let old = path
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path: path.map(Path::to_path_buf),
            old,
            seen: Mutex::default(),
        }
    }

    fn file(&self, url: &str) -> Option<(u64, String)> {
        self.old.files.get(url).cloned()
    }

    fn dir(&self, url: &str) -> Option<bool> {
        self.old.dirs.get(url).copied()
    }

    fn note_file(&self, url: &str, size: u64, month: &str) {
        if self.path.is_some() {
            let entry = (size, month.to_string());
            self.seen
                .lock()
                .unwrap()
                .files
                .insert(url.to_string(), entry);
        }
    }

    fn note_dir(&self, url: &str, hidden: bool) {
        if self.path.is_some() {
            self.seen
                .lock()
                .unwrap()
                .dirs
                .insert(url.to_string(), hidden);
        }
    }

    /// После обхода `roots`: о них — только увиденное сейчас (удалённое с телефона
    /// забывается), о других устройствах — прежнее. Через временный файл.
    pub(super) fn save(self, roots: &[Location]) {
        let Some(path) = self.path else { return };
        let mut next = self.seen.into_inner().unwrap();
        let walked = |url: &str| {
            roots
                .iter()
                .any(|r| matches!(r, Location::Kio(root) if url.starts_with(root.as_str())))
        };
        for (url, v) in self.old.files {
            if !walked(&url) {
                next.files.entry(url).or_insert(v);
            }
        }
        for (url, v) in self.old.dirs {
            if !walked(&url) {
                next.dirs.entry(url).or_insert(v);
            }
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(&next) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
}

enum Task {
    /// `visible` — папка заведомо не скрыта (из памяти): `.nomedia` не проверять.
    List {
        url: String,
        visible: bool,
    },
    Stat {
        url: String,
        name: String,
    },
}

/// Телефон не ответил — ещё раз чуть позже: после простоя служба KDE заново открывает
/// устройство, и первые запросы бывают неудачны.
fn again<T>(request: impl Fn() -> Option<T>) -> Option<T> {
    request().or_else(|| {
        std::thread::sleep(Duration::from_millis(500));
        request()
    })
}

/// Обход папки на телефоне: общая очередь «список папки» и «сведения о файле», которую
/// разбирают `PARALLEL` потоков. Долгий список (кэш мессенджера на десятки тысяч
/// файлов читается по MTP полминуты) не задерживает остальное. Сведения
/// запрашиваются только у фото, видео и папок — и только о том, чего нет в памяти.
/// Очередь — стопка: `DCIM` кладётся последней и разбирается первой, за ней — её папки.
/// Возвращает, сколько папок и файлов телефон не отдал и со второго раза.
pub(super) fn walk(
    root: &str,
    cancel: &AtomicBool,
    memory: &Remembered,
    add: &(dyn Fn(Found) + Sync),
) -> Result<usize> {
    // Очередь и сколько задач сейчас в работе: пусто и никто не работает — конец.
    let first = Task::List {
        url: root.to_string(),
        visible: true,
    };
    let state = Mutex::new((vec![first], 0usize));
    let wake = Condvar::new();
    let root_failed = AtomicBool::new(false);
    let unread = AtomicUsize::new(0);
    let found = |url: String, name: String, size: u64, month: String| {
        memory.note_file(&url, size, &month);
        add(Found {
            location: Location::Kio(url),
            name,
            size,
            month,
        });
    };
    let process = |task: Task| -> Vec<Task> {
        match task {
            Task::List { url: dir, visible } => {
                // Скрытую папку не читать вовсе: у кэша мессенджера список — полминуты,
                // а проверка `.nomedia` — секунда.
                if !visible && stat(&child(&dir, NOMEDIA)).is_some() {
                    memory.note_dir(&dir, true);
                    return Vec::new();
                }
                let Some(names) = again(|| ls(&dir)) else {
                    if dir == root {
                        root_failed.store(true, Ordering::Relaxed);
                    }
                    unread.fetch_add(1, Ordering::Relaxed);
                    return Vec::new();
                };
                let hidden = names.iter().any(|n| n == NOMEDIA);
                if dir != root {
                    memory.note_dir(&dir, hidden);
                }
                if hidden {
                    return Vec::new();
                }
                let parent = dir.rsplit('/').next().unwrap_or_default();
                let mut tasks: Vec<Task> = names
                    .into_iter()
                    .filter(|n| !skipped_name(n) && !skipped_dir(parent, n) && worth_stat(n))
                    .map(|name| Task::Stat {
                        url: child(&dir, &name),
                        name,
                    })
                    .collect();
                tasks.sort_by_key(
                    |t| matches!(t, Task::Stat { name, .. } if name.eq_ignore_ascii_case("DCIM")),
                );
                tasks
            }
            Task::Stat { url, name } => {
                if let Some((size, month)) = memory.file(&url) {
                    found(url, name, size, month);
                    return Vec::new();
                }
                match memory.dir(&url) {
                    Some(true) => {
                        memory.note_dir(&url, true);
                        return Vec::new();
                    }
                    Some(false) => return vec![Task::List { url, visible: true }],
                    None => {}
                }
                // Имя только что было в списке папки: нет сведений — телефон не ответил.
                match again(|| stat(&url)) {
                    Some(st) if st.dir => vec![Task::List {
                        url,
                        visible: false,
                    }],
                    Some(st) if is_media(&name) => {
                        found(url, name, st.size, st.month);
                        Vec::new()
                    }
                    Some(_) => Vec::new(),
                    None => {
                        unread.fetch_add(1, Ordering::Relaxed);
                        Vec::new()
                    }
                }
            }
        }
    };
    std::thread::scope(|s| {
        for _ in 0..PARALLEL {
            s.spawn(|| {
                loop {
                    let task = {
                        let mut st = state.lock().unwrap();
                        loop {
                            if cancel.load(Ordering::Relaxed) {
                                wake.notify_all();
                                return;
                            }
                            if let Some(task) = st.0.pop() {
                                st.1 += 1;
                                break task;
                            }
                            if st.1 == 0 {
                                wake.notify_all();
                                return;
                            }
                            // С таймаутом — чтобы заметить отмену.
                            st = wake.wait_timeout(st, Duration::from_millis(200)).unwrap().0;
                        }
                    };
                    let more = process(task);
                    let mut st = state.lock().unwrap();
                    st.0.extend(more);
                    st.1 -= 1;
                    wake.notify_all();
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if root_failed.load(Ordering::Relaxed) {
        return Err(Error::Invalid(
            "телефон не отвечает: разблокируйте его и выберите «Передача файлов»".into(),
        ));
    }
    Ok(unread.into_inner())
}

/// Первые `limit` байт файла с телефона (`kioclient cat`, затем процесс прерывается).
pub(super) fn head(url: &str, limit: usize) -> Option<Vec<u8>> {
    let mut child = crate::sys::host_command("kioclient")
        .args(["--noninteractive", "--platform", "offscreen", "cat", url])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(limit);
        let mut chunk = [0u8; 64 << 10];
        while buf.len() < limit {
            match out.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        buf.truncate(limit);
        buf
    });
    let start = Instant::now();
    while !reader.is_finished() && start.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    reader.join().ok().filter(|b| !b.is_empty())
}

/// Копия с телефона в `tmp`. KIO сохраняет время изменения. Предел времени — по размеру
/// (MTP не медленнее ~2 МБ/с), не меньше двух минут. Прогресс — по размеру `tmp`.
pub(super) fn copy(
    url: &str,
    tmp: &Path,
    size: u64,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
) -> Result<()> {
    let _ = std::fs::remove_file(tmp);
    let dest = tmp.to_string_lossy();
    let tick = || progress(std::fs::metadata(tmp).map_or(0, |m| m.len()));
    let done = run_watched(&["copy", url, &dest], limit(size), Some(cancel), &tick).is_some();
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if !done || !tmp.exists() {
        return Err(not_copied());
    }
    Ok(())
}

fn limit(size: u64) -> Duration {
    Duration::from_secs(120 + size / (2 << 20))
}

fn not_copied() -> Error {
    Error::Invalid(
        "файл не скопировался с телефона — проверьте, что он подключён и разблокирован".into(),
    )
}

fn run_watched(
    args: &[&str],
    limit: Duration,
    cancel: Option<&AtomicBool>,
    tick: &dyn Fn(),
) -> Option<String> {
    let mut all = vec!["--noninteractive", "--platform", "offscreen"];
    all.extend_from_slice(args);
    super::run_watched("kioclient", &all, limit, cancel, tick)
}

/// Пачка — не больше стольких файлов и (кроме первого) байт: прогресс и «Остановить»
/// остаются отзывчивыми.
const BATCH_FILES: usize = 16;
const BATCH_BYTES: u64 = 32 << 20;

/// Сколько первых файлов копировать одной командой: с телефона, в одну папку месяца и
/// с разными именами (в пачке файл получает имя, как на телефоне).
pub(super) fn batch_len(items: &[ImportItem]) -> usize {
    let dir = items[0].dest.parent();
    let mut names = HashSet::new();
    let mut bytes = 0;
    items
        .iter()
        .take(BATCH_FILES)
        .take_while(|item| {
            let Location::Kio(url) = &item.source else {
                return false;
            };
            bytes += item.size;
            item.dest.parent() == dir
                && names.insert(file_name(url))
                && (names.len() == 1 || bytes <= BATCH_BYTES)
        })
        .count()
        .max(1)
}

/// Имя файла на телефоне — обратное к [`child`].
pub(super) fn file_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or_default()
        .replace("%3F", "?")
        .replace("%23", "#")
        .replace("%25", "%")
}

/// Пачка одной командой `kioclient copy a b … папка/` во временную папку рядом с местом
/// назначения, затем каждый файл — проверка и своё имя. Что не скопировалось пачкой,
/// повторяется по одному; при отмене остаются только целиком скопированные (KIO пишет
/// в `.part` и переименовывает в конце).
pub(super) fn copy_batch(
    batch: &[ImportItem],
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
) -> Vec<Result<PathBuf>> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = batch[0]
        .dest
        .parent()
        .expect("у файла назначения есть папка");
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".import-{}-{n}", std::process::id()));
    if std::fs::create_dir_all(&tmp).is_err() {
        return batch
            .iter()
            .map(|item| copy_one(item, cancel, &|_| {}))
            .collect();
    }
    let urls: Vec<&str> = batch
        .iter()
        .filter_map(|item| match &item.source {
            Location::Kio(url) => Some(url.as_str()),
            Location::Path(_) => None,
        })
        .collect();
    let into = format!("{}/", tmp.to_string_lossy());
    let mut args = vec!["copy"];
    args.extend(&urls);
    args.push(&into);
    let total = batch.iter().map(|i| i.size).sum();
    let tick = || {
        let done = std::fs::read_dir(&tmp).map_or(0, |entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        });
        progress(done);
    };
    run_watched(&args, limit(total), Some(cancel), &tick);
    let results = batch
        .iter()
        .zip(&urls)
        .map(
            |(item, url)| match finish(&tmp.join(file_name(url)), item) {
                Ok(target) => Ok(target),
                Err(_) if cancel.load(Ordering::Relaxed) => Err(Error::Cancelled),
                Err(_) => copy_one(item, cancel, &|_| {}),
            },
        )
        .collect();
    let _ = std::fs::remove_dir_all(&tmp);
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_forgets_what_left_the_walked_phone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("phone-memory.json");
        let first = Remembered::open(Some(&path));
        first.note_file("mtp:/A/S/DCIM/old.jpg", 1, "2026-01");
        first.note_file("mtp:/B/S/DCIM/b.jpg", 2, "2026-02");
        first.note_dir("mtp:/A/S/Cache", true);
        first.save(&[]);

        let second = Remembered::open(Some(&path));
        assert_eq!(
            second.file("mtp:/A/S/DCIM/old.jpg"),
            Some((1, "2026-01".into()))
        );
        assert_eq!(second.dir("mtp:/A/S/Cache"), Some(true));
        second.note_file("mtp:/A/S/DCIM/new.jpg", 3, "2026-03");
        second.save(&[Location::Kio("mtp:/A/S".into())]);

        let third = Remembered::open(Some(&path));
        assert_eq!(third.file("mtp:/A/S/DCIM/old.jpg"), None);
        assert_eq!(third.dir("mtp:/A/S/Cache"), None);
        assert!(third.file("mtp:/A/S/DCIM/new.jpg").is_some());
        // Другой телефон не обходили — о нём помнится прежнее.
        assert!(third.file("mtp:/B/S/DCIM/b.jpg").is_some());

        let off = Remembered::open(None);
        off.note_file("mtp:/A/S/x.jpg", 1, "2026-01");
        off.save(&[]);
    }

    /// Настоящий `kioclient` по адресам `file://` (без KDE тест пропускается): пачка и
    /// одиночная копия, имена с номером при совпадении, время изменения сохраняется.
    #[test]
    fn copies_batches_through_kioclient() {
        if ls("file:///").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let phone = dir.path().join("phone");
        std::fs::create_dir_all(&phone).unwrap();
        let mut items = Vec::new();
        for (i, name) in ["a.jpg", "b #1.jpg", "c.mp4"].iter().enumerate() {
            let path = phone.join(name);
            std::fs::write(&path, vec![i as u8; 1000 * (i + 1)]).unwrap();
            items.push(ImportItem {
                source: Location::Kio(child(&format!("file://{}", phone.display()), name)),
                dest: dir.path().join("out/2026-10").join(name),
                size: 1000 * (i as u64 + 1),
            });
        }
        // Имя уже занято — ляжет рядом с номером.
        std::fs::create_dir_all(dir.path().join("out/2026-10")).unwrap();
        std::fs::write(dir.path().join("out/2026-10/a.jpg"), b"other").unwrap();
        let plan = ImportPlan {
            bytes: 6000,
            items,
            ..ImportPlan::default()
        };
        assert_eq!(batch_len(&plan.items), 3);
        let report = crate::import::run(&plan, &AtomicBool::new(false), |_, _| {});
        assert_eq!(
            (report.copied, report.failed.len()),
            (3, 0),
            "{:?}",
            report.failed
        );
        let out = dir.path().join("out/2026-10");
        assert!(
            report.outputs.contains(&out.join("a_1.jpg")),
            "{:?}",
            report.outputs
        );
        assert_eq!(
            std::fs::read(out.join("b #1.jpg")).unwrap(),
            vec![1u8; 2000]
        );
        assert_eq!(
            std::fs::metadata(out.join("c.mp4"))
                .unwrap()
                .modified()
                .unwrap(),
            std::fs::metadata(phone.join("c.mp4"))
                .unwrap()
                .modified()
                .unwrap()
        );
        // Временных папок не осталось.
        let left: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        // Одиночная копия с прогрессом.
        let tmp = dir.path().join("one.tmp");
        copy(
            match &plan.items[2].source {
                Location::Kio(u) => u,
                Location::Path(_) => unreachable!(),
            },
            &tmp,
            3000,
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        assert_eq!(std::fs::metadata(&tmp).unwrap().len(), 3000);
    }
}
