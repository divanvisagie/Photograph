//! Spot removal (ADR-0018): the first pipeline stage, run on the source
//! image before geometry and color.
//!
//! Every spot reads from the *unretouched* image and blends into the result
//! in list order, so overlapping spots never clone each other's patches and
//! the GPU can do the whole stage in one per-pixel pass. `SpotPx` is the one
//! place spot parameters become pixels; the CPU path here and the GPU pass
//! both consume it, which keeps them in parity.

use image::{DynamicImage, RgbaImage};

use crate::state::Spot;

/// A spot resolved to pixel units for a `width`×`height` image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpotPx {
    /// Target center, in continuous pixel coordinates (pixel centers at +0.5).
    pub cx: f32,
    pub cy: f32,
    pub radius: f32,
    /// Radius inside which the patch is fully opaque.
    pub inner: f32,
    /// Whole-pixel offset from a target pixel to the pixel it copies.
    pub dx: i32,
    pub dy: i32,
}

impl SpotPx {
    pub fn new(spot: &Spot, width: u32, height: u32) -> Self {
        let (w, h) = (width as f32, height as f32);
        let radius = spot.radius * w.min(h);
        let feather = spot.feather.clamp(0.0, 1.0);
        Self {
            cx: spot.target[0] * w,
            cy: spot.target[1] * h,
            radius,
            inner: radius * (1.0 - feather),
            dx: ((spot.source[0] - spot.target[0]) * w).round() as i32,
            dy: ((spot.source[1] - spot.target[1]) * h).round() as i32,
        }
    }

    /// Patch opacity for the pixel whose center is `(px, py)`: 1 inside
    /// `inner`, smoothly falling to 0 at `radius`.
    pub fn alpha(&self, px: f32, py: f32) -> f32 {
        let d = ((px - self.cx).powi(2) + (py - self.cy).powi(2)).sqrt();
        if d >= self.radius {
            0.0
        } else if d <= self.inner {
            1.0
        } else {
            1.0 - smoothstep(self.inner, self.radius, d)
        }
    }
}

/// Same definition as WGSL's `smoothstep`.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Applies `spots` to `img` (CPU path; the GPU pass mirrors it).
pub fn apply(img: &DynamicImage, spots: &[Spot]) -> DynamicImage {
    let src = img.to_rgba8();
    DynamicImage::ImageRgba8(apply_rgba(&src, spots))
}

fn apply_rgba(src: &RgbaImage, spots: &[Spot]) -> RgbaImage {
    let (w, h) = src.dimensions();
    let mut out = src.clone();
    if w == 0 || h == 0 {
        return out;
    }
    for spot in spots {
        let s = SpotPx::new(spot, w, h);
        // Only pixels in the target circle's bounding box can change.
        let x0 = (s.cx - s.radius).floor().max(0.0) as u32;
        let y0 = (s.cy - s.radius).floor().max(0.0) as u32;
        let x1 = ((s.cx + s.radius).ceil() as u32).min(w);
        let y1 = ((s.cy + s.radius).ceil() as u32).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let a = s.alpha(x as f32 + 0.5, y as f32 + 0.5);
                if a <= 0.0 {
                    continue;
                }
                let sx = (x as i32 + s.dx).clamp(0, w as i32 - 1) as u32;
                let sy = (y as i32 + s.dy).clamp(0, h as i32 - 1) as u32;
                let from = src.get_pixel(sx, sy).0;
                let px = out.get_pixel_mut(x, y);
                for c in 0..4 {
                    let cur = px.0[c] as f32 / 255.0;
                    let new = from[c] as f32 / 255.0;
                    px.0[c] = ((cur + (new - cur) * a) * 255.0).round() as u8;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Spot, SpotMode};
    use image::Rgba;

    fn spot(target: [f32; 2], source: [f32; 2], radius: f32, feather: f32) -> Spot {
        Spot {
            target,
            source,
            radius,
            feather,
            mode: SpotMode::Clone,
        }
    }

    /// Left half black, right half white.
    fn split_image() -> RgbaImage {
        RgbaImage::from_fn(40, 20, |x, _| {
            if x < 20 { Rgba([0, 0, 0, 255]) } else { Rgba([255, 255, 255, 255]) }
        })
    }

    #[test]
    fn hard_edged_clone_copies_source_inside_and_nothing_outside() {
        let img = split_image();
        // Target in the black half, source in the white half.
        let out = apply_rgba(&img, &[spot([0.25, 0.5], [0.75, 0.5], 0.25, 0.0)]);
        assert_eq!(out.get_pixel(10, 10).0, [255, 255, 255, 255], "center is cloned");
        assert_eq!(out.get_pixel(0, 0).0, [0, 0, 0, 255], "outside the circle is untouched");
        assert_eq!(out.get_pixel(30, 10).0, [255, 255, 255, 255], "source is untouched");
    }

    #[test]
    fn feathered_edge_blends_between_target_and_source() {
        let img = split_image();
        let s = spot([0.25, 0.5], [0.75, 0.5], 0.4, 1.0);
        let out = apply_rgba(&img, &[s.clone()]);
        let px = SpotPx::new(&s, 40, 20);
        // A pixel partway out from the center gets a partial blend.
        let (x, y) = (14, 10);
        let a = px.alpha(x as f32 + 0.5, y as f32 + 0.5);
        assert!(a > 0.05 && a < 0.95, "alpha {a}");
        let v = out.get_pixel(x, y).0[0];
        assert_eq!(v, (a * 255.0).round() as u8);
    }

    #[test]
    fn overlapping_spots_read_the_unretouched_image() {
        let img = split_image();
        // First spot whitens the black-side area; the second copies from
        // that same area and must still get the original black.
        let spots = [
            spot([0.25, 0.5], [0.75, 0.5], 0.25, 0.0),
            spot([0.75, 0.5], [0.25, 0.5], 0.25, 0.0),
        ];
        let out = apply_rgba(&img, &spots);
        assert_eq!(out.get_pixel(10, 10).0, [255, 255, 255, 255]);
        assert_eq!(out.get_pixel(30, 10).0, [0, 0, 0, 255]);
    }

    #[test]
    fn spot_pixels_scale_with_resolution() {
        let s = spot([0.5, 0.5], [0.6, 0.5], 0.1, 0.5);
        let small = SpotPx::new(&s, 400, 300);
        let large = SpotPx::new(&s, 4000, 3000);
        assert_eq!((small.radius, small.dx), (30.0, 40));
        assert_eq!((large.radius, large.dx), (300.0, 400));
        assert_eq!(large.inner, 150.0);
    }
}
