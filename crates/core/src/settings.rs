//! Настройки приложения в `settings.toml` (FR-13.1, FR-13.2).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::dupes::{KeepRule, KindFilter, Similarity};
use crate::error::{Error, IoContext, Result};
use crate::ffmpeg::FfmpegSource;
use crate::governor::LoadLevel;
use crate::media::ImageKind;
use crate::options::ConvertOptions;
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DupesSettings {
    pub min_size: u64,
    pub kinds: KindFilter,
    pub include_hidden: bool,
    pub include_cloud: bool,
    pub keep: KeepRule,
    /// Не искать в путях, содержащих эти строки (FR-7.2).
    pub exclude: Vec<String>,
    /// Искать одно фото в разных форматах (FR-9.2).
    pub find_formats: bool,
    /// Какой формат оставлять у одного фото в разных форматах; по умолчанию JPEG.
    pub keep_format: ImageKind,
    /// Искать похожие фото (FR-9.1); по умолчанию нет — это долго.
    pub similar: Similarity,
}

impl Default for DupesSettings {
    fn default() -> Self {
        Self {
            min_size: 1024,
            kinds: KindFilter::All,
            include_hidden: false,
            include_cloud: false,
            keep: KeepRule::Oldest,
            exclude: Vec::new(),
            find_formats: true,
            keep_format: ImageKind::Jpeg,
            similar: Similarity::Off,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub convert: ConvertOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub convert: ConvertOptions,
    pub load: LoadLevel,
    pub ffmpeg: FfmpegSource,
    pub theme: Theme,
    /// Увеличенный масштаб интерфейса (ТЗ 8.5).
    pub large_text: bool,
    /// Ограничивать нагрузку «Средним» от батареи (FR-12.5).
    pub limit_on_battery: bool,
    /// Не давать системе уснуть во время работы (FR-6.7).
    pub prevent_sleep: bool,
    pub dupes: DupesSettings,
    pub presets: Vec<Preset>,
    /// Обход вложенных папок при добавлении файлов (FR-1.2).
    pub scan_recursive: bool,
    /// Добавлять скрытые файлы (FR-1.2).
    pub scan_hidden: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            convert: ConvertOptions::default(),
            load: LoadLevel::Auto,
            ffmpeg: FfmpegSource::Auto,
            theme: Theme::System,
            large_text: false,
            limit_on_battery: true,
            prevent_sleep: true,
            dupes: DupesSettings::default(),
            presets: Vec::new(),
            scan_recursive: true,
            scan_hidden: false,
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        paths::config_dir().join("settings.toml")
    }

    /// Битый или отсутствующий файл — значения по умолчанию: настройки не должны мешать запуску.
    pub fn load() -> Settings {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).at(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| Error::Invalid(e.to_string()))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).at(&tmp)?;
        std::fs::rename(&tmp, &path).at(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{OutputDir, VideoMode, WebmCodec};

    #[test]
    fn roundtrips_through_toml_with_enum_payloads() {
        let mut s = Settings::default();
        s.convert.output = OutputDir::IntoKeepTree("/out/Фото".into());
        s.convert.video.mode = VideoMode::Webm(WebmCodec::Vp9);
        s.load = LoadLevel::Custom {
            threads: 3,
            low_priority: true,
            hw_sessions: 1,
        };
        s.dupes.keep = KeepRule::PriorityFolders(vec!["/a".into()]);
        s.presets.push(Preset {
            name: "Для Telegram".into(),
            convert: s.convert.clone(),
        });
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.convert.output, s.convert.output);
        assert_eq!(back.convert.video.mode, s.convert.video.mode);
        assert_eq!(back.load, s.load);
        assert_eq!(back.dupes.keep, s.dupes.keep);
        assert_eq!(back.presets.len(), 1);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = toml::from_str("theme = \"Dark\"\n").unwrap();
        assert_eq!(s.theme, Theme::Dark);
        assert!(s.prevent_sleep);
        assert_eq!(s.convert.image.quality, 90);
    }
}
