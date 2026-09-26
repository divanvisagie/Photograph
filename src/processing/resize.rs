//! Fast, parallel downscaling for previews and thumbnails.
//!
//! `image`'s generic resize/thumbnail filters cost ~150ms to take a 24 MP
//! photo to preview size. A box (area-average) filter is all a downscale
//! needs, and rows parallelize trivially, so these do it in ~10ms.

use image::{DynamicImage, RgbaImage};
use rayon::prelude::*;

/// Output size fitting `w`×`h` within `cap` on the longest side, keeping the
/// aspect ratio. Never upscales.
pub fn fit_within(w: u32, h: u32, cap: u32) -> (u32, u32) {
    if w <= cap && h <= cap {
        return (w, h);
    }
    let scale = cap as f64 / w.max(h) as f64;
    (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    )
}

/// Source index range covered by output index `i` when shrinking `src`
/// samples to `dst`: every source sample belongs to exactly one output.
fn span(i: u32, src: u32, dst: u32) -> (usize, usize) {
    let start = (i as u64 * src as u64 / dst as u64) as usize;
    let end = (((i as u64 + 1) * src as u64 / dst as u64) as usize).max(start + 1);
    (start, end.min(src as usize))
}

/// Box-downscales interleaved `C`-channel samples of type `T` from `w`×`h`
/// to `ow`×`oh`, averaging each output cell's source pixels in `f32`.
fn box_downscale<T, const C: usize>(
    src: &[T],
    w: u32,
    h: u32,
    ow: u32,
    oh: u32,
    to_f32: impl Fn(T) -> f32 + Sync,
    from_f32: impl Fn(f32) -> T + Sync,
) -> Vec<T>
where
    T: Copy + Default + Send + Sync,
{
    let columns: Vec<(usize, usize)> = (0..ow).map(|x| span(x, w, ow)).collect();
    let mut out = vec![T::default(); ow as usize * oh as usize * C];
    out.par_chunks_mut(ow as usize * C)
        .enumerate()
        .for_each(|(y, row)| {
            let (y0, y1) = span(y as u32, h, oh);
            let mut acc = vec![[0.0f32; C]; ow as usize];
            for sy in y0..y1 {
                let line = &src[sy * w as usize * C..][..w as usize * C];
                for (a, &(x0, x1)) in acc.iter_mut().zip(&columns) {
                    for px in line[x0 * C..x1 * C].chunks_exact(C) {
                        for c in 0..C {
                            a[c] += to_f32(px[c]);
                        }
                    }
                }
            }
            for ((a, &(x0, x1)), out_px) in acc.iter().zip(&columns).zip(row.chunks_exact_mut(C)) {
                let n = ((x1 - x0) * (y1 - y0)) as f32;
                for c in 0..C {
                    out_px[c] = from_f32(a[c] / n);
                }
            }
        });
    out
}

/// `img` as RGBA8 fitting within `cap`, for display. 8-bit RGB and RGBA are
/// scaled directly; other formats are converted first.
pub fn downscale_rgba8(img: &DynamicImage, cap: u32) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    let (ow, oh) = fit_within(w, h, cap);
    let u8_to = |v: u8| v as f32;
    let to_u8 = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    match img {
        DynamicImage::ImageRgb8(rgb) => {
            let small = if (ow, oh) == (w, h) {
                rgb.as_raw().clone()
            } else {
                box_downscale::<u8, 3>(rgb.as_raw(), w, h, ow, oh, u8_to, to_u8)
            };
            let rgba = small
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect();
            RgbaImage::from_raw(ow, oh, rgba).expect("buffer matches dimensions")
        }
        _ => {
            let converted;
            let rgba = match img.as_rgba8() {
                Some(rgba) => rgba,
                None => {
                    converted = img.to_rgba8();
                    &converted
                }
            };
            if (ow, oh) == (w, h) {
                return rgba.clone();
            }
            let small = box_downscale::<u8, 4>(rgba.as_raw(), w, h, ow, oh, u8_to, to_u8);
            RgbaImage::from_raw(ow, oh, small).expect("buffer matches dimensions")
        }
    }
}

/// Linear RGB pixels fitting within `cap`, averaged in linear light (the
/// physically right way to shrink an image, unlike averaging after gamma).
pub fn downscale_linear_rgb(
    pixels: &[[f32; 3]],
    w: u32,
    h: u32,
    cap: u32,
) -> (Vec<[f32; 3]>, u32, u32) {
    let (ow, oh) = fit_within(w, h, cap);
    if (ow, oh) == (w, h) {
        return (pixels.to_vec(), w, h);
    }
    let flat: &[f32] = bytemuck::cast_slice(pixels);
    let small = box_downscale::<f32, 3>(flat, w, h, ow, oh, |v| v, |v| v);
    let small = small.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
    (small, ow, oh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_within_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_within(6000, 4000, 1920), (1920, 1280));
        assert_eq!(fit_within(4000, 6000, 1920), (1280, 1920));
        assert_eq!(fit_within(800, 600, 1920), (800, 600));
    }

    #[test]
    fn spans_cover_every_source_sample_exactly_once() {
        let (src, dst) = (6000, 1920);
        let mut next = 0;
        for i in 0..dst {
            let (s, e) = span(i, src, dst);
            assert_eq!(s, next);
            assert!(e > s);
            next = e;
        }
        assert_eq!(next, src as usize);
    }

    #[test]
    fn box_downscale_averages_cells() {
        // 4×2 RGBA: left half black, right half white → 2×1: black, white.
        let img = RgbaImage::from_fn(4, 2, |x, _| {
            if x < 2 { image::Rgba([0, 0, 0, 255]) } else { image::Rgba([255, 255, 255, 255]) }
        });
        let out = downscale_rgba8(&DynamicImage::ImageRgba8(img), 2);
        assert_eq!(out.dimensions(), (2, 1));
        assert_eq!(out.get_pixel(0, 0).0, [0, 0, 0, 255]);
        assert_eq!(out.get_pixel(1, 0).0, [255, 255, 255, 255]);
        // A 2×2 checker of black/white averages to mid grey.
        let checker = RgbaImage::from_fn(2, 2, |x, y| {
            let v = if (x + y) % 2 == 0 { 0 } else { 255 };
            image::Rgba([v, v, v, 255])
        });
        let grey = downscale_rgba8(&DynamicImage::ImageRgba8(checker), 1);
        assert_eq!(grey.get_pixel(0, 0).0, [128, 128, 128, 255]);
    }

    #[test]
    fn rgb8_input_gets_opaque_alpha() {
        let img = image::RgbImage::from_pixel(10, 10, image::Rgb([10, 20, 30]));
        let out = downscale_rgba8(&DynamicImage::ImageRgb8(img), 5);
        assert_eq!(out.dimensions(), (5, 5));
        assert!(out.pixels().all(|p| p.0 == [10, 20, 30, 255]));
    }

    #[test]
    fn linear_downscale_averages_in_linear_light() {
        let px = vec![[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]];
        let (out, w, h) = downscale_linear_rgb(&px, 2, 1, 1);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out[0], [0.5, 0.5, 0.5]);
    }
}
