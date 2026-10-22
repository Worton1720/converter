use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ошибка ввода-вывода ({path}): {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("ffmpeg не найден: {0}")]
    FfmpegNotFound(String),
    #[error("ffmpeg завершился с ошибкой (код {code:?}): {stderr}")]
    Ffmpeg { code: Option<i32>, stderr: String },
    #[error("ffprobe не смог прочитать файл: {0}")]
    Probe(String),
    #[error("ошибка декодирования изображения: {0}")]
    Decode(String),
    #[error("ошибка кодирования изображения: {0}")]
    Encode(String),
    #[error(
        "изображение {width}×{height} слишком большое для {format} (максимум {max} px по стороне)"
    )]
    TooLarge {
        width: u32,
        height: u32,
        format: &'static str,
        max: u32,
    },
    #[error("результат не прошёл проверку: {0}")]
    Verify(String),
    #[error("корзина недоступна: {0}")]
    TrashUnavailable(String),
    #[error("файл изменился после сканирования: {0}")]
    ChangedSinceScan(PathBuf),
    #[error("{0}")]
    Invalid(String),
    #[error("отменено")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

/// Добавляет путь к `std::io::Error`.
pub trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| Error::io(path, e))
    }
}
