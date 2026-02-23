use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Per-hue HSL adjustment used for selective color controls.
pub struct HslAdjust {
    pub hue: f32,
    pub saturation: f32,
    pub lightness: f32,
}

impl Default for HslAdjust {
    fn default() -> Self {
        Self {
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Keystone perspective correction parameters.
pub struct Keystone {
    pub vertical: f32,
    pub horizontal: f32,
}

impl Default for Keystone {
    fn default() -> Self {
        Self {
            vertical: 0.0,
            horizontal: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Normalized rectangle in image coordinates.
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Graduated filter parameters applied from top to bottom.
pub struct GradFilter {
    pub top: f32,
    pub bottom: f32,
    pub exposure: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
/// Serialized edit parameters stored alongside an image.
pub struct EditState {
    pub rotate: i32,
    pub flip_h: bool,
    pub flip_v: bool,
    pub crop: Option<Rect>,
    pub straighten: f32,
    pub keystone: Keystone,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub temperature: f32,
    pub saturation: f32,
    pub hue_shift: f32,
    // red, orange, yellow, green, cyan, blue, purple, pink
    pub selective_color: [HslAdjust; 8],
    pub graduated_filter: Option<GradFilter>,
    pub sharpness: f32,
}

impl Default for EditState {
    fn default() -> Self {
        Self {
            rotate: 0,
            flip_h: false,
            flip_v: false,
            crop: None,
            straighten: 0.0,
            keystone: Keystone::default(),
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            temperature: 0.0,
            saturation: 0.0,
            hue_shift: 0.0,
            selective_color: Default::default(),
            graduated_filter: None,
            sharpness: 0.0,
        }
    }
}

impl EditState {
    /// Loads edit state from the image sidecar JSON, if present and valid.
    pub fn load(image_path: &Path) -> Option<Self> {
        let sidecar = sidecar_path(image_path);
        let json = std::fs::read_to_string(sidecar).ok()?;
        serde_json::from_str(&json).ok()
    }

    /// Saves the current edit state to the image sidecar JSON.
    pub fn save(&self, image_path: &Path) -> anyhow::Result<()> {
        let sidecar = sidecar_path(image_path);
        if let Some(parent) = sidecar.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(sidecar, json)?;
        Ok(())
    }
}

fn sidecar_path(image_path: &Path) -> std::path::PathBuf {
    let dir = image_path.parent().unwrap_or(Path::new("."));
    let filename = image_path.file_name().unwrap().to_string_lossy();
    dir.join(".edits").join(format!("{}.json", filename))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn sidecar_uses_edits_folder() {
        let p = sidecar_path(Path::new("/photos/IMG_001.RAF"));
        assert_eq!(p, PathBuf::from("/photos/.edits/IMG_001.RAF.json"));
    }
}
