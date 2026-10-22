//! Чтение свойств видео через ffprobe (FR-1.3, FR-1.4, FR-3.3–3.5).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ffmpeg::{self, Tools};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: usize,
    pub kind: StreamKind,
    /// `None` — ffprobe не знает кодек (например, Apple APAC): поток не декодируется.
    pub codec: Option<String>,
    pub codec_tag: String,
    pub profile: Option<String>,
    pub width: u32,
    pub height: u32,
    pub pix_fmt: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub color_space: Option<String>,
    /// Поворот из display matrix, градусы.
    pub rotation: i32,
    /// Обложка (attached_pic), а не настоящее видео.
    pub attached_pic: bool,
}

impl StreamInfo {
    pub fn decodable(&self) -> bool {
        self.codec
            .as_deref()
            .is_some_and(|c| !matches!(c, "none" | "unknown" | ""))
    }

    pub fn bit_depth(&self) -> u8 {
        let pf = self.pix_fmt.as_deref().unwrap_or("");
        if pf.contains("12") {
            12
        } else if pf.contains("10") {
            10
        } else {
            8
        }
    }

    /// HLG или PQ (FR-3.3).
    pub fn is_hdr(&self) -> bool {
        matches!(
            self.color_transfer.as_deref(),
            Some("arib-std-b67" | "smpte2084")
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub format_name: String,
    pub duration: Option<f64>,
    pub size: u64,
    pub streams: Vec<StreamInfo>,
    pub tags: HashMap<String, String>,
}

impl VideoInfo {
    pub fn main_video(&self) -> Option<&StreamInfo> {
        self.streams
            .iter()
            .find(|s| s.kind == StreamKind::Video && !s.attached_pic)
    }

    pub fn first_decodable_audio(&self) -> Option<&StreamInfo> {
        self.audio_streams().find(|s| s.decodable())
    }

    pub fn audio_streams(&self) -> impl Iterator<Item = &StreamInfo> {
        self.streams.iter().filter(|s| s.kind == StreamKind::Audio)
    }

    /// Аудиодорожки, которые придётся пропустить (FR-3.5).
    pub fn undecodable_audio(&self) -> impl Iterator<Item = &StreamInfo> {
        self.audio_streams().filter(|s| !s.decodable())
    }

    pub fn is_container(&self, name: &str) -> bool {
        self.format_name.split(',').any(|f| f == name)
    }

    /// Дата съёмки для `{date}` в виде `YYYY:MM:DD HH:MM:SS`.
    pub fn creation_date(&self) -> Option<String> {
        let raw = self
            .tags
            .get("com.apple.quicktime.creationdate")
            .or_else(|| self.tags.get("creation_time"))?;
        // 2024-05-17T10:20:30+0300 / 2024-05-17T10:20:30.000000Z
        let s = raw.get(..19)?;
        Some(s.replace('-', ":").replacen('T', " ", 1))
    }
}

#[derive(Deserialize)]
struct RawProbe {
    #[serde(default)]
    streams: Vec<RawStream>,
    format: Option<RawFormat>,
}

#[derive(Deserialize)]
struct RawFormat {
    #[serde(default)]
    format_name: String,
    duration: Option<String>,
    size: Option<String>,
    #[serde(default)]
    tags: HashMap<String, String>,
}

#[derive(Deserialize)]
struct RawStream {
    index: usize,
    codec_type: Option<String>,
    codec_name: Option<String>,
    #[serde(default)]
    codec_tag_string: String,
    profile: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    color_space: Option<String>,
    #[serde(default)]
    disposition: HashMap<String, i64>,
    #[serde(default)]
    side_data_list: Vec<serde_json::Value>,
    #[serde(default)]
    tags: HashMap<String, String>,
}

pub fn probe(tools: &Tools, path: &Path) -> Result<VideoInfo> {
    let mut cmd = std::process::Command::new(&tools.ffprobe);
    cmd.args([
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ])
    .arg(path);
    let out = ffmpeg::output(&mut cmd).map_err(|e| Error::Probe(e.to_string()))?;
    if !out.status.success() {
        return Err(Error::Probe(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    parse(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse(json: &str) -> Result<VideoInfo> {
    let raw: RawProbe = serde_json::from_str(json).map_err(|e| Error::Probe(e.to_string()))?;
    let format = raw
        .format
        .ok_or_else(|| Error::Probe("нет секции format".into()))?;
    let streams = raw
        .streams
        .into_iter()
        .map(|s| {
            let rotation = s
                .side_data_list
                .iter()
                .find_map(|d| d.get("rotation").and_then(|r| r.as_f64()))
                .map(|r| r.round() as i32)
                .or_else(|| s.tags.get("rotate").and_then(|r| r.parse().ok()))
                .unwrap_or(0);
            StreamInfo {
                index: s.index,
                kind: match s.codec_type.as_deref() {
                    Some("video") => StreamKind::Video,
                    Some("audio") => StreamKind::Audio,
                    Some("subtitle") => StreamKind::Subtitle,
                    Some("data") => StreamKind::Data,
                    Some("attachment") => StreamKind::Attachment,
                    _ => StreamKind::Other,
                },
                codec: s.codec_name,
                codec_tag: s.codec_tag_string,
                profile: s.profile,
                width: s.width.unwrap_or(0),
                height: s.height.unwrap_or(0),
                pix_fmt: s.pix_fmt,
                color_transfer: s.color_transfer,
                color_primaries: s.color_primaries,
                color_space: s.color_space,
                rotation,
                attached_pic: s.disposition.get("attached_pic").copied().unwrap_or(0) == 1,
            }
        })
        .collect();
    Ok(VideoInfo {
        format_name: format.format_name,
        duration: format.duration.and_then(|d| d.parse().ok()),
        size: format.size.and_then(|s| s.parse().ok()).unwrap_or(0),
        streams,
        tags: format.tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IPHONE: &str = r#"{
      "streams": [
        {"index":0,"codec_name":"hevc","codec_type":"video","codec_tag_string":"hvc1","profile":"Main 10",
         "width":1920,"height":1080,"pix_fmt":"yuv420p10le","color_transfer":"arib-std-b67",
         "color_primaries":"bt2020","color_space":"bt2020nc",
         "disposition":{"attached_pic":0},
         "side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}]},
        {"index":1,"codec_type":"audio","codec_tag_string":"apac"},
        {"index":2,"codec_name":"aac","codec_type":"audio","codec_tag_string":"mp4a"},
        {"index":3,"codec_type":"data","codec_tag_string":"mebx"}
      ],
      "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"12.5","size":"1000",
        "tags":{"com.apple.quicktime.creationdate":"2024-05-17T10:20:30+0300"}}
    }"#;

    #[test]
    fn parses_iphone_hdr_video() {
        let info = parse(IPHONE).unwrap();
        let v = info.main_video().unwrap();
        assert!(v.is_hdr());
        assert_eq!(v.bit_depth(), 10);
        assert_eq!(v.rotation, -90);
        assert_eq!(info.first_decodable_audio().unwrap().index, 2);
        assert_eq!(info.undecodable_audio().count(), 1);
        assert!(info.is_container("mov"));
        assert_eq!(info.duration, Some(12.5));
        assert_eq!(info.creation_date().as_deref(), Some("2024:05:17 10:20:30"));
    }
}
