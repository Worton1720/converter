//! Поиск ffmpeg/ffprobe (FR-13.3) и запуск с прогрессом и отменой (FR-6.3).

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::governor::Governor;
use crate::sys::{self, Priority};

const EXE: &str = std::env::consts::EXE_SUFFIX;
/// Минимальная версия системного ffmpeg (FR-13.3).
const MIN_SYSTEM_VERSION: (u32, u32) = (6, 1);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FfmpegSource {
    /// Windows — встроенный; Linux — системный, если версия подходит, иначе встроенный.
    #[default]
    Auto,
    Bundled,
    System,
    Custom(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Origin {
    Bundled,
    System,
    Custom,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// Первая строка `ffmpeg -version`.
    pub version: String,
    pub origin: Origin,
}

impl Tools {
    pub fn locate(source: &FfmpegSource) -> Result<Tools> {
        Self::locate_with(source, &[])
    }

    /// `bundled_dirs` — где ещё искать встроенный ffmpeg (например, папка ресурсов приложения);
    /// проверяются раньше папок рядом с программой.
    pub fn locate_with(source: &FfmpegSource, bundled_dirs: &[PathBuf]) -> Result<Tools> {
        let bundled = || {
            bundled_dirs
                .iter()
                .find_map(|d| pair_in(d, Origin::Bundled))
                .or_else(|| bundled_dir().and_then(|d| pair_in(&d, Origin::Bundled)))
        };
        let system = || {
            let ffmpeg = find_in_path(&format!("ffmpeg{EXE}"))?;
            pair_in(ffmpeg.parent()?, Origin::System)
        };
        let found = match source {
            FfmpegSource::Bundled => bundled(),
            FfmpegSource::System => system(),
            FfmpegSource::Custom(p) => {
                let dir = if p.is_dir() {
                    p.as_path()
                } else {
                    p.parent().unwrap_or(p)
                };
                pair_in(dir, Origin::Custom)
            }
            FfmpegSource::Auto if cfg!(windows) => bundled().or_else(system),
            FfmpegSource::Auto => system()
                .filter(|t| parse_version(&t.version).is_some_and(|v| v >= MIN_SYSTEM_VERSION))
                .or_else(bundled)
                .or_else(system),
        };
        found.ok_or_else(|| {
            Error::FfmpegNotFound(
                "положите ffmpeg и ffprobe рядом с программой или установите их в систему".into(),
            )
        })
    }

    pub fn ffmpeg_cmd(&self) -> Command {
        Command::new(&self.ffmpeg)
    }

    /// Ключ для кэша детекта HW: путь, размер и время изменения бинарника + версия.
    pub fn fingerprint(&self) -> String {
        let meta = std::fs::metadata(&self.ffmpeg).ok();
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        format!(
            "{}|{}|{}|{}",
            self.ffmpeg.display(),
            meta.map_or(0, |m| m.len()),
            mtime,
            self.version
        )
    }
}

/// Каталоги, где ищется встроенный ffmpeg: рядом с программой и в `tools/ffmpeg`.
fn bundled_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let mut candidates = vec![
        exe_dir.clone(),
        exe_dir.join("ffmpeg"),
        exe_dir.join("tools").join("ffmpeg"),
    ];
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("tools").join("ffmpeg"));
    }
    candidates.into_iter().find(|d| {
        d.join(format!("ffmpeg{EXE}")).is_file() && d.join(format!("ffprobe{EXE}")).is_file()
    })
}

fn pair_in(dir: &Path, origin: Origin) -> Option<Tools> {
    let ffmpeg = dir.join(format!("ffmpeg{EXE}"));
    let ffprobe = dir.join(format!("ffprobe{EXE}"));
    if !ffmpeg.is_file() || !ffprobe.is_file() {
        return None;
    }
    let version = version_line(&ffmpeg)?;
    Some(Tools {
        ffmpeg,
        ffprobe,
        version,
        origin,
    })
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn version_line(ffmpeg: &Path) -> Option<String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner").arg("-version");
    let out = output(&mut cmd).ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    })
}

/// `ffmpeg version n9.0.2 …` → (9, 0). Сборки из git (`N-…`, `2025-…-git`) считаются свежими.
pub fn parse_version(line: &str) -> Option<(u32, u32)> {
    let token = line.split_whitespace().nth(2)?;
    if token.starts_with("N-") || token.contains("-git") || token.starts_with("git") {
        return Some((u32::MAX, 0));
    }
    let token = token.trim_start_matches('n');
    let mut parts = token.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// Запуск с ожиданием, без всплывающих окон консоли на Windows.
pub fn output(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sys::spawn(cmd, Priority::Normal)?.wait_with_output()
}

/// Прогресс одной операции ffmpeg.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub fraction: f32,
    pub speed: Option<f32>,
}

pub struct RunContext<'a> {
    pub priority: Priority,
    pub governor: Option<&'a Governor>,
    pub cancelled: &'a dyn Fn() -> bool,
    /// Длительность входа для пересчёта `out_time` в долю.
    pub duration: Option<f64>,
}

/// Запускает ffmpeg с `-progress pipe:1`. Возвращает `Err(Cancelled)` при отмене.
pub fn run(
    tools: &Tools,
    args: &[OsString],
    ctx: &RunContext<'_>,
    mut on_progress: impl FnMut(Progress),
) -> Result<()> {
    let mut cmd = tools.ffmpeg_cmd();
    cmd.args([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-nostats",
        "-progress",
        "pipe:1",
    ])
    .args(args)
    // SVT-AV1 пишет свой лог в stderr в обход -loglevel; 1 — только ошибки.
    .env("SVT_LOG", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = sys::spawn(&mut cmd, ctx.priority)
        .map_err(|e| Error::FfmpegNotFound(format!("{}: {e}", tools.ffmpeg.display())))?;
    let pid = child.id();
    if let Some(g) = ctx.governor {
        g.register_child(pid);
    }

    let stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let (line_tx, line_rx) = crossbeam_channel::unbounded::<String>();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let mut cancelled = false;
    let mut speed = None;
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    loop {
        if (ctx.cancelled)() {
            cancelled = true;
            let _ = child.kill();
            break;
        }
        match line_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                match key {
                    "speed" => speed = value.trim().trim_end_matches('x').parse::<f32>().ok(),
                    // Несмотря на имя, out_time_ms тоже в микросекундах.
                    "out_time_us" | "out_time_ms" => {
                        let (Ok(us), Some(total)) = (value.parse::<i64>(), ctx.duration) else {
                            continue;
                        };
                        if total > 0.0 && last_emit.elapsed() >= Duration::from_millis(100) {
                            last_emit = Instant::now();
                            let fraction = (us as f64 / 1e6 / total).clamp(0.0, 1.0) as f32;
                            on_progress(Progress { fraction, speed });
                        }
                    }
                    _ => {}
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    let status = child.wait();
    let _ = reader.join();
    let stderr = err_reader.join().unwrap_or_default();
    if let Some(g) = ctx.governor {
        g.unregister_child(pid);
    }
    if cancelled {
        return Err(Error::Cancelled);
    }
    let status = status.map_err(|e| Error::Ffmpeg {
        code: None,
        stderr: e.to_string(),
    })?;
    if !status.success() {
        let text = String::from_utf8_lossy(&stderr);
        // Хвост stderr: в начале часто идут предупреждения, причина — в конце.
        let lines: Vec<&str> = text.lines().collect();
        return Err(Error::Ffmpeg {
            code: status.code(),
            stderr: lines[lines.len().saturating_sub(12)..].join("\n"),
        });
    }
    on_progress(Progress {
        fraction: 1.0,
        speed,
    });
    Ok(())
}

/// Признак нехватки места в выводе ffmpeg (FR-4.6).
pub fn is_no_space(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("no space left on device") || s.contains("not enough space on the disk")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_lines() {
        assert_eq!(
            parse_version("ffmpeg version n9.0.2 Copyright"),
            Some((9, 0))
        );
        assert_eq!(
            parse_version("ffmpeg version 7.1.1-full_build-www.gyan.dev"),
            Some((7, 1))
        );
        assert_eq!(parse_version("ffmpeg version 6.1 Copyright"), Some((6, 1)));
        assert_eq!(
            parse_version("ffmpeg version N-117000-gabcdef-20250101"),
            Some((u32::MAX, 0))
        );
        assert_eq!(parse_version("garbage"), None);
    }
}
