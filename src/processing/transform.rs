use image::{DynamicImage, Rgba};
use imageproc::geometric_transformations::{Interpolation, Projection, rotate_about_center, warp};

use crate::state::{EditState, Keystone};

use super::{color, exposure, filters};

/// Apply all geometry transforms from `state` to `img`.
/// Order: straighten → keystone → orthogonal rotate → flip → crop.
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

    // Keystone (perspective) correction
    if state.keystone.vertical.abs() > 0.001 || state.keystone.horizontal.abs() > 0.001 {
        out = apply_keystone(out, &state.keystone);
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

    // Crop (applied last, in normalized 0.0–1.0 coordinates)
    if let Some(ref crop) = state.crop {
        let w = out.width() as f32;
        let h = out.height() as f32;
        let cx = (crop.x * w) as u32;
        let cy = (crop.y * h) as u32;
        let cw = (crop.width * w).min(w - cx as f32) as u32;
        let ch = (crop.height * h).min(h - cy as f32) as u32;
        if cw > 0 && ch > 0 {
            out = out.crop_imm(cx, cy, cw, ch);
        }
    }

    out = exposure::apply(out, state);
    out = color::apply(out, state);
    out = filters::apply(out, state);

    out
}

/// Apply keystone (perspective) correction.
///
/// `vertical` shifts top corners inward (positive) or bottom corners inward (negative).
/// `horizontal` shifts left corners inward (positive) or right corners inward (negative).
/// Both values are in the range ±0.5, scaled by the image dimensions.
fn apply_keystone(img: DynamicImage, keystone: &Keystone) -> DynamicImage {
    let rgba = img.to_rgba8();
    let w = rgba.width() as f32;
    let h = rgba.height() as f32;

    let v = keystone.vertical;
    let hz = keystone.horizontal;

    // Source corners: top-left, top-right, bottom-right, bottom-left
    let src: [(f32, f32); 4] = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];

    // Destination corners with perspective shifts applied
    let dst: [(f32, f32); 4] = [
        // top-left: shift right for +v, shift down for +h
        (v.max(0.0) * w, hz.max(0.0) * h),
        // top-right: shift left for +v, shift down for -h
        (w - v.max(0.0) * w, (-hz).max(0.0) * h),
        // bottom-right: shift left for -v, shift up for -h
        (w - (-v).max(0.0) * w, h - (-hz).max(0.0) * h),
        // bottom-left: shift right for -v, shift up for +h
        ((-v).max(0.0) * w, h - hz.max(0.0) * h),
    ];

    let Some(projection) = Projection::from_control_points(src, dst) else {
        return img;
    };

    let warped = warp(
        &rgba,
        &projection,
        Interpolation::Bilinear,
        Rgba([0, 0, 0, 255]),
    );
    DynamicImage::ImageRgba8(warped)
}
