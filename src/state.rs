use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
/// How a spot fills its target circle (see ADR-0018).
pub enum SpotMode {
    /// Copy the source circle's pixels as-is.
    #[default]
    Clone,
    /// Take texture from the source, tone and color from around the target.
    Heal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One spot-removal patch. Positions are normalized (0–1) coordinates of the
/// *source image* — before any geometry edit — and `radius` is a fraction of
/// its shorter side, so spots stay put under later geometry changes and map
/// identically onto previews and full-resolution exports (ADR-0018).
pub struct Spot {
    pub target: [f32; 2],
    pub source: [f32; 2],
    pub radius: f32,
    /// Soft edge width as a fraction of `radius`, 0–1.
    pub feather: f32,
    pub mode: SpotMode,
}

impl Spot {
    pub const DEFAULT_FEATHER: f32 = 0.5;

    /// A spot at `target` with a default source beside it.
    pub fn new(target: [f32; 2], radius: f32, image_aspect: f32) -> Self {
        let target = clamp_center(target, radius, image_aspect);
        Self {
            target,
            source: default_source(target, radius, image_aspect),
            radius,
            feather: Self::DEFAULT_FEATHER,
            mode: SpotMode::default(),
        }
    }
}

/// `radius` (a fraction of the shorter side) in normalized x and y units for
/// an image of the given width/height aspect.
pub fn spot_radius_xy(radius: f32, image_aspect: f32) -> [f32; 2] {
    if image_aspect >= 1.0 {
        [radius / image_aspect, radius]
    } else {
        [radius, radius * image_aspect]
    }
}

/// Moves a circle's center so the whole circle lies inside the image.
pub fn clamp_center(center: [f32; 2], radius: f32, image_aspect: f32) -> [f32; 2] {
    let [rx, ry] = spot_radius_xy(radius, image_aspect);
    let clamp = |v: f32, r: f32| if r >= 0.5 { 0.5 } else { v.clamp(r, 1.0 - r) };
    [clamp(center[0], rx), clamp(center[1], ry)]
}

/// A source position next to `target` — right, then left, below, above —
/// far enough that the circles don't overlap, and fully inside the image.
/// A placeholder until source selection matches surroundings (ADR-0018 step 4).
pub fn default_source(target: [f32; 2], radius: f32, image_aspect: f32) -> [f32; 2] {
    let [rx, ry] = spot_radius_xy(radius, image_aspect);
    let (dx, dy) = (rx * 2.2, ry * 2.2);
    let candidates = [
        [target[0] + dx, target[1]],
        [target[0] - dx, target[1]],
        [target[0], target[1] + dy],
        [target[0], target[1] - dy],
    ];
    candidates
        .into_iter()
        .find(|c| clamp_center(*c, radius, image_aspect) == *c)
        .unwrap_or_else(|| clamp_center(candidates[0], radius, image_aspect))
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
/// A mask's own adjustments — the global color sliders and selective color,
/// neutral at zero (ADR-0019).
pub struct MaskAdjust {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub temperature: f32,
    pub saturation: f32,
    pub hue_shift: f32,
    /// Same bands as `EditState::selective_color`. Omitted from the sidecar
    /// while untouched.
    #[serde(skip_serializing_if = "is_neutral_selective")]
    pub selective_color: [HslAdjust; 8],
}

fn is_neutral_selective(bands: &[HslAdjust; 8]) -> bool {
    bands.iter().all(|b| *b == HslAdjust::default())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One brush stroke. Like spots, `points` are normalized source-image
/// coordinates (before geometry) and `radius` is a fraction of the shorter
/// side (ADR-0019).
pub struct Stroke {
    pub points: Vec<[f32; 2]>,
    pub radius: f32,
    /// Soft edge width as a fraction of `radius`, 0–1.
    pub feather: f32,
    /// Subtracts from the mask instead of adding to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub erase: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// A named painted mask and the adjustments applied through it.
pub struct Mask {
    pub name: String,
    /// Whether the mask's adjustments are applied. Hidden masks keep their
    /// paint and settings but change nothing, in previews or exports.
    #[serde(default = "enabled_default", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default)]
    pub strokes: Vec<Stroke>,
    #[serde(default)]
    pub adjust: MaskAdjust,
}

fn enabled_default() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
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
    /// Spot-removal patches, applied in order before geometry (ADR-0018).
    /// Omitted from the sidecar when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spots: Vec<Spot>,
    /// Painted adjustment masks, applied in order after the global color
    /// adjustments (ADR-0019). Omitted from the sidecar when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<Mask>,
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
            spots: Vec::new(),
            masks: Vec::new(),
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

    /// Brings the sidecar in line with this state: written when there are
    /// edits, removed — with its edited thumbnail — when there are none (so
    /// resetting everything doesn't leave old edits to come back). Returns
    /// whether the sidecar changed.
    pub fn sync_sidecar(&self, image_path: &Path) -> anyhow::Result<bool> {
        let sidecar = sidecar_path(image_path);
        let existing = std::fs::read_to_string(&sidecar).ok();
        if !self.has_edits() {
            let _ = std::fs::remove_file(edited_thumbnail_path(image_path));
            if existing.is_some() {
                std::fs::remove_file(&sidecar)?;
                return Ok(true);
            }
            return Ok(false);
        }
        let json = serde_json::to_string_pretty(self)?;
        if existing.as_deref() == Some(json.as_str()) {
            return Ok(false);
        }
        self.save(image_path)?;
        Ok(true)
    }

    /// Whether anything differs from the unedited default.
    pub fn has_edits(&self) -> bool {
        serde_json::to_string(self).ok() != serde_json::to_string(&EditState::default()).ok()
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

/// Thumbnail showing an edited photo's edits, saved beside its sidecar:
/// `.edits/<filename>.webp`. The library shows it in place of the plain
/// thumbnail while it exists.
pub fn edited_thumbnail_path(image_path: &Path) -> std::path::PathBuf {
    let dir = image_path.parent().unwrap_or(Path::new("."));
    let filename = image_path.file_name().unwrap_or_default().to_string_lossy();
    dir.join(".edits").join(format!("{}.webp", filename))
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
    fn sidecars_without_spots_still_load_and_stay_spot_free() {
        let json = r#"{"rotate": 90, "exposure": 0.5}"#;
        let state: EditState = serde_json::from_str(json).unwrap();
        assert!(state.spots.is_empty());
        assert!(!serde_json::to_string(&state).unwrap().contains("spots"));
    }

    #[test]
    fn spots_round_trip_through_json() {
        let mut state = EditState::default();
        state.spots.push(Spot::new([0.5, 0.5], 0.05, 1.5));
        let back: EditState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(back.spots, state.spots);
    }

    #[test]
    fn masks_round_trip_and_stay_out_of_mask_free_sidecars() {
        let mut state = EditState::default();
        assert!(!serde_json::to_string(&state).unwrap().contains("masks"));
        state.masks.push(Mask {
            name: "Face".into(),
            enabled: true,
            strokes: vec![Stroke {
                points: vec![[0.1, 0.2], [0.3, 0.4]],
                radius: 0.05,
                feather: 0.5,
                erase: false,
            }],
            adjust: MaskAdjust {
                exposure: 0.5,
                ..Default::default()
            },
        });
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("erase"), "paint strokes omit the erase flag");
        assert!(!json.contains("enabled"), "visible masks omit the flag");
        let old: EditState =
            serde_json::from_str(r#"{"masks":[{"name":"Old","strokes":[]}]}"#).unwrap();
        assert!(old.masks[0].enabled, "masks saved before the flag load as visible");
        let adjust_json = serde_json::to_string(&state.masks[0].adjust).unwrap();
        assert!(!adjust_json.contains("selective_color"), "untouched bands are omitted");
        state.masks[0].adjust.selective_color[3].saturation = -0.5;
        let json = serde_json::to_string(&state).unwrap();
        let back: EditState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.masks, state.masks);
    }

    #[test]
    fn spot_radius_is_relative_to_the_shorter_side() {
        assert_eq!(spot_radius_xy(0.1, 2.0), [0.05, 0.1]); // landscape
        assert_eq!(spot_radius_xy(0.1, 0.5), [0.1, 0.05]); // portrait
    }

    #[test]
    fn default_source_sits_beside_target_inside_the_image() {
        let aspect = 1.5;
        let r = 0.05;
        let [rx, _] = spot_radius_xy(r, aspect);
        // Room on the right: source goes right, clear of the target.
        let s = default_source([0.5, 0.5], r, aspect);
        assert!(s[0] - 0.5 >= 2.0 * rx && s[1] == 0.5);
        // Against the right edge: falls back to the left.
        let t = clamp_center([1.0, 0.5], r, aspect);
        let s = default_source(t, r, aspect);
        assert!(s[0] < t[0]);
        assert_eq!(clamp_center(s, r, aspect), s);
    }

    #[test]
    fn spot_new_keeps_the_whole_target_circle_inside() {
        let spot = Spot::new([0.0, 1.0], 0.05, 1.0);
        assert_eq!(spot.target, [0.05, 0.95]);
    }

    #[test]
    fn sync_sidecar_writes_only_on_change_and_removes_when_reset() {
        let dir = std::env::temp_dir().join(format!("photograph-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("a.jpg");
        let mut state = EditState::default();
        assert!(!state.sync_sidecar(&image).unwrap(), "nothing to write or remove");
        state.exposure = 0.5;
        assert!(state.sync_sidecar(&image).unwrap(), "first write");
        assert!(!state.sync_sidecar(&image).unwrap(), "unchanged");
        std::fs::write(edited_thumbnail_path(&image), b"thumb").unwrap();
        state.exposure = 0.0;
        assert!(state.sync_sidecar(&image).unwrap(), "reset removes the sidecar");
        assert!(!sidecar_path(&image).exists());
        assert!(!edited_thumbnail_path(&image).exists(), "and its edited thumbnail");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidecar_uses_edits_folder() {
        let p = sidecar_path(Path::new("/photos/IMG_001.RAF"));
        assert_eq!(p, PathBuf::from("/photos/.edits/IMG_001.RAF.json"));
    }
}
