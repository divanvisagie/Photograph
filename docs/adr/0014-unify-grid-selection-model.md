# 0014. Unify grid selection to drive fullscreen view, filmstrip, and Render targeting

Date: 2026-08-08

## Status

Accepted

## Context

Clicking a thumbnail in the Library grid used to do two jobs at once: it set `Browser::selected`
*and* immediately opened the single-photo `Detail` view, which bundled the image, the full edit
tools panel, and a filmstrip of every photo in the folder into one mode. There was no way to just
glance at a photo without also being dropped into the edit UI.

Separately, Ctrl/Cmd-click toggled membership in a `marked: HashSet<PathBuf>`, used only to pick a
batch for Render. It had no relationship to viewing — the filmstrip never reflected it.

We wanted three things: (1) grid click to behave like a normal file browser, where selecting and
opening are different gestures; (2) a lightweight fullscreen/loupe view distinct from the heavier
edit UI, reachable on demand; (3) the filmstrip repurposed to show "the photos currently selected"
rather than "everything in this folder" (redundant with the grid itself).

Once Shift-click was added to build that "currently selected" working set via range-select, having
Ctrl-click write to a *different* set (`marked`, invisible to the filmstrip) produced a visible
inconsistency: Ctrl-selecting photos checked their boxes but didn't add them to the filmstrip while
editing, even though both gestures look like "select this photo" to the user.

Keeping "marked for export" and "selected for filmstrip" as two separate sets — one updated by
Ctrl-click, one by Shift-click, with the checkbox visual just layered on top of whichever set
was relevant — was considered and rejected: it preserves exactly the inconsistency that prompted
the question, since two clicks that both look like selection would keep driving two different,
partially-overlapping pieces of state.

## Decision

We will split single-photo viewing into two distinct `ViewMode`s: `Fullscreen` (image only, no
tools panel, no filmstrip) and `Edit` (tools panel + filmstrip + crop/save toolbar). A thumbnail
double-click opens `Fullscreen`. The `Edit` view is reached explicitly via an "Edit" button on the
top bar, enabled whenever a photo is focused (`Browser::selected.is_some()`) — not only when
already in `Fullscreen`.

We will collapse "marked for batch export" and "selected for filmstrip" into one
`Browser::selection: HashSet<PathBuf>`:

- Plain click focuses a photo (`Browser::selected`) and resets `selection` to just that one photo.
- Ctrl/Cmd-click toggles a photo in/out of `selection` and moves the range-selection anchor to it.
- Shift-click range-selects from the anchor to the clicked photo, replacing `selection` with that
  range.

This single set drives the checkbox badge on each thumbnail, the filmstrip's contents, and what
Render targets (falling back to the lone focused photo when `selection` is empty).
`Browser::selected` stays a separate single "focused/open" photo — the blue-highlighted grid cell
and the Edit target — decoupled from the (possibly multi-item) `selection`.

## Consequences

- Ctrl-click and Shift-click are now interchangeable ways of building the same working set; either
  one keeps the filmstrip and the Render target in sync, closing the gap that prompted this
  change.
- The filmstrip's meaning changed: it no longer browses the whole folder while editing — it shows
  only the current `selection`. Anyone expecting "click any thumbnail to jump to it while editing"
  now has to select it first (or use arrow-key stepping through `Browser::images`, which still
  walks the full folder).
- Opening a photo now requires a double-click; a single click only selects. This is a bigger
  behavioral change from the previous single-click-opens model, deliberately traded for
  file-manager-style select/open separation.
- `Browser::selected_paths()` / `selection_count()` replace the old `marked_paths()` /
  `marked_count()`. The concept is now overloaded across three jobs (checkbox, filmstrip, Render
  target) — a future feature needing "flagged for later, independent of today's viewing session"
  would need its own new set rather than reusing this one.
- `Browser::step_active_photo` (arrow-key stepping in Fullscreen/Edit) moves `selected` without
  touching `selection`, so stepping away from a selection with the arrow keys silently detaches
  the focused photo from the filmstrip/Render set. Known, not fixed here.
