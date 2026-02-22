use std::path::{Path, PathBuf};

use image::DynamicImage;

pub const THUMB_SIZE: u32 = 300;

static RAW_EXTS: &[&str] = &["raf", "dng", "nef", "cr2", "arw"];
static SUPPORTED_IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "tiff", "tif", "webp", "bmp", "raf", "dng", "nef", "cr2", "arw", "heic",
    "avif",
];

fn has_extension(path: &Path, exts: &[&str]) -> bool {
    let Some(ext) = path.extension().map(|e| e.to_string_lossy()) else {
        return false;
    };
    exts.iter().any(|known| ext.eq_ignore_ascii_case(known))
}

/// Returns `true` if the path has a supported image extension.
pub fn is_supported_image(path: &Path) -> bool {
    has_extension(path, SUPPORTED_IMAGE_EXTS)
}

/// Returns the cached thumbnail path for a given source image.
pub fn cache_path(source: &Path, cache_dir: &Path) -> PathBuf {
    let stem = source.file_name().unwrap_or_default().to_string_lossy();
    cache_dir.join(format!("{}.webp", stem))
}

/// Open an image, falling back to raw decoding for RAW extensions.
pub fn open_image(path: &Path) -> anyhow::Result<DynamicImage> {
    // Fast path: try the standard image crate first.
    if let Ok(img) = image::open(path) {
        return Ok(img);
    }

    // Fallback: try raw decode for known raw extensions.
    let is_raw = has_extension(path, RAW_EXTS);

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

/// Open an image for interactive preview.
///
/// Uses the same raw development pipeline as the render/export path so that
/// preview and final output have identical exposure and tone.
pub fn open_image_for_preview(path: &Path) -> anyhow::Result<DynamicImage> {
    open_image(path)
}

fn open_embedded_raw_preview(path: &Path) -> anyhow::Result<Option<DynamicImage>> {
    let source = rawler::rawsource::RawSource::new(path)?;
    let decoder = rawler::get_decoder(&source)?;
    let params = rawler::decoders::RawDecodeParams::default();

    if let Some(img) = decoder.preview_image(&source, &params)? {
        return Ok(Some(img));
    }
    if let Some(img) = decoder.thumbnail_image(&source, &params)? {
        return Ok(Some(img));
    }
    if let Some(img) = decoder.full_image(&source, &params)? {
        return Ok(Some(img));
    }

    Ok(None)
}

/// Generate a thumbnail for `source` and write it to `dest`.
pub fn generate(source: &Path, dest: &Path) -> anyhow::Result<()> {
    let img = open_image_for_preview(source)?;
    let thumb = img.thumbnail(THUMB_SIZE, THUMB_SIZE);
    std::fs::create_dir_all(dest.parent().unwrap())?;
    thumb.save(dest)?;
    Ok(())
}
