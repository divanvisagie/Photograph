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
- **Each mask adjusts the same colour controls as the global panel:** exposure, contrast,
  highlights, shadows, temperature, saturation, hue, and the eight selective colour bands.
- **One floating Masks window,** like the debug window, shown while the tool is active. It
  manages masks and the brush.
- **No second set of sliders.** While the tool is active, the normal adjustments panel edits
  the selected mask instead of the whole photo. Leaving the tool returns it to the whole photo.

Options considered:

- **Storing masks as bitmaps.** A bitmap is simple to sample, but it's tied to one resolution,
  bloats the sidecar, and isn't editable as strokes. Rejected in favour of strokes, which stay
  small, work at any resolution and match the non-destructive, JSON-sidecar model
  ([ADR-0002](0002-edit-state-sidecar-persistence.md)).
- **Gradient and radial mask shapes.** Not needed. The graduated filter already covers the
  common gradient case.
- **Per-mask sharpening.** Left out: sharpening blurs neighbouring pixels and is a separate
  multi-pass step on the GPU, not per-pixel colour maths. Masking it would mean running it once
  per mask and blending, roughly doubling the GPU work for masks. Selective colour, by contrast,
  is per-pixel maths in the same colour stage. It only adds parameters per mask, so it was
  included after the first draft left it out.
- **Per-mask graduated filter.** Left out: it's already a mask of its own, so one inside a mask
  would be a mask within a mask.
- **Separate slider sets for masks,** in the Masks window. Tried first and rejected in favour of
  reusing the normal adjustments panel, which keeps a single place for adjustments.
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
  - `enabled`: a visibility flag, true by default and left out of the sidecar while true. A
    hidden mask keeps its paint and settings but changes nothing, in previews or exports;
  - `strokes: Vec<Stroke>`;
  - `adjust`: its own `exposure`, `contrast`, `highlights`, `shadows`, `temperature`,
    `saturation`, `hue_shift` and `selective_color` (8 bands, like the global setting), all
    neutral by default. Untouched selective colour bands are left out of the sidecar.
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
2. **Following geometry.** The coverage is scaled to the source size and run through the same
   geometry as the image (straighten, keystone, rotate, flip, crop), so it stays aligned with
   it. This happens **on the CPU, in one shared helper** (`masks::output_coverage`, using
   `transform::apply_geometry`). The GPU path uploads the result as one texture per mask.
   - The first draft planned to warp coverage in the GPU geometry pass instead. Sharing the CPU
     result means coverage is identical on both paths by construction, and only the colour
     work needs parity tests.
3. **Applying adjustments.** In the colour stage, after the global adjustments and the
   graduated filter and before sharpening, each mask in order does:

   `pixel = mix(pixel, adjust(pixel, mask.adjust), coverage)`

   `adjust` is the same maths the global sliders use:
   - **GPU:** the existing colour shader, run with the mask's settings, then a small blend pass.
   - **CPU:** the existing exposure and colour functions.

   No colour maths is duplicated.
4. **GPU and CPU.** Both paths implement all of the above, with parity tests
   ([ADR-0005](0005-shared-preview-export-backend.md),
   [ADR-0007](0007-guard-parity-tests.md)).

### Editing

- **Tool.** A **Mask** toolbar button, exclusive with Crop and Spot. While it's active, the
  preview drops straighten and keystone and keeps rotate, flip and crop, with zoom and pan.
  This is the spot tool's view, and it reuses its screen-to-source mapping, generalized as
  `SourceProjection`.
- **Masks window,** shown while the tool is active:
  - a list of masks with a **+** button. Each row has the name on the left (click the row to
    select it) and a visibility checkbox on the right, checked by default. The selected mask can
    be renamed or deleted;
  - the brush: size, feather, a Paint/Erase toggle, and whether to show the overlay.
- **Adjustments panel.** While the tool is active, the normal panel shows "Editing mask:
  <name>" and edits that mask. It shows the sliders a mask carries: the basic colour sliders and
  selective colour. It hides the ones masks don't have: sharpening, the graduated filter, crop and
  transform. The whole-photo panel and the mask panel share one slider implementation.
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
- **Per-render CPU cost: watch this.** Every render with active masks recomputes each mask's
  coverage on the CPU: rasterize, scale to the source, then warp.
  - Measured at step 2: about **80ms per mask** for a 1920px preview, about 105ms with
    straighten on, and correspondingly less for the smaller previews used during slider drags.
  - Long painting sessions add segments, though bounding-box culling limits their cost.
  - If this becomes noticeable, the mitigation is to cache each mask's coverage, keyed by its
    strokes, the image size and the geometry. Colour-only edits, the common case, then reuse it.
- **Edge precision:** at the 1024px cap, a hard-edged brush (feather 0) on a very large export
  gets a slightly soft edge. That's acceptable for adjustment masks.
- **Two meanings for one panel.** The adjustments panel means "whole photo" or "selected mask"
  depending on the tool. A clear header shows which, but it's modal behaviour users have to
  notice.
- **Sidecar size: watch this.** Sidecars are pretty-printed JSON, which puts every number of an
  array on its own line. A single 200-point stroke becomes roughly 800 lines. Sparse point
  recording limits the growth, but long painting sessions could still make sidecars large and
  slow to save and load. They're written on every save and read on every image switch. We keep
  the current format for now and monitor this. If it gets out of hand, the mitigations, in order
  of cost, are:
  1. round stored coordinates to about 5 decimal places;
  2. write point lists compactly, one `[x, y]` per line, or write the whole sidecar compactly;
  3. simplify strokes when they're finished, dropping points that barely change the path.
