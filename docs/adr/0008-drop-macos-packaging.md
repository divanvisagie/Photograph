# 0008. Drop macOS packaging, ship Linux .deb only

Date: 2026-02-28

## Status

Superseded by [ADR-0011](0011-restore-macos-metal-backend.md)

## Context

[ADR-0001](0001-platform-packaging-linux-and-macos.md) added macOS packaging alongside Linux only
six days earlier. Maintaining both platform-specific Makefile targets and their branching added
ongoing packaging cost for a second platform that wasn't the primary development or distribution
target. Linux (Ubuntu/Debian, `.deb`) was the platform actually being used and supported day to
day.

## Decision

We will drop the macOS-specific make targets and platform branching, keeping Linux `.deb`
packaging as the sole build/install path. Icon generation becomes an explicit `make icons` step
instead of running on every `make build`. The README is updated to state Ubuntu support intent and
Linux packaging commands only.

## Consequences

- Removes macOS-specific Makefile branching and the packaging assets that only existed to serve
  it, simplifying the build to a single supported target.
- `make build` no longer regenerates icons on every build, making iteration faster.
- macOS users lose an install path entirely — acceptable at this point since macOS wasn't the
  platform actually being validated or distributed to.
- This did not hold: macOS support was requested again and restored five weeks later in
  [ADR-0011](0011-restore-macos-metal-backend.md).
