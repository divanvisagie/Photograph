# 0023. The spot and mask tools show all of the edit's geometry

Date: 2026-10-10

## Status

Accepted

## Context

Spots ([ADR-0018](0018-spot-removal-in-source-coordinates.md)) and mask strokes
([ADR-0019](0019-painted-adjustment-masks.md)) are stored in source-image coordinates, before
any geometry. Placing them means mapping screen positions back to the source. The mapping,
`SourceProjection`, only inverted rotate, flip and crop, so both tools drew the preview without
straighten and keystone. Both ADRs listed this mode switch as a cost.

In use it reads as a bug: entering Mask mode turns a straightened photo back, and the framing
changes under the brush. The user asked for crops and rotations to stay applied while masking.

The pipeline already inverts straighten and keystone for every output pixel: the GPU geometry
shader maps each output pixel back to the source. Both are simple to run either way:

- straighten is a rotation about the centre;
- keystone is a homography, built from four corner pairs, so its inverse is the homography
  built from the same pairs swapped.

## Decision

We will show all of the edit's geometry in the spot and mask tools.

- `SourceProjection` maps through the pipeline's whole order: straighten, keystone, rotate,
  flip, crop. `to_source` undoes them in reverse. Straighten and keystone keep the image's size,
  so they're worked in source units: a height of 1 and a width of the source aspect ratio.
- The keystone homographies come from `gpu_pipeline::keystone_homographies`, which shares its
  corner pairs with the GPU's matrix, so the tools and the render can't disagree on them.
- `render_state` no longer drops straighten and keystone for the spot and mask tools; only crop
  mode still changes the rendered state (it shows the uncropped image). Entering or leaving a
  tool no longer re-renders the preview.
- The mask overlay, a source-space texture, is drawn as a 16×16 grid of quads, so it follows
  keystone's perspective rather than one stretched quad.
- A test runs a marked point through the CPU geometry pipeline (straighten, keystone, every
  rotation and flip, crop) and checks `SourceProjection` puts it within 1.5 px of where the
  pipeline did.

## Consequences

Benefits:

- Spots and strokes are placed on exactly the framing the photo will have. The tools are no
  longer a mode switch that moves the image.
- Switching tools is cheaper: no re-render.

Costs:

- Brush and spot outlines are drawn as circles scaled for straighten, rotate and crop only.
  Keystone stretches the image unevenly, so on a keystoned photo an outline can be slightly
  larger or smaller on screen than the area it actually covers.
- The projection ignores the pipeline's half-pixel offsets. That's under a pixel at preview
  sizes, within the test's tolerance.
- Areas brought into the frame by straighten or keystone, which show black, map to points
  outside the source. Strokes painted there are kept but have no effect.
