//! Очередь конвертации (разделы 4.6, 4.7, 6.3 ТЗ).
//!
//! Диспетчер в отдельном потоке выбирает задачи, которые помещаются в бюджет
//! токенов, и запускает каждую в своём потоке. Результаты возвращаются по каналу.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::error::{Error, Result};
use crate::events::{Event, JobId, JobOutcome, LogLevel, RunSummary, SourceAction};
use crate::ffmpeg::{self, RunContext, Tools};
use crate::fs_ops::{self, TempFile};
use crate::governor::Governor;
use crate::hw::HwCaps;
use crate::media::{ImageKind, MediaKind};
use crate::options::{ConflictPolicy, ConvertOptions, SourcePolicy};
use crate::photo;
use crate::plan::{OutputPlanner, Planned};
use crate::pool::Permit;
use crate::probe;
use crate::scan::{LivePhoto, MediaItem};
use crate::sys;
use crate::video::{self, ArgsInput, Cost, VideoPlan};

/// Больше 8 потоков на одно видео x264/x265 почти не ускоряют.
const MAX_THREADS_PER_VIDEO: usize = 8;

#[derive(Default)]
struct Control {
    cancel: AtomicBool,
    paused: AtomicBool,
    cancelled_jobs: Mutex<HashSet<JobId>>,
}

impl Control {
    fn job_cancelled(&self, id: JobId) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.cancelled_jobs.lock().unwrap().contains(&id)
    }
}

/// Управление запущенной очередью.
pub struct RunHandle {
    pub events: Receiver<Event>,
    control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
}

impl RunHandle {
    pub fn pause(&self) {
        self.control.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.control.paused.store(false, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.control.paused.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.control.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancel_job(&self, id: JobId) {
        self.control.cancelled_jobs.lock().unwrap().insert(id);
    }

    /// Ждёт завершения диспетчера.
    pub fn join(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Clone)]
pub struct Engine {
    tools: Arc<Tools>,
    caps: Arc<HwCaps>,
    governor: Arc<Governor>,
}

#[derive(Clone)]
enum Work {
    Image(ImageKind),
    Video(VideoPlan),
}

#[derive(Clone)]
struct Job {
    id: JobId,
    item: MediaItem,
    output: PathBuf,
    work: Work,
}

enum WorkResult {
    Outcome(JobOutcome),
    /// HW-кодирование не удалось — повторить на CPU (FR-3.8).
    NeedsCpu(Job, String),
    /// Кончилось место — очередь на паузу, задачу вернуть в начало (FR-4.6).
    NoSpace(Job, PathBuf),
}

struct Finished {
    id: JobId,
    source: PathBuf,
    heavy_tokens: usize,
    result: WorkResult,
}

struct Ctx {
    tools: Arc<Tools>,
    governor: Arc<Governor>,
    opts: Arc<ConvertOptions>,
    control: Arc<Control>,
    events: Sender<Event>,
}

impl Engine {
    pub fn new(tools: Arc<Tools>, caps: Arc<HwCaps>, governor: Arc<Governor>) -> Self {
        Self {
            tools,
            caps,
            governor,
        }
    }

    pub fn tools(&self) -> &Arc<Tools> {
        &self.tools
    }

    pub fn caps(&self) -> &Arc<HwCaps> {
        &self.caps
    }

    pub fn governor(&self) -> &Arc<Governor> {
        &self.governor
    }

    pub fn convert(&self, items: Vec<MediaItem>, opts: ConvertOptions) -> RunHandle {
        let (tx, rx) = crossbeam_channel::unbounded();
        let control = Arc::new(Control::default());
        let ctx = Ctx {
            tools: Arc::clone(&self.tools),
            governor: Arc::clone(&self.governor),
            opts: Arc::new(opts),
            control: Arc::clone(&control),
            events: tx,
        };
        let caps = Arc::clone(&self.caps);
        let thread = thread::Builder::new()
            .name("dispatcher".into())
            .spawn(move || dispatch(ctx, &caps, items))
            .expect("не удалось запустить диспетчер");
        RunHandle {
            events: rx,
            control,
            thread: Some(thread),
        }
    }
}

enum Prepared {
    Job(Box<Job>),
    Done(JobOutcome),
}

fn prepare(
    id: JobId,
    item: MediaItem,
    opts: &ConvertOptions,
    caps: &HwCaps,
    planner: &mut OutputPlanner,
) -> Prepared {
    let skip = |reason: &str| {
        Prepared::Done(JobOutcome::Skipped {
            reason: reason.into(),
        })
    };
    let (work, ext, date) = match item.kind {
        MediaKind::Image(kind) => {
            if photo::is_noop(kind, &opts.image, opts.metadata) {
                return skip("уже в нужном формате");
            }
            let date = planner
                .needs_date()
                .then(|| photo::read_date_taken(&item.path, kind))
                .flatten();
            (Work::Image(kind), opts.image.format.extension(), date)
        }
        MediaKind::Video => {
            if opts.skip_live_photo_video && matches!(item.live_photo, LivePhoto::VideoOf(_)) {
                return skip("видео Live Photo");
            }
            let Some(info) = item.video.as_ref() else {
                let error = item
                    .probe_error
                    .clone()
                    .unwrap_or_else(|| "файл не прочитан ffprobe".into());
                return Prepared::Done(JobOutcome::Failed { error });
            };
            let plan = match video::plan(info, &opts.video, caps, false) {
                Ok(p) => p,
                Err(e) => {
                    return Prepared::Done(JobOutcome::Failed {
                        error: e.to_string(),
                    });
                }
            };
            if video::is_noop(&plan, info, opts.metadata) {
                return skip("уже в нужном формате");
            }
            let date = info.creation_date();
            (Work::Video(plan), opts.video.mode.extension(), date)
        }
    };
    match planner.plan(&item.path, Some(&item.root), ext, date.as_deref()) {
        Planned::Path(output) => Prepared::Job(Box::new(Job {
            id,
            item,
            output,
            work,
        })),
        Planned::Skip(existing) => Prepared::Done(JobOutcome::Skipped {
            reason: format!("уже существует: {}", existing.display()),
        }),
    }
}

/// Сколько токенов нужно задаче. `None` — сейчас не запускать.
struct Need {
    cpu: usize,
    hw: bool,
    heavy: bool,
}

/// Больше 4 потоков декодирование одного HEIC почти не ускоряет.
const MAX_THREADS_PER_PHOTO: usize = 4;

/// `photos_pending` — сколько фото ждёт в очереди, включая эту задачу.
fn need(
    job: &Job,
    budget: usize,
    used: usize,
    heavy_in_use: usize,
    photos_pending: usize,
) -> Option<Need> {
    let light = |hw| Need {
        cpu: 1,
        hw,
        heavy: false,
    };
    let (hw, heavy) = match &job.work {
        // Фото много — по потоку на фото (лучшая пропускная способность);
        // мало — свободные потоки отдаются декодированию тайлов HEIC.
        Work::Image(_) => {
            let share = (budget / photos_pending.max(1)).clamp(1, MAX_THREADS_PER_PHOTO);
            let free = budget.saturating_sub(used);
            return (free >= 1).then_some(Need {
                cpu: share.min(free),
                hw: false,
                heavy: false,
            });
        }
        Work::Video(plan) => match plan.cost() {
            Cost::Light => return Some(light(false)),
            Cost::Hw { cpu_filters: false } => return Some(light(true)),
            Cost::Hw { cpu_filters: true } => (true, true),
            Cost::Cpu => (false, true),
        },
    };
    debug_assert!(heavy);
    // Пока в очереди есть фото, тяжёлому видео достаётся не больше ⌊бюджет/2⌋.
    let cap = if photos_pending > 0 {
        budget - budget.div_ceil(2)
    } else {
        budget
    };
    let target = budget.clamp(1, MAX_THREADS_PER_VIDEO);
    let available = cap
        .saturating_sub(heavy_in_use)
        .min(budget.saturating_sub(used))
        .min(target);
    // Не дробим видео на однопоточные задачи, если бюджет позволяет больше.
    (available >= target.min(2)).then_some(Need {
        cpu: available,
        hw,
        heavy,
    })
}

fn dispatch(ctx: Ctx, caps: &HwCaps, items: Vec<MediaItem>) {
    let ctx = Arc::new(ctx);
    let opts = Arc::clone(&ctx.opts);
    let mut summary = RunSummary {
        done: 0,
        skipped: 0,
        failed: 0,
        cancelled: 0,
    };
    let send = |e: Event| {
        let _ = ctx.events.send(e);
    };
    let finish = |summary: &mut RunSummary, id: JobId, source: PathBuf, outcome: JobOutcome| {
        match &outcome {
            JobOutcome::Done { .. } => summary.done += 1,
            JobOutcome::Skipped { .. } => summary.skipped += 1,
            JobOutcome::Failed { .. } => summary.failed += 1,
            JobOutcome::Cancelled => summary.cancelled += 1,
        }
        send(Event::JobFinished {
            id,
            source,
            outcome,
        });
    };

    let mut planner = OutputPlanner::new(
        opts.output.clone(),
        opts.name_template.clone(),
        opts.conflict,
    );
    let mut pending: VecDeque<Job> = VecDeque::new();
    for (n, item) in items.into_iter().enumerate() {
        let id = n as JobId + 1;
        let source = item.path.clone();
        match prepare(id, item, &opts, caps, &mut planner) {
            Prepared::Job(job) => pending.push_back(*job),
            Prepared::Done(outcome) => finish(&mut summary, id, source, outcome),
        }
    }

    let (done_tx, done_rx) = crossbeam_channel::unbounded::<Finished>();
    let pool = Arc::clone(ctx.governor.pool());
    let mut running = 0usize;
    let mut heavy_in_use = 0usize;

    let handle = |f: Finished,
                  pending: &mut VecDeque<Job>,
                  running: &mut usize,
                  heavy_in_use: &mut usize,
                  summary: &mut RunSummary| {
        *running -= 1;
        *heavy_in_use -= f.heavy_tokens;
        match f.result {
            WorkResult::Outcome(outcome) => finish(summary, f.id, f.source, outcome),
            WorkResult::NeedsCpu(mut job, reason) => {
                send(Event::Log {
                    id: Some(job.id),
                    level: LogLevel::Warn,
                    message: format!("аппаратное кодирование не удалось, повтор на CPU: {reason}"),
                });
                let info = job.item.video.as_ref().expect("видео с пробой");
                match video::plan(info, &opts.video, caps, true) {
                    Ok(plan) => {
                        job.work = Work::Video(plan);
                        pending.push_front(job);
                    }
                    Err(e) => finish(
                        summary,
                        f.id,
                        f.source,
                        JobOutcome::Failed {
                            error: e.to_string(),
                        },
                    ),
                }
            }
            WorkResult::NoSpace(job, path) => {
                ctx.control.paused.store(true, Ordering::Relaxed);
                send(Event::PausedNoSpace { path });
                pending.push_front(job);
            }
        }
    };

    loop {
        while let Ok(f) = done_rx.try_recv() {
            handle(
                f,
                &mut pending,
                &mut running,
                &mut heavy_in_use,
                &mut summary,
            );
        }
        if ctx.control.cancel.load(Ordering::Relaxed) {
            for job in pending.drain(..) {
                finish(&mut summary, job.id, job.item.path, JobOutcome::Cancelled);
            }
        } else {
            // Отменённые по одной задачи из очереди.
            let cancelled = ctx.control.cancelled_jobs.lock().unwrap().clone();
            if !cancelled.is_empty() {
                pending.retain(|job| {
                    let keep = !cancelled.contains(&job.id);
                    if !keep {
                        finish(
                            &mut summary,
                            job.id,
                            job.item.path.clone(),
                            JobOutcome::Cancelled,
                        );
                    }
                    keep
                });
            }
        }

        if !ctx.control.paused.load(Ordering::Relaxed) {
            let mut i = 0;
            while i < pending.len() {
                let state = pool.snapshot();
                let photos_pending = pending
                    .iter()
                    .filter(|j| matches!(j.work, Work::Image(_)))
                    .count();
                let Some(n) = need(
                    &pending[i],
                    state.budget,
                    state.used,
                    heavy_in_use,
                    photos_pending,
                ) else {
                    i += 1;
                    continue;
                };
                let Some(permit) = pool.try_acquire(n.cpu, n.hw) else {
                    i += 1;
                    continue;
                };
                let job = pending.remove(i).expect("индекс в пределах");
                let heavy_tokens = if n.heavy { n.cpu } else { 0 };
                running += 1;
                heavy_in_use += heavy_tokens;
                spawn_worker(Arc::clone(&ctx), job, permit, heavy_tokens, done_tx.clone());
            }
        }

        if pending.is_empty() && running == 0 {
            break;
        }
        // Таймаут нужен, чтобы подхватить рост бюджета от регулятора.
        if let Ok(f) = done_rx.recv_timeout(Duration::from_millis(150)) {
            handle(
                f,
                &mut pending,
                &mut running,
                &mut heavy_in_use,
                &mut summary,
            );
        }
    }
    send(Event::RunFinished(summary));
}

fn spawn_worker(
    ctx: Arc<Ctx>,
    job: Job,
    permit: Permit,
    heavy_tokens: usize,
    done: Sender<Finished>,
) {
    thread::Builder::new()
        .name(format!("job-{}", job.id))
        .spawn(move || {
            sys::set_current_thread_priority(permit.priority());
            let id = job.id;
            let source = job.item.path.clone();
            let operation = match &job.work {
                Work::Image(kind) => {
                    format!("{} → {}", kind.label(), ctx.opts.image.format.label())
                }
                Work::Video(plan) => plan.label(),
            };
            let _ = ctx.events.send(Event::JobStarted {
                id,
                source: source.clone(),
                operation: operation.clone(),
            });
            if let Work::Video(plan) = &job.work {
                for w in &plan.warnings {
                    let _ = ctx.events.send(Event::Log {
                        id: Some(id),
                        level: LogLevel::Warn,
                        message: w.clone(),
                    });
                }
            }
            let result = match &job.work {
                Work::Image(kind) => run_image(&ctx, &job, *kind, &permit, operation),
                Work::Video(plan) => run_video(&ctx, &job, plan, &permit),
            };
            drop(permit);
            let _ = done.send(Finished {
                id,
                source,
                heavy_tokens,
                result,
            });
        })
        .expect("не удалось запустить задачу");
}

fn outcome_from_error(e: Error) -> JobOutcome {
    match e {
        Error::Cancelled => JobOutcome::Cancelled,
        e => JobOutcome::Failed {
            error: e.to_string(),
        },
    }
}

/// Фиксирует результат, переносит дату и обрабатывает исходник (FR-4.4, 4.5, 5.x).
fn finalize(ctx: &Ctx, job: &Job, temp: TempFile, operation: String) -> Result<JobOutcome> {
    let opts = &ctx.opts;
    let output = temp.commit(&job.output, opts.conflict == ConflictPolicy::Overwrite)?;
    if opts.preserve_mtime
        && let Err(e) = fs_ops::copy_mtime(&job.item.path, &output)
    {
        let _ = ctx.events.send(Event::Log {
            id: Some(job.id),
            level: LogLevel::Warn,
            message: format!("не удалось перенести дату изменения: {e}"),
        });
    }
    let source = fs_ops::dispose_source(&job.item.path, opts.sources);
    if opts.remove_live_photo_video
        && opts.sources != SourcePolicy::Keep
        && let (LivePhoto::PhotoWith(video), SourceAction::Trashed | SourceAction::Deleted) =
            (&job.item.live_photo, &source)
        && let SourceAction::KeptBecause(why) = fs_ops::dispose_source(video, opts.sources)
    {
        let _ = ctx.events.send(Event::Log {
            id: Some(job.id),
            level: LogLevel::Warn,
            message: format!("видео Live Photo не удалено: {why}"),
        });
    }
    Ok(JobOutcome::Done {
        output,
        operation,
        source,
    })
}

fn run_image(
    ctx: &Ctx,
    job: &Job,
    kind: ImageKind,
    permit: &Permit,
    operation: String,
) -> WorkResult {
    let attempt = || -> Result<JobOutcome> {
        let temp = TempFile::new(&job.output)?;
        let opts = &ctx.opts;
        let converted = photo::convert(
            &job.item.path,
            kind,
            temp.path(),
            &opts.image,
            opts.metadata,
            permit.cpu() as u32,
        )?;
        if ctx.control.job_cancelled(job.id) {
            return Err(Error::Cancelled);
        }
        // Полное декодирование — только если исходник будет удалён (FR-5.2).
        let full = opts.sources != SourcePolicy::Keep;
        photo::verify(temp.path(), converted.width, converted.height, full)?;
        finalize(ctx, job, temp, operation)
    };
    match attempt() {
        Ok(outcome) => WorkResult::Outcome(outcome),
        Err(e) if fs_ops::is_no_space(&e) => WorkResult::NoSpace(job.clone(), job.output.clone()),
        Err(e) => WorkResult::Outcome(outcome_from_error(e)),
    }
}

fn verify_video(tools: &Tools, path: &Path, expected: Option<f64>) -> Result<()> {
    let info = probe::probe(tools, path).map_err(|e| Error::Verify(e.to_string()))?;
    if info.main_video().is_none() {
        return Err(Error::Verify("в результате нет видеопотока".into()));
    }
    if let (Some(src), Some(out)) = (expected, info.duration) {
        let tolerance = (src * 0.01).max(0.5);
        if (src - out).abs() > tolerance {
            return Err(Error::Verify(format!(
                "длительность {out:.2} с вместо {src:.2} с"
            )));
        }
    }
    Ok(())
}

fn run_video(ctx: &Ctx, job: &Job, plan: &VideoPlan, permit: &Permit) -> WorkResult {
    let info = job.item.video.as_ref().expect("видео с пробой");
    let attempts: &[bool] = if plan.gpu_only_eligible(info) {
        &[true, false]
    } else {
        &[false]
    };
    let mut last_error = String::new();
    for &gpu_only in attempts {
        let temp = match TempFile::new(&job.output) {
            Ok(t) => t,
            Err(e) => return WorkResult::Outcome(outcome_from_error(e)),
        };
        let args = video::build_args(
            &ArgsInput {
                plan,
                info,
                opts: &ctx.opts.video,
                metadata: ctx.opts.metadata,
                threads: permit.cpu(),
                gpu_only,
            },
            &job.item.path,
            temp.path(),
        );
        let id = job.id;
        let cancelled = || ctx.control.job_cancelled(id);
        let run_ctx = RunContext {
            priority: permit.priority(),
            governor: Some(&ctx.governor),
            cancelled: &cancelled,
            duration: info.duration,
        };
        let started = Instant::now();
        let result = ffmpeg::run(&ctx.tools, &args, &run_ctx, |p| {
            let _ = ctx.events.send(Event::JobProgress {
                id,
                fraction: p.fraction,
                speed: p.speed,
            });
        })
        .and_then(|()| verify_video(&ctx.tools, temp.path(), info.duration));
        match result {
            Ok(()) => {
                let mut operation = plan.label();
                if gpu_only {
                    operation.push_str(", только GPU");
                }
                let _ = ctx.events.send(Event::Log {
                    id: Some(id),
                    level: LogLevel::Info,
                    message: format!("{operation}: {:.1} с", started.elapsed().as_secs_f32()),
                });
                return match finalize(ctx, job, temp, operation) {
                    Ok(o) => WorkResult::Outcome(o),
                    Err(e) => WorkResult::Outcome(outcome_from_error(e)),
                };
            }
            Err(Error::Cancelled) => return WorkResult::Outcome(JobOutcome::Cancelled),
            Err(e) if fs_ops::is_no_space(&e) => {
                return WorkResult::NoSpace(job.clone(), job.output.clone());
            }
            Err(e) => {
                last_error = e.to_string();
                let _ = ctx.events.send(Event::Log {
                    id: Some(id),
                    level: LogLevel::Warn,
                    message: format!(
                        "попытка {} не удалась: {last_error}",
                        if gpu_only {
                            "«только GPU»"
                        } else {
                            "кодирования"
                        }
                    ),
                });
            }
        }
    }
    if plan.hw_backend().is_some() {
        WorkResult::NeedsCpu(job.clone(), last_error)
    } else {
        WorkResult::Outcome(JobOutcome::Failed { error: last_error })
    }
}

/// Нехватка места на томе результата (FR-4.6).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SpaceCheck {
    pub mount: PathBuf,
    pub needed: u64,
    pub free: u64,
}

/// Грубая оценка размера результата. Коэффициенты — по замерам на фото и видео iPhone.
pub fn estimate_bytes(item: &MediaItem, opts: &ConvertOptions) -> u64 {
    use crate::options::ImageFormat;
    match item.kind {
        MediaKind::Image(kind) => {
            if photo::is_noop(kind, &opts.image, opts.metadata) {
                return 0;
            }
            let per_pixel = match opts.image.format {
                ImageFormat::Jpeg => 0.16,
                ImageFormat::Webp => 0.09,
                ImageFormat::Png if opts.image.png_16bit => 2.4,
                ImageFormat::Png => 1.2,
            };
            match item.dimensions {
                Some((w, h)) => {
                    let mut pixels = w as f64 * h as f64;
                    if let Some(side) = opts.image.max_side {
                        let scale = (side as f64 / w.max(h) as f64).min(1.0);
                        pixels *= scale * scale;
                    }
                    (pixels * per_pixel) as u64
                }
                None => {
                    item.size
                        * if opts.image.format == ImageFormat::Png {
                            10
                        } else {
                            2
                        }
                }
            }
        }
        MediaKind::Video => match (&item.video, opts.skip_live_photo_video, &item.live_photo) {
            (_, true, LivePhoto::VideoOf(_)) => 0,
            (Some(info), ..) => match video::plan(info, &opts.video, &HwCaps::default(), true) {
                Ok(plan) if plan.is_remux() => item.size,
                _ => item.size * 6 / 5,
            },
            (None, ..) => 0,
        },
    }
}

/// Сколько места нужно на каждом томе результата и сколько там свободно.
/// Возвращает только тома, где места не хватает.
pub fn space_check(items: &[MediaItem], opts: &ConvertOptions) -> Vec<SpaceCheck> {
    use crate::options::OutputDir;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut needed: std::collections::HashMap<PathBuf, (u64, u64)> = Default::default();
    for item in items {
        let dir = match &opts.output {
            OutputDir::BesideSource => item
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            OutputDir::Into(d) | OutputDir::IntoKeepTree(d) => d.clone(),
        };
        if let Some((mount, free)) = fs_ops::volume_of(&dir, &disks) {
            let entry = needed.entry(mount).or_insert((0, free));
            entry.0 += estimate_bytes(item, opts);
        }
    }
    let mut out: Vec<SpaceCheck> = needed
        .into_iter()
        .filter(|(_, (need, free))| need > free)
        .map(|(mount, (needed, free))| SpaceCheck {
            mount,
            needed,
            free,
        })
        .collect();
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{Container, Encoder, SwEncoder, VideoAction};

    fn video_job(cost_cpu: bool) -> Job {
        let video = if cost_cpu {
            VideoAction::Encode {
                encoder: Encoder::Sw(SwEncoder::X264),
                tonemap: false,
            }
        } else {
            VideoAction::Copy
        };
        Job {
            id: 1,
            item: MediaItem {
                path: "a.mov".into(),
                root: ".".into(),
                kind: MediaKind::Video,
                size: 0,
                video: None,
                dimensions: None,
                probe_error: None,
                live_photo: LivePhoto::No,
            },
            output: "a.mp4".into(),
            work: Work::Video(VideoPlan {
                container: Container::Mp4,
                video_index: 0,
                video,
                audio: vec![],
                subtitles: vec![],
                keep_hdr: false,
                warnings: vec![],
            }),
        }
    }

    #[test]
    fn cpu_video_threads_follow_budget_and_photo_share() {
        let job = video_job(true);
        // Нет фото: видео берёт до 8 потоков.
        assert_eq!(need(&job, 8, 0, 0, 0).unwrap().cpu, 8);
        assert_eq!(need(&job, 16, 0, 0, 0).unwrap().cpu, 8);
        // Есть фото: видео — не больше ⌊8/2⌋ = 4.
        assert_eq!(need(&job, 8, 0, 0, 3).unwrap().cpu, 4);
        // Бюджет 1 и фото в очереди: видео ждёт.
        assert!(need(&job, 1, 0, 0, 1).is_none());
        // Бюджет 1 без фото: одно видео в один поток.
        assert_eq!(need(&job, 1, 0, 0, 0).unwrap().cpu, 1);
        // Свободен один токен из восьми: не дробим.
        assert!(need(&job, 8, 7, 0, 0).is_none());
    }

    #[test]
    fn remux_is_light() {
        let n = need(&video_job(false), 1, 0, 0, 1).unwrap();
        assert_eq!((n.cpu, n.hw, n.heavy), (1, false, false));
    }

    #[test]
    fn photo_threads_depend_on_queue_size() {
        let mut job = video_job(false);
        job.work = Work::Image(ImageKind::Heif);
        assert_eq!(
            need(&job, 8, 0, 0, 1).unwrap().cpu,
            4,
            "одно фото — до 4 потоков"
        );
        assert_eq!(need(&job, 8, 0, 0, 4).unwrap().cpu, 2);
        assert_eq!(
            need(&job, 8, 0, 0, 100).unwrap().cpu,
            1,
            "пачка — по потоку на фото"
        );
        assert_eq!(
            need(&job, 8, 7, 0, 1).unwrap().cpu,
            1,
            "не больше свободных"
        );
        assert!(need(&job, 8, 8, 0, 1).is_none());
    }
}
