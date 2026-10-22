//! CLI `converter` (FR-14.1): то же ядро, что и в GUI, прогресс в терминале.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use converter_core::dupes::{
    self, DupAction, DupesOptions, DupesProgress, GroupKind, KeepRule, KindFilter, Similarity,
};
use converter_core::engine::Engine;
use converter_core::events::{Event, JobOutcome, LogLevel, SourceAction};
use converter_core::ffmpeg::{FfmpegSource, Tools};
use converter_core::fs_ops;
use converter_core::governor::{Governor, GovernorStatus, LoadLevel};
use converter_core::hw;
use converter_core::media::ImageKind;
use converter_core::options::{
    AudioFormat, ConflictPolicy, ConvertOptions, HwBackend, HwPreference, ImageFormat, ManualCodec,
    ManualContainer, ManualVideo, MetadataPolicy, OutputDir, PngCompression, QualityProfile,
    SourcePolicy, VideoMode, WebmCodec,
};
use converter_core::scan::{self, ScanOptions};

#[derive(Parser)]
#[command(
    name = "converter",
    version,
    about = "Конвертация фото и видео и поиск дубликатов"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Конвертировать файлы и папки
    Convert(ConvertArgs),
    /// Найти дубликаты
    Dupes(DupesArgs),
    /// Показать ffmpeg и найденные аппаратные кодировщики
    Hw {
        /// Перепроверить, не используя кэш
        #[arg(long)]
        refresh: bool,
        #[command(flatten)]
        ffmpeg: FfmpegArg,
    },
}

#[derive(Args)]
struct FfmpegArg {
    /// Путь к ffmpeg (файл или папка с ffmpeg и ffprobe)
    #[arg(long, global = true)]
    ffmpeg: Option<PathBuf>,
}

#[derive(Args)]
struct LoadArg {
    /// Уровень нагрузки: auto, low, medium, high, max или число потоков
    #[arg(long, default_value = "auto", value_parser = parse_load)]
    load: LoadLevel,
}

fn parse_load(s: &str) -> Result<LoadLevel, String> {
    Ok(match s {
        "auto" => LoadLevel::Auto,
        "low" => LoadLevel::Low,
        "medium" => LoadLevel::Medium,
        "high" => LoadLevel::High,
        "max" => LoadLevel::Max,
        n => LoadLevel::Custom {
            threads: n.parse().map_err(|_| {
                format!("ожидалось auto|low|medium|high|max или число, получено «{n}»")
            })?,
            low_priority: false,
            hw_sessions: 2,
        },
    })
}

#[derive(Clone, Copy, ValueEnum)]
enum ToFormat {
    Jpeg,
    Png,
    Webp,
}

#[derive(Clone, Copy, ValueEnum)]
enum VideoArg {
    /// H.264 + AAC в MP4, играет везде
    Compat,
    /// HEVC в MP4 (нужно расширение HEVC на Windows)
    Compact,
    /// AV1 + Opus в WebM
    Webm,
    /// VP9 + Opus в WebM (медленно)
    WebmVp9,
    /// Копирование потоков в MKV
    Mkv,
    /// Только звук в MP3
    Mp3,
    /// Только звук в M4A (AAC)
    M4a,
    /// Ручной режим: --codec, --container, --crf, --short-side, --fps
    Manual,
}

#[derive(Clone, Copy, ValueEnum)]
enum CodecArg {
    H264,
    Hevc,
    Av1,
    Vp9,
}

#[derive(Clone, Copy, ValueEnum)]
enum ContainerArg {
    Mp4,
    Mkv,
    Webm,
}

#[derive(Clone, Copy, ValueEnum)]
enum ProfileArg {
    Fast,
    Balanced,
    High,
}

#[derive(Clone, Copy, ValueEnum)]
enum HwArg {
    Auto,
    Off,
    Nvenc,
    Qsv,
    Amf,
    Vaapi,
}

#[derive(Clone, Copy, ValueEnum)]
enum ConflictArg {
    Suffix,
    Overwrite,
    Skip,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourcesArg {
    Keep,
    Trash,
    Delete,
}

#[derive(Clone, Copy, ValueEnum)]
enum MetadataArg {
    Keep,
    StripGps,
    StripAll,
}

#[derive(Args)]
struct ConvertArgs {
    /// Файлы и папки
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// Формат фото
    #[arg(long, value_enum, default_value = "jpeg")]
    to: ToFormat,
    /// Качество JPEG/WebP (1–100)
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,
    /// PNG: сжимать сильнее (медленнее)
    #[arg(long)]
    png_compact: bool,
    /// PNG: 16 бит для глубоких исходников
    #[arg(long)]
    png_16bit: bool,
    /// Уменьшить по большей стороне до N px
    #[arg(long)]
    max_side: Option<u32>,
    /// Перевести цвета в sRGB вместо переноса профиля
    #[arg(long)]
    srgb: bool,
    /// Режим видео
    #[arg(long, value_enum, default_value = "compat")]
    video: VideoArg,
    /// Ручной режим: кодек
    #[arg(long, value_enum, default_value = "h264")]
    codec: CodecArg,
    /// Ручной режим: контейнер
    #[arg(long, value_enum, default_value = "mp4")]
    container: ContainerArg,
    /// Ручной режим: CRF/QP (меньше — лучше); по умолчанию — по --profile
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=63))]
    crf: Option<u8>,
    /// Ручной режим: меньшая сторона кадра, только уменьшение (720, 1080…)
    #[arg(long, value_parser = clap::value_parser!(u32).range(144..))]
    short_side: Option<u32>,
    /// Ручной режим: частота кадров
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=240))]
    fps: Option<u32>,
    /// Битрейт звука при перекодировании, кбит/с
    #[arg(long, default_value_t = 192)]
    audio_kbps: u32,
    /// Профиль качества видео
    #[arg(long, value_enum, default_value = "balanced")]
    profile: ProfileArg,
    /// Аппаратное ускорение
    #[arg(long, value_enum, default_value = "auto")]
    hw: HwArg,
    #[command(flatten)]
    load: LoadArg,
    /// Папка результата (по умолчанию — рядом с исходником)
    #[arg(long)]
    out: Option<PathBuf>,
    /// Сохранять структуру подпапок в --out
    #[arg(long, requires = "out")]
    keep_tree: bool,
    /// Шаблон имени: {name}, {date}, {ext}, {n}
    #[arg(long, default_value = "{name}")]
    name: String,
    #[arg(long, value_enum, default_value = "suffix")]
    conflict: ConflictArg,
    /// Что делать с исходниками после успешной конвертации
    #[arg(long, value_enum, default_value = "keep")]
    sources: SourcesArg,
    #[arg(long, value_enum, default_value = "keep")]
    metadata: MetadataArg,
    /// Конвертировать и видео Live Photo
    #[arg(long)]
    include_live_video: bool,
    /// Не заходить в подпапки
    #[arg(long)]
    no_recursive: bool,
    /// Не спрашивать подтверждение
    #[arg(long, short)]
    yes: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum KeepArg {
    Oldest,
    Newest,
    ShortestPath,
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Jpeg,
    Heic,
    Png,
    Webp,
}

impl FormatArg {
    fn kind(self) -> ImageKind {
        match self {
            FormatArg::Jpeg => ImageKind::Jpeg,
            FormatArg::Heic => ImageKind::Heif,
            FormatArg::Png => ImageKind::Png,
            FormatArg::Webp => ImageKind::Webp,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum SimilarArg {
    Off,
    Strict,
    Normal,
    Loose,
}

#[derive(Clone, Copy, ValueEnum)]
enum ActionArg {
    Report,
    Trash,
    Move,
}

#[derive(Args)]
struct DupesArgs {
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// Какой файл в группе оставить
    #[arg(long, value_enum, default_value = "oldest")]
    keep: KeepArg,
    /// Оставлять файлы из этих папок (по порядку приоритета); заменяет --keep
    #[arg(long)]
    prefer: Vec<PathBuf>,
    /// Одно фото в разных форматах (HEIC + JPEG): какой формат оставлять
    #[arg(long, value_enum, default_value = "jpeg")]
    keep_format: FormatArg,
    /// Убирать и лишние форматы одного фото (иначе они только показываются)
    #[arg(long, conflicts_with = "no_formats")]
    formats: bool,
    /// Не искать одно фото в разных форматах
    #[arg(long)]
    no_formats: bool,
    /// Похожие фото (серии, пересжатые копии): только показываются, сами не убираются
    #[arg(long, value_enum, default_value = "normal")]
    similar: SimilarArg,
    #[arg(long, value_enum, default_value = "report")]
    action: ActionArg,
    /// Папка для --action move
    #[arg(long, required_if_eq("action", "move"))]
    move_to: Option<PathBuf>,
    /// Минимальный размер файла, байт
    #[arg(long, default_value_t = 1024)]
    min_size: u64,
    #[arg(long, conflicts_with = "videos")]
    images: bool,
    #[arg(long)]
    videos: bool,
    /// Включить облачные файлы, которых нет на диске (скачает их!)
    #[arg(long)]
    include_cloud: bool,
    #[arg(long)]
    include_hidden: bool,
    /// Не использовать кэш хэшей
    #[arg(long)]
    no_cache: bool,
    /// Сохранить отчёт в CSV
    #[arg(long)]
    csv: Option<PathBuf>,
    #[command(flatten)]
    load: LoadArg,
    #[arg(long, short)]
    yes: bool,
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} Б")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn confirm(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).is_ok()
        && matches!(
            line.trim().to_lowercase().as_str(),
            "y" | "yes" | "д" | "да"
        )
}

fn level_name(s: &GovernorStatus) -> String {
    let level = match s.level {
        LoadLevel::Auto => "Авто",
        LoadLevel::Low => "Низкий",
        LoadLevel::Medium => "Средний",
        LoadLevel::High => "Высокий",
        LoadLevel::Max => "Максимум",
        LoadLevel::Custom { .. } => "Свой",
    };
    let mut out = format!(
        "{level} · {} из {} потоков · внешняя нагрузка {:.0} %",
        s.budget,
        s.threads_total,
        s.external_load * 100.0
    );
    if s.on_battery {
        out.push_str(" · от батареи");
    }
    if s.memory_low {
        out.push_str(" · мало памяти");
    }
    out
}

/// Без терминала indicatif скрывает вывод — тогда пишем напрямую.
fn say(mp: &MultiProgress, line: String) {
    if mp.is_hidden() {
        eprintln!("{line}");
    } else {
        let _ = mp.println(line);
    }
}

fn ctrlc_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&flag);
    let _ = ctrlc::set_handler(move || {
        if f.swap(true, Ordering::SeqCst) {
            // Второе нажатие — немедленный выход.
            std::process::exit(130);
        }
        eprintln!("\nОтмена… (ещё раз Ctrl+C — выйти сразу)");
    });
    flag
}

fn locate_tools(ffmpeg: &Option<PathBuf>) -> anyhow::Result<Tools> {
    let source = ffmpeg
        .clone()
        .map_or(FfmpegSource::Auto, FfmpegSource::Custom);
    Ok(Tools::locate(&source)?)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Convert(args) => run_convert(args),
        Command::Dupes(args) => run_dupes(args),
        Command::Hw { refresh, ffmpeg } => run_hw(refresh, &ffmpeg.ffmpeg),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Ошибка: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run_hw(refresh: bool, ffmpeg: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let tools = locate_tools(ffmpeg)?;
    println!("ffmpeg:  {} ({:?})", tools.ffmpeg.display(), tools.origin);
    println!("версия:  {}", tools.version);
    let caps = hw::detect(&tools, refresh);
    if caps.encoders.is_empty() {
        println!("Аппаратные кодировщики не найдены — будет использоваться CPU.");
    }
    for e in &caps.encoders {
        println!(
            "  {:<6} {:<7} {}",
            e.backend.name(),
            format!("{:?}", e.codec),
            e.device.as_deref().unwrap_or("")
        );
    }
    println!("одновременных HW-сессий: до {}", caps.max_sessions);
    Ok(ExitCode::SUCCESS)
}

fn convert_options(a: &ConvertArgs) -> ConvertOptions {
    let mut o = ConvertOptions::default();
    o.image.format = match a.to {
        ToFormat::Jpeg => ImageFormat::Jpeg,
        ToFormat::Png => ImageFormat::Png,
        ToFormat::Webp => ImageFormat::Webp,
    };
    o.image.quality = a.quality;
    o.image.png_compression = if a.png_compact {
        PngCompression::Compact
    } else {
        PngCompression::Fast
    };
    o.image.png_16bit = a.png_16bit;
    o.image.max_side = a.max_side;
    o.image.to_srgb = a.srgb;
    o.video.mode = match a.video {
        VideoArg::Compat => VideoMode::Compat,
        VideoArg::Compact => VideoMode::Compact,
        VideoArg::Webm => VideoMode::Webm(WebmCodec::Av1),
        VideoArg::WebmVp9 => VideoMode::Webm(WebmCodec::Vp9),
        VideoArg::Mkv => VideoMode::Mkv,
        VideoArg::Mp3 => VideoMode::Audio(AudioFormat::Mp3),
        VideoArg::M4a => VideoMode::Audio(AudioFormat::M4a),
        VideoArg::Manual => VideoMode::Manual(ManualVideo {
            codec: match a.codec {
                CodecArg::H264 => ManualCodec::H264,
                CodecArg::Hevc => ManualCodec::Hevc,
                CodecArg::Av1 => ManualCodec::Av1,
                CodecArg::Vp9 => ManualCodec::Vp9,
            },
            container: match a.container {
                ContainerArg::Mp4 => ManualContainer::Mp4,
                ContainerArg::Mkv => ManualContainer::Mkv,
                ContainerArg::Webm => ManualContainer::Webm,
            },
            crf: a.crf,
            short_side: a.short_side,
            fps: a.fps,
        }),
    };
    o.video.audio_bitrate_kbps = a.audio_kbps;
    o.video.quality = match a.profile {
        ProfileArg::Fast => QualityProfile::Fast,
        ProfileArg::Balanced => QualityProfile::Balanced,
        ProfileArg::High => QualityProfile::High,
    };
    o.video.hw = match a.hw {
        HwArg::Auto => HwPreference::Auto,
        HwArg::Off => HwPreference::Off,
        HwArg::Nvenc => HwPreference::Only(HwBackend::Nvenc),
        HwArg::Qsv => HwPreference::Only(HwBackend::Qsv),
        HwArg::Amf => HwPreference::Only(HwBackend::Amf),
        HwArg::Vaapi => HwPreference::Only(HwBackend::Vaapi),
    };
    o.output = match (&a.out, a.keep_tree) {
        (None, _) => OutputDir::BesideSource,
        (Some(d), false) => OutputDir::Into(d.clone()),
        (Some(d), true) => OutputDir::IntoKeepTree(d.clone()),
    };
    o.name_template = a.name.clone();
    o.conflict = match a.conflict {
        ConflictArg::Suffix => ConflictPolicy::Suffix,
        ConflictArg::Overwrite => ConflictPolicy::Overwrite,
        ConflictArg::Skip => ConflictPolicy::Skip,
    };
    o.sources = match a.sources {
        SourcesArg::Keep => SourcePolicy::Keep,
        SourcesArg::Trash => SourcePolicy::Trash,
        SourcesArg::Delete => SourcePolicy::Delete,
    };
    o.metadata = match a.metadata {
        MetadataArg::Keep => MetadataPolicy::Keep,
        MetadataArg::StripGps => MetadataPolicy::StripGps,
        MetadataArg::StripAll => MetadataPolicy::StripAll,
    };
    o.skip_live_photo_video = !a.include_live_video;
    o
}

fn run_convert(args: ConvertArgs) -> anyhow::Result<ExitCode> {
    let opts = convert_options(&args);
    if opts.sources == SourcePolicy::Delete
        && !args.yes
        && !confirm("Исходники будут удалены НАВСЕГДА после успешной конвертации. Продолжить?")
    {
        return Ok(ExitCode::from(2));
    }
    let stale = fs_ops::cleanup_stale_temps();
    if stale > 0 {
        eprintln!("Удалено временных файлов после прошлого сбоя: {stale}");
    }
    let tools = Arc::new(locate_tools(&None)?);
    let caps = Arc::new(hw::detect(&tools, false));
    let governor = Governor::start(args.load.load, caps.max_sessions);
    let interrupted = ctrlc_flag();

    let scan_opts = ScanOptions {
        recursive: !args.no_recursive,
        ..Default::default()
    };
    let mut items = scan::scan(&args.paths, &scan_opts, &interrupted);
    if items.is_empty() {
        eprintln!("Медиафайлы не найдены.");
        return Ok(ExitCode::SUCCESS);
    }
    let videos = items.iter().filter(|i| !i.kind.is_image()).count();
    eprintln!(
        "Найдено: {} фото, {} видео ({}). ffmpeg: {}. GPU: {}",
        items.len() - videos,
        videos,
        human(items.iter().map(|i| i.size).sum()),
        tools.version.split_whitespace().nth(2).unwrap_or("?"),
        if caps.encoders.is_empty() {
            "нет".to_string()
        } else {
            caps.backends()
                .iter()
                .map(|b| b.name())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    scan::inspect(&tools, &mut items, governor.threads_total(), &interrupted);

    let engine = Engine::new(Arc::clone(&tools), caps, Arc::clone(&governor));
    let total = items.len() as u64;
    let run = engine.convert(items, opts);
    let gov_events = governor.subscribe();

    let mp = MultiProgress::new();
    let overall = mp.add(ProgressBar::new(total));
    overall.set_style(
        ProgressStyle::with_template(
            "{bar:30.green/dim} {pos}/{len} · {elapsed_precise} · ≈{eta} · {msg}",
        )
        .unwrap()
        .progress_chars("━╸─"),
    );
    overall.enable_steady_tick(Duration::from_millis(200));
    let job_style = ProgressStyle::with_template("  {bar:20.cyan/dim} {percent:>3}% {msg}")
        .unwrap()
        .progress_chars("━╸─");
    let mut bars: HashMap<u64, ProgressBar> = HashMap::new();
    let mut failed = 0usize;
    let mut cancel_sent = false;

    loop {
        if interrupted.load(Ordering::Relaxed) && !cancel_sent {
            run.cancel();
            cancel_sent = true;
        }
        while let Ok(status) = gov_events.try_recv() {
            overall.set_message(level_name(&status));
        }
        let event = match run.events.recv_timeout(Duration::from_millis(200)) {
            Ok(e) => e,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        match event {
            Event::JobStarted {
                id,
                source,
                operation,
            } => {
                let bar = mp.insert_before(&overall, ProgressBar::new(1000));
                bar.set_style(job_style.clone());
                let name = source
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                bar.set_message(format!("{name} — {operation}"));
                bars.insert(id, bar);
            }
            Event::JobProgress {
                id,
                fraction,
                speed,
            } => {
                if let Some(bar) = bars.get(&id) {
                    bar.set_position((fraction * 1000.0) as u64);
                    if let Some(s) = speed {
                        let msg = bar.message();
                        let base = msg.split(" · ×").next().unwrap_or(&msg).to_string();
                        bar.set_message(format!("{base} · ×{s:.1}"));
                    }
                }
            }
            Event::JobFinished {
                id,
                source,
                outcome,
            } => {
                if let Some(bar) = bars.remove(&id) {
                    bar.finish_and_clear();
                }
                overall.inc(1);
                let name = source.display();
                let line = match outcome {
                    JobOutcome::Done {
                        output,
                        operation,
                        source,
                    } => {
                        let src = match source {
                            SourceAction::Kept => String::new(),
                            SourceAction::Trashed => ", исходник в корзине".into(),
                            SourceAction::Deleted => ", исходник удалён".into(),
                            SourceAction::KeptBecause(why) => format!(", исходник оставлен: {why}"),
                        };
                        format!("✓ {name} → {} ({operation}{src})", output.display())
                    }
                    JobOutcome::Skipped { reason } => format!("– {name}: пропущено, {reason}"),
                    JobOutcome::Failed { error } => {
                        failed += 1;
                        format!("✗ {name}: {error}")
                    }
                    JobOutcome::Cancelled => format!("■ {name}: отменено"),
                };
                say(&mp, line);
            }
            Event::Log { level, message, .. } => {
                if level != LogLevel::Info {
                    say(&mp, format!("  ! {message}"));
                }
            }
            Event::PausedNoSpace { path } => {
                say(
                    &mp,
                    format!(
                        "Кончилось место на диске ({}). Освободите место и нажмите Enter, чтобы продолжить.",
                        path.display()
                    ),
                );
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                run.resume();
            }
            Event::Governor(_) => {}
            Event::RunFinished(s) => {
                overall.finish_and_clear();
                eprintln!(
                    "Готово: {} успешно, {} пропущено, {} ошибок, {} отменено.",
                    s.done, s.skipped, s.failed, s.cancelled
                );
            }
        }
    }
    run.join();
    governor.shutdown();
    Ok(if failed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn run_dupes(args: DupesArgs) -> anyhow::Result<ExitCode> {
    let governor = Governor::start(args.load.load, 0);
    let interrupted = ctrlc_flag();
    let opts = DupesOptions {
        roots: args.paths.clone(),
        include_hidden: args.include_hidden,
        include_cloud: args.include_cloud,
        min_size: args.min_size,
        kinds: if args.images {
            KindFilter::Images
        } else if args.videos {
            KindFilter::Videos
        } else {
            KindFilter::All
        },
        use_cache: !args.no_cache,
        find_formats: !args.no_formats && !args.videos,
        similar: match args.similar {
            SimilarArg::Off => Similarity::Off,
            SimilarArg::Strict => Similarity::Strict,
            SimilarArg::Normal => Similarity::Normal,
            SimilarArg::Loose => Similarity::Loose,
        },
        ..Default::default()
    };
    let bar = ProgressBar::new_spinner();
    bar.enable_steady_tick(Duration::from_millis(120));
    let report = dupes::find(&opts, &governor, &interrupted, |p| match p {
        DupesProgress::Scanning { found } => {
            bar.set_message(format!("Сканирование: {found} файлов"))
        }
        DupesProgress::Hashing {
            stage: 2,
            done,
            total,
            ..
        } => bar.set_message(format!("Быстрая проверка: {done}/{total}")),
        DupesProgress::Hashing {
            stage: 4,
            done,
            total,
            ..
        } => bar.set_message(format!("Сведения о фото: {done}/{total}")),
        DupesProgress::Hashing {
            stage: 5,
            done,
            total,
            ..
        } => bar.set_message(format!("Сравнение фото по изображению: {done}/{total}")),
        DupesProgress::Hashing {
            done,
            total,
            bytes_done,
            bytes_total,
            ..
        } => bar.set_message(format!(
            "Полная проверка: {done}/{total} · {} из {}",
            human(bytes_done),
            human(bytes_total)
        )),
    });
    bar.finish_and_clear();
    governor.shutdown();
    let report = report?;

    let rule = if args.prefer.is_empty() {
        match args.keep {
            KeepArg::Oldest => KeepRule::Oldest,
            KeepArg::Newest => KeepRule::Newest,
            KeepArg::ShortestPath => KeepRule::ShortestPath,
        }
    } else {
        KeepRule::PriorityFolders(args.prefer.clone())
    };
    // Группы «одно фото в разных форматах» убираются только по --formats: файлы в них
    // разные, и выбор формата — решение человека, а не очевидная лишняя копия.
    let acts = |g: &dupes::DupGroup| g.kind == GroupKind::Exact || args.formats;
    let marks: Vec<Vec<bool>> = report
        .groups
        .iter()
        .map(|g| {
            let m = dupes::auto_select(g, &rule, args.keep_format.kind());
            if acts(g) { m } else { vec![false; m.len()] }
        })
        .collect();

    for g in &report.groups {
        let m = dupes::auto_select(g, &rule, args.keep_format.kind());
        match g.kind {
            GroupKind::Exact => println!(
                "\n{} × {} (освободится {})",
                g.files.len(),
                human(g.size),
                human(g.reclaimable())
            ),
            GroupKind::Formats => println!(
                "\nОдно фото в {} форматах (освободится {}){}",
                g.files.len(),
                human(g.reclaimable()),
                if acts(g) {
                    ""
                } else {
                    " — убрать: --formats"
                }
            ),
            GroupKind::Similar => println!(
                "\nПохожие фото: {} (не отмечаются — решите сами)",
                g.files.len()
            ),
        }
        for (f, &remove) in g.files.iter().zip(&m) {
            let links = if f.links.is_empty() {
                String::new()
            } else {
                format!(" (+{} жёстк. ссылок на этот же файл)", f.links.len())
            };
            let format = match (f.format, f.pixels) {
                (Some(k), Some((w, h))) => format!(" [{}, {w}×{h}, {}]", k.label(), human(f.size)),
                (Some(k), None) => format!(" [{}, {}]", k.label(), human(f.size)),
                _ => String::new(),
            };
            println!(
                "  {} {}{format}{links}",
                if remove { "✗" } else { "✓" },
                f.path.display()
            );
        }
    }
    let marked = || {
        report
            .groups
            .iter()
            .zip(&marks)
            .flat_map(|(g, m)| g.files.iter().zip(m).filter(|(_, x)| **x))
    };
    let reclaim: u64 = marked().map(|(f, _)| f.size).sum();
    let to_remove = marked().count();
    let count = |kind| report.groups.iter().filter(|g| g.kind == kind).count();
    println!(
        "\nПроверено файлов: {}. Групп копий: {}, одно фото в разных форматах: {}, похожих фото: {}. \
         Будет убрано: {to_remove}, освободится {}.",
        report.scanned,
        count(GroupKind::Exact),
        count(GroupKind::Formats),
        count(GroupKind::Similar),
        human(reclaim)
    );
    if report.skipped_cloud > 0 {
        println!(
            "Пропущено облачных файлов без локальной копии: {}",
            report.skipped_cloud
        );
    }
    if !report.hardlinks.is_empty() {
        println!("Жёстких ссылок (не дубликаты): {}", report.hardlinks.len());
    }
    for (p, e) in &report.errors {
        eprintln!("  ! {}: {e}", p.display());
    }
    if let Some(csv) = &args.csv {
        std::fs::write(csv, dupes::to_csv(&report.groups, Some(&marks)))?;
        println!("Отчёт: {}", csv.display());
    }

    let action = match args.action {
        ActionArg::Report => return Ok(ExitCode::SUCCESS),
        ActionArg::Trash => DupAction::Trash,
        ActionArg::Move => DupAction::MoveTo(args.move_to.clone().expect("проверено clap")),
    };
    if to_remove == 0 {
        return Ok(ExitCode::SUCCESS);
    }
    let verb = match &action {
        DupAction::Trash => "Отправить в корзину".to_string(),
        DupAction::MoveTo(d) => format!("Переместить в {}", d.display()),
        DupAction::Delete => "Удалить".to_string(),
    };
    if !args.yes && !confirm(&format!("{verb} {to_remove} файлов ({})?", human(reclaim))) {
        return Ok(ExitCode::SUCCESS);
    }
    let results = dupes::apply(&report.groups, &marks, &action)?;
    let errors: Vec<_> = results.iter().filter(|r| r.error.is_some()).collect();
    for r in &errors {
        eprintln!(
            "  ✗ {}: {}",
            r.path.display(),
            r.error.as_deref().unwrap_or_default()
        );
    }
    println!(
        "Готово: {} из {} файлов.",
        results.len() - errors.len(),
        results.len()
    );
    Ok(if errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
