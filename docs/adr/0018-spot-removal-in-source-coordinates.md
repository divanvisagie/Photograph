# 0018. Spot removal (clone and heal) as the first pipeline stage, in source-image coordinates

Date: 2026-09-26

## Status

Accepted

## Context

Users need to remove small blemishes: dust on the sensor, a spot on skin, food stuck in teeth.
Photograph has global adjustments (exposure, colour, selective HSL, a graduated filter) and
geometry edits (straighten, keystone, rotate, flip, crop). It has no local retouching.

Options considered:

- **Local adjustment brush.** Paint a mask and apply exposure, saturation or similar inside it.
  This is useful on its own, but it doesn't remove anything. Food in teeth becomes grey food in
  teeth, because its shape and shadow are still there. Deferred rather than rejected; it may come
  later as a separate tool.
- **Content-aware fill (PatchMatch-style synthesis).** This gives the best results on large or
  complex areas, but it's much more complex, slow at full resolution, and hard to keep identical
  between the GPU pipeline and the CPU fallback
  ([ADR-0005](0005-shared-preview-export-backend.md), [ADR-0007](0007-guard-parity-tests.md)).
  Rejected for now.
- **Spot removal with clone and heal modes.** This is the familiar Lightroom-style tool. Each
  spot is a target circle filled from a source circle, with a feathered edge.
  - **Clone** copies the source pixels as they are.
  - **Heal** takes only the texture from the source, and the brightness and colour from the ring
    of pixels around the target, so it blends into shading differences. This matters on teeth and
    skin.

  Both modes are small, per-spot computations that can run identically on the GPU and the CPU.

The rest of the pipeline raises two questions:

1. **Where spots are stored.** Geometry edits are applied in this order: straighten, keystone,
   rotate, flip, crop (`transform::apply`). A spot stored in the coordinates of the displayed
   (rotated, cropped) image would drift off its blemish as soon as any geometry edit changed.
   Preview renders are downscaled (up to `PREVIEW_MAX`) while exports run at full resolution, so
   positions in pixels wouldn't carry over either.
2. **Where spots are applied.** If spots were applied after colour grading, later exposure or
   colour changes would stop matching the patches that were already placed.

## Decision

We will add spot removal as a non-destructive edit with Clone and Heal modes, applied as the
**first** stage of the pipeline, before geometry and colour.

- **Data model.** `EditState` gains `spots: Vec<Spot>`, saved in the sidecar JSON
  ([ADR-0002](0002-edit-state-sidecar-persistence.md)). Each `Spot` records:
  - `target` and `source` centres, as normalized (0–1) coordinates of the **source image**, before
    any geometry is applied;
  - `radius`, as a fraction of the source image's shorter side;
  - `feather`, from 0 to 1;
  - `mode`, `Clone` or `Heal`.

  Normalized values make spots independent of resolution, so a spot placed on the preview lands in
  the same place in a full-resolution export.
- **Pipeline.** Spots are applied before straighten, in both the GPU pipeline and the CPU
  `transform::apply`. Parity tests cover both modes.
  - Every spot copies from the *unretouched* source and blends into the result in list order.
    Overlapping spots therefore never copy each other's patches, and the GPU does the whole
    stage in one per-pixel pass.
  - Both paths resolve spots to pixels through one shared helper, `processing::spots::SpotPx`.
- **Editing view.** While the spot tool is active, the preview is drawn without straighten and
  keystone. Crop, rotation and flips stay:
  - they're exactly reversible, so screen positions map exactly back to source-image
    coordinates;
  - this means spots are placed on the same cropped, upright view the user is editing, with
    zoom and pan available.

  Straighten and keystone can't be reversed that simply, so they're dropped while placing
  spots. On a straightened or keystoned photo, the visible area is therefore slightly different
  from the final framing. Colour adjustments stay on, so patches are judged on the graded image.
- **Source placement.** A new spot automatically picks a nearby source circle whose surroundings
  best match the target's. The user can drag the source circle to override it.

The work is delivered in steps:

1. The data model, plus spot and source circles drawn over the preview.
2. Clone mode on the CPU and GPU, with parity tests.
3. Heal mode.
4. Automatic source selection.

## Consequences

Benefits:

- Spots survive later rotate, crop, straighten and keystone changes, and look the same in the
  preview and in exports at any resolution.
- Colour adjustments apply on top of retouched pixels, so grading after retouching doesn't break
  the patches.
- The feature is self-contained: one new `EditState` field, one pipeline stage, and one editor
  mode.

Costs:

- The spot tool has to show the image without straighten and keystone, which is a mode switch.
  Placing spots directly on the straightened or keystoned view would need an inverse of those
  transforms that exactly matches the pipeline, and that's out of scope.
- Every spot costs GPU and CPU work on every render, including interactive previews. That's fine
  for tens of spots, but large retouching jobs may need the spot pass cached separately from
  colour edits.
- Heal needs the average of the ring of pixels around the target and around the source. That's
  more shader work than Clone, and more to cover in parity tests.
- Anything larger or more complex than a spot, such as removing a person or a power line, still
  isn't possible without content-aware fill.
