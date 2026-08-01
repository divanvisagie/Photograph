# 0006. Preserve Generation-Based Preview Cancellation

Date: 2026-02-28

## Status

Accepted

## Context

Rapid UI updates can apply stale frames when background jobs complete out of order.

## Decision

Keep generation tokens and stale-result dropping in viewer background processing.

## Consequences

Ensures responsive editing without visual rollback artifacts.

Implemented in `src/viewer.rs`.
