# 0021. Follow the desktop's light/dark setting through the XDG portal

Date: 2026-10-10

## Status

Accepted

## Context

Photograph defines Yaru-style visuals for both themes and sets egui's theme preference to "system",
intending to match the desktop. But egui learns the system theme only from winit, and winit 0.30
never reports one on Linux: `system_theme()` returns `None` and no `ThemeChanged` event is sent.
So egui always used its fallback theme, which is dark, whatever the desktop said. The same problem
came up in UnAmp, the sister app, and was solved there first.

On Linux the desktop publishes the setting through the XDG desktop portal, as `color-scheme` in
`org.freedesktop.appearance` (0 no preference, 1 dark, 2 light). The portal emits
`SettingChanged` when it changes. GNOME's dark-style switch sets it, and so do other desktops'
portal backends.

## Decision

We will port UnAmp's watcher (`src/appearance.rs`), adding `zbus` as a dependency.

- A thread subscribes to `SettingChanged`, then reads `color-scheme`, using `ReadOne` and falling
  back to the older `Read`.
- It sets egui's `fallback_theme`: 1 means dark, 0 or 2 mean light. egui still prefers a theme
  reported by the platform, so this only takes effect where winit reports none.
- Photograph's visuals are already set for both themes, so the whole UI switches live.
- When the desktop draws the frame (see [ADR-0022](0022-draw-own-window-frame.md)), the app also
  sends `ViewportCommand::SetTheme` on each change. winit's Wayland frame otherwise reads the
  setting only when the window opens.
- With no session bus or portal, the thread logs one line and Photograph stays dark as before.

## Consequences

- Photograph matches the desktop at startup and switches live when dark style is flipped.
- It adds the `zbus` dependency and a background thread.
- "No preference" now means light rather than dark.
- Desktops without an appearance portal keep the old always-dark behaviour.
- The module can go once winit or eframe report the Linux theme themselves.
