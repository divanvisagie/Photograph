use image::{DynamicImage, Rgba};
use imageproc::geometric_transformations::{rotate_about_center, Interpolation};

use crate::state::EditState;

/// Apply all geometry transforms from `state` to `img`.
/// Order: straighten → orthogonal rotate → flip.
pub fn apply(img: &DynamicImage, state: &EditState) -> DynamicImage {
    let mut out = img.clone();

    // Straighten — arbitrary angle, bilinear interpolation
    if state.straighten.abs() > 0.01 {
        let rgba = out.to_rgba8();
        let rotated = rotate_about_center(
            &rgba,
            state.straighten.to_radians(),
            Interpolation::Bilinear,
            Rgba([0u8, 0u8, 0u8, 255u8]),
        );
        out = DynamicImage::ImageRgba8(rotated);
    }

    // Orthogonal rotate
    out = match state.rotate.rem_euclid(360) {
        90 => out.rotate90(),
        180 => out.rotate180(),
        270 => out.rotate270(),
        _ => out,
    };

    // Flip
    if state.flip_h {
        out = out.fliph();
    }
    if state.flip_v {
        out = out.flipv();
    }

    out
}
