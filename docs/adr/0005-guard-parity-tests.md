# 5. Guard Against False-Positive Parity Tests

Date: 2026-02-28

## Status

Accepted

## Context

Simple fill-pixel skipping can hide severe regressions (for example, near-black
outputs).

## Decision

Keep fill-aware comparisons, but bound the allowed skipped-fill ratio.

## Consequences

Maintains tolerance for boundary interpolation differences while still catching
broken output.

Implemented in `src/processing/gpu_pipeline.rs` test helpers.
