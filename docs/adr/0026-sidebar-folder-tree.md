# 0026. A folder tree in the sidebar, from Patina

Date: 2026-10-11

## Status

Accepted: the user asked for UnAmp's folder tree, now Patina's `FolderTree`, in Photograph. The
layout below, copied from UnAmp, is Proposed.

## Context

Photograph's sidebar listed the current folder's subfolders as a flat column of buttons under
the path bar. Moving deeper meant clicking down one level at a time, and moving across meant
going up first. UnAmp has a collapsible folder tree whose folders are listed on worker threads, so
a slow network share never blocks the UI. It moved into Patina as `patina::tree::FolderTree`
(Patina's ADR-0001).

## Decision

- **The FOLDERS list becomes a `FolderTree`**, laid out as in UnAmp:
  - **Root:** the tree is rooted at the sidebar location that holds the current folder, the
    most specific one (Home, Pictures, Computer, or a storage or network mount).
  - **Locations:** clicking one re-roots the tree there, even inside another location.
  - **Path bar and Up button:** they navigate as before. The tree re-roots only when the new
    folder is outside its root.
  - **Clicking a folder** opens it in the grid, and the tree expands to it and scrolls it into
    view. Entering a folder re-lists it, so new folders appear.
- **Pictures** joins Home and Computer under Locations, as Music does in UnAmp.
- **Scrolling:** the tree scrolls on its own, below the fixed locations and path bar, so
  revealing a deep folder doesn't scroll them away.
- `make screenshot` copies `user-dirs.dirs` into its throwaway config, so Pictures is still known.

## Consequences

- Folders anywhere under the root are a click away, and listing happens off the UI thread.
- The flat subfolder list is gone. The folder scan still records subfolders, for the "empty
  folder" message.
- Fixes to the tree are made in Patina and reach UnAmp too once it moves onto Patina. UnAmp still
  has its own copy.
