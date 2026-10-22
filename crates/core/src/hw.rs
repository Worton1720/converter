//! Детект аппаратных кодировщиков пробным кодированием (FR-3.7) с кэшем на диске.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ffmpeg::{self, Tools};
use crate::options::{HwBackend, HwPreference, QualityProfile};
use crate::paths;
use crate::sys::{self, Priority};
use crate::video::{self, Codec, Encoder, HwEncoder};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HwCaps {
    /// Рабочие кодировщики в порядке предпочтения.
    pub encoders: Vec<HwEncoder>,
    pub max_sessions: usize,
}

impl HwCaps {
    pub fn find(&self, codec: Codec, pref: HwPreference) -> Option<&HwEncoder> {
        self.encoders.iter().find(|e| {
            e.codec == codec
                && match pref {
                    HwPreference::Auto => true,
                    HwPreference::Off => false,
                    HwPreference::Only(b) => e.backend == b,
                }
        })
    }

    pub fn backends(&self) -> Vec<HwBackend> {
        let mut seen = Vec::new();
        for e in &self.encoders {
            if !seen.contains(&e.backend) {
                seen.push(e.backend);
            }
        }
        seen
    }
}

fn backend_order() -> &'static [HwBackend] {
    if cfg!(windows) {
        &[HwBackend::Nvenc, HwBackend::Qsv, HwBackend::Amf]
    } else {
        &[HwBackend::Vaapi, HwBackend::Nvenc, HwBackend::Qsv]
    }
}

const CODECS: [Codec; 4] = [Codec::H264, Codec::Hevc, Codec::Hevc10, Codec::Av1];

#[derive(Serialize, Deserialize)]
struct CacheFile {
    fingerprint: String,
    caps: HwCaps,
}

fn cache_path() -> PathBuf {
    paths::cache_dir().join("hw-detect.json")
}

/// Возвращает закэшированный результат, если ffmpeg не менялся; иначе детектит заново.
pub fn detect(tools: &Tools, force: bool) -> HwCaps {
    let fingerprint = format!("{}|{}", tools.fingerprint(), drivers_fingerprint());
    if !force
        && let Some(cached) = std::fs::read(cache_path())
            .ok()
            .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
            .filter(|c| c.fingerprint == fingerprint)
    {
        return cached.caps;
    }
    let caps = detect_uncached(tools);
    let file = CacheFile {
        fingerprint,
        caps: caps.clone(),
    };
    if let Ok(json) = serde_json::to_vec_pretty(&file) {
        let path = cache_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, json);
    }
    caps
}

/// Признак смены видеодрайвера. На Linux — версии модулей ядра; на Windows
/// версия драйвера без WMI не читается, поэтому полагаемся на «Перепроверить» в настройках.
fn drivers_fingerprint() -> String {
    if cfg!(target_os = "linux") {
        ["i915", "xe", "amdgpu", "nvidia"]
            .iter()
            .filter_map(|m| {
                let v = std::fs::read_to_string(format!("/sys/module/{m}/srcversion")).ok()?;
                Some(format!("{m}={}", v.trim()))
            })
            .collect::<Vec<_>>()
            .join(",")
    } else {
        String::new()
    }
}

pub fn detect_uncached(tools: &Tools) -> HwCaps {
    let available = available_encoders(tools);
    let mut candidates = Vec::new();
    for &backend in backend_order() {
        let devices = match backend {
            HwBackend::Vaapi => vaapi_devices().into_iter().map(Some).collect(),
            _ => vec![None],
        };
        for device in devices {
            for codec in CODECS {
                if available.contains(video::hw_encoder_name(backend, codec)) {
                    candidates.push(HwEncoder {
                        backend,
                        codec,
                        device: device.clone(),
                    });
                }
            }
        }
    }

    let handles: Vec<_> = candidates
        .into_iter()
        .map(|e| {
            let tools = tools.clone();
            thread::spawn(move || test_encoder(&tools, &e).then_some(e))
        })
        .collect();
    let working: Vec<HwEncoder> = handles
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect();

    // Для VAAPI на каждый кодек оставляем одно устройство: дискретное, если оно есть.
    let mut encoders: Vec<HwEncoder> = Vec::new();
    let mut taken: HashSet<(HwBackend, Codec)> = HashSet::new();
    let mut ordered = working;
    ordered.sort_by_key(|e| {
        (
            backend_order().iter().position(|b| *b == e.backend),
            e.device.as_deref().map(is_integrated).unwrap_or(false),
        )
    });
    for e in ordered {
        if taken.insert((e.backend, e.codec)) {
            encoders.push(e);
        }
    }
    let max_sessions = if encoders.iter().any(|e| e.backend == HwBackend::Nvenc) {
        3
    } else if encoders.is_empty() {
        0
    } else {
        2
    };
    HwCaps {
        encoders,
        max_sessions,
    }
}

fn available_encoders(tools: &Tools) -> HashSet<String> {
    let mut cmd = tools.ffmpeg_cmd();
    cmd.args(["-hide_banner", "-encoders"]);
    let Ok(out) = ffmpeg::output(&mut cmd) else {
        return HashSet::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
        .collect()
}

fn vaapi_devices() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/dev/dri") else {
        return Vec::new();
    };
    let mut v: Vec<String> = dir
        .flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.contains("renderD"))
        .collect();
    v.sort();
    v
}

/// Встроенная графика Intel (vendor 0x8086) уступает дискретной.
fn is_integrated(device: &str) -> bool {
    let name = device.rsplit('/').next().unwrap_or_default();
    std::fs::read_to_string(format!("/sys/class/drm/{name}/device/vendor"))
        .is_ok_and(|v| v.trim() == "0x8086")
}

/// Кодирует 10 кадров тестового сигнала; успех — кодировщик рабочий.
fn test_encoder(tools: &Tools, e: &HwEncoder) -> bool {
    let encoder = Encoder::Hw(e.clone());
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |v: Vec<String>| args.extend(v.into_iter().map(OsString::from));
    let s = |x: &str| x.to_string();
    push(vec![s("-hide_banner"), s("-loglevel"), s("error")]);
    if e.backend == HwBackend::Vaapi {
        let dev = e.device.as_deref().unwrap_or("/dev/dri/renderD128");
        push(vec![
            s("-init_hw_device"),
            format!("vaapi=va:{dev}"),
            s("-filter_hw_device"),
            s("va"),
        ]);
    }
    push(vec![
        s("-f"),
        s("lavfi"),
        s("-i"),
        s("testsrc2=size=640x360:rate=30"),
        s("-frames:v"),
        s("10"),
        s("-vf"),
        video::filter_tail(&encoder, false),
        s("-c:v"),
        s(encoder.name()),
    ]);
    push(video::quality_args(&encoder, QualityProfile::Balanced, 1));
    push(vec![s("-f"), s("null"), s("-")]);

    let mut cmd = tools.ffmpeg_cmd();
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = sys::spawn(&mut cmd, Priority::Normal) else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}
