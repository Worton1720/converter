//! Tauri-оболочка над `converter-core`: команды для интерфейса и события прогресса.

mod devtest;
mod dto;
mod update;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use converter_core::dupes::{self, DupAction, DupesOptions, DupesReport, KeepRule};
use converter_core::engine::{self, Engine, RunHandle, SpaceCheck};
use converter_core::events::{Event, JobOutcome};
use converter_core::ffmpeg::Tools;
use converter_core::fs_ops;
use converter_core::governor::{Governor, LoadLevel};
use converter_core::history::{self, Change, Fate, History};
use converter_core::hw::{self, HwCaps};
use converter_core::logfile::LogFile;
use converter_core::media::{self, ImageKind};
use converter_core::options::ConvertOptions;
use converter_core::scan::{self, MediaItem, ScanOptions};
use converter_core::settings::Settings;
use converter_core::sys::{self, Priority};
use converter_core::thumb;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;

use dto::{AppInfo, DupesResult, FfmpegInfo, ItemDto};

const THUMB_SIZE: u32 = 320;
const THUMB_PARALLEL: usize = 3;

struct Core {
    tools: Arc<Tools>,
    caps: Arc<HwCaps>,
}

struct ActiveRun {
    handle: RunHandle,
    /// Путь → id задачи для отмены отдельных файлов.
    ids: Vec<PathBuf>,
}

impl AppState {
    /// Изменить запись истории и сохранить файл (не чаще, чем просит вызывающий).
    fn with_history(&self, id: u64, save: bool, f: impl FnOnce(&mut history::Entry)) {
        let mut h = self.history.lock().unwrap();
        if let Some(e) = h.get_mut(id) {
            f(e);
        }
        if save {
            h.save();
        }
    }
}

#[derive(Default)]
struct DupesState {
    report: Option<DupesReport>,
    cancel: Option<Arc<AtomicBool>>,
}

struct AppState {
    governor: Arc<Governor>,
    log: LogFile,
    core: Mutex<Option<Core>>,
    ffmpeg_error: Mutex<Option<String>>,
    items: Mutex<Vec<MediaItem>>,
    run: Mutex<Option<ActiveRun>>,
    dupes: Mutex<DupesState>,
    settings: Mutex<Settings>,
    /// История действий — для «Истории», «Вернуть как было» и продолжения после сбоя.
    history: Mutex<History>,
    awake: Mutex<Option<keepawake::KeepAwake>>,
    allow_close: AtomicBool,
    startup_paths: Mutex<Vec<String>>,
    /// Папки встроенного ffmpeg: ресурсы приложения (известны только после старта Tauri).
    ffmpeg_dirs: Mutex<Vec<PathBuf>>,
    thumb_gate: (Mutex<usize>, Condvar),
}

impl AppState {
    fn busy(&self) -> bool {
        self.run.lock().unwrap().is_some() || self.dupes.lock().unwrap().cancel.is_some()
    }

    fn engine(&self) -> Result<Engine, String> {
        let core = self.core.lock().unwrap();
        let core = core.as_ref().ok_or_else(|| {
            self.ffmpeg_error
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| "ffmpeg не найден".into())
        })?;
        Ok(Engine::new(
            Arc::clone(&core.tools),
            Arc::clone(&core.caps),
            Arc::clone(&self.governor),
        ))
    }

    fn tools(&self) -> Option<Arc<Tools>> {
        self.core
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| Arc::clone(&c.tools))
    }

    /// Запрет сна на время работы (FR-6.7).
    fn update_awake(&self) {
        let wanted = self.settings.lock().unwrap().prevent_sleep && self.busy();
        let mut awake = self.awake.lock().unwrap();
        if wanted && awake.is_none() {
            *awake = keepawake::Builder::default()
                .idle(true)
                .sleep(true)
                .reason("Идёт конвертация файлов")
                .app_name("Converter")
                .app_reverse_domain("com.worton.converter")
                .create()
                .ok();
        } else if !wanted {
            *awake = None;
        }
    }
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Ищет ffmpeg и аппаратные кодировщики. Без ffmpeg работает только поиск дубликатов.
fn init_core(state: &AppState, force_detect: bool) {
    let source = state.settings.lock().unwrap().ffmpeg.clone();
    let dirs = state.ffmpeg_dirs.lock().unwrap().clone();
    match Tools::locate_with(&source, &dirs) {
        Ok(tools) => {
            let caps = hw::detect(&tools, force_detect);
            state.log.write(
                "INFO",
                &format!(
                    "ffmpeg: {} ({:?}), {}; видеокарта: {}",
                    tools.ffmpeg.display(),
                    tools.origin,
                    tools.version,
                    if caps.encoders.is_empty() {
                        "нет".to_string()
                    } else {
                        caps.backends()
                            .iter()
                            .map(|b| b.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ),
            );
            state.governor.set_hw_max(caps.max_sessions);
            *state.core.lock().unwrap() = Some(Core {
                tools: Arc::new(tools),
                caps: Arc::new(caps),
            });
            *state.ffmpeg_error.lock().unwrap() = None;
        }
        Err(e) => {
            state.log.write("ERROR", &format!("ffmpeg не найден: {e}"));
            *state.core.lock().unwrap() = None;
            *state.ffmpeg_error.lock().unwrap() = Some(e.to_string());
        }
    }
}

fn app_info_of(state: &AppState) -> AppInfo {
    let core = state.core.lock().unwrap();
    AppInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        threads: state.governor.threads_total(),
        ffmpeg: core.as_ref().map(|c| FfmpegInfo {
            path: c.tools.ffmpeg.to_string_lossy().into_owned(),
            version: c.tools.version.clone(),
            origin: format!("{:?}", c.tools.origin),
        }),
        ffmpeg_error: state.ffmpeg_error.lock().unwrap().clone(),
        hw: core
            .as_ref()
            .map(|c| c.caps.encoders.iter().map(dto::hw_encoder).collect())
            .unwrap_or_default(),
        max_sessions: core.as_ref().map_or(0, |c| c.caps.max_sessions),
        settings: state.settings.lock().unwrap().clone(),
        settings_path: Settings::path().to_string_lossy().into_owned(),
        log_path: state.log.path().to_string_lossy().into_owned(),
        startup_paths: std::mem::take(&mut *state.startup_paths.lock().unwrap()),
        status: state.governor.last_status(),
    }
}

#[tauri::command]
fn app_info(state: State<'_, AppState>) -> AppInfo {
    app_info_of(&state)
}

#[tauri::command]
async fn refresh_hw(app: AppHandle) -> CmdResult<AppInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        if state.run.lock().unwrap().is_some() {
            return Err("нельзя перепроверять видеокарту во время конвертации".to_string());
        }
        init_core(&state, true);
        Ok(app_info_of(&state))
    })
    .await
    .map_err(err)?
}

#[tauri::command]
fn save_settings(settings: Settings, state: State<'_, AppState>) -> CmdResult<()> {
    let ffmpeg_changed = state.settings.lock().unwrap().ffmpeg != settings.ffmpeg;
    state
        .governor
        .set_limit_on_battery(settings.limit_on_battery);
    if state.governor.level() != settings.load {
        state.governor.set_level(settings.load);
    }
    settings.save().map_err(err)?;
    *state.settings.lock().unwrap() = settings;
    if ffmpeg_changed && state.run.lock().unwrap().is_none() {
        init_core(&state, false);
    }
    state.update_awake();
    Ok(())
}

#[tauri::command]
fn set_load(level: LoadLevel, state: State<'_, AppState>) -> CmdResult<()> {
    state.governor.set_level(level);
    let mut settings = state.settings.lock().unwrap();
    settings.load = level;
    settings.save().map_err(err)
}

// ---------- список файлов ----------

#[tauri::command]
async fn add_paths(paths: Vec<String>, app: AppHandle) -> CmdResult<usize> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let inputs: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let cancel = AtomicBool::new(false);
        let opts = {
            let s = state.settings.lock().unwrap();
            ScanOptions {
                recursive: s.scan_recursive,
                include_hidden: s.scan_hidden,
            }
        };
        let mut found = scan::scan(&inputs, &opts, &cancel);
        {
            let items = state.items.lock().unwrap();
            let known: HashSet<&Path> = items.iter().map(|i| i.path.as_path()).collect();
            found.retain(|i| !known.contains(i.path.as_path()));
        }
        if found.is_empty() {
            return Ok(0);
        }
        let added = found.len();
        state.items.lock().unwrap().extend(found.iter().cloned());
        let _ = app.emit(
            "items:added",
            found.iter().map(ItemDto::from).collect::<Vec<_>>(),
        );

        // Пробы и размеры — порциями, чтобы список заполнялся по ходу.
        let threads = state.governor.threads_total().max(2);
        if let Some(tools) = state.tools() {
            for chunk in found.chunks_mut(64) {
                scan::inspect(&tools, chunk, threads, &cancel);
                let mut items = state.items.lock().unwrap();
                for updated in chunk.iter() {
                    if let Some(slot) = items.iter_mut().find(|i| i.path == updated.path) {
                        *slot = updated.clone();
                    }
                }
                // Live Photo размечается по всему списку: пара могла прийти из разных добавлений.
                scan::mark_live_photos(&mut items);
                let dtos: Vec<ItemDto> = items.iter().map(ItemDto::from).collect();
                drop(items);
                let _ = app.emit("items:updated", dtos);
            }
        }
        Ok(added)
    })
    .await
    .map_err(err)?
}

/// Текущий список — после перезагрузки интерфейса он восстанавливается из ядра.
#[tauri::command]
fn list_items(state: State<'_, AppState>) -> Vec<ItemDto> {
    state
        .items
        .lock()
        .unwrap()
        .iter()
        .map(ItemDto::from)
        .collect()
}

#[tauri::command]
fn remove_items(paths: Vec<String>, state: State<'_, AppState>) {
    let remove: HashSet<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    state
        .items
        .lock()
        .unwrap()
        .retain(|i| !remove.contains(&i.path));
}

#[tauri::command]
fn clear_items(state: State<'_, AppState>) {
    state.items.lock().unwrap().clear();
}

fn selected_items(state: &AppState, paths: &[String]) -> Vec<MediaItem> {
    let wanted: HashSet<&Path> = paths.iter().map(Path::new).collect();
    state
        .items
        .lock()
        .unwrap()
        .iter()
        .filter(|i| wanted.contains(i.path.as_path()))
        .cloned()
        .collect()
}

#[tauri::command]
fn space_check(
    options: ConvertOptions,
    paths: Vec<String>,
    state: State<'_, AppState>,
) -> Vec<SpaceCheck> {
    engine::space_check(&selected_items(&state, &paths), &options)
}

#[tauri::command]
fn space_estimate(
    options: ConvertOptions,
    paths: Vec<String>,
    state: State<'_, AppState>,
) -> engine::SpaceEstimate {
    engine::space_estimate(&selected_items(&state, &paths), &options)
}

// ---------- конвертация ----------

#[tauri::command]
fn start_convert(
    options: ConvertOptions,
    paths: Vec<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    let mut run = state.run.lock().unwrap();
    if run.is_some() {
        return Err("конвертация уже идёт".into());
    }
    let engine = state.engine()?;
    let items = selected_items(&state, &paths);
    if items.is_empty() {
        return Err("нет файлов для конвертации".into());
    }
    let ids = items.iter().map(|i| i.path.clone()).collect();
    {
        let mut settings = state.settings.lock().unwrap();
        settings.convert = options.clone();
        let _ = settings.save();
    }
    let entry = history::Entry::convert(
        options.clone(),
        items.iter().map(|i| i.path.clone()).collect(),
    );
    let history_id = entry.id;
    {
        let mut h = state.history.lock().unwrap();
        h.push(entry);
        h.save();
    }
    let handle = engine.convert(items, options);
    let events = handle.events.clone();
    *run = Some(ActiveRun { handle, ids });
    drop(run);
    state.update_awake();
    std::thread::spawn(move || forward_events(app, events, history_id));
    Ok(())
}

/// Пачки событий раз в 100 мс: интерфейс не захлёбывается на тысячах файлов.
/// История: результат файла. Сохраняется не чаще раза в 2 с — при сбое теряется не больше.
fn record_finished(
    state: &AppState,
    history_id: u64,
    source: &Path,
    outcome: &JobOutcome,
    save: bool,
) {
    state.with_history(history_id, save, |e| match outcome {
        JobOutcome::Done {
            output,
            source: action,
            ..
        } => e.file_done(
            source,
            Some(Change {
                source: source.to_path_buf(),
                output: Some(output.clone()),
                fate: Fate::from(action),
            }),
            false,
            false,
        ),
        JobOutcome::Skipped { .. } => e.file_done(source, None, true, false),
        JobOutcome::Failed { .. } => e.file_done(source, None, false, true),
        // Отменённые остаются в списке «не сделано».
        JobOutcome::Cancelled => {}
    });
}

fn forward_events(app: AppHandle, events: crossbeam_channel::Receiver<Event>, history_id: u64) {
    let state = app.state::<AppState>();
    let log = &state.log;
    let mut batch: Vec<Event> = Vec::new();
    let mut last = Instant::now();
    let mut last_save = Instant::now();
    let mut summary = None;
    loop {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(e) => {
                log.event(&e);
                if let Event::JobFinished {
                    source, outcome, ..
                } = &e
                {
                    let save = last_save.elapsed() >= Duration::from_secs(2);
                    if save {
                        last_save = Instant::now();
                    }
                    record_finished(&state, history_id, source, outcome, save);
                }
                if let Event::RunFinished(s) = &e {
                    state.with_history(history_id, true, history::Entry::finish);
                    summary = Some(s.clone());
                }
                batch.push(e);
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
        if !batch.is_empty() && (last.elapsed() >= Duration::from_millis(100) || summary.is_some())
        {
            let _ = app.emit("convert:events", std::mem::take(&mut batch));
            last = Instant::now();
        }
        if summary.is_some() {
            break;
        }
    }
    if !batch.is_empty() {
        let _ = app.emit("convert:events", batch);
    }
    if let Some(run) = state.run.lock().unwrap().take() {
        run.handle.join();
    }
    state.update_awake();

    // Уведомление, если окно не в фокусе (FR-6.6).
    let focused = app
        .get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(true);
    if let (Some(s), false) = (summary, focused) {
        let mut body = format!("Готово: {}", s.done);
        if s.failed > 0 {
            body.push_str(&format!(", ошибок: {}", s.failed));
        }
        if s.skipped > 0 {
            body.push_str(&format!(", пропущено: {}", s.skipped));
        }
        let _ = app
            .notification()
            .builder()
            .title("Конвертация завершена")
            .body(body)
            .show();
    }
}

#[tauri::command]
fn pause_convert(paused: bool, state: State<'_, AppState>) {
    if let Some(run) = state.run.lock().unwrap().as_ref() {
        if paused {
            run.handle.pause();
        } else {
            run.handle.resume();
        }
    }
}

#[tauri::command]
fn cancel_convert(state: State<'_, AppState>) {
    if let Some(run) = state.run.lock().unwrap().as_ref() {
        run.handle.cancel();
    }
}

#[tauri::command]
fn cancel_item(path: String, state: State<'_, AppState>) {
    if let Some(run) = state.run.lock().unwrap().as_ref()
        && let Some(pos) = run.ids.iter().position(|p| p == Path::new(&path))
    {
        run.handle.cancel_job(pos as u64 + 1);
    }
}

// ---------- дубликаты ----------

#[tauri::command]
async fn dupes_find(options: DupesOptions, app: AppHandle) -> CmdResult<DupesResult> {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let state = app.state::<AppState>();
        let mut d = state.dupes.lock().unwrap();
        if d.cancel.is_some() {
            return Err("поиск уже идёт".into());
        }
        d.cancel = Some(Arc::clone(&cancel));
        let mut settings = state.settings.lock().unwrap();
        settings.dupes.min_size = options.min_size;
        settings.dupes.kinds = options.kinds.clone();
        settings.dupes.include_hidden = options.include_hidden;
        settings.dupes.include_cloud = options.include_cloud;
        settings.dupes.find_formats = options.find_formats;
        settings.dupes.similar = options.similar;
        let _ = settings.save();
    }
    app.state::<AppState>().update_awake();
    let worker = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = worker.state::<AppState>();
        let last = Mutex::new(Instant::now() - Duration::from_secs(1));
        let report = dupes::find(&options, &state.governor, &cancel, |p| {
            let mut last = last.lock().unwrap();
            if last.elapsed() >= Duration::from_millis(100) {
                *last = Instant::now();
                let _ = worker.emit("dupes:progress", p);
            }
        });
        let (keep, keep_format) = {
            let s = state.settings.lock().unwrap();
            (s.dupes.keep.clone(), s.dupes.keep_format)
        };
        let result = report.map(|r| {
            let dto = DupesResult::new(&r, &keep, keep_format);
            state.dupes.lock().unwrap().report = Some(r);
            dto
        });
        state.dupes.lock().unwrap().cancel = None;
        result
    })
    .await
    .map_err(err)?;
    app.state::<AppState>().update_awake();
    result.map_err(err)
}

#[tauri::command]
fn dupes_cancel(state: State<'_, AppState>) {
    if let Some(c) = &state.dupes.lock().unwrap().cancel {
        c.store(true, Ordering::Relaxed);
    }
}

/// Новые отметки по правилу «что оставить»; `keep_format` — для одного фото в разных форматах.
#[tauri::command]
fn dupes_auto(
    rule: KeepRule,
    keep_format: ImageKind,
    state: State<'_, AppState>,
) -> CmdResult<Vec<Vec<bool>>> {
    {
        let mut settings = state.settings.lock().unwrap();
        settings.dupes.keep = rule.clone();
        settings.dupes.keep_format = keep_format;
        let _ = settings.save();
    }
    let d = state.dupes.lock().unwrap();
    let report = d.report.as_ref().ok_or("сначала найдите дубликаты")?;
    Ok(report
        .groups
        .iter()
        .map(|g| dupes::auto_select(g, &rule, keep_format))
        .collect())
}

#[tauri::command]
async fn dupes_apply(
    marks: Vec<Vec<bool>>,
    action: DupAction,
    app: AppHandle,
) -> CmdResult<Vec<dupes::ActionResult>> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut d = state.dupes.lock().unwrap();
        let report = d.report.as_mut().ok_or("сначала найдите дубликаты")?;
        let results = dupes::apply(&report.groups, &marks, &action).map_err(err)?;
        record_dupes(&state, &report.groups, &results, &action);
        // Убираем из отчёта обработанные файлы и группы, где остался один файл.
        let done: HashSet<&Path> = results
            .iter()
            .filter(|r| r.error.is_none())
            .map(|r| r.path.as_path())
            .collect();
        for g in report.groups.iter_mut() {
            g.files.retain(|f| !done.contains(f.path.as_path()));
        }
        report.groups.retain(|g| g.files.len() > 1);
        Ok(results)
    })
    .await
    .map_err(err)?
}

/// История: что стало с убранными копиями (в корзину, удалены, куда перемещены).
fn record_dupes(
    state: &AppState,
    groups: &[dupes::DupGroup],
    results: &[dupes::ActionResult],
    action: &DupAction,
) {
    let ok: Vec<&dupes::ActionResult> = results.iter().filter(|r| r.error.is_none()).collect();
    if ok.is_empty() {
        return;
    }
    let done: HashSet<&Path> = ok.iter().map(|r| r.path.as_path()).collect();
    let freed = groups
        .iter()
        .flat_map(|g| &g.files)
        .filter(|f| done.contains(f.path.as_path()))
        .map(|f| f.size)
        .sum();
    let changes = ok
        .iter()
        .map(|r| Change {
            source: r.path.clone(),
            output: None,
            fate: match action {
                DupAction::Trash => Fate::Trashed,
                DupAction::Delete => Fate::Deleted,
                DupAction::MoveTo(_) => Fate::MovedTo(r.moved_to.clone().unwrap_or_default()),
            },
        })
        .collect();
    let now = history::now_ms();
    let mut h = state.history.lock().unwrap();
    h.push(history::Entry {
        id: now,
        finished_ms: Some(now),
        action: history::Action::Dupes { freed },
        changes,
        undone: false,
    });
    h.save();
}

#[tauri::command]
fn history_list(state: State<'_, AppState>) -> Vec<dto::HistoryItem> {
    let h = state.history.lock().unwrap();
    h.entries.iter().rev().map(dto::HistoryItem::new).collect()
}

#[tauri::command]
fn dupes_report(state: State<'_, AppState>) -> Option<DupesResult> {
    let d = state.dupes.lock().unwrap();
    let s = state.settings.lock().unwrap();
    d.report
        .as_ref()
        .map(|r| DupesResult::new(r, &s.dupes.keep, s.dupes.keep_format))
}

#[tauri::command]
fn dupes_export_csv(
    path: String,
    marks: Vec<Vec<bool>>,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    let d = state.dupes.lock().unwrap();
    let report = d.report.as_ref().ok_or("сначала найдите дубликаты")?;
    std::fs::write(path, dupes::to_csv(&report.groups, Some(&marks))).map_err(err)
}

#[tauri::command]
fn clear_thumb_cache() -> u64 {
    thumb::clear_cache()
}

#[tauri::command]
fn write_text_file(path: String, contents: String) -> CmdResult<()> {
    std::fs::write(path, contents).map_err(err)
}

/// Выход из диалога «Отменить и выйти» (FR-6.8): даём ядру убрать временные файлы.
#[tauri::command]
async fn quit(app: AppHandle) {
    let state = app.state::<AppState>();
    state.allow_close.store(true, Ordering::Relaxed);
    if let Some(run) = state.run.lock().unwrap().as_ref() {
        run.handle.cancel();
    }
    if let Some(c) = &state.dupes.lock().unwrap().cancel {
        c.store(true, Ordering::Relaxed);
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    while state.run.lock().unwrap().is_some() && Instant::now() < deadline {
        tauri::async_runtime::spawn_blocking(|| std::thread::sleep(Duration::from_millis(50)))
            .await
            .ok();
    }
    app.exit(0);
}

// ---------- миниатюры ----------

fn thumb_response(
    app: &AppHandle,
    uri_path: &str,
    query: Option<&str>,
) -> tauri::http::Response<Vec<u8>> {
    let not_found = || {
        tauri::http::Response::builder()
            .status(404)
            .header("Access-Control-Allow-Origin", "*")
            .body(Vec::new())
            .expect("корректный ответ")
    };
    let decoded = percent_encoding::percent_decode_str(uri_path.trim_start_matches('/'))
        .decode_utf8_lossy()
        .into_owned();
    let path = PathBuf::from(decoded);
    let Some(kind) = media::detect(&path) else {
        return not_found();
    };
    let state = app.state::<AppState>();
    let Some(tools) = state.tools() else {
        return not_found();
    };
    // Не больше нескольких миниатюр одновременно: они не должны отнимать CPU у конвертации.
    let (lock, cv) = &state.thumb_gate;
    let mut active = lock.lock().unwrap();
    while *active >= THUMB_PARALLEL {
        active = cv.wait(active).unwrap();
    }
    *active += 1;
    drop(active);
    sys::set_current_thread_priority(Priority::BelowNormal);
    // `?s=1024` — крупное превью для просмотра; по умолчанию миниатюра для списка.
    let size = query
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("s=")))
        .and_then(|v| v.parse::<u32>().ok())
        .map_or(THUMB_SIZE, |s| s.clamp(64, 2048));
    let result = thumb::thumbnail(&tools, &path, kind, size)
        .and_then(|p| std::fs::read(&p).map_err(|e| converter_core::Error::io(p, e)));
    *lock.lock().unwrap() -= 1;
    cv.notify_one();
    match result {
        // Страница и протокол — разные источники (в dev — http://localhost:1420), WebKit требует CORS.
        Ok(bytes) => tauri::http::Response::builder()
            .header("Access-Control-Allow-Origin", "*")
            .header("Content-Type", "image/jpeg")
            .header("Cache-Control", "max-age=86400")
            .body(bytes)
            .expect("корректный ответ"),
        Err(_) => not_found(),
    }
}

fn existing_paths(args: impl IntoIterator<Item = String>) -> Vec<String> {
    args.into_iter().filter(|a| Path::new(a).exists()).collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let args = devtest::take_args(std::env::args().skip(1).collect());
    let removed = fs_ops::cleanup_stale_temps();
    let settings = Settings::load();
    let governor = Governor::start(settings.load, 0);
    governor.set_limit_on_battery(settings.limit_on_battery);
    let state = AppState {
        governor,
        log: LogFile::open_default(),
        core: Mutex::new(None),
        ffmpeg_error: Mutex::new(None),
        items: Mutex::new(Vec::new()),
        run: Mutex::new(None),
        dupes: Mutex::new(DupesState::default()),
        settings: Mutex::new(settings),
        history: Mutex::new(History::load()),
        awake: Mutex::new(None),
        allow_close: AtomicBool::new(false),
        startup_paths: Mutex::new(existing_paths(args)),
        ffmpeg_dirs: Mutex::new(Vec::new()),
        thumb_gate: (Mutex::new(0), Condvar::new()),
    };
    if removed > 0 {
        state.log.write(
            "INFO",
            &format!("удалено временных файлов после прошлого сбоя: {removed}"),
        );
    }
    state.log.write(
        "INFO",
        &format!("запуск Converter {}", env!("CARGO_PKG_VERSION")),
    );

    let builder = tauri::Builder::default();
    // В автотесте — свой экземпляр, даже если программа уже открыта у пользователя.
    let builder = if devtest::active() {
        builder
    } else {
        builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Только для отладки: `converter-app --dev-eval "<js>"` выполняет код в окне,
            // чтобы проверять интерфейс без ручных кликов.
            #[cfg(debug_assertions)]
            if argv.get(1).map(String::as_str) == Some("--dev-eval") {
                if let (Some(js), Some(w)) = (argv.get(2), app.get_webview_window("main")) {
                    let _ = w.eval(js);
                }
                return;
            }
            let paths = existing_paths(argv.into_iter().skip(1));
            if !paths.is_empty() {
                let _ = app.emit("open-paths", paths);
            }
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
    };
    builder
        // Размер, положение и «развёрнуто» окна между запусками (FR-13.5).
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state)
        .manage(update::UpdateState::default())
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished
                && let Some(script) = devtest::script_once()
            {
                let _ = webview.eval(&script);
            }
        })
        .setup(|app| {
            // Встроенный ffmpeg лежит в ресурсах приложения (AppImage, установщик Windows).
            let state = app.state::<AppState>();
            if let Ok(dir) = app.path().resource_dir() {
                state.ffmpeg_dirs.lock().unwrap().push(dir.join("ffmpeg"));
            }
            init_core(&state, false);

            #[cfg(debug_assertions)]
            if std::env::var_os("CONVERTER_DEVTOOLS").is_some()
                && let Some(w) = app.get_webview_window("main")
            {
                w.open_devtools();
            }
            // Статус регулятора раз в секунду — для индикатора нагрузки.
            let handle = app.handle().clone();
            let status = app.state::<AppState>().governor.subscribe();
            std::thread::spawn(move || {
                for s in status.iter() {
                    if handle.emit("governor", s).is_err() {
                        break;
                    }
                }
            });
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("thumb", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let path = request.uri().path().to_string();
            let query = request.uri().query().map(str::to_string);
            std::thread::spawn(move || {
                responder.respond(thumb_response(&app, &path, query.as_deref()))
            });
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if state.busy() && !state.allow_close.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.emit("close-requested", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            refresh_hw,
            save_settings,
            set_load,
            add_paths,
            list_items,
            remove_items,
            clear_items,
            space_check,
            space_estimate,
            start_convert,
            pause_convert,
            cancel_convert,
            cancel_item,
            dupes_find,
            dupes_cancel,
            dupes_auto,
            dupes_apply,
            dupes_report,
            dupes_export_csv,
            clear_thumb_cache,
            write_text_file,
            quit,
            history_list,
            update::check_update,
            update::install_update,
            devtest::dev_report,
            devtest::dev_exit,
        ])
        .run(tauri::generate_context!())
        .expect("ошибка запуска приложения");
}
