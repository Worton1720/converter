//! Ядро Converter: конвертация фото и видео, поиск дубликатов, управление нагрузкой.
//!
//! Ядро не зависит от интерфейса: CLI и GUI получают события через канал
//! [`events::Event`] и управляют запуском через [`engine::RunHandle`].

pub mod dupes;
pub mod engine;
pub mod error;
pub mod events;
pub mod exif;
pub mod ffmpeg;
pub mod fs_ops;
pub mod governor;
pub mod hw;
pub mod logfile;
pub mod media;
pub mod options;
pub mod paths;
pub mod photo;
pub mod plan;
pub mod pool;
pub mod probe;
pub mod scan;
pub mod settings;
pub mod sys;
pub mod thumb;
pub mod video;

pub use error::{Error, Result};
