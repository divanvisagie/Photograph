# RAW Load Latency Notes

This note documents the recent RAW-loading improvement where initial preview now appears significantly faster in typical workflows.

## What Changed

1. Preview decode path now prefers embedded RAW assets.
- `src/thumbnail.rs` now tries embedded preview/thumbnail/full payloads first for RAW files.
- Full RAW develop is used as fallback when embedded payloads are unavailable.

2. Viewer open now uses staged RAW loading.
- Stage A: show fast embedded preview first.
- Stage B: run background full RAW decode only when Stage A used embedded preview, then replace preview for quality convergence.

3. Browser thumbnails benefit automatically.
- Thumbnail cache misses call the preview path, so RAW-heavy folders get faster first thumbnail readiness.

## Why It Feels Faster

- Embedded RAW preview payloads are much cheaper than full RAW develop.
- The UI can show a useful first frame quickly, while full-fidelity image processing continues in the background.

## Relevant Files

- `src/thumbnail.rs`
- `src/viewer.rs`
- `src/browser.rs`

## Verification

Use the same fixed RAW dataset and compare before/after:

```bash
cargo run --release --bin perf_probe -- <raw-dir> 20 auto
```

Primary metrics to compare:

- `METRIC preview_ms_median`
- `METRIC slider_ms_median`

If needed for debugging-only baseline comparisons:

```bash
PHOTOGRAPH_DEBUG_ALLOW_CPU_FALLBACK=1 cargo run --release --bin perf_probe -- <raw-dir> 20 cpu
```

## Load Flow

```mermaid
sequenceDiagram
    participant User
    participant Viewer
    participant Thumbnail as thumbnail.rs
    participant RAW as RAW decoder

    User->>Viewer: Open RAW image
    Viewer->>Thumbnail: open_image_for_preview_with_source
    Thumbnail->>RAW: Try embedded preview payload
    alt Embedded preview found
        RAW-->>Thumbnail: Embedded image
        Thumbnail-->>Viewer: Stage A (fast preview)
        Viewer-->>User: First image appears quickly
        Viewer->>Thumbnail: open_image (full decode, background)
        Thumbnail-->>Viewer: Stage B (full develop)
        Viewer-->>User: Preview converges to full quality
    else Embedded preview missing
        Thumbnail->>RAW: Full RAW develop
        RAW-->>Thumbnail: Full image
        Thumbnail-->>Viewer: Full-develop preview
        Viewer-->>User: First image appears (slower path)
    end
```
