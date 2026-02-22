use image::DynamicImage;

use crate::state::EditState;

// red, orange, yellow, green, cyan, blue, purple, pink
const SELECTIVE_CENTERS_DEG: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 285.0, 330.0];
const SELECTIVE_HALF_WIDTH_DEG: f32 = 30.0;

/// Applies white balance, global HSL, and selective color adjustments.
pub fn apply(img: DynamicImage, state: &EditState) -> DynamicImage {
    let any_selective = state
        .selective_color
        .iter()
        .any(|a| a.hue.abs() > 0.001 || a.saturation.abs() > 0.001 || a.lightness.abs() > 0.001);
    if state.temperature.abs() < 0.001
        && state.saturation.abs() < 0.001
        && state.hue_shift.abs() < 0.001
        && !any_selective
    {
        return img;
    }

    let temp = state.temperature.clamp(-1.0, 1.0);
    let sat_adjust = state.saturation.clamp(-1.0, 1.0);
    let hue_shift_unit = state.hue_shift / 360.0;

    let mut rgba = img.to_rgba8();
    for px in rgba.pixels_mut() {
        let mut r = px[0] as f32 / 255.0;
        let mut g = px[1] as f32 / 255.0;
        let mut b = px[2] as f32 / 255.0;

        // White balance: positive warms (more red, less blue), negative cools.
        if temp > 0.0 {
            r += (1.0 - r) * temp * 0.25;
            b *= 1.0 - temp * 0.25;
        } else if temp < 0.0 {
            let cool = -temp;
            b += (1.0 - b) * cool * 0.25;
            r *= 1.0 - cool * 0.25;
        }
        r = r.clamp(0.0, 1.0);
        g = g.clamp(0.0, 1.0);
        b = b.clamp(0.0, 1.0);

        let (mut h, mut s, mut l) = rgb_to_hsl(r, g, b);

        // Global HSL
        h = wrap_unit(h + hue_shift_unit);
        s = (s * (1.0 + sat_adjust)).clamp(0.0, 1.0);

        // Selective color by hue ranges.
        for (idx, adj) in state.selective_color.iter().enumerate() {
            if adj.hue.abs() < 0.001 && adj.saturation.abs() < 0.001 && adj.lightness.abs() < 0.001
            {
                continue;
            }
            let weight = selective_weight(h, SELECTIVE_CENTERS_DEG[idx], SELECTIVE_HALF_WIDTH_DEG);
            if weight <= 0.0 {
                continue;
            }
            h = wrap_unit(h + (adj.hue / 360.0) * weight);
            s = (s * (1.0 + adj.saturation * weight)).clamp(0.0, 1.0);
            l = (l + adj.lightness * weight).clamp(0.0, 1.0);
        }

        let (r2, g2, b2) = hsl_to_rgb(h, s, l);
        px[0] = (r2 * 255.0).round() as u8;
        px[1] = (g2 * 255.0).round() as u8;
        px[2] = (b2 * 255.0).round() as u8;
    }

    DynamicImage::ImageRgba8(rgba)
}

fn selective_weight(hue_unit: f32, center_deg: f32, half_width_deg: f32) -> f32 {
    let hue_deg = wrap_unit(hue_unit) * 360.0;
    let dist = hue_distance_deg(hue_deg, center_deg);
    if dist >= half_width_deg {
        0.0
    } else {
        1.0 - (dist / half_width_deg)
    }
}

fn hue_distance_deg(a: f32, b: f32) -> f32 {
    let diff = (a - b).abs();
    diff.min(360.0 - diff)
}

fn wrap_unit(mut v: f32) -> f32 {
    while v < 0.0 {
        v += 1.0;
    }
    while v >= 1.0 {
        v -= 1.0;
    }
    v
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g.max(b));
    let min = r.min(g.min(b));
    let l = (max + min) * 0.5;
    let d = max - min;

    if d <= 1e-6 {
        return (0.0, 0.0, l);
    }

    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let mut h = if (max - r).abs() < f32::EPSILON {
        ((g - b) / d) % 6.0
    } else if (max - g).abs() < f32::EPSILON {
        ((b - r) / d) + 2.0
    } else {
        ((r - g) / d) + 4.0
    };
    h /= 6.0;
    h = wrap_unit(h);
    (h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s <= 1e-6 {
        return (l, l, l);
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;

    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);
    (r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0))
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    t = wrap_unit(t);
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 0.5 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
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

    fn pixel_rgb(img: &DynamicImage) -> [u8; 3] {
        let p = img.to_rgba8().get_pixel(0, 0).0;
        [p[0], p[1], p[2]]
    }

    #[test]
    fn hue_shift_rotates_red_toward_green() {
        let mut state = EditState::default();
        state.hue_shift = 120.0;
        let out = apply(one_pixel([255, 0, 0]), &state);
        let rgb = pixel_rgb(&out);
        assert!(rgb[1] > rgb[0]);
    }

    #[test]
    fn positive_temperature_warms_image() {
        let mut state = EditState::default();
        state.temperature = 1.0;
        let out = apply(one_pixel([120, 120, 120]), &state);
        let rgb = pixel_rgb(&out);
        assert!(rgb[0] > rgb[2]);
    }

    #[test]
    fn selective_red_saturation_reduction_affects_red_pixel() {
        let mut state = EditState::default();
        state.selective_color[0].saturation = -1.0; // red range
        let out = apply(one_pixel([255, 32, 32]), &state);
        let rgb = pixel_rgb(&out);
        assert!((rgb[0] as i32 - rgb[1] as i32).abs() < (255 - 32));
    }
}
