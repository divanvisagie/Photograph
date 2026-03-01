/// Highlight recovery for linear-space RGB data from RAW development.
///
/// Operates on f32 pixel data between calibration and sRGB gamma.
/// Reconstructs partially-clipped channels using information from
/// unclipped channels, then applies soft-knee compression to map
/// extended-range values smoothly into [0, 1].

/// Threshold above which a channel is considered clipped.
const CLIP_THRESH: f32 = 0.99;

/// Recover highlights in linear RGB pixel data.
///
/// For pixels where some but not all channels are clipped (>= [`CLIP_THRESH`]),
/// the clipped channels are reconstructed using brightness information from
/// the remaining unclipped channels. This preserves color gradation in
/// overexposed regions where at least one channel retains sensor detail.
///
/// After reconstruction, a soft-knee compressor maps values above the knee
/// point smoothly into [0, 1], preventing hard clipping artifacts.
pub fn recover(pixels: &mut [[f32; 3]]) {
    // Pass 1: reconstruct partially-clipped channels.
    for px in pixels.iter_mut() {
        let r_clip = px[0] >= CLIP_THRESH;
        let g_clip = px[1] >= CLIP_THRESH;
        let b_clip = px[2] >= CLIP_THRESH;

        let n_clipped = r_clip as u8 + g_clip as u8 + b_clip as u8;
        if n_clipped == 0 || n_clipped == 3 {
            continue;
        }

        // Average of unclipped channels gives the best available
        // estimate of scene brightness in this region.
        let mut sum = 0.0_f32;
        let mut count = 0u8;
        if !r_clip {
            sum += px[0];
            count += 1;
        }
        if !g_clip {
            sum += px[1];
            count += 1;
        }
        if !b_clip {
            sum += px[2];
            count += 1;
        }
        let unclip_avg = sum / count as f32;

        // Blend clipped channels toward the unclipped average.
        // This desaturates the highlight gradually rather than
        // leaving a hard color fringe at the clip boundary.
        if r_clip {
            px[0] = unclip_avg;
        }
        if g_clip {
            px[1] = unclip_avg;
        }
        if b_clip {
            px[2] = unclip_avg;
        }
    }

    // Pass 2: soft-knee compression to [0, 1].
    // Values below the knee pass through unchanged.
    // Values above the knee are compressed with a smooth curve.
    const KNEE: f32 = 0.85;
    const INV_HEADROOM: f32 = 1.0 / (1.0 - KNEE); // ≈ 6.67

    for px in pixels.iter_mut() {
        for ch in px.iter_mut() {
            if *ch > KNEE {
                let t = (*ch - KNEE) * INV_HEADROOM;
                // Attempt to preserve gradation: map [knee, ∞) → [knee, 1)
                // using a deceleration curve.
                *ch = KNEE + (1.0 - KNEE) * (1.0 - (-t).exp());
            }
            // Clamp to handle edge cases (negative values, extreme overshoot).
            *ch = ch.clamp(0.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn no_change_below_threshold() {
        let mut pixels = vec![[0.2, 0.4, 0.6]];
        let original = pixels.clone();
        recover(&mut pixels);
        // Values well below clip should be unchanged (below knee too).
        assert_eq!(pixels, original);
    }

    #[test]
    fn no_change_at_moderate_values() {
        let mut pixels = vec![[0.5, 0.5, 0.5]];
        let original = pixels.clone();
        recover(&mut pixels);
        assert_eq!(pixels, original);
    }

    #[test]
    fn single_channel_clipped_is_reconstructed() {
        // Red clipped, green and blue unclipped.
        let mut pixels = vec![[1.0, 0.6, 0.4]];
        recover(&mut pixels);
        // Red should have been pulled toward the average of green and blue (0.5).
        // Then soft-knee may adjust slightly.
        // The key assertion: red is no longer at 1.0 and is closer to the
        // unclipped channels.
        assert!(pixels[0][0] < 1.0, "red should be reconstructed below 1.0");
        assert!(
            pixels[0][0] < 0.8,
            "red should be pulled toward unclipped average"
        );
    }

    #[test]
    fn two_channels_clipped_uses_remaining() {
        // Red and green clipped, blue unclipped at 0.7.
        let mut pixels = vec![[1.0, 1.0, 0.7]];
        recover(&mut pixels);
        // Both clipped channels should be reconstructed toward blue's value.
        assert!(
            pixels[0][0] < 1.0,
            "red should be reconstructed below 1.0"
        );
        assert!(
            pixels[0][1] < 1.0,
            "green should be reconstructed below 1.0"
        );
        // Both reconstructed channels should be near blue's value.
        assert!(approx_eq(pixels[0][0], pixels[0][1], 0.01));
    }

    #[test]
    fn all_channels_clipped_stays_near_white() {
        let mut pixels = vec![[1.0, 1.0, 1.0]];
        recover(&mut pixels);
        // All channels clipped → no reconstruction, soft-knee compresses
        // from 1.0 to ~0.945. This is expected: the knee creates a smooth
        // rolloff in highlights. After sRGB gamma, this maps to ~0.976.
        for ch in &pixels[0] {
            assert!(*ch > 0.9, "fully clipped pixels should stay near white");
        }
    }

    #[test]
    fn soft_knee_compresses_above_knee() {
        // All channels at 0.95 (above knee but not clipped → no reconstruction,
        // but soft-knee still applies).
        let mut pixels = vec![[0.95, 0.95, 0.95]];
        recover(&mut pixels);
        for ch in &pixels[0] {
            assert!(*ch <= 1.0, "should be clamped to 1.0");
            assert!(*ch > 0.9, "should preserve most of the brightness");
        }
    }

    #[test]
    fn preserves_relative_order() {
        // Channels below knee should maintain their relative ordering.
        let mut pixels = vec![[0.3, 0.5, 0.7]];
        recover(&mut pixels);
        assert!(pixels[0][0] < pixels[0][1]);
        assert!(pixels[0][1] < pixels[0][2]);
    }

    #[test]
    fn negative_values_clamped() {
        let mut pixels = vec![[-0.1, 0.5, 0.5]];
        recover(&mut pixels);
        assert!(pixels[0][0] >= 0.0);
    }

    #[test]
    fn reconstruction_desaturates_toward_neutral() {
        // Simulate a sunset scene: red heavily clipped, green moderate, blue low.
        let mut pixels = vec![[1.0, 0.5, 0.2]];
        recover(&mut pixels);
        let [r, g, b] = pixels[0];
        // Red should be pulled down (reconstructed from avg of G+B = 0.35).
        assert!(r < 0.5, "red should be heavily reduced from 1.0");
        // Green and blue should be mostly preserved (below knee).
        assert!(approx_eq(g, 0.5, 0.01));
        assert!(approx_eq(b, 0.2, 0.01));
    }
}
