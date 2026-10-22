//! Автообновление: проверка выпуска на GitHub (`latest.json`), загрузка, установка, перезапуск.
//! Файлы обновлений подписаны ключом проекта; открытый ключ — в `tauri.conf.json`.

use std::sync::Mutex;
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::utils::platform::bundle_type;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::AppState;

/// Найденное обновление ждёт, пока человек нажмёт «Обновить».
#[derive(Default)]
pub struct UpdateState {
    pending: Mutex<Option<Update>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    /// Что нового — раздел выпуска из CHANGELOG.md.
    pub notes: Option<String>,
    /// Как установлена программа: `appimage`, `deb`, `nsis`. `None` — переносная версия:
    /// обновиться сама она не может, ей предлагается страница загрузки.
    pub installer: Option<&'static str>,
}

fn installer() -> Option<&'static str> {
    use tauri::utils::config::BundleType;
    match bundle_type()? {
        BundleType::AppImage => Some("appimage"),
        BundleType::Deb => Some("deb"),
        BundleType::Nsis => Some("nsis"),
        _ => None,
    }
}

/// Ошибка обновления — в журнал на диске: по нему потом разбираются, что пошло не так.
fn logged(app: &AppHandle, what: &str, e: impl std::fmt::Display) -> String {
    let msg = format!("{what}: {e}");
    app.state::<AppState>().log.write("ERROR", &msg);
    msg
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    let update = app
        .updater()
        .map_err(|e| logged(&app, "проверка обновлений", e))?
        .check()
        .await
        .map_err(|e| logged(&app, "проверка обновлений", e))?;
    let Some(update) = update else {
        return Ok(None);
    };
    let info = UpdateInfo {
        version: update.version.clone(),
        notes: update.body.clone(),
        installer: installer(),
    };
    app.state::<AppState>()
        .log
        .write("INFO", &format!("доступна новая версия {}", update.version));
    *app.state::<UpdateState>().pending.lock().unwrap() = Some(update);
    Ok(Some(info))
}

/// Скачивает и ставит обновление, затем перезапускает программу. Прогресс — событием
/// `update:progress` (доля 0–1). На Windows установщик сам закрывает программу.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    if state.busy() {
        return Err("дождитесь окончания работы".into());
    }
    if installer().is_none() {
        return Err("переносная версия не обновляется сама".into());
    }
    let update = app
        .state::<UpdateState>()
        .pending
        .lock()
        .unwrap()
        .take()
        .ok_or("сначала проверьте обновления")?;
    let mut done = 0u64;
    let mut last_percent = u64::MAX;
    let emitter = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                done += chunk as u64;
                if let Some(total) = total.filter(|&t| t > 0) {
                    let percent = done * 100 / total;
                    if percent != last_percent {
                        last_percent = percent;
                        let _ = emitter.emit("update:progress", done as f64 / total as f64);
                    }
                }
            },
            || {},
        )
        .await
        .map_err(|e| logged(&app, "установка обновления", e))?;
    state.log.write(
        "INFO",
        &format!("обновление до {} установлено, перезапуск", update.version),
    );
    // Окно не должно спрашивать «прервать работу?» при перезапуске.
    state.allow_close.store(true, Ordering::Relaxed);
    app.restart()
}
