//! События ядра для интерфейса (раздел 9 ТЗ).

use std::path::PathBuf;

use serde::Serialize;

use crate::governor::GovernorStatus;

pub type JobId = u64;

#[derive(Debug, Clone, Serialize)]
pub enum SourceAction {
    Kept,
    Trashed,
    Deleted,
    /// Удалить не удалось (например, нет корзины) — исходник оставлен.
    KeptBecause(String),
}

#[derive(Debug, Clone, Serialize)]
pub enum JobOutcome {
    Done {
        output: PathBuf,
        operation: String,
        source: SourceAction,
    },
    Skipped {
        reason: String,
    },
    Failed {
        error: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub done: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: usize,
}

#[derive(Debug, Clone, Serialize)]
pub enum Event {
    JobStarted {
        id: JobId,
        source: PathBuf,
        operation: String,
    },
    JobProgress {
        id: JobId,
        /// 0.0–1.0
        fraction: f32,
        /// Скорость относительно реального времени (для видео).
        speed: Option<f32>,
    },
    JobFinished {
        id: JobId,
        source: PathBuf,
        outcome: JobOutcome,
    },
    Log {
        id: Option<JobId>,
        level: LogLevel,
        message: String,
    },
    /// Место на диске кончилось — очередь на паузе (FR-4.6).
    PausedNoSpace {
        path: PathBuf,
    },
    Governor(GovernorStatus),
    RunFinished(RunSummary),
}
