use std::path::Path;

#[derive(Debug, Default, Clone)]
/// EXIF-derived metadata shown for the selected image.
pub struct ImageMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub shutter_speed: Option<String>,
    pub aperture: Option<String>,
    pub focal_length: Option<String>,
    pub date_taken: Option<String>,
}

/// Reads EXIF metadata from an image file.
///
/// RAW containers kamadak-exif can't parse (e.g. Canon CR3, which is ISO
/// BMFF rather than TIFF) fall back to the metadata rawler decodes.
pub fn read(path: &Path) -> anyhow::Result<ImageMetadata> {
    match read_exif(path) {
        Err(e) if crate::thumbnail::is_raw_image(path) => read_raw(path).map_err(|_| e),
        result => result,
    }
}

fn read_exif(path: &Path) -> anyhow::Result<ImageMetadata> {
    let file = std::fs::File::open(path)?;
    let mut bufreader = std::io::BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut bufreader)?;

    let field = |tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
    };

    Ok(ImageMetadata {
        camera_make: field(exif::Tag::Make),
        camera_model: field(exif::Tag::Model),
        lens: field(exif::Tag::LensModel),
        iso: exif
            .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            .and_then(|f| match f.value {
                exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
                _ => None,
            }),
        shutter_speed: field(exif::Tag::ExposureTime),
        aperture: field(exif::Tag::FNumber),
        focal_length: field(exif::Tag::FocalLength),
        date_taken: field(exif::Tag::DateTimeOriginal),
        ..Default::default()
    })
}

fn read_raw(path: &Path) -> anyhow::Result<ImageMetadata> {
    let source = rawler::rawsource::RawSource::new(path)?;
    let decoder = rawler::get_decoder(&source)?;
    let md = decoder.raw_metadata(&source, &rawler::decoders::RawDecodeParams::default())?;
    let exif = md.exif;
    let non_empty = |s: String| Some(s.trim().to_string()).filter(|s| !s.is_empty());

    Ok(ImageMetadata {
        camera_make: non_empty(md.make),
        camera_model: non_empty(md.model),
        lens: exif
            .lens_model
            .or(md.lens.map(|l| l.lens_model))
            .and_then(non_empty),
        iso: exif.iso_speed_ratings.map(u32::from).or(exif.iso_speed),
        shutter_speed: exif.exposure_time.and_then(|r| format_exposure(r.n, r.d)),
        aperture: exif.fnumber.and_then(|r| format_decimal(r.n, r.d)),
        focal_length: exif.focal_length.and_then(|r| format_decimal(r.n, r.d)),
        date_taken: exif.date_time_original.as_deref().map(format_exif_datetime),
        ..Default::default()
    })
}

// The formatters below mirror kamadak-exif's `display_value`, so the EXIF
// panel reads the same whichever reader produced the metadata.

/// `1/250` for fractions of a second, `2` or `2.5` for longer exposures.
fn format_exposure(n: u32, d: u32) -> Option<String> {
    if n == 0 || d == 0 {
        return None;
    }
    Some(if n >= d {
        format!("{}", n as f64 / d as f64)
    } else {
        format!("1/{}", d as f64 / n as f64)
    })
}

fn format_decimal(n: u32, d: u32) -> Option<String> {
    (d != 0).then(|| format!("{}", n as f64 / d as f64))
}

/// EXIF `YYYY:MM:DD HH:MM:SS` to `YYYY-MM-DD HH:MM:SS`.
fn format_exif_datetime(raw: &str) -> String {
    let raw = raw.trim();
    match raw.split_once(' ') {
        Some((date, time)) if date.len() == 10 => format!("{} {}", date.replace(':', "-"), time),
        _ => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_exposure_like_kamadak() {
        assert_eq!(format_exposure(1, 250).as_deref(), Some("1/250"));
        assert_eq!(format_exposure(10, 2500).as_deref(), Some("1/250"));
        assert_eq!(format_exposure(5, 2).as_deref(), Some("2.5"));
        assert_eq!(format_exposure(0, 1), None);
    }

    #[test]
    fn formats_decimals_like_kamadak() {
        assert_eq!(format_decimal(28, 10).as_deref(), Some("2.8"));
        assert_eq!(format_decimal(50, 1).as_deref(), Some("50"));
        assert_eq!(format_decimal(1, 0), None);
    }

    #[test]
    fn formats_exif_datetime_with_dashed_date() {
        assert_eq!(format_exif_datetime("2024:01:31 12:34:56"), "2024-01-31 12:34:56");
        assert_eq!(format_exif_datetime("garbage"), "garbage");
    }
}
