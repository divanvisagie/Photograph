# 0025. Build on Patina: its frame, appearance watcher and skins

Date: 2026-10-11

## Status

The port is Accepted: the user asked for Photograph to use the Patina library core. Keeping
Photograph's look as its own "Photograph" skin and starting in it, and the File/View/Skins menus,
are Proposed.

Supersedes the parts of [ADR-0021](0021-follow-desktop-color-scheme.md) and
[ADR-0022](0022-draw-own-window-frame.md) that kept the portal watcher and the frame in this repo.
What they decided, following the desktop and drawing our own frame, still stands.

## Context

Photograph's `frame.rs` and `appearance.rs` were ported from UnAmp, and the copies had drifted.
ADR-0022's revisit trigger was "the frame needs fixing in both often enough to justify a shared
crate". That crate now exists: [Patina](https://github.com/Sector-F-Labs/patina) (`patina-ui`),
the shared frame, components, skin format and design language for the suite. Notebook and
Calendar are built on it.

Patina's skins each fix a light or dark base; only Default follows the desktop. Photograph's look
follows the desktop: Yaru greys in light or dark, with Ubuntu orange in both. Patina's Dark Orange
is the dark half of it.

## Decision

- **Frame:** `patina::frame` replaces `src/frame.rs`:
  - `frame::viewport` on the `ViewportBuilder`;
  - `header_frame` and `TitleBar` (`.menu_bar(true)`) for the header;
  - `edges`, `clear_color` and `release_grab`.

  Its colours come from the active skin's `frame_style`, computed every frame.
- **Appearance:** `patina::appearance::watch` replaces `src/appearance.rs`.
- **Skins:**
  - Patina's built-ins (Default, Calendar, Dark Orange, Notebook, Steam Classic) are offered,
    plus `~/.config/patina/skins/`.
  - **Photograph's look stays, as the "Photograph" skin.** It isn't a skin file: choosing it
    applies `photograph_visuals` for both themes and follows the desktop, as before. It's the
    starting skin (`skin` unset in `config.toml`). A user skin named "Photograph" replaces it.
  - `Skin::apply` replaces the whole egui style, so Photograph's GTK-style spacing is applied
    after every skin. The Ubuntu fonts are separate and stay.
- **Menus:** the header's left side becomes word menus, as in other Patina apps:
  - **File:** Render…, Quit (Ctrl+Q);
  - **View:** System Title Bar, which until now could only be set in `config.toml`;
  - **Skins:** the skin list, Reload, Copy built-in skins to folder, Open skins folder, and load
    errors.

  The Render button stays next to them, and the right side (Edit, GPU badge and status) is
  unchanged.
- **Screenshot mode:** `PHOTOGRAPH_SCREENSHOT=path.png` saves the window a second after start
  and quits. `PHOTOGRAPH_SKIN` sets that run's skin. `make screenshot` writes three into
  `target/`, with config and data in throwaway folders. They show the real Pictures folder, so
  they aren't committed.

## Consequences

- Frame fixes now come from Patina releases: the title-bar hover fix, the second-drag fix, and
  coloured-bar controls. Photograph picks them up with its next `patina-ui` update.
- `patina-ui` is a new dependency, and Photograph must stay on the same egui version as Patina.
- Photograph can be skinned. Its own look isn't a file, so other Patina apps can't offer it;
  Dark Orange is the closest.
- Patina's design guide says a component a second app wants moves into Patina. Photograph's
  sidebar is still its own; Patina's `FolderTree` (from UnAmp) is the candidate to replace it.
- Patina's known issues apply here too: windows that stop redrawing on GNOME Shell until
  minimised (Patina's `docs/known-issues.md`).
