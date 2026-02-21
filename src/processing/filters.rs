use image::DynamicImage;

use crate::state::EditState;

pub fn apply(img: DynamicImage, state: &EditState) -> DynamicImage {
    let Some(grad) = state.graduated_filter.as_ref() else {
        return img;
    };
    if grad.exposure.abs() < 0.001 {
        return img;
    }

    let top = grad.top.clamp(0.0, 1.0);
    let bottom = grad.bottom.clamp(0.0, 1.0);
    if bottom <= top + 0.0001 {
        return img;
    }

    let exposure = grad.exposure.clamp(-5.0, 5.0);
    let mut rgba = img.to_rgba8();
    let h = rgba.height().max(1);
    let h_denom = (h - 1).max(1) as f32;

    for (y, row) in rgba.enumerate_rows_mut() {
        let y_norm = y as f32 / h_denom;
        let weight = if y_norm <= top {
            1.0
        } else if y_norm >= bottom {
            0.0
        } else {
            (bottom - y_norm) / (bottom - top)
        };
        if weight <= 0.0 {
            continue;
        }
        let gain = 2.0_f32.powf(exposure * weight);
        for (_, _, px) in row {
            for c in 0..3 {
                let v = px[c] as f32 / 255.0;
                px[c] = (v * gain).clamp(0.0, 1.0).mul_add(255.0, 0.0).round() as u8;
            }
        }
    }

    DynamicImage::ImageRgba8(rgba)
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, Rgba};

    use crate::state::{EditState, GradFilter};

    use super::apply;

    #[test]
    fn negative_exposure_darkens_top_more_than_bottom() {
        let img = DynamicImage::ImageRgba8(ImageBuffer::from_fn(1, 2, |_x, _y| {
            Rgba([200, 200, 200, 255])
        }));
        let mut state = EditState::default();
        state.graduated_filter = Some(GradFilter {
            top: 0.0,
            bottom: 1.0,
            exposure: -1.0,
        });

        let out = apply(img, &state).to_rgba8();
        assert!(out.get_pixel(0, 0)[0] < out.get_pixel(0, 1)[0]);
    }
}
