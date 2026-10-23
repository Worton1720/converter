//! Автотесты интерфейса (только отладочная сборка): `converter-app --dev-test <сценарий.js>`
//! выполняет сценарий в окне после загрузки страницы. Сценарий сообщает итоги командой
//! `dev_report` (строки в stdout) и завершает программу `dev_exit` с кодом 0 или 1.
//! Запуск — `tools/ui-test.sh`: он изолирует настройки, кэш, журналы и корзину.

use std::io::Write;
use std::sync::OnceLock;

use tauri::AppHandle;

static SCRIPT: OnceLock<Option<String>> = OnceLock::new();

/// Аргументы без пары `--dev-test <файл>`; сценарий запоминается до загрузки окна.
pub fn take_args(args: Vec<String>) -> Vec<String> {
    let Some(i) = args.iter().position(|a| a == "--dev-test") else {
        let _ = SCRIPT.set(None);
        return args;
    };
    let script = if cfg!(debug_assertions) {
        args.get(i + 1)
            .and_then(|p| std::fs::read_to_string(p).ok())
    } else {
        None
    };
    let _ = SCRIPT.set(script);
    args.into_iter()
        .enumerate()
        .filter(|&(j, _)| j != i && j != i + 1)
        .map(|(_, a)| a)
        .collect()
}

/// Идёт автотест: второй экземпляр не должен передавать работу уже открытой программе.
pub fn active() -> bool {
    SCRIPT.get().is_some_and(Option::is_some)
}

/// Код сценария для `eval` (один раз) с данными теста из `CONVERTER_TEST_DATA`.
pub fn script_once() -> Option<String> {
    static SENT: OnceLock<()> = OnceLock::new();
    let script = SCRIPT.get()?.as_ref()?;
    SENT.set(()).ok()?;
    let data = std::env::var("CONVERTER_TEST_DATA").unwrap_or_default();
    Some(format!(
        "window.__TEST_DATA = {};\n{script}",
        serde_json::to_string(&data).unwrap_or_else(|_| "\"\"".into())
    ))
}

#[tauri::command]
pub fn dev_report(line: String) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err("только в отладочной сборке".into());
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "[ui-test] {line}");
    let _ = out.flush();
    Ok(())
}

#[tauri::command]
pub fn dev_exit(code: i32, app: AppHandle) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err("только в отладочной сборке".into());
    }
    app.exit(code);
    Ok(())
}
