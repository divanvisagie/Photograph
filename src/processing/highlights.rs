/// Highlight recovery for linear-space RGB data from RAW development.
///
/// Operates on f32 pixel data between calibration and sRGB gamma.
/// Reconstructs partially-clipped channels with a luminance/chroma model,
/// then applies a near-clip shoulder rolloff to keep highlights smooth.

/// Threshold above which a channel is considered clipped.
const CLIP_THRESH: f32 = 0.99;
/// Shoulder start for highlight rolloff.
const SHOULDER_START: f32 = 0.95;
/// Approximate Rec.709 luminance coefficients.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Recover highlights in linear RGB pixel data.
///
/// For pixels where some but not all channels are clipped (>= [`CLIP_THRESH`]),
/// clipped channels are reconstructed from unclipped-channel luminance and
/// per-channel chroma offsets. This preserves color gradation in
/// overexposed regions where at least one channel retains sensor detail.
///
/// After reconstruction, a shoulder rolloff maps values above
/// [`SHOULDER_START`] smoothly into [0, 1], preventing hard clipping artifacts.
pub fn recover(pixels: &mut [[f32; 3]]) {
    // Pass 1: reconstruct partially-clipped channels.
    // Keep per-channel chroma relationship where possible by rebuilding
    // clipped channels from a luminance/chroma decomposition.
    for px in pixels.iter_mut() {
        let original = *px;
        let r_clip = px[0] >= CLIP_THRESH;
        let g_clip = px[1] >= CLIP_THRESH;
        let b_clip = px[2] >= CLIP_THRESH;

        let n_clipped = r_clip as u8 + g_clip as u8 + b_clip as u8;
        if n_clipped == 0 || n_clipped == 3 {
            continue;
        }

        let mut unclipped_luma_sum = 0.0_f32;
        let mut unclipped_weight_sum = 0.0_f32;
        if !r_clip {
            unclipped_luma_sum += original[0] * LUMA[0];
            unclipped_weight_sum += LUMA[0];
        }
        if !g_clip {
            unclipped_luma_sum += original[1] * LUMA[1];
            unclipped_weight_sum += LUMA[1];
        }
        if !b_clip {
            unclipped_luma_sum += original[2] * LUMA[2];
            unclipped_weight_sum += LUMA[2];
        }

        // At least one channel is unclipped due to early-continue above.
        let target_luma = unclipped_luma_sum / unclipped_weight_sum;
        let original_luma =
            original[0] * LUMA[0] + original[1] * LUMA[1] + original[2] * LUMA[2];
        let chroma = [
            original[0] - original_luma,
            original[1] - original_luma,
            original[2] - original_luma,
        ];

        if r_clip {
            px[0] = (target_luma + chroma[0]).clamp(0.0, 1.0);
        }
        if g_clip {
            px[1] = (target_luma + chroma[1]).clamp(0.0, 1.0);
        }
        if b_clip {
            px[2] = (target_luma + chroma[2]).clamp(0.0, 1.0);
        }
    }

    // Pass 2: soft shoulder compression to [0, 1].
    // Values below SHOULDER_START pass through unchanged.
    // Values above are gently rolled off to avoid harsh clipping.
    const INV_HEADROOM: f32 = 1.0 / (1.0 - SHOULDER_START);

    for px in pixels.iter_mut() {
        for ch in px.iter_mut() {
            if *ch > SHOULDER_START {
                let t = (*ch - SHOULDER_START) * INV_HEADROOM;
                // Map [shoulder, ∞) with a gentle deceleration curve.
                *ch = SHOULDER_START + (1.0 - SHOULDER_START) * (1.0 - (-t).exp());
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
        // Values well below clip should be unchanged (below shoulder too).
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
        // Red should be reduced from hard clip while staying warmer than
        // unclipped channels (avoid neutral-grey collapse).
        assert!(pixels[0][0] < 1.0, "red should be reconstructed below 1.0");
        assert!(pixels[0][0] > pixels[0][1], "red should remain dominant");
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
        // All channels clipped → no reconstruction, shoulder rolloff compresses
        // from 1.0 only slightly. This keeps near-white highlights bright
        // while avoiding hard clipping.
        for ch in &pixels[0] {
            assert!(*ch > 0.97, "fully clipped pixels should stay near white");
        }
    }

    #[test]
    fn shoulder_does_not_darken_broad_brights() {
        let mut pixels = vec![[0.90, 0.90, 0.90]];
        let original = pixels[0];
        recover(&mut pixels);
        assert!(approx_eq(pixels[0][0], original[0], 0.0001));
        assert!(approx_eq(pixels[0][1], original[1], 0.0001));
        assert!(approx_eq(pixels[0][2], original[2], 0.0001));
    }

    #[test]
    fn shoulder_compresses_near_clip_gently() {
        let mut pixels = vec![[0.99, 0.99, 0.99]];
        recover(&mut pixels);
        for ch in &pixels[0] {
            assert!(*ch < 0.99, "shoulder should compress near-clip values");
            assert!(*ch > 0.97, "compression should be gentle");
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
    fn reconstruction_preserves_warm_highlights() {
        // Simulate a sunset scene: red heavily clipped, green moderate, blue low.
        let mut pixels = vec![[1.0, 0.5, 0.2]];
        recover(&mut pixels);
        let [r, g, b] = pixels[0];
        // Red should still be reduced from hard clip...
        assert!(r < 1.0, "red should be reduced from 1.0");
        // ...but remain warm relative to green/blue.
        assert!(r > g, "red should remain warmer than green");
        assert!(g > b, "green should remain warmer than blue");
        // Green and blue should be mostly preserved (below shoulder).
        assert!(approx_eq(g, 0.5, 0.02));
        assert!(approx_eq(b, 0.2, 0.02));
    }
}
