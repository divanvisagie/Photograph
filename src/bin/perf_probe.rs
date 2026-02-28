use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use rayon::prelude::*;

#[path = "../processing/mod.rs"]
mod processing;
#[path = "../state.rs"]
mod state;
#[path = "../thumbnail.rs"]
mod thumbnail;

const PREVIEW_MAX: u32 = 1920;
const RAW_EXTS: &[&str] = &["raf", "dng", "nef", "cr2", "arw"];

#[derive(Clone, Copy, Debug)]
enum ProbeBackend {
    Cpu,
    Auto,
    GpuPipeline,
}

impl ProbeBackend {
    fn from_arg(raw: Option<&str>) -> Self {
        match raw.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
            "cpu" => ProbeBackend::Cpu,
            "auto" => ProbeBackend::Auto,
            "gpu" | "gpu_pipeline" | "gpu_spike" | "wgpu" | "spike" => ProbeBackend::GpuPipeline,
            _ => ProbeBackend::Auto,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ProbeBackend::Cpu => "cpu",
            ProbeBackend::Auto => "auto",
            ProbeBackend::GpuPipeline => "gpu_pipeline",
        }
    }
}

fn has_extension(path: &Path, exts: &[&str]) -> bool {
    let Some(ext) = path.extension().map(|e| e.to_string_lossy()) else {
        return false;
    };
    exts.iter().any(|known| ext.eq_ignore_ascii_case(known))
}

fn list_raw_files(dir: &Path, limit: usize) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("read_dir failed for {}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && has_extension(p, RAW_EXTS))
        .collect();
    files.sort();
    if files.len() > limit {
        files.truncate(limit);
    }
    Ok(files)
}

fn median_ms(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) * 0.5
    } else {
        sorted[mid]
    }
}

fn build_state() -> state::EditState {
    let mut s = state::EditState::default();
    s.exposure = 0.35;
    s.contrast = 0.2;
    s.highlights = -0.2;
    s.shadows = 0.2;
    s.temperature = 0.1;
    s.saturation = 0.15;
    s.hue_shift = 8.0;
    s
}

fn ensure_preview_size(img: image::DynamicImage) -> image::DynamicImage {
    if img.width() > PREVIEW_MAX || img.height() > PREVIEW_MAX {
        img.thumbnail(PREVIEW_MAX, PREVIEW_MAX)
    } else {
        img
    }
}

fn apply_preview_backend(
    preview: &image::DynamicImage,
    state: &state::EditState,
    backend: ProbeBackend,
) -> Result<image::DynamicImage> {
    let allow_cpu_fallback = processing::gpu_pipeline::allow_debug_cpu_fallback();

    match backend {
        ProbeBackend::Cpu if allow_cpu_fallback => Ok(processing::transform::apply(preview, state)),
        ProbeBackend::Cpu => anyhow::bail!(
            "cpu backend requires {}=1",
            processing::gpu_pipeline::DEBUG_ALLOW_CPU_FALLBACK_ENV
        ),
        ProbeBackend::Auto | ProbeBackend::GpuPipeline => {
            match processing::gpu_pipeline::try_apply(preview, state) {
                Some(img) => Ok(img),
                None if allow_cpu_fallback => Ok(processing::transform::apply(preview, state)),
                None => anyhow::bail!(
                    "gpu pipeline unavailable/failed and CPU fallback is disabled (set {}=1 for debug fallback)",
                    processing::gpu_pipeline::DEBUG_ALLOW_CPU_FALLBACK_ENV
                ),
            }
        }
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args();
    let _bin = args.next();
    let dir = args
        .next()
        .map(PathBuf::from)
        .context("usage: perf_probe <raw-dir> [count] [auto|cpu|gpu_pipeline]")?;
    let count = args
        .next()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);
    let backend = ProbeBackend::from_arg(args.next().as_deref());
    let gpu_status = processing::gpu_pipeline::runtime_status();
    let allow_cpu_fallback = processing::gpu_pipeline::allow_debug_cpu_fallback();

    if !gpu_status.available && !allow_cpu_fallback {
        anyhow::bail!(
            "no compatible discrete Vulkan GPU detected (set {}=1 for debug CPU fallback)",
            processing::gpu_pipeline::DEBUG_ALLOW_CPU_FALLBACK_ENV
        );
    }

    let files = list_raw_files(&dir, count)?;
    if files.is_empty() {
        anyhow::bail!("No RAW files found in {}", dir.display());
    }
    eprintln!(
        "Using {} RAW files from {} (preview backend: {})",
        files.len(),
        dir.display(),
        backend.label()
    );
    if matches!(backend, ProbeBackend::Auto | ProbeBackend::GpuPipeline) {
        let adapter_desc = match (
            gpu_status.adapter_name.as_deref(),
            gpu_status.adapter_backend.as_deref(),
        ) {
            (Some(name), Some(api)) => format!("{} ({})", name, api),
            (Some(name), None) => name.to_string(),
            _ => "n/a".to_string(),
        };
        eprintln!(
            "gpu_pipeline availability: {}{}",
            if gpu_status.available {
                "available"
            } else {
                "unavailable (debug cpu fallback)"
            },
            if gpu_status.available {
                format!(", adapter={}", adapter_desc)
            } else {
                String::new()
            },
        );
    }

    let mut first_preview_samples = Vec::with_capacity(files.len());
    for path in &files {
        let t0 = Instant::now();
        let img = thumbnail::open_image_for_preview(path)
            .with_context(|| format!("preview open failed for {}", path.display()))?;
        let _preview = ensure_preview_size(img);
        first_preview_samples.push(t0.elapsed().as_secs_f64() * 1000.0);
    }

    let state = build_state();
    let mut slider_samples = Vec::with_capacity(files.len());
    for path in &files {
        let img = thumbnail::open_image_for_preview(path)
            .with_context(|| format!("preview open failed for {}", path.display()))?;
        let preview = ensure_preview_size(img);
        let t0 = Instant::now();
        let processed = apply_preview_backend(&preview, &state, backend)?;
        let _raw = processed.to_rgba8().into_raw();
        slider_samples.push(t0.elapsed().as_secs_f64() * 1000.0);
    }

    let out_dir = std::env::temp_dir().join(format!(
        "photograph-perf-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    ));
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("create_dir_all {}", out_dir.display()))?;

    let export_start = Instant::now();
    files.par_iter().try_for_each(|path| -> Result<()> {
        let input = thumbnail::open_image(path)
            .with_context(|| format!("full open failed for {}", path.display()))?;
        let processed = processing::transform::apply(&input, &state);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
        let output = out_dir.join(format!("{}.jpg", stem));
        let file = fs::File::create(&output)
            .with_context(|| format!("create output failed {}", output.display()))?;
        let writer = BufWriter::new(file);
        let encoder = JpegEncoder::new_with_quality(writer, 90);
        processed
            .write_with_encoder(encoder)
            .with_context(|| format!("jpeg encode failed {}", output.display()))?;
        Ok(())
    })?;
    let export_wall_s = export_start.elapsed().as_secs_f64();
    let images_per_sec = files.len() as f64 / export_wall_s.max(1e-9);

    println!("METRIC file_count={}", files.len());
    println!("METRIC preview_backend={}", backend.label());
    println!(
        "METRIC preview_ms_median={:.2}",
        median_ms(&first_preview_samples)
    );
    println!("METRIC slider_ms_median={:.2}", median_ms(&slider_samples));
    println!("METRIC export_wall_s={:.2}", export_wall_s);
    println!("METRIC export_images_per_sec={:.3}", images_per_sec);
    println!("METRIC export_out_dir={}", out_dir.display());

    Ok(())
}
