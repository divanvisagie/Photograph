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

pub fn is_raw_image(path: &Path) -> bool {
    has_extension(path, RAW_EXTS)
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
    let is_raw = is_raw_image(path);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewSource {
    Embedded,
    FullDevelop,
}

fn open_image_full_for_preview(path: &Path) -> anyhow::Result<(DynamicImage, PreviewSource)> {
    Ok((open_image(path)?, PreviewSource::FullDevelop))
}

fn open_image_for_preview_with_hooks<FEmbedded, FFull>(
    path: &Path,
    open_embedded: FEmbedded,
    open_full: FFull,
) -> anyhow::Result<(DynamicImage, PreviewSource)>
where
    FEmbedded: Fn(&Path) -> anyhow::Result<Option<DynamicImage>>,
    FFull: Fn(&Path) -> anyhow::Result<(DynamicImage, PreviewSource)>,
{
    if is_raw_image(path) {
        match open_embedded(path) {
            Ok(Some(img)) => return Ok((img, PreviewSource::Embedded)),
            Ok(None) => {}
            Err(_) => {}
        }
    }
    open_full(path)
}

pub fn open_image_for_preview_with_source(
    path: &Path,
) -> anyhow::Result<(DynamicImage, PreviewSource)> {
    open_image_for_preview_with_hooks(path, open_embedded_raw_preview, open_image_full_for_preview)
}

/// Open an image for interactive preview.
///
/// For RAW files this prefers embedded preview/thumbnail payloads first and
/// falls back to full RAW develop if embedded assets are unavailable.
pub fn open_image_for_preview(path: &Path) -> anyhow::Result<DynamicImage> {
    Ok(open_image_for_preview_with_source(path)?.0)
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;

    use image::{DynamicImage, ImageBuffer, Rgba};

    use super::{PreviewSource, is_raw_image, open_image_for_preview_with_hooks};

    fn img(px: [u8; 4]) -> DynamicImage {
        DynamicImage::ImageRgba8(ImageBuffer::from_pixel(1, 1, Rgba(px)))
    }

    #[test]
    fn raw_preview_prefers_embedded_and_skips_full_decode() {
        let embedded_calls = Cell::new(0);
        let full_calls = Cell::new(0);
        let path = Path::new("/tmp/test.raf");

        let out = open_image_for_preview_with_hooks(
            path,
            |_: &Path| {
                embedded_calls.set(embedded_calls.get() + 1);
                Ok(Some(img([1, 2, 3, 255])))
            },
            |_: &Path| {
                full_calls.set(full_calls.get() + 1);
                Ok((img([9, 9, 9, 255]), PreviewSource::FullDevelop))
            },
        )
        .expect("preview open should succeed");

        assert_eq!(embedded_calls.get(), 1);
        assert_eq!(full_calls.get(), 0);
        assert!(matches!(out.1, PreviewSource::Embedded));
        assert_eq!(out.0.to_rgba8().get_pixel(0, 0).0, [1, 2, 3, 255]);
    }

    #[test]
    fn raw_preview_falls_back_to_full_decode_when_embedded_missing() {
        let embedded_calls = Cell::new(0);
        let full_calls = Cell::new(0);
        let path = Path::new("/tmp/test.raf");

        let out = open_image_for_preview_with_hooks(
            path,
            |_: &Path| {
                embedded_calls.set(embedded_calls.get() + 1);
                Ok(None)
            },
            |_: &Path| {
                full_calls.set(full_calls.get() + 1);
                Ok((img([9, 9, 9, 255]), PreviewSource::FullDevelop))
            },
        )
        .expect("preview open should succeed");

        assert_eq!(embedded_calls.get(), 1);
        assert_eq!(full_calls.get(), 1);
        assert!(matches!(out.1, PreviewSource::FullDevelop));
        assert_eq!(out.0.to_rgba8().get_pixel(0, 0).0, [9, 9, 9, 255]);
    }

    #[test]
    fn raw_preview_falls_back_to_full_decode_when_embedded_probe_errors() {
        let embedded_calls = Cell::new(0);
        let full_calls = Cell::new(0);
        let path = Path::new("/tmp/test.raf");

        let out = open_image_for_preview_with_hooks(
            path,
            |_: &Path| {
                embedded_calls.set(embedded_calls.get() + 1);
                anyhow::bail!("embedded decode failed");
            },
            |_: &Path| {
                full_calls.set(full_calls.get() + 1);
                Ok((img([5, 6, 7, 255]), PreviewSource::FullDevelop))
            },
        )
        .expect("preview open should succeed");

        assert_eq!(embedded_calls.get(), 1);
        assert_eq!(full_calls.get(), 1);
        assert!(matches!(out.1, PreviewSource::FullDevelop));
        assert_eq!(out.0.to_rgba8().get_pixel(0, 0).0, [5, 6, 7, 255]);
    }

    #[test]
    fn non_raw_preview_skips_embedded_probe() {
        let embedded_calls = Cell::new(0);
        let full_calls = Cell::new(0);
        let path = Path::new("/tmp/test.jpg");

        let out = open_image_for_preview_with_hooks(
            path,
            |_: &Path| {
                embedded_calls.set(embedded_calls.get() + 1);
                Ok(Some(img([1, 2, 3, 255])))
            },
            |_: &Path| {
                full_calls.set(full_calls.get() + 1);
                Ok((img([8, 8, 8, 255]), PreviewSource::FullDevelop))
            },
        )
        .expect("preview open should succeed");

        assert_eq!(embedded_calls.get(), 0);
        assert_eq!(full_calls.get(), 1);
        assert!(matches!(out.1, PreviewSource::FullDevelop));
        assert_eq!(out.0.to_rgba8().get_pixel(0, 0).0, [8, 8, 8, 255]);
    }

    #[test]
    fn raw_extension_detection_is_case_insensitive() {
        assert!(is_raw_image(Path::new("/tmp/a.raf")));
        assert!(is_raw_image(Path::new("/tmp/a.RAF")));
        assert!(!is_raw_image(Path::new("/tmp/a.jpg")));
    }
}
