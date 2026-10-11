//! Screenshot mode for the docs (`make screenshot`): with
//! `PHOTOGRAPH_SCREENSHOT` set to a PNG path, the app saves a picture of its
//! window there a second after starting, and quits.

use std::path::Path;

pub fn save(image: &egui::ColorImage, path: &Path) -> Result<(), String> {
    let [w, h] = image.size;
    let buf = image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec())
        .ok_or("screenshot has the wrong size")?;
    buf.save(path).map_err(|e| e.to_string())
}
