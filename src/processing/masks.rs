//! Painted adjustment masks (ADR-0019): turning brush strokes into coverage.
//!
//! Coverage is 0–1 per pixel. A stroke contributes the brush falloff at a
//! point — 1 within `radius × (1 − feather)` of its path, falling smoothly to
//! 0 at `radius` — and strokes apply in order: paint `c = max(c, s)`, erase
//! `c = c × (1 − s)`. Masks are rasterized in source-image space at a capped
//! resolution; this module is the one definition of that shape, shared by
//! the editor overlay and (from step 2) the CPU and GPU pipelines.

use crate::state::{Mask, Stroke};

/// Longest side, in pixels, masks are rasterized at before being scaled to
/// the image. Masks are soft, so this loses nothing visible and keeps the
/// cost independent of export size.
pub const MASK_RASTER_MAX: u32 = 1024;

/// Raster size for a source image of the given width/height aspect, with
/// the longest side at most `max`.
pub fn raster_size(aspect: f32, max: u32) -> (u32, u32) {
    if aspect >= 1.0 {
        (max, ((max as f32 / aspect).round() as u32).max(1))
    } else {
        (((max as f32 * aspect).round() as u32).max(1), max)
    }
}

/// Brush falloff at distance `d` from the stroke path.
pub fn falloff(d: f32, radius: f32, feather: f32) -> f32 {
    let inner = radius * (1.0 - feather.clamp(0.0, 1.0));
    if d >= radius {
        0.0
    } else if d <= inner {
        1.0
    } else {
        let t = ((d - inner) / (radius - inner)).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    }
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len2 > 0.0 {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let d = [ap[0] - ab[0] * t, ap[1] - ab[1] * t];
    (d[0] * d[0] + d[1] * d[1]).sqrt()
}

/// Rasterizes `mask` into a `width`×`height` coverage buffer (row-major)
/// covering the whole source image.
pub fn rasterize(mask: &Mask, width: u32, height: u32) -> Vec<f32> {
    let (w, h) = (width as usize, height as usize);
    let mut coverage = vec![0.0f32; w * h];
    if w == 0 || h == 0 {
        return coverage;
    }
    let mut scratch = Vec::new();
    for stroke in &mask.strokes {
        apply_stroke(&mut coverage, &mut scratch, stroke, width, height);
    }
    coverage
}

fn apply_stroke(
    coverage: &mut [f32],
    scratch: &mut Vec<f32>,
    stroke: &Stroke,
    width: u32,
    height: u32,
) {
    let (w, h) = (width as f32, height as f32);
    let short = w.min(h);
    // Work in raster pixels: points scale per axis, radius by the short side.
    let points: Vec<[f32; 2]> = stroke.points.iter().map(|p| [p[0] * w, p[1] * h]).collect();
    let Some(first) = points.first().copied() else {
        return;
    };
    let radius = stroke.radius * short;
    if radius <= 0.0 {
        return;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (first[0], first[1], first[0], first[1]);
    for p in &points {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    let bx0 = (x0 - radius).floor().max(0.0) as usize;
    let by0 = (y0 - radius).floor().max(0.0) as usize;
    let bx1 = ((x1 + radius).ceil().max(0.0) as usize).min(width as usize);
    let by1 = ((y1 + radius).ceil().max(0.0) as usize).min(height as usize);
    if bx0 >= bx1 || by0 >= by1 {
        return;
    }
    let bw = bx1 - bx0;

    // The stroke's own coverage over its bounding box: the max falloff over
    // its segments (falloff shrinks with distance, so that's the falloff of
    // the nearest one). Each segment only touches its own bounding box.
    scratch.clear();
    scratch.resize(bw * (by1 - by0), 0.0);
    let segments: Vec<([f32; 2], [f32; 2])> = if points.len() == 1 {
        vec![(first, first)]
    } else {
        points.windows(2).map(|s| (s[0], s[1])).collect()
    };
    for (a, b) in segments {
        let sx0 = ((a[0].min(b[0]) - radius).floor().max(0.0) as usize).max(bx0);
        let sy0 = ((a[1].min(b[1]) - radius).floor().max(0.0) as usize).max(by0);
        let sx1 = ((a[0].max(b[0]) + radius).ceil().max(0.0) as usize).min(bx1);
        let sy1 = ((a[1].max(b[1]) + radius).ceil().max(0.0) as usize).min(by1);
        for y in sy0..sy1 {
            for x in sx0..sx1 {
                let p = [x as f32 + 0.5, y as f32 + 0.5];
                let s = falloff(segment_distance(p, a, b), radius, stroke.feather);
                let cell = &mut scratch[(y - by0) * bw + (x - bx0)];
                *cell = cell.max(s);
            }
        }
    }

    for y in by0..by1 {
        for x in bx0..bx1 {
            let s = scratch[(y - by0) * bw + (x - bx0)];
            let c = &mut coverage[y * width as usize + x];
            *c = if stroke.erase { *c * (1.0 - s) } else { c.max(s) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MaskAdjust;

    fn stroke(points: &[[f32; 2]], radius: f32, feather: f32, erase: bool) -> Stroke {
        Stroke {
            points: points.to_vec(),
            radius,
            feather,
            erase,
        }
    }

    fn mask(strokes: Vec<Stroke>) -> Mask {
        Mask {
            name: "m".into(),
            strokes,
            adjust: MaskAdjust::default(),
        }
    }

    fn at(c: &[f32], w: u32, x: u32, y: u32) -> f32 {
        c[(y * w + x) as usize]
    }

    #[test]
    fn raster_size_caps_the_longest_side() {
        assert_eq!(raster_size(1.5, 1024), (1024, 683));
        assert_eq!(raster_size(0.5, 1024), (512, 1024));
    }

    #[test]
    fn falloff_is_solid_inside_and_smooth_to_the_edge() {
        assert_eq!(falloff(0.0, 10.0, 0.5), 1.0);
        assert_eq!(falloff(5.0, 10.0, 0.5), 1.0);
        let mid = falloff(7.5, 10.0, 0.5);
        assert!((mid - 0.5).abs() < 1e-6);
        assert_eq!(falloff(10.0, 10.0, 0.5), 0.0);
        assert_eq!(falloff(9.99, 10.0, 0.0), 1.0, "feather 0 is a hard edge");
    }

    #[test]
    fn a_horizontal_stroke_covers_its_path_and_nothing_far_away() {
        let (w, h) = (100, 100);
        let c = rasterize(&mask(vec![stroke(&[[0.2, 0.5], [0.8, 0.5]], 0.05, 0.0, false)]), w, h);
        assert_eq!(at(&c, w, 50, 50), 1.0, "on the path");
        assert_eq!(at(&c, w, 20, 52), 1.0, "near an end, within the radius");
        assert_eq!(at(&c, w, 50, 60), 0.0, "past the radius");
        assert_eq!(at(&c, w, 5, 5), 0.0);
    }

    #[test]
    fn erase_strokes_subtract_in_order() {
        let (w, h) = (100, 100);
        let paint = stroke(&[[0.2, 0.5], [0.8, 0.5]], 0.05, 0.0, false);
        let erase = stroke(&[[0.5, 0.5]], 0.1, 0.0, true);
        let c = rasterize(&mask(vec![paint.clone(), erase.clone()]), w, h);
        assert_eq!(at(&c, w, 50, 50), 0.0, "erased");
        assert_eq!(at(&c, w, 25, 50), 1.0, "outside the eraser");
        // Painting again after erasing restores coverage.
        let c = rasterize(&mask(vec![paint.clone(), erase, paint]), w, h);
        assert_eq!(at(&c, w, 50, 50), 1.0);
    }

    #[test]
    fn repainting_never_builds_past_full_coverage() {
        let (w, h) = (50, 50);
        let s = stroke(&[[0.5, 0.5]], 0.2, 1.0, false);
        let once = rasterize(&mask(vec![s.clone()]), w, h);
        let twice = rasterize(&mask(vec![s.clone(), s]), w, h);
        assert_eq!(once, twice);
    }

    #[test]
    fn radius_follows_the_shorter_side_on_wide_images() {
        // 200×100: radius 0.1 of the short side is 10px in both directions.
        let (w, h) = (200, 100);
        let c = rasterize(&mask(vec![stroke(&[[0.5, 0.5]], 0.1, 0.0, false)]), w, h);
        assert_eq!(at(&c, w, 109, 50), 1.0);
        assert_eq!(at(&c, w, 111, 50), 0.0);
        assert_eq!(at(&c, w, 100, 59), 1.0);
        assert_eq!(at(&c, w, 100, 61), 0.0);
    }
}
