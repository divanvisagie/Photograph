# 0022. Draw Photograph's own window frame

Date: 2026-10-10

## Status

Accepted

## Context

On GNOME's Wayland session apps draw their own window frames. Photograph's was winit's built-in
one (sctk-adwaita). It reads the light/dark setting once at startup and has hard-coded colours, so
it didn't follow theme changes and only approximated Ubuntu's look. UnAmp replaced it with a frame
of its own. The user asked for the same frame in Photograph.

## Decision

We will port UnAmp's frame (`src/frame.rs`).

- **The window opens undecorated and transparent.** The top panel becomes a GNOME-style header
  bar: the Render button on the left, the title centred, and the GPU status, Edit and
  minimise/maximise/close on the right.
  - Empty space drags the window. Double-click toggles maximise, and right-click opens a window
    menu.
  - Moving and resizing go to the compositor (`StartDrag`, `BeginResize`).
- **Resize handles** are invisible edges above everything else, with a one-pixel border in the
  window stroke colour.
- **Corners** use libadwaita's 15 px window radius, as in Ubuntu's Yaru styles. Corners are
  square while maximised. The clear colour is transparent, and whichever panel holds each corner
  carries the rounding:
  - the top bar holds both top corners;
  - the sidebar holds the bottom-left;
  - the tools panel, filmstrip or central panel holds the bottom-right, whichever is showing.
- **Escape hatch:** `system_title_bar = true` in `config.toml` restores desktop decorations.

## Consequences

- The frame matches Photograph's own visuals in both themes and switches live
  ([ADR-0021](0021-follow-desktop-color-scheme.md)). It also saves the space of a separate
  title bar.
- Photograph owns behaviour the system frame gave for free. There's no drop shadow, no tiled-state
  corners and no native window-menu extras.
- Without a compositor the transparent corners show black, so use the config switch there.
- The frame code is a copy of UnAmp's, as with the mount discovery code. Fixes need making in
  both until it's worth extracting a shared crate.
