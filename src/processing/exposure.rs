use image::DynamicImage;

use crate::state::EditState;

/// Applies exposure, contrast, highlights, and shadows adjustments.
pub fn apply(img: DynamicImage, state: &EditState) -> DynamicImage {
    if state.exposure.abs() < 0.001
        && state.contrast.abs() < 0.001
        && state.highlights.abs() < 0.001
        && state.shadows.abs() < 0.001
    {
        return img;
    }

    let exposure_gain = 2.0_f32.powf(state.exposure.clamp(-5.0, 5.0));
    let contrast_gain = 1.0 + state.contrast.clamp(-1.0, 1.0);
    let highlights = state.highlights.clamp(-1.0, 1.0);
    let shadows = state.shadows.clamp(-1.0, 1.0);

    let mut rgba = img.to_rgba8();
    for px in rgba.pixels_mut() {
        let mut r = px[0] as f32 / 255.0;
        let mut g = px[1] as f32 / 255.0;
        let mut b = px[2] as f32 / 255.0;

        // Global exposure + contrast around mid-gray.
        r = ((r * exposure_gain - 0.5) * contrast_gain + 0.5).clamp(0.0, 1.0);
        g = ((g * exposure_gain - 0.5) * contrast_gain + 0.5).clamp(0.0, 1.0);
        b = ((b * exposure_gain - 0.5) * contrast_gain + 0.5).clamp(0.0, 1.0);

        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let mut target_luma = luma;

        if shadows.abs() > 0.001 {
            let w = 1.0 - smoothstep(0.0, 0.5, target_luma);
            if shadows >= 0.0 {
                target_luma += (1.0 - target_luma) * shadows * w;
            } else {
                target_luma *= 1.0 + shadows * w;
            }
        }

        if highlights.abs() > 0.001 {
            let w = smoothstep(0.5, 1.0, target_luma);
            if highlights >= 0.0 {
                target_luma += (1.0 - target_luma) * highlights * w;
            } else {
                target_luma *= 1.0 + highlights * w;
            }
        }

        let scale = if luma > 1e-5 { target_luma / luma } else { 1.0 };
        r = (r * scale).clamp(0.0, 1.0);
        g = (g * scale).clamp(0.0, 1.0);
        b = (b * scale).clamp(0.0, 1.0);

        px[0] = (r * 255.0).round() as u8;
        px[1] = (g * 255.0).round() as u8;
        px[2] = (b * 255.0).round() as u8;
    }

    DynamicImage::ImageRgba8(rgba)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, Rgba};

    use crate::state::EditState;

    use super::apply;

    fn one_pixel(rgb: [u8; 3]) -> DynamicImage {
        DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            1,
            1,
            Rgba([rgb[0], rgb[1], rgb[2], 255]),
        ))
    }

    fn red(img: &DynamicImage) -> u8 {
        img.to_rgba8().get_pixel(0, 0)[0]
    }

    #[test]
    fn exposure_positive_brightens() {
        let mut state = EditState::default();
        state.exposure = 1.0;
        let out = apply(one_pixel([64, 64, 64]), &state);
        assert!(red(&out) > 64);
    }

    #[test]
    fn shadows_positive_lifts_darks() {
        let mut state = EditState::default();
        state.shadows = 1.0;
        let out = apply(one_pixel([24, 24, 24]), &state);
        assert!(red(&out) > 24);
    }

    #[test]
    fn highlights_negative_reduces_brights() {
        let mut state = EditState::default();
        state.highlights = -1.0;
        let out = apply(one_pixel([240, 240, 240]), &state);
        assert!(red(&out) < 240);
    }
}
