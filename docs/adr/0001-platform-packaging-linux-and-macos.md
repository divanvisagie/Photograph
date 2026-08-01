# 0001. Support Linux and macOS via platform-specific packaging

Date: 2026-02-22

## Status

Superseded by [ADR-0008](0008-drop-macos-packaging.md)

## Context

Photograph needed a real install path beyond `cargo run`. Two platforms were realistic targets at
the time: Linux (the primary development platform) and macOS (the author's other machine). A
single shared build path couldn't serve both — Linux wants a `.deb` with desktop integration
(`.desktop` launcher, SVG icon, apt-based install), while macOS wants an `.app`/`.dmg` bundle with
`Info.plist` templating and an embedded PNG window icon for native window-chrome consistency.

## Decision

We will route `make build`/`make install` through platform-specific targets (`build-linux`,
`build-macos`, etc.) selected from `uname -s`, so Linux and macOS packaging can evolve
independently without one platform's packaging assumptions leaking into the other's.

## Consequences

- Each platform gets packaging that matches its own conventions (`.deb` vs `.app`/`.dmg`) instead
  of a lowest-common-denominator install script.
- The Makefile grows platform branching that has to be maintained on both sides going forward.
- Two supported platforms means twice the packaging surface to keep working — a cost that showed
  up quickly enough to prompt reversal in [ADR-0008](0008-drop-macos-packaging.md) less than a
  week later.
