# 3. One Processing Backend Contract for Preview and Export

Date: 2026-02-28

## Status

Accepted

## Context

Divergent preview/export backends increase parity bugs and maintenance cost.

## Decision

Both preview and export attempt `gpu_pipeline::try_apply` first and follow the same
fallback policy (see [ADR-0002](0002-cpu-fallback-debug-only.md)).

## Consequences

One contract improves predictability and testing leverage.

Implemented in:
- Preview: `src/viewer.rs`
- Export: `src/app.rs`
