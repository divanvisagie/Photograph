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
    s.straighten = 1.5;
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

fn main() -> Result<()> {
    let mut args = std::env::args();
    let _bin = args.next();
    let dir = args
        .next()
        .map(PathBuf::from)
        .context("usage: perf_probe <raw-dir> [count]")?;
    let count = args
        .next()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);

    let files = list_raw_files(&dir, count)?;
    if files.is_empty() {
        anyhow::bail!("No RAW files found in {}", dir.display());
    }
    eprintln!("Using {} RAW files from {}", files.len(), dir.display());

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
        let processed = processing::transform::apply(&preview, &state);
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
