//! Мастер «Перенести фото с телефона»: поиск устройства, план, перенос (`converter_core::import`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use converter_core::import::{self, ImportPlan, Source, SourceKind};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

/// Найденные устройства и последний план — команды ссылаются на них по номеру.
#[derive(Default)]
pub struct PhoneState {
    sources: Mutex<Vec<Source>>,
    plan: Mutex<Option<ImportPlan>>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDto {
    count: usize,
    bytes: u64,
    already: usize,
    photos: usize,
    videos: usize,
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    files: usize,
    bytes: u64,
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

/// Что перенести с устройства `index` в `dest`. Прогресс просмотра — событием `phone:scan`.
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
        let emitter = app.clone();
        let result = import::plan(&source.roots, &PathBuf::from(&dest), &cancel, |n| {
            let _ = emitter.emit("phone:scan", n);
        });
        phone.end();
        let plan = result.map_err(|e| e.to_string())?;
        let videos = plan.items.iter().filter(|i| is_video(&i.dest)).count();
        let dto = PlanDto {
            count: plan.items.len(),
            bytes: plan.bytes,
            already: plan.already,
            photos: plan.items.len() - videos,
            videos,
        };
        *phone.plan.lock().unwrap() = Some(plan);
        Ok(dto)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Перенос по последнему плану. Прогресс — `phone:progress` не чаще 10 раз в секунду.
#[tauri::command]
pub async fn phone_import(app: AppHandle) -> Result<ReportDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let phone = app.state::<PhoneState>();
        let plan = phone
            .plan
            .lock()
            .unwrap()
            .take()
            .ok_or("сначала найдите, что перенести")?;
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
