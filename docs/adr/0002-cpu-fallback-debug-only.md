# 2. CPU Fallback Is Debug-Only

Date: 2026-02-28

## Status

Accepted

## Context

Silent runtime fallback to CPU hides performance regressions and policy violations.

## Decision

CPU fallback is disabled by default and only enabled via
`PHOTOGRAPH_DEBUG_ALLOW_CPU_FALLBACK=1`.

## Consequences

Keeps performance expectations deterministic in normal operation.

Implemented in:
- `src/processing/gpu_pipeline.rs` (`DEBUG_ALLOW_CPU_FALLBACK_ENV`,
  `allow_debug_cpu_fallback`)
- `src/main.rs` (startup enforcement)
- `src/viewer.rs` and `src/app.rs` (preview/export behavior)
