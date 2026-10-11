# 0028. Dark Orange by default, dark whatever the desktop's setting

Date: 2026-10-11

## Status

Accepted: the user decided it. Supersedes ADR-0025's "Photograph" skin, the original look kept as
a skin of its own and used by default. Supersedes ADR-0021's following the desktop's light/dark
setting by default; it still applies under the Default skin.

## Context

ADR-0025 kept Photograph's original look, `photograph_visuals` in light and dark following the
desktop, as a "Photograph" entry in the Skins menu, and started in it. The user pointed out that
Patina's Dark Orange is an improved version of that look. They also said that Photograph, a photo
editor, should be dark by default even when the desktop is light, while users can still pick a
light skin.

## Decision

- **Default skin:** Photograph starts in Patina's Dark Orange (`HOME_SKIN`) until another is
  picked. Dark Orange has a dark base, so Photograph is dark whatever the desktop says.
- **The "Photograph" look is removed:** the menu entry, `photograph_visuals` and its palette.
- **Old configs:** a `skin` saved by an earlier build that's no longer offered (such as
  "Photograph") falls back to Dark Orange.
- **Light:** for a light window, users pick Default (which follows the desktop) or a light skin
  such as Calendar or Notebook.

## Consequences

- Photographs are judged against a dark surround by default, as in most photo editors.
- Photograph has no colours of its own any more: its look is Patina's, so fixes to Dark Orange
  reach it with a `patina-ui` update.
- The GTK-style spacing and the Ubuntu fonts stay Photograph's own.
