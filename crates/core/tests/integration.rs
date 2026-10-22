//! Интеграционные тесты на файлах из `data_media/` (раздел 12 ТЗ).
//! Пропускаются, если нет образцов или ffmpeg.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use converter_core::engine::Engine;
use converter_core::events::{Event, JobOutcome, RunSummary};
use converter_core::exif;
use converter_core::ffmpeg::{FfmpegSource, Tools};
use converter_core::governor::{Governor, LoadLevel};
use converter_core::hw;
use converter_core::options::{ConvertOptions, HwPreference, OutputDir};
use converter_core::probe;
use converter_core::scan::{self, ScanOptions};

fn samples() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data_media");
    dir.join("фото отэц.HEIC").exists().then_some(dir)
}

fn setup() -> Option<(PathBuf, Engine)> {
    let dir = samples()?;
    let tools = Arc::new(Tools::locate(&FfmpegSource::Auto).ok()?);
    let caps = Arc::new(hw::detect(&tools, false));
    let governor = Governor::start(LoadLevel::Max, caps.max_sessions);
    Some((dir, Engine::new(tools, caps, governor)))
}

fn copy_to(tmp: &Path, src: &Path) -> PathBuf {
    let dst = tmp.join(src.file_name().unwrap());
    std::fs::copy(src, &dst).unwrap();
    dst
}

fn run(engine: &Engine, inputs: &[PathBuf], opts: ConvertOptions) -> (RunSummary, Vec<JobOutcome>) {
    let cancel = AtomicBool::new(false);
    let mut items = scan::scan(inputs, &ScanOptions::default(), &cancel);
    scan::inspect(engine.tools(), &mut items, 4, &cancel);
    let handle = engine.convert(items, opts);
    let mut outcomes = Vec::new();
    let mut summary = None;
    for e in handle.events.iter() {
        match e {
            Event::JobFinished { outcome, .. } => outcomes.push(outcome),
            Event::RunFinished(s) => summary = Some(s),
            _ => {}
        }
    }
    handle.join();
    (summary.expect("RunFinished"), outcomes)
}

fn no_temp_files(dir: &Path) -> bool {
    walk(dir)
        .iter()
        .all(|p| !p.to_string_lossy().contains(".cvtmp-"))
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn heic_to_jpeg_keeps_icc_exif_and_resets_orientation() {
    let Some((dir, engine)) = setup() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = copy_to(tmp.path(), &dir.join("фото отэц.HEIC"));
    let (summary, outcomes) = run(
        &engine,
        std::slice::from_ref(&src),
        ConvertOptions::default(),
    );
    assert_eq!(summary.done, 1, "{outcomes:?}");
    let JobOutcome::Done { output, .. } = &outcomes[0] else {
        unreachable!()
    };
    assert_eq!(output.extension().unwrap(), "jpg");

    let mut decoder = image::ImageReader::open(output)
        .unwrap()
        .with_guessed_format()
        .unwrap()
        .into_decoder()
        .unwrap();
    use image::ImageDecoder;
    assert_eq!(decoder.dimensions(), (4284, 5712));
    assert!(
        decoder.icc_profile().unwrap().is_some(),
        "ICC-профиль должен сохраниться"
    );
    let raw = decoder.exif_metadata().unwrap().expect("EXIF");
    let tiff = exif::normalize(&raw).unwrap();
    assert_eq!(exif::orientation(&tiff), Some(1));
    assert!(exif::date_time_original(&tiff).is_some());
    assert!(src.exists(), "исходник по умолчанию остаётся");
    assert!(no_temp_files(tmp.path()));
}

#[test]
fn hevc_video_to_compat_mp4_matches_duration() {
    let Some((dir, engine)) = setup() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = copy_to(tmp.path(), &dir.join("Я-люблю-свою-работу-финал.HEVC"));
    let opts = ConvertOptions {
        output: OutputDir::Into(tmp.path().join("out")),
        ..Default::default()
    };
    let (summary, outcomes) = run(&engine, std::slice::from_ref(&src), opts);
    assert_eq!(summary.done, 1, "{outcomes:?}");
    let JobOutcome::Done { output, .. } = &outcomes[0] else {
        unreachable!()
    };
    let out = probe::probe(engine.tools(), output).unwrap();
    let src_info = probe::probe(engine.tools(), &src).unwrap();
    let v = out.main_video().unwrap();
    assert_eq!(v.codec.as_deref(), Some("h264"));
    assert_eq!(v.bit_depth(), 8);
    assert!((out.duration.unwrap() - src_info.duration.unwrap()).abs() < 0.5);
    assert!(no_temp_files(tmp.path()));
}

#[test]
fn cancel_mid_encode_leaves_no_temp_and_keeps_source() {
    let Some((dir, engine)) = setup() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let src = copy_to(tmp.path(), &dir.join("IMG_0822.MOV"));
    let size = std::fs::metadata(&src).unwrap().len();
    let cancel = AtomicBool::new(false);
    let mut items = scan::scan(std::slice::from_ref(&src), &ScanOptions::default(), &cancel);
    scan::inspect(engine.tools(), &mut items, 1, &cancel);
    let mut opts = ConvertOptions::default();
    // CPU-кодирование длинного файла гарантированно не успеет закончиться.
    opts.video.hw = HwPreference::Off;
    opts.sources = converter_core::options::SourcePolicy::Delete;
    let handle = engine.convert(items, opts);

    let started = Instant::now();
    let mut cancelled_at = None;
    let mut outcome = None;
    for e in handle.events.iter() {
        match e {
            Event::JobProgress { .. } if cancelled_at.is_none() => {
                handle.cancel();
                cancelled_at = Some(Instant::now());
            }
            Event::JobFinished { outcome: o, .. } => outcome = Some(o),
            _ => {}
        }
        if started.elapsed() > Duration::from_secs(120) {
            panic!("кодирование не отменилось");
        }
    }
    let waited = cancelled_at.expect("был прогресс").elapsed();
    handle.join();
    assert!(
        matches!(outcome, Some(JobOutcome::Cancelled)),
        "{outcome:?}"
    );
    assert!(waited < Duration::from_secs(3), "отмена заняла {waited:?}");
    assert_eq!(std::fs::metadata(&src).unwrap().len(), size, "исходник цел");
    assert!(no_temp_files(tmp.path()));
}
