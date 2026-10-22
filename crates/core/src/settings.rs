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
    /// Искать похожие фото (FR-9.1); по умолчанию — «Похожие». Сами они не отмечаются.
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
            similar: Similarity::Normal,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub convert: ConvertOptions,
}

/// Версия схемы `settings.toml`. Поднимается, когда меняется смысл сохранённого значения
/// (например, умолчание), и тогда в [`Settings::migrate`] добавляется шаг.
pub const SETTINGS_VERSION: u32 = 2;

/// Файлы без поля `version` записаны до появления миграций (выпуск 0.1.0 и сборки до него).
fn legacy_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(default = "legacy_version")]
    pub version: u32,
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
            version: SETTINGS_VERSION,
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
    /// Файл старой схемы обновляется и сразу записывается обратно: после этого значения в нём —
    /// выбор пользователя, и следующая миграция их уже не тронет.
    pub fn load() -> Settings {
        let mut s: Settings = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        if s.migrate() {
            let _ = s.save();
        }
        s
    }

    /// Обновляет настройки старой схемы по шагам. `true` — что-то поменялось.
    pub fn migrate(&mut self) -> bool {
        let from = self.version;
        if self.version < 2 {
            // 1 → 2: похожие фото ищутся по умолчанию. В файлах версии 1 «Off» — прежнее
            // умолчание, а не выбор: в выпуске 0.1.0 этой настройки ещё не было.
            if self.dupes.similar == Similarity::Off {
                self.dupes.similar = Similarity::Normal;
            }
            self.version = 2;
        }
        self.version = self.version.max(SETTINGS_VERSION);
        self.version != from
    }

    /// Всегда пишет текущую версию схемы: даже если интерфейс прислал настройки без неё.
    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).at(dir)?;
        }
        let current = Settings {
            version: SETTINGS_VERSION,
            ..self.clone()
        };
        let text = toml::to_string_pretty(&current).map_err(|e| Error::Invalid(e.to_string()))?;
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
    fn migrates_old_files_once() {
        // Сборка до миграций записала прежнее умолчание «не искать похожие».
        let mut old: Settings = toml::from_str("[dupes]\nsimilar = \"Off\"\n").unwrap();
        assert_eq!(old.version, 1);
        assert!(old.migrate());
        assert_eq!(old.dupes.similar, Similarity::Normal);
        assert_eq!(old.version, SETTINGS_VERSION);
        assert!(!old.migrate(), "повторная миграция ничего не меняет");

        // Файл выпуска 0.1.0 — поля нет вовсе.
        let mut v010: Settings = toml::from_str("theme = \"Dark\"\n").unwrap();
        v010.migrate();
        assert_eq!(v010.dupes.similar, Similarity::Normal);

        // Новый файл: «Off» — выбор пользователя, его не трогаем.
        let mut chosen: Settings = toml::from_str(&format!(
            "version = {SETTINGS_VERSION}\n[dupes]\nsimilar = \"Off\"\n"
        ))
        .unwrap();
        assert!(!chosen.migrate());
        assert_eq!(chosen.dupes.similar, Similarity::Off);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = toml::from_str("theme = \"Dark\"\n").unwrap();
        assert_eq!(s.theme, Theme::Dark);
        assert!(s.prevent_sleep);
        assert_eq!(s.convert.image.quality, 90);
    }
}
