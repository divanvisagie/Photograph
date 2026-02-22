use image::DynamicImage;
use imageproc::filter::gaussian_blur_f32;

use crate::state::EditState;

/// Applies an unsharp-mask style sharpening pass.
pub fn apply(img: DynamicImage, state: &EditState) -> DynamicImage {
    if state.sharpness < 0.001 {
        return img;
    }

    let amount = state.sharpness;
    let sigma = 1.5_f32;

    let rgba = img.to_rgba8();
    let blurred = gaussian_blur_f32(&rgba, sigma);

    let mut out = rgba.clone();
    for (o, (s, b)) in out.pixels_mut().zip(rgba.pixels().zip(blurred.pixels())) {
        for c in 0..3 {
            let sharp = s[c] as f32 + amount * (s[c] as f32 - b[c] as f32);
            o[c] = sharp.round().clamp(0.0, 255.0) as u8;
        }
        // preserve alpha
    }

    DynamicImage::ImageRgba8(out)
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, Rgba};

    use crate::state::EditState;

    use super::apply;

    #[test]
    fn zero_sharpness_is_identity() {
        let img =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(4, 4, Rgba([128, 128, 128, 255])));
        let state = EditState::default();
        let out = apply(img.clone(), &state);
        assert_eq!(img.to_rgba8(), out.to_rgba8());
    }

    #[test]
    fn positive_sharpness_changes_pixels() {
        // Create image with an edge: left half dark, right half bright
        let mut buf = ImageBuffer::from_pixel(8, 1, Rgba([200u8, 200, 200, 255]));
        for x in 0..4 {
            buf.put_pixel(x, 0, Rgba([50, 50, 50, 255]));
        }
        let img = DynamicImage::ImageRgba8(buf);
        let mut state = EditState::default();
        state.sharpness = 1.0;
        let out = apply(img.clone(), &state);
        // The edge pixels should differ from the original
        assert_ne!(img.to_rgba8(), out.to_rgba8());
    }
}
