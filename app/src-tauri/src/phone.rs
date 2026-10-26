//! Мастер «Перенести фото с телефона»: поиск устройства, план, перенос (`converter_core::import`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use converter_core::import::{self, ImportItem, ImportPlan, Source, SourceKind};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

/// Найденные устройства и последний план — команды ссылаются на них по номеру.
#[derive(Default)]
pub struct PhoneState {
    sources: Mutex<Vec<Source>>,
    plan: Mutex<Option<ImportPlan>>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Сколько превью строится сейчас.
    preview_gate: (Mutex<usize>, std::sync::Condvar),
}

impl PhoneState {
    /// Флаг отмены на время операции; `None` — уже идёт другая.
    fn begin(&self) -> Option<Arc<AtomicBool>> {
        let mut c = self.cancel.lock().unwrap();
        if c.is_some() {
            return None;
        }
        let flag = Arc::new(AtomicBool::new(false));
        *c = Some(Arc::clone(&flag));
        Some(flag)
    }

    fn end(&self) {
        *self.cancel.lock().unwrap() = None;
    }
}

#[derive(Serialize)]
pub struct SourceDto {
    name: String,
    /// `phone` или `card`.
    kind: &'static str,
}

/// Файл в галерее мастера; номер — индекс в плане (по нему превью и выбор).
#[derive(Clone, Serialize)]
pub struct PlanItemDto {
    name: String,
    size: u64,
    /// «ГГГГ-ММ» или «Без даты» — папка, куда он ляжет.
    month: String,
    video: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDto {
    count: usize,
    bytes: u64,
    already: usize,
    /// Сколько папок и файлов телефон не отдал — найдено не всё.
    unread: usize,
    photos: usize,
    videos: usize,
    items: Vec<PlanItemDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportDto {
    copied: usize,
    bytes: u64,
    outputs: Vec<String>,
    failed: Vec<(String, String)>,
    cancelled: bool,
}

/// Ход поиска (`phone:scan`): сколько просмотрено и новое с прошлого события — по порядку
/// номеров в плане.
#[derive(Clone, Serialize)]
struct ScanDto {
    seen: usize,
    items: Vec<PlanItemDto>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    files: usize,
    bytes: u64,
}

fn item_dto(item: &ImportItem) -> PlanItemDto {
    let name = |p: Option<&std::path::Path>| {
        p.and_then(|p| p.file_name())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    PlanItemDto {
        name: name(Some(&item.dest)),
        size: item.size,
        month: name(item.dest.parent()),
        video: is_video(&item.dest),
    }
}

fn is_video(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["mov", "mp4", "m4v", "3gp", "avi", "mkv"].contains(&e.to_ascii_lowercase().as_str())
    })
}

#[tauri::command]
pub async fn phone_sources(app: AppHandle) -> Result<Vec<SourceDto>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let found = import::find_sources();
        let dtos = found
            .iter()
            .map(|s| SourceDto {
                name: s.name.clone(),
                kind: match s.kind {
                    SourceKind::Phone => "phone",
                    SourceKind::Card => "card",
                },
            })
            .collect();
        *app.state::<PhoneState>().sources.lock().unwrap() = found;
        dtos
    })
    .await
    .map_err(|e| e.to_string())
}

/// Что перенести с устройства `index` в `dest`. Найденное по ходу — событием `phone:scan`
/// не чаще 5 раз в секунду (галерея наполняется, превью доступны сразу).
#[tauri::command]
pub async fn phone_plan(index: usize, dest: String, app: AppHandle) -> Result<PlanDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let phone = app.state::<PhoneState>();
        let source = phone
            .sources
            .lock()
            .unwrap()
            .get(index)
            .cloned()
            .ok_or("устройство не найдено — подключите его заново")?;
        let cancel = phone.begin().ok_or("уже идёт перенос")?;
        let remember = app
            .state::<AppState>()
            .settings
            .lock()
            .unwrap()
            .remember_phone;
        let memory = import::memory_path();
        if !remember {
            let _ = std::fs::remove_file(&memory);
        }
        *phone.plan.lock().unwrap() = Some(ImportPlan::default());
        let pending = Mutex::new((Instant::now(), Vec::new()));
        let emit = |seen: usize, items: Vec<PlanItemDto>| {
            let _ = app.emit("phone:scan", ScanDto { seen, items });
        };
        let result = import::plan(
            &source.roots,
            &PathBuf::from(&dest),
            &cancel,
            remember.then_some(memory.as_path()),
            |scan| {
                let mut pending = pending.lock().unwrap();
                if let Some((_, item)) = scan.new {
                    if let Some(plan) = phone.plan.lock().unwrap().as_mut() {
                        plan.items.push(item.clone());
                    }
                    pending.1.push(item_dto(item));
                }
                if pending.0.elapsed() >= Duration::from_millis(200) {
                    pending.0 = Instant::now();
                    emit(scan.seen, std::mem::take(&mut pending.1));
                }
            },
        );
        phone.end();
        let plan = match result {
            Ok(plan) => plan,
            Err(e) => {
                *phone.plan.lock().unwrap() = None;
                return Err(e.to_string());
            }
        };
        let items: Vec<PlanItemDto> = plan.items.iter().map(item_dto).collect();
        let videos = items.iter().filter(|i| i.video).count();
        let dto = PlanDto {
            count: plan.items.len(),
            bytes: plan.bytes,
            already: plan.already,
            unread: plan.unread,
            photos: plan.items.len() - videos,
            videos,
            items,
        };
        *phone.plan.lock().unwrap() = Some(plan);
        Ok(dto)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Превью файла `index` из плана — путь для `thumb://` (`None` — показать значок).
/// Не больше четырёх одновременно: каждое — запуск `kioclient`.
#[tauri::command]
pub async fn phone_preview(index: usize, app: AppHandle) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let phone = app.state::<PhoneState>();
        let (location, size) = {
            let plan = phone.plan.lock().unwrap();
            let item = plan.as_ref()?.items.get(index)?;
            (item.source.clone(), item.size)
        };
        let (lock, cvar) = &phone.preview_gate;
        let mut running = cvar.wait_while(lock.lock().unwrap(), |n| *n >= 4).unwrap();
        *running += 1;
        drop(running);
        let path = import::preview(&location, size);
        *lock.lock().unwrap() -= 1;
        cvar.notify_one();
        path.map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .ok()
    .flatten()
}

/// Перенос выбранных файлов последнего плана (`selected` — номера в нём).
/// Прогресс — `phone:progress` не чаще 10 раз в секунду.
#[tauri::command]
pub async fn phone_import(selected: Vec<usize>, app: AppHandle) -> Result<ReportDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let phone = app.state::<PhoneState>();
        let mut plan = phone
            .plan
            .lock()
            .unwrap()
            .clone()
            .ok_or("сначала найдите, что перенести")?;
        let keep: std::collections::HashSet<usize> = selected.into_iter().collect();
        plan.items = plan
            .items
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep.contains(i))
            .map(|(_, item)| item)
            .collect();
        plan.bytes = plan.items.iter().map(|i| i.size).sum();
        let cancel = phone.begin().ok_or("уже идёт перенос")?;
        let state = app.state::<AppState>();
        state.phone_busy.store(true, Ordering::Relaxed);
        state.update_awake();
        state.log.write(
            "INFO",
            &format!("перенос с телефона: {} файлов", plan.items.len()),
        );
        let last = Mutex::new(Instant::now() - Duration::from_secs(1));
        let emitter = app.clone();
        let report = import::run(&plan, &cancel, |files, bytes| {
            let mut last = last.lock().unwrap();
            if last.elapsed() >= Duration::from_millis(100) {
                *last = Instant::now();
                let _ = emitter.emit("phone:progress", Progress { files, bytes });
            }
        });
        let cancelled = cancel.load(Ordering::Relaxed);
        phone.end();
        state.phone_busy.store(false, Ordering::Relaxed);
        state.update_awake();
        state.log.write(
            "INFO",
            &format!(
                "перенесено с телефона: {}, не получилось: {}{}",
                report.copied,
                report.failed.len(),
                if cancelled {
                    ", остановлено"
                } else {
                    ""
                }
            ),
        );
        for (src, e) in &report.failed {
            state.log.write("WARN", &format!("{}: {e}", src.display()));
        }
        Ok(ReportDto {
            copied: report.copied,
            bytes: report.bytes,
            outputs: report
                .outputs
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            failed: report
                .failed
                .iter()
                .map(|(s, e)| (s.display(), e.clone()))
                .collect(),
            cancelled,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn phone_cancel(app: AppHandle) {
    if let Some(c) = app.state::<PhoneState>().cancel.lock().unwrap().as_ref() {
        c.store(true, Ordering::Relaxed);
    }
}
