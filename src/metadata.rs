use std::path::Path;

#[derive(Debug, Default, Clone)]
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

pub fn read(path: &Path) -> anyhow::Result<ImageMetadata> {
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
