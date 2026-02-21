use std::path::{Path, PathBuf};

pub const THUMB_SIZE: u32 = 300;

/// Returns the cached thumbnail path for a given source image.
pub fn cache_path(source: &Path, cache_dir: &Path) -> PathBuf {
    let stem = source
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    cache_dir.join(format!("{}.webp", stem))
}

/// Generate a thumbnail for `source` and write it to `dest`.
/// Uses the `image` crate for now; swap for magick-rust once verified.
pub fn generate(source: &Path, dest: &Path) -> anyhow::Result<()> {
    let img = image::open(source)?;
    let thumb = img.thumbnail(THUMB_SIZE, THUMB_SIZE);
    std::fs::create_dir_all(dest.parent().unwrap())?;
    thumb.save(dest)?;
    Ok(())
}
