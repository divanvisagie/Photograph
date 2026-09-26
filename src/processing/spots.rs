//! Spot removal (ADR-0018): the first pipeline stage, run on the source
//! image before geometry and color.
//!
//! Every spot reads from the *unretouched* image and blends into the result
//! in list order, so overlapping spots never clone each other's patches and
//! the GPU can do the whole stage in one per-pixel pass. `SpotPx` is the one
//! place spot parameters become pixels; the CPU path here and the GPU pass
//! both consume it, which keeps them in parity.

use image::{DynamicImage, RgbaImage};

use crate::state::{Spot, clamp_center, spot_radius_xy};

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

/// How far out the compared ring sits, as a multiple of the spot radius:
/// just outside the circle, where the pixels are unaffected by the blemish.
const RING_SCALE: f32 = 1.3;
const RING_SAMPLES: usize = 32;
/// Candidate distances from the target, in spot radii (closest first, so
/// equal scores keep the nearer source), and angles per distance.
const CANDIDATE_DISTANCES: [f32; 4] = [2.5, 3.5, 5.0, 6.5];
const CANDIDATE_ANGLES: usize = 16;

/// Picks a source for a new spot at `target` on the unedited source image:
/// the nearby circle whose surroundings best match the target's. The
/// target's inside is the blemish, so only the ring just outside each
/// circle is compared, as raw color: Clone doesn't adjust tone, so a match
/// in similar light is what makes the patch seamless.
///
/// Candidates must fit inside the image with their ring, stay clear of the
/// target, and not sit on another spot's target (a known blemish). Returns
/// `None` when nothing fits, e.g. a large spot in a small image.
pub fn find_source(
    img: &RgbaImage,
    target: [f32; 2],
    radius: f32,
    avoid: &[Spot],
) -> Option<[f32; 2]> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || radius <= 0.0 {
        return None;
    }
    let aspect = w as f32 / h as f32;
    let [rx, ry] = spot_radius_xy(radius, aspect);
    let ring_radius = radius * RING_SCALE;
    let [ring_rx, ring_ry] = spot_radius_xy(ring_radius, aspect);
    let ring_offsets: Vec<[f32; 2]> = (0..RING_SAMPLES)
        .map(|i| {
            let a = i as f32 / RING_SAMPLES as f32 * std::f32::consts::TAU;
            [a.cos() * ring_rx, a.sin() * ring_ry]
        })
        .collect();
    let sample = |p: [f32; 2]| -> Option<[f32; 3]> {
        let (x, y) = (p[0] * w as f32, p[1] * h as f32);
        if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
            return None;
        }
        let px = img.get_pixel(x as u32, y as u32).0;
        Some([px[0] as f32, px[1] as f32, px[2] as f32])
    };

    // The target's ring; points off the image are left out of every score.
    let target_ring: Vec<Option<[f32; 3]>> = ring_offsets
        .iter()
        .map(|o| sample([target[0] + o[0], target[1] + o[1]]))
        .collect();
    if target_ring.iter().all(Option::is_none) {
        return None;
    }

    // Distance between two normalized points in spot-radius units.
    let short = w.min(h) as f32;
    let dist = |a: [f32; 2], b: [f32; 2]| {
        let dx = (a[0] - b[0]) * w as f32 / short;
        let dy = (a[1] - b[1]) * h as f32 / short;
        (dx * dx + dy * dy).sqrt()
    };

    let mut best: Option<([f32; 2], f32)> = None;
    for &d in &CANDIDATE_DISTANCES {
        for k in 0..CANDIDATE_ANGLES {
            let a = k as f32 / CANDIDATE_ANGLES as f32 * std::f32::consts::TAU;
            let c = [target[0] + a.cos() * rx * d, target[1] + a.sin() * ry * d];
            // The candidate's ring must be on the image to be compared.
            if clamp_center(c, ring_radius, aspect) != c {
                continue;
            }
            if avoid.iter().any(|s| dist(c, s.target) < radius + s.radius) {
                continue;
            }
            let score: f32 = target_ring
                .iter()
                .zip(&ring_offsets)
                .filter_map(|(t, o)| {
                    let t = (*t)?;
                    let s = sample([c[0] + o[0], c[1] + o[1]])?;
                    Some((0..3).map(|i| (t[i] - s[i]).powi(2)).sum::<f32>())
                })
                .sum();
            if best.is_none_or(|(_, b)| score < b) {
                best = Some((c, score));
            }
        }
    }
    best.map(|(c, _)| clamp_center(c, radius, aspect))
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

    /// Red image with a blue rectangle; the target sits inside the blue near
    /// its right edge, so most nearby candidates have red in their ring.
    fn blue_patch_image() -> RgbaImage {
        RgbaImage::from_fn(200, 200, |x, y| {
            let (u, v) = (x as f32 / 200.0, y as f32 / 200.0);
            if (0.1..0.6).contains(&u) && (0.3..0.7).contains(&v) {
                Rgba([30, 60, 200, 255])
            } else {
                Rgba([200, 40, 30, 255])
            }
        })
    }

    #[test]
    fn find_source_picks_matching_surroundings() {
        let img = blue_patch_image();
        let (target, r) = ([0.5, 0.5], 0.04);
        let src = find_source(&img, target, r, &[]).expect("a source fits");
        let ring = r * RING_SCALE;
        assert!(
            src[0] - ring >= 0.1 && src[0] + ring <= 0.6 && src[1] - ring >= 0.3 && src[1] + ring <= 0.7,
            "source ring must lie in the blue patch: {src:?}"
        );
    }

    #[test]
    fn find_source_avoids_other_spots_targets() {
        let img = blue_patch_image();
        let (target, r) = ([0.5, 0.5], 0.04);
        let first = find_source(&img, target, r, &[]).unwrap();
        // Mark the best match as a blemish: the pick must move off it.
        let blemish = spot(first, [0.9, 0.9], r, 0.5);
        let second = find_source(&img, target, r, &[blemish.clone()]).unwrap();
        let d = ((second[0] - first[0]).powi(2) + (second[1] - first[1]).powi(2)).sqrt();
        assert!(d >= 2.0 * r - 1e-4, "overlaps the other spot's target: {second:?}");
    }

    #[test]
    fn find_source_keeps_the_source_inside_the_image() {
        let img = RgbaImage::from_pixel(300, 200, Rgba([120, 120, 120, 255]));
        let r = 0.05;
        let src = find_source(&img, [0.02, 0.03], r, &[]).expect("a source fits");
        let [rx, ry] = spot_radius_xy(r, 1.5);
        assert!(src[0] >= rx && src[0] <= 1.0 - rx && src[1] >= ry && src[1] <= 1.0 - ry);
    }

    #[test]
    fn find_source_gives_up_when_nothing_fits() {
        let img = RgbaImage::from_pixel(100, 100, Rgba([0, 0, 0, 255]));
        assert_eq!(find_source(&img, [0.5, 0.5], 0.3, &[]), None);
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
