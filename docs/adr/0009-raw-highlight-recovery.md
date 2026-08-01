# 0009. Apply Highlight Recovery During RAW Develop

Date: 2026-03-01

## Status

Accepted

## Context

The default `RawDevelop` pipeline applies sRGB gamma then converts to u16, clipping
any channel values above 1.0 in linear space. In overexposed RAW regions where only
some channels are sensor-saturated, the unclipped channels carry recoverable scene
information that is lost by this clipping.

## Decision

Use a custom `RawDevelop` pipeline that omits the `SRgb` step, apply highlight
reconstruction on the linear f32 intermediate data, then apply sRGB gamma before
conversion to `DynamicImage`.

Algorithm: a two-pass over linear RGB pixels:

1. **Channel reconstruction**: pixels with 1 or 2 channels above the clip threshold
   (0.99) rebuild clipped channels from a luminance/chroma decomposition anchored by
   unclipped-channel luminance.
2. **Near-clip shoulder**: an exponential shoulder rolloff engages only above 0.95,
   compressing near-clip values gently into [0, 1].

## Consequences

Operating on linear f32 data between calibration and gamma allows reconstruction of
partially-clipped highlights. Clipped channels are rebuilt from a luminance/chroma
decomposition so warm/cool highlight bias is preserved better, and a near-clip
shoulder rolloff reduces hard clipping without darkening broad bright tones. This
improves highlight gradation without changing the downstream GPU/CPU processing
pipeline.

Both preview (Stage B full decode) and export flow through `open_image()`, so
recovery policy is shared automatically (per [ADR-0005](0005-shared-preview-export-backend.md)).

Implemented in:
- `src/processing/highlights.rs` (recovery algorithm)
- `src/thumbnail.rs` (`develop_raw_with_recovery`)
