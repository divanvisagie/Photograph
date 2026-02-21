use std::path::{Path, PathBuf};

use image::DynamicImage;

pub const THUMB_SIZE: u32 = 300;

static RAW_EXTS: &[&str] = &["raf", "dng", "nef", "cr2", "arw"];

/// Returns the cached thumbnail path for a given source image.
pub fn cache_path(source: &Path, cache_dir: &Path) -> PathBuf {
    let stem = source
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    cache_dir.join(format!("{}.webp", stem))
}

/// Open an image, falling back to raw decoding for RAW extensions.
pub fn open_image(path: &Path) -> anyhow::Result<DynamicImage> {
    // Fast path: try the standard image crate first.
    if let Ok(img) = image::open(path) {
        return Ok(img);
    }

    // Fallback: try raw decode for known raw extensions.
    let is_raw = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| RAW_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false);

    if !is_raw {
        // Re-attempt to get the original error message.
        return Ok(image::open(path)?);
    }

    let raw = rawler::decode_file(path)?;
    let develop = rawler::imgop::develop::RawDevelop::default();
    let intermediate = develop.develop_intermediate(&raw)?;
    intermediate
        .to_dynamic_image()
        .ok_or_else(|| anyhow::anyhow!("raw develop produced invalid image"))
}

/// Generate a thumbnail for `source` and write it to `dest`.
pub fn generate(source: &Path, dest: &Path) -> anyhow::Result<()> {
    let img = open_image(source)?;
    let thumb = img.thumbnail(THUMB_SIZE, THUMB_SIZE);
    std::fs::create_dir_all(dest.parent().unwrap())?;
    thumb.save(dest)?;
    Ok(())
}

