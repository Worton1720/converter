//! Планирование конвертации видео и сборка аргументов ffmpeg (раздел 4.4 ТЗ).

use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hw::HwCaps;
use crate::options::{
    HwBackend, HwPreference, MetadataPolicy, QualityProfile, VideoMode, VideoOptions, WebmCodec,
};
use crate::probe::{StreamInfo, StreamKind, VideoInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    H264,
    Hevc,
    Hevc10,
    Av1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HwEncoder {
    pub backend: HwBackend,
    pub codec: Codec,
    /// VAAPI: `/dev/dri/renderD*`.
    pub device: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SwEncoder {
    X264,
    X265,
    X265Main10,
    SvtAv1,
    Vp9,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Encoder {
    Hw(HwEncoder),
    Sw(SwEncoder),
}

impl Encoder {
    pub fn name(&self) -> &'static str {
        match self {
            Encoder::Hw(e) => hw_encoder_name(e.backend, e.codec),
            Encoder::Sw(SwEncoder::X264) => "libx264",
            Encoder::Sw(SwEncoder::X265 | SwEncoder::X265Main10) => "libx265",
            Encoder::Sw(SwEncoder::SvtAv1) => "libsvtav1",
            Encoder::Sw(SwEncoder::Vp9) => "libvpx-vp9",
        }
    }

    fn ten_bit(&self) -> bool {
        matches!(
            self,
            Encoder::Sw(SwEncoder::X265Main10)
                | Encoder::Hw(HwEncoder {
                    codec: Codec::Hevc10,
                    ..
                })
        )
    }
}

pub fn hw_encoder_name(backend: HwBackend, codec: Codec) -> &'static str {
    use HwBackend::*;
    match (codec, backend) {
        (Codec::H264, Nvenc) => "h264_nvenc",
        (Codec::H264, Qsv) => "h264_qsv",
        (Codec::H264, Amf) => "h264_amf",
        (Codec::H264, Vaapi) => "h264_vaapi",
        (Codec::Hevc | Codec::Hevc10, Nvenc) => "hevc_nvenc",
        (Codec::Hevc | Codec::Hevc10, Qsv) => "hevc_qsv",
        (Codec::Hevc | Codec::Hevc10, Amf) => "hevc_amf",
        (Codec::Hevc | Codec::Hevc10, Vaapi) => "hevc_vaapi",
        (Codec::Av1, Nvenc) => "av1_nvenc",
        (Codec::Av1, Qsv) => "av1_qsv",
        (Codec::Av1, Amf) => "av1_amf",
        (Codec::Av1, Vaapi) => "av1_vaapi",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum VideoAction {
    Copy,
    Encode { encoder: Encoder, tonemap: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AudioCodec {
    Aac,
    Opus,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AudioAction {
    Copy(usize),
    Encode(usize, AudioCodec),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Container {
    Mp4,
    Webm,
    Mkv,
}

/// Стоимость задачи для планировщика (раздел 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cost {
    /// Перепаковка: 1 токен.
    Light,
    /// HW-кодирование: 1 токен + слот сессии; `cpu_filters` — с тональной компрессией на CPU.
    Hw { cpu_filters: bool },
    /// Программное кодирование: t токенов.
    Cpu,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VideoPlan {
    pub container: Container,
    pub video_index: usize,
    pub video: VideoAction,
    pub audio: Vec<AudioAction>,
    pub subtitles: Vec<usize>,
    /// HDR сохраняется (HEVC Main10 в режиме «Компактно»).
    pub keep_hdr: bool,
    pub warnings: Vec<String>,
}

impl VideoPlan {
    pub fn is_remux(&self) -> bool {
        self.video == VideoAction::Copy
    }

    pub fn cost(&self) -> Cost {
        match &self.video {
            VideoAction::Copy => Cost::Light,
            VideoAction::Encode {
                encoder: Encoder::Hw(_),
                tonemap,
            } => Cost::Hw {
                cpu_filters: *tonemap,
            },
            VideoAction::Encode { .. } => Cost::Cpu,
        }
    }

    pub fn hw_backend(&self) -> Option<HwBackend> {
        match &self.video {
            VideoAction::Encode {
                encoder: Encoder::Hw(e),
                ..
            } => Some(e.backend),
            _ => None,
        }
    }

    pub fn label(&self) -> String {
        let audio_encoded = self
            .audio
            .iter()
            .any(|a| matches!(a, AudioAction::Encode(..)));
        match &self.video {
            VideoAction::Copy if audio_encoded => "перепаковка, аудио перекодировано".into(),
            VideoAction::Copy => "перепаковка без перекодирования".into(),
            VideoAction::Encode { encoder, tonemap } => {
                let hw = match encoder {
                    Encoder::Hw(e) => format!(" ({})", e.backend.name()),
                    Encoder::Sw(_) => String::new(),
                };
                let tm = if *tonemap { ", HDR→SDR" } else { "" };
                format!("{}{hw}{tm}", encoder.name())
            }
        }
    }

    /// Вариант «только GPU» (кадры не покидают видеопамять) — пока только VAAPI,
    /// для 8-битных исходников без поворота и фильтров на CPU.
    pub fn gpu_only_eligible(&self, info: &VideoInfo) -> bool {
        let Some(src) = info.streams.get(self.video_index) else {
            return false;
        };
        matches!(
            &self.video,
            VideoAction::Encode {
                encoder: Encoder::Hw(HwEncoder {
                    backend: HwBackend::Vaapi,
                    ..
                }),
                tonemap: false,
            }
        ) && src.rotation == 0
            && matches!(
                src.pix_fmt.as_deref(),
                Some("yuv420p" | "yuvj420p" | "nv12")
            )
    }
}

fn codec_is(s: &StreamInfo, names: &[&str]) -> bool {
    s.codec.as_deref().is_some_and(|c| names.contains(&c))
}

/// Субтитры, которые MKV принимает без перекодирования.
const MKV_SUBTITLES: &[&str] = &[
    "subrip",
    "ass",
    "ssa",
    "webvtt",
    "hdmv_pgs_subtitle",
    "dvd_subtitle",
];

pub fn plan(
    info: &VideoInfo,
    opts: &VideoOptions,
    caps: &HwCaps,
    force_sw: bool,
) -> Result<VideoPlan> {
    let src = info
        .main_video()
        .ok_or_else(|| Error::Invalid("в файле нет видеопотока".into()))?;
    if !src.decodable() {
        return Err(Error::Invalid(format!(
            "видеокодек «{}» не поддерживается",
            src.codec_tag
        )));
    }
    let mut warnings: Vec<String> = info
        .undecodable_audio()
        .map(|s| {
            format!(
                "аудиодорожка #{} ({}) не поддерживается и пропущена",
                s.index, s.codec_tag
            )
        })
        .collect();

    let hw_or = |codec: Codec, sw: SwEncoder| -> Encoder {
        if force_sw || opts.hw == HwPreference::Off {
            return Encoder::Sw(sw);
        }
        caps.find(codec, opts.hw)
            .cloned()
            .map_or(Encoder::Sw(sw), Encoder::Hw)
    };

    let mut keep_hdr = false;
    let (container, video) = match opts.mode {
        VideoMode::Compat => {
            let ok = codec_is(src, &["h264"])
                && src.bit_depth() == 8
                && matches!(src.pix_fmt.as_deref(), Some("yuv420p" | "yuvj420p"));
            let action = if ok {
                VideoAction::Copy
            } else {
                VideoAction::Encode {
                    encoder: hw_or(Codec::H264, SwEncoder::X264),
                    tonemap: src.is_hdr(),
                }
            };
            (Container::Mp4, action)
        }
        VideoMode::Compact => {
            let action = if codec_is(src, &["hevc"]) {
                keep_hdr = src.is_hdr();
                VideoAction::Copy
            } else if src.bit_depth() > 8 {
                keep_hdr = src.is_hdr();
                VideoAction::Encode {
                    encoder: hw_or(Codec::Hevc10, SwEncoder::X265Main10),
                    tonemap: false,
                }
            } else {
                VideoAction::Encode {
                    encoder: hw_or(Codec::Hevc, SwEncoder::X265),
                    tonemap: false,
                }
            };
            (Container::Mp4, action)
        }
        VideoMode::Webm(codec) => {
            let (names, encoder): (&[&str], _) = match codec {
                WebmCodec::Av1 => (&["av1"], hw_or(Codec::Av1, SwEncoder::SvtAv1)),
                WebmCodec::Vp9 => (&["vp9"], Encoder::Sw(SwEncoder::Vp9)),
            };
            let action = if codec_is(src, names) {
                VideoAction::Copy
            } else {
                VideoAction::Encode {
                    encoder,
                    tonemap: src.is_hdr(),
                }
            };
            (Container::Webm, action)
        }
        VideoMode::Mkv => (Container::Mkv, VideoAction::Copy),
    };

    let audio = match container {
        Container::Mkv => info
            .audio_streams()
            .filter(|s| s.decodable())
            .map(|s| AudioAction::Copy(s.index))
            .collect(),
        Container::Mp4 => {
            let copyable: &[&str] = if opts.mode == VideoMode::Compat {
                &["aac", "mp3"]
            } else {
                &["aac", "mp3", "alac", "ac3", "eac3"]
            };
            info.first_decodable_audio()
                .map(|a| {
                    if codec_is(a, copyable) {
                        AudioAction::Copy(a.index)
                    } else {
                        AudioAction::Encode(a.index, AudioCodec::Aac)
                    }
                })
                .into_iter()
                .collect()
        }
        Container::Webm => info
            .first_decodable_audio()
            .map(|a| {
                if codec_is(a, &["opus", "vorbis"]) {
                    AudioAction::Copy(a.index)
                } else {
                    AudioAction::Encode(a.index, AudioCodec::Opus)
                }
            })
            .into_iter()
            .collect(),
    };

    let subtitles = match container {
        Container::Mkv => info
            .streams
            .iter()
            .filter(|s| s.kind == StreamKind::Subtitle && codec_is(s, MKV_SUBTITLES))
            .map(|s| s.index)
            .collect(),
        _ => Vec::new(),
    };
    if container != Container::Mkv && info.streams.iter().any(|s| s.kind == StreamKind::Subtitle) {
        warnings.push("субтитры не переносятся в этот контейнер".into());
    }

    Ok(VideoPlan {
        container,
        video_index: src.index,
        video,
        audio,
        subtitles,
        keep_hdr,
        warnings,
    })
}

/// Результат совпал бы с исходником (раздел 4.2): тот же контейнер, только копирование.
pub fn is_noop(plan: &VideoPlan, info: &VideoInfo, metadata: MetadataPolicy) -> bool {
    let same_container = match plan.container {
        // У MOV и MP4 одинаковый format_name — различает только major_brand.
        Container::Mp4 => {
            info.is_container("mp4")
                && info
                    .tags
                    .get("major_brand")
                    .is_some_and(|b| b.trim() != "qt")
        }
        Container::Webm => info.is_container("webm"),
        Container::Mkv => info.is_container("matroska") && !info.is_container("webm"),
    };
    same_container
        && plan.is_remux()
        && plan.audio.iter().all(|a| matches!(a, AudioAction::Copy(_)))
        && metadata == MetadataPolicy::Keep
}

/// Параметры качества для кодировщика (FR-3.2). Таблица — в одном месте, покрыта тестом.
pub fn quality_args(encoder: &Encoder, q: QualityProfile, threads: usize) -> Vec<String> {
    let pick = |v: [&str; 3]| -> String {
        match q {
            QualityProfile::Fast => v[0],
            QualityProfile::Balanced => v[1],
            QualityProfile::High => v[2],
        }
        .to_string()
    };
    let s = |x: &str| x.to_string();
    let t = threads.max(1).to_string();
    match encoder {
        Encoder::Sw(SwEncoder::X264) => vec![
            s("-preset"),
            pick(["veryfast", "medium", "slow"]),
            s("-crf"),
            pick(["26", "23", "19"]),
            s("-threads"),
            t,
        ],
        Encoder::Sw(SwEncoder::X265 | SwEncoder::X265Main10) => vec![
            s("-preset"),
            pick(["veryfast", "fast", "medium"]),
            s("-crf"),
            pick(["28", "26", "22"]),
            s("-x265-params"),
            format!("pools={t}:log-level=error"),
        ],
        Encoder::Sw(SwEncoder::SvtAv1) => vec![
            s("-preset"),
            pick(["9", "7", "5"]),
            s("-crf"),
            pick(["38", "32", "26"]),
            s("-svtav1-params"),
            format!("lp={t}"),
        ],
        Encoder::Sw(SwEncoder::Vp9) => vec![
            s("-b:v"),
            s("0"),
            s("-crf"),
            pick(["36", "32", "28"]),
            s("-deadline"),
            s("good"),
            s("-cpu-used"),
            pick(["5", "3", "2"]),
            s("-row-mt"),
            s("1"),
            s("-threads"),
            t,
        ],
        Encoder::Hw(e) => {
            let (h264, hevc, av1) = match e.backend {
                HwBackend::Nvenc => (["28", "24", "20"], ["30", "26", "22"], ["36", "32", "28"]),
                // AV1 у VAAPI и AMF задаётся q-index 0–255.
                HwBackend::Vaapi | HwBackend::Amf => {
                    (["27", "24", "20"], ["29", "26", "22"], ["150", "120", "90"])
                }
                HwBackend::Qsv => (["27", "24", "20"], ["29", "26", "22"], ["32", "28", "24"]),
            };
            let qp = pick(match e.codec {
                Codec::H264 => h264,
                Codec::Hevc | Codec::Hevc10 => hevc,
                Codec::Av1 => av1,
            });
            let mut v = match e.backend {
                HwBackend::Vaapi => vec![s("-rc_mode"), s("CQP"), s("-qp"), qp],
                HwBackend::Qsv => vec![
                    s("-preset"),
                    pick(["veryfast", "medium", "slower"]),
                    s("-global_quality"),
                    qp,
                ],
                HwBackend::Nvenc => vec![
                    s("-preset"),
                    pick(["p3", "p5", "p7"]),
                    s("-tune"),
                    s("hq"),
                    s("-rc"),
                    s("vbr"),
                    s("-cq"),
                    qp,
                    s("-b:v"),
                    s("0"),
                ],
                HwBackend::Amf => vec![
                    s("-quality"),
                    pick(["speed", "balanced", "quality"]),
                    s("-rc"),
                    s("cqp"),
                    s("-qp_i"),
                    qp.clone(),
                    s("-qp_p"),
                    qp,
                ],
            };
            if e.codec == Codec::Hevc10 {
                v.extend([s("-profile:v"), s("main10")]);
            }
            v
        }
    }
}

/// Аргументы инициализации устройства и аппаратного декодирования.
/// `gpu_only` — кадры остаются в видеопамяти (только VAAPI).
pub fn hw_input_args(e: &HwEncoder, gpu_only: bool) -> Vec<String> {
    let s = |x: &str| x.to_string();
    match e.backend {
        HwBackend::Vaapi => {
            let dev = e.device.as_deref().unwrap_or("/dev/dri/renderD128");
            let mut v = vec![
                s("-init_hw_device"),
                format!("vaapi=va:{dev}"),
                s("-filter_hw_device"),
                s("va"),
                s("-hwaccel"),
                s("vaapi"),
                s("-hwaccel_device"),
                s("va"),
            ];
            if gpu_only {
                v.extend([s("-hwaccel_output_format"), s("vaapi")]);
            }
            v
        }
        // Без -hwaccel_output_format кадры сами копируются в память; если GPU не умеет
        // декодировать исходный кодек, ffmpeg переходит на программное декодирование.
        HwBackend::Nvenc => vec![s("-hwaccel"), s("cuda")],
        HwBackend::Qsv | HwBackend::Amf if cfg!(windows) => vec![s("-hwaccel"), s("d3d11va")],
        HwBackend::Qsv | HwBackend::Amf => vec![s("-hwaccel"), s("vaapi")],
    }
}

const TONEMAP: &str = "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
                       tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv";

/// Хвост цепочки фильтров: формат пикселей и загрузка в видеопамять.
pub fn filter_tail(encoder: &Encoder, gpu_only: bool) -> String {
    let ten = encoder.ten_bit();
    match encoder {
        Encoder::Hw(HwEncoder {
            backend: HwBackend::Vaapi,
            ..
        }) => {
            if gpu_only {
                "format=nv12|vaapi,hwupload".into()
            } else if ten {
                "format=p010le,hwupload".into()
            } else {
                "format=nv12,hwupload".into()
            }
        }
        Encoder::Hw(_) => if ten { "format=p010le" } else { "format=nv12" }.into(),
        Encoder::Sw(_) => if ten {
            "format=yuv420p10le"
        } else {
            "format=yuv420p"
        }
        .into(),
    }
}

pub struct ArgsInput<'a> {
    pub plan: &'a VideoPlan,
    pub info: &'a VideoInfo,
    pub opts: &'a VideoOptions,
    pub metadata: MetadataPolicy,
    pub threads: usize,
    pub gpu_only: bool,
}

pub fn build_args(input: &ArgsInput<'_>, source: &Path, output: &Path) -> Vec<OsString> {
    let ArgsInput {
        plan,
        info,
        opts,
        metadata,
        threads,
        gpu_only,
    } = *input;
    let hw = match &plan.video {
        VideoAction::Encode {
            encoder: Encoder::Hw(e),
            ..
        } => Some(e),
        _ => None,
    };
    let decode_threads = if hw.is_some() { 2 } else { threads.max(1) };
    let mut input_opts: Vec<String> = Vec::new();
    if let Some(e) = hw {
        input_opts.extend(hw_input_args(e, gpu_only));
    }
    if !plan.is_remux() {
        input_opts.extend(["-threads".into(), decode_threads.to_string()]);
    }
    let mut a: Vec<OsString> = input_opts.into_iter().map(OsString::from).collect();
    a.push("-i".into());
    a.push(source.as_os_str().to_owned());

    let mut push = |items: Vec<String>| a.extend(items.into_iter().map(OsString::from));
    let s = |x: &str| x.to_string();

    push(vec![s("-map"), format!("0:{}", plan.video_index)]);
    for au in &plan.audio {
        let idx = match au {
            AudioAction::Copy(i) | AudioAction::Encode(i, _) => *i,
        };
        push(vec![s("-map"), format!("0:{idx}")]);
    }
    for sub in &plan.subtitles {
        push(vec![s("-map"), format!("0:{sub}")]);
    }

    let src = info.streams.get(plan.video_index);
    match &plan.video {
        VideoAction::Copy => push(vec![s("-c:v"), s("copy")]),
        VideoAction::Encode { encoder, tonemap } => {
            // Цветовые свойства задаются кадрам через setparams: выходные флаги
            // -color_trc/-color_primaries libx265 в ffmpeg 9 игнорирует.
            let color: Option<[String; 3]> = if *tonemap {
                Some(["bt709".into(), "bt709".into(), "bt709".into()])
            } else if plan.keep_hdr {
                src.and_then(|s| {
                    Some([
                        s.color_primaries.clone()?,
                        s.color_transfer.clone()?,
                        s.color_space.clone()?,
                    ])
                })
            } else {
                None
            };
            let mut chain = Vec::new();
            if *tonemap {
                chain.push(TONEMAP.to_string());
            }
            if let Some([p, t, c]) = &color {
                chain.push(format!(
                    "setparams=color_primaries={p}:color_trc={t}:colorspace={c}"
                ));
            }
            chain.push(filter_tail(encoder, gpu_only));
            push(vec![
                s("-filter_threads"),
                decode_threads.to_string(),
                s("-vf"),
                chain.join(","),
                s("-c:v"),
                s(encoder.name()),
            ]);
            push(quality_args(encoder, opts.quality, threads));
            // Флаги — для атома colr в контейнере.
            if let Some([p, t, c]) = color {
                push(vec![
                    s("-color_primaries"),
                    p,
                    s("-color_trc"),
                    t,
                    s("-colorspace"),
                    c,
                ]);
            }
            push(vec![s("-fps_mode"), s("passthrough")]);
        }
    }

    for (audio_out, au) in plan.audio.iter().enumerate() {
        match au {
            AudioAction::Copy(_) => push(vec![format!("-c:a:{audio_out}"), s("copy")]),
            AudioAction::Encode(_, codec) => {
                let (name, kbps) = match codec {
                    AudioCodec::Aac => ("aac", opts.audio_bitrate_kbps),
                    AudioCodec::Opus => ("libopus", 128),
                };
                push(vec![
                    format!("-c:a:{audio_out}"),
                    s(name),
                    format!("-b:a:{audio_out}"),
                    format!("{kbps}k"),
                ]);
            }
        }
    }
    if !plan.subtitles.is_empty() {
        push(vec![s("-c:s"), s("copy")]);
    }

    let hevc_out = match &plan.video {
        VideoAction::Copy => src.is_some_and(|s| codec_is(s, &["hevc"])),
        VideoAction::Encode { encoder, .. } => {
            encoder.name().contains("265") || encoder.name().starts_with("hevc")
        }
    };
    if plan.container == Container::Mp4 && hevc_out {
        push(vec![s("-tag:v"), s("hvc1")]);
    }

    match metadata {
        MetadataPolicy::Keep => push(vec![s("-map_metadata"), s("0")]),
        MetadataPolicy::StripGps => {
            push(vec![s("-map_metadata"), s("0")]);
            for key in [
                "location",
                "location-eng",
                "com.apple.quicktime.location.ISO6709",
            ] {
                push(vec![s("-metadata"), format!("{key}=")]);
            }
        }
        MetadataPolicy::StripAll => push(vec![s("-map_metadata"), s("-1")]),
    }

    match plan.container {
        Container::Mp4 => push(vec![
            s("-movflags"),
            s("+faststart+use_metadata_tags"),
            s("-f"),
            s("mp4"),
        ]),
        Container::Webm => push(vec![s("-f"), s("webm")]),
        Container::Mkv => push(vec![s("-f"), s("matroska")]),
    }
    a.push(output.as_os_str().to_owned());
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::parse;

    fn info(codec: &str, pix: &str, trc: Option<&str>, audio: &str, format: &str) -> VideoInfo {
        let trc = trc.map_or(String::new(), |t| {
            format!(
                r#","color_transfer":"{t}","color_primaries":"bt2020","color_space":"bt2020nc""#
            )
        });
        parse(&format!(
            r#"{{"streams":[
              {{"index":0,"codec_type":"video","codec_name":"{codec}","pix_fmt":"{pix}","width":1920,"height":1080{trc}}},
              {{"index":1,"codec_type":"audio","codec_name":"{audio}"}},
              {{"index":2,"codec_type":"data","codec_tag_string":"mebx"}}],
              "format":{{"format_name":"{format}","duration":"10"}}}}"#
        ))
        .unwrap()
    }

    fn opts(mode: VideoMode) -> VideoOptions {
        VideoOptions {
            mode,
            ..VideoOptions::default()
        }
    }

    fn vaapi() -> HwCaps {
        HwCaps {
            encoders: [Codec::H264, Codec::Hevc, Codec::Hevc10]
                .into_iter()
                .map(|codec| HwEncoder {
                    backend: HwBackend::Vaapi,
                    codec,
                    device: Some("/dev/dri/renderD128".into()),
                })
                .collect(),
            max_sessions: 2,
        }
    }

    fn args_str(plan: &VideoPlan, info: &VideoInfo, o: &VideoOptions, gpu_only: bool) -> String {
        build_args(
            &ArgsInput {
                plan,
                info,
                opts: o,
                metadata: MetadataPolicy::StripGps,
                threads: 4,
                gpu_only,
            },
            Path::new("in.mov"),
            Path::new("out.mp4"),
        )
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
    }

    #[test]
    fn compat_hevc_sdr_uses_hw_and_drops_data_track() {
        let i = info("hevc", "yuv420p", None, "aac", "mov,mp4,m4a,3gp,3g2,mj2");
        let o = opts(VideoMode::Compat);
        let p = plan(&i, &o, &vaapi(), false).unwrap();
        assert_eq!(p.cost(), Cost::Hw { cpu_filters: false });
        assert_eq!(p.audio, vec![AudioAction::Copy(1)]);
        assert!(p.gpu_only_eligible(&i));
        let a = args_str(&p, &i, &o, true);
        assert!(a.contains("-hwaccel_output_format vaapi"), "{a}");
        assert!(a.contains("format=nv12|vaapi,hwupload"), "{a}");
        assert!(a.contains("-c:v h264_vaapi -rc_mode CQP -qp 24"), "{a}");
        assert!(a.contains("-map 0:0 -map 0:1 "), "{a}");
        assert!(
            !a.contains("0:2"),
            "дорожка mebx не должна попадать в MP4: {a}"
        );
        assert!(a.contains("-metadata location="), "{a}");
        assert!(
            a.ends_with("-movflags +faststart+use_metadata_tags -f mp4 out.mp4"),
            "{a}"
        );
    }

    #[test]
    fn compat_hdr_is_tonemapped_on_cpu_then_uploaded() {
        let i = info(
            "hevc",
            "yuv420p10le",
            Some("arib-std-b67"),
            "aac",
            "mov,mp4",
        );
        let o = opts(VideoMode::Compat);
        let p = plan(&i, &o, &vaapi(), false).unwrap();
        assert_eq!(p.cost(), Cost::Hw { cpu_filters: true });
        assert!(!p.gpu_only_eligible(&i));
        let a = args_str(&p, &i, &o, false);
        assert!(a.contains("tonemap=tonemap=hable"), "{a}");
        assert!(a.contains(",format=nv12,hwupload"), "{a}");
        assert!(a.contains("-color_trc bt709"), "{a}");
    }

    #[test]
    fn compat_h264_is_remuxed_and_mp4_source_is_noop() {
        let i = info("h264", "yuv420p", None, "aac", "mov,mp4,m4a,3gp,3g2,mj2");
        let p = plan(&i, &opts(VideoMode::Compat), &HwCaps::default(), false).unwrap();
        assert!(p.is_remux());
        assert_eq!(p.cost(), Cost::Light);
        assert!(
            !is_noop(&p, &i, MetadataPolicy::Keep),
            "MOV → MP4 не пропускается"
        );
    }

    #[test]
    fn compact_keeps_hdr_with_x265_main10_without_hw() {
        let i = info(
            "h264",
            "yuv420p10le",
            Some("smpte2084"),
            "pcm_s16le",
            "mov,mp4",
        );
        let o = opts(VideoMode::Compact);
        let p = plan(&i, &o, &HwCaps::default(), false).unwrap();
        assert!(p.keep_hdr);
        assert_eq!(p.audio, vec![AudioAction::Encode(1, AudioCodec::Aac)]);
        let a = args_str(&p, &i, &o, false);
        assert!(a.contains("-c:v libx265"), "{a}");
        assert!(a.contains("format=yuv420p10le"), "{a}");
        assert!(a.contains("-color_trc smpte2084"), "{a}");
        assert!(a.contains("-tag:v hvc1"), "{a}");
        assert!(a.contains("pools=4"), "{a}");
    }

    #[test]
    fn force_sw_ignores_hw_caps() {
        let i = info("hevc", "yuv420p", None, "aac", "mov");
        let p = plan(&i, &opts(VideoMode::Compat), &vaapi(), true).unwrap();
        assert_eq!(p.cost(), Cost::Cpu);
    }

    #[test]
    fn webm_encodes_audio_to_opus_and_mkv_copies() {
        let i = info("hevc", "yuv420p", None, "aac", "mov");
        let p = plan(
            &i,
            &opts(VideoMode::Webm(WebmCodec::Av1)),
            &HwCaps::default(),
            false,
        )
        .unwrap();
        assert_eq!(p.audio, vec![AudioAction::Encode(1, AudioCodec::Opus)]);
        assert_eq!(p.container, Container::Webm);
        let p = plan(&i, &opts(VideoMode::Mkv), &HwCaps::default(), false).unwrap();
        assert!(p.is_remux());
    }

    #[test]
    fn quality_table_covers_every_encoder_and_profile() {
        let encoders = [
            Encoder::Sw(SwEncoder::X264),
            Encoder::Sw(SwEncoder::X265),
            Encoder::Sw(SwEncoder::SvtAv1),
            Encoder::Sw(SwEncoder::Vp9),
        ]
        .into_iter()
        .chain(
            [
                HwBackend::Nvenc,
                HwBackend::Qsv,
                HwBackend::Amf,
                HwBackend::Vaapi,
            ]
            .into_iter()
            .flat_map(|backend| {
                [Codec::H264, Codec::Hevc, Codec::Hevc10, Codec::Av1]
                    .into_iter()
                    .map(move |codec| {
                        Encoder::Hw(HwEncoder {
                            backend,
                            codec,
                            device: None,
                        })
                    })
            }),
        );
        for e in encoders {
            let all: Vec<_> = [
                QualityProfile::Fast,
                QualityProfile::Balanced,
                QualityProfile::High,
            ]
            .into_iter()
            .map(|q| quality_args(&e, q, 2))
            .collect();
            assert!(
                all.iter().all(|a| !a.is_empty() && a.len() % 2 == 0),
                "{e:?}"
            );
            assert_ne!(all[0], all[2], "профили должны различаться: {e:?}");
        }
    }
}
