# 0002. Persist edit state via per-image JSON sidecar files

Date: 2026-02-23

## Status

Accepted

## Context

`EditState` (crop, color, geometry adjustments) was fully modeled but never actually persisted:
edits were lost on image switch or app restart, with no save/load wired to any lifecycle event.
Persistence needed a location and a moment to trigger.

Options considered:

- **A central database** (SQLite or similar) keyed by image path. Rejected — adds a dependency and
  a migration story for what is, per image, a small blob of settings; also complicates moving a
  photo folder to another machine, since the edits wouldn't travel with the files.
- **JSON file directly beside the source image** (`IMG_001.RAF.json`). This was the original
  layout. Rejected in favor of a subfolder — it clutters the user's photo directory with one extra
  file per edited image, visible in any file manager or `ls`.
- **`.edits/` sidecar subfolder, mirroring the existing `.thumbnails/` cache convention** (e.g.
  `.edits/IMG_001.RAF.json`). Chosen — see Decision.

## Decision

We will store edit state as per-image JSON sidecar files under a `.edits/` subfolder next to the
source images, mirroring the existing `.thumbnails/` cache folder. `EditState::save`/`load` write
to and read from `.edits/<original-filename>.json`, creating the subfolder on demand. Saving is
wired to image switch, app exit (`on_exit`), and an explicit Save button in the viewer toolbar.

## Consequences

- Edits travel with the photo folder (e.g. across machines, backups) without needing a database or
  export/import step.
- The user's photo directory stays visually clean — sidecar files live in a dotfolder, consistent
  with how `.thumbnails/` already behaves.
- Renaming or moving an individual image file independently of its sidecar silently orphans the
  saved edits (no rename-tracking), since the link between image and sidecar is filename-based, not
  content-based.
- Concurrent access from multiple app instances to the same photo folder isn't guarded against —
  acceptable for a single-user desktop app, but would need revisiting if that usage pattern changed.
