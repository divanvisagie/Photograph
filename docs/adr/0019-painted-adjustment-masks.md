# 0019. Painted adjustment masks stored as brush strokes

Date: 2026-09-26

## Status

Accepted

## Context

Every colour adjustment in Photograph applies to the whole image. The only exception is the
graduated filter, which is a fixed top-to-bottom exposure ramp. Users need to adjust part of a
photo, for example to brighten a face in shadow or warm one area, without affecting the rest.
[ADR-0018](0018-spot-removal-in-source-coordinates.md) listed a local adjustment brush as
deferred, to come later as a separate tool.

The project keeps features minimal and practical: only what's actually used, not parity with
other editors. The user chose:

- **Painted masks.** A round brush with size and feather, and a Paint/Erase toggle. Masks are
  saved as strokes in the sidecar, not as bitmaps.
- **Several masks per photo,** which can be added, named and selected.
- **Each mask adjusts the same basic colour controls as the global panel:** exposure, contrast,
  highlights, shadows, temperature, saturation and hue.
- **Floating windows** for the brush settings and for the mask list, like the debug window,
  shown while the tool is active.

Options considered:

- **Storing masks as bitmaps.** A bitmap is simple to sample, but it's tied to one resolution,
  bloats the sidecar, and isn't editable as strokes. Rejected in favour of strokes, which stay
  small, work at any resolution and match the non-destructive, JSON-sidecar model
  ([ADR-0002](0002-edit-state-sidecar-persistence.md)).
- **Gradient and radial mask shapes.** Not needed. The graduated filter already covers the
  common gradient case.
- **Per-mask selective colour, graduated filter or sharpness.** Left out to keep the per-mask
  panel to one short list of sliders.
- **Switching sidecars from JSON to TOML.** TOML's advantage is hand editing, and sidecars are
  written and read by the app. Nested lists of strokes and points are noisier in TOML than in
  JSON. TOML has no null. Existing `.edits/*.json` sidecars would need migrating, or both formats
  reading forever. Rejected: JSON stays.

## Decision

We will add painted adjustment masks.

### Data model

`EditState` gains `masks: Vec<Mask>`, omitted from the sidecar when empty.

- **`Mask`** has:
  - `name`;
  - `strokes: Vec<Stroke>`;
  - `adjust`: its own `exposure`, `contrast`, `highlights`, `shadows`, `temperature`,
    `saturation` and `hue_shift`, all neutral by default.
- **`Stroke`** has:
  - `points`, in normalized **source-image** coordinates before geometry, like spots in
    ADR-0018;
  - `radius`, as a fraction of the shorter side;
  - `feather`, from 0 to 1;
  - `erase: bool`.

  Points are recorded only once the pointer has moved a fraction of the radius, which keeps
  the sidecar small.

### Coverage

A mask's coverage at a point runs from 0 to 1.

- Each stroke contributes the brush falloff at that point: fully on within
  `radius × (1 − feather)`, then a smooth drop-off to 0 at `radius`, measured to the stroke's
  nearest segment.
- Strokes are applied in order:
  - a paint stroke raises coverage: `c = max(c, s)`;
  - an erase stroke lowers it: `c = c × (1 − s)`.

### Pipeline

1. **Rasterizing.** Masks are drawn in source-image space, at a fixed cap resolution (longest
   side 1024), not at full resolution.
   - Masks are soft, so this loses nothing visible.
   - The cost doesn't grow with the size of the export.
   - It's identical for previews and exports.
2. **Following geometry.** The coverage bitmaps go through the same geometry pass as the image
   (straighten, keystone, rotate, flip, crop), so they stay aligned with it. The bitmaps are
   packed four masks per RGBA texture, and sampled bilinearly when scaled up to the output.
3. **Applying adjustments.** In the colour stage, after the global adjustments and the
   graduated filter and before sharpening, each mask in order does:

   `pixel = mix(pixel, adjust(pixel, mask.adjust), coverage)`

   `adjust` is the same maths the global sliders use.
4. **GPU and CPU.** Both paths implement all of the above, with parity tests
   ([ADR-0005](0005-shared-preview-export-backend.md),
   [ADR-0007](0007-guard-parity-tests.md)). The coverage maths lives in one shared function,
   the same way spots share `SpotPx`.

### Editing

- **Tool.** A **Mask** toolbar button, exclusive with Crop and Spot. While it's active, the
  preview drops straighten and keystone and keeps rotate, flip and crop, with zoom and pan.
  This is the spot tool's view, and it reuses its screen-to-source mapping (`SpotProjection`,
  which will be generalized).
- **Floating windows,** shown while the tool is active:
  - **Brush:** size, feather, and a Paint/Erase toggle.
  - **Masks:** a list of masks with a **+** button, click to select, rename in place, and
    delete. Below the list are the selected mask's adjustment sliders.
- **Painting.** Dragging with the brush paints into the selected mask. The mask's coverage is
  shown as a translucent overlay while the tool is active. The brush outline follows the
  cursor.

## Consequences

Benefits:

- Local adjustments with a small, readable sidecar. Masks survive later geometry changes, as
  spots do, and apply identically to previews and exports.
- Reusing the global adjustment maths keeps per-mask results consistent with the global
  sliders, with no second set of colour code to maintain.
- Rasterizing at a capped resolution bounds the cost for exports.

Costs:

- **The largest pipeline change so far.** It adds a stroke-rasterizing pass, warps the masks
  through the geometry pass, and makes the colour pass loop over masks. Parity tests have to
  cover all three.
- **Per-render cost:** every render redraws every stroke. Long painting sessions produce many
  segments. Bounding-box culling on the CPU, and the capped resolution, keep this manageable.
  If that isn't enough, masks may need caching separately from colour edits.
- **Edge precision:** at the 1024px cap, a hard-edged brush (feather 0) on a very large export
  gets a slightly soft edge. That's acceptable for adjustment masks.
- **Two new floating windows.** They add UI state: position, visibility, and which mask is
  selected.
- **Sidecar size: watch this.** Sidecars are pretty-printed JSON, which puts every number of an
  array on its own line. A single 200-point stroke becomes roughly 800 lines. Sparse point
  recording limits the growth, but long painting sessions could still make sidecars large and
  slow to save and load. They're written on every save and read on every image switch. We keep
  the current format for now and monitor this. If it gets out of hand, the mitigations, in order
  of cost, are:
  1. round stored coordinates to about 5 decimal places;
  2. write point lists compactly, one `[x, y]` per line, or write the whole sidecar compactly;
  3. simplify strokes when they're finished, dropping points that barely change the path.
