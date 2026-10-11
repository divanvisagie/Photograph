# 0027. Render at the top right, GPU status in a status bar

Date: 2026-10-11

## Status

Accepted: the user asked for the GPU information to move to the bottom right, so the Render button
can sit at the top right of the title bar. Supersedes ADR-0025's placement of Render beside the
menus and of the GPU status in the header.

## Context

After the port to Patina (ADR-0025), the header held File/View/Skins and a Render button on the
left, and Edit, the GPU badge and the preview-backend status on the right. The status took most
of the right side.

## Decision

- **Header, right side:** Render, at the far right before the window buttons, then Edit when a
  photo is selected in the library. File → Render… stays.
- **Status bar:** a slim bar along the bottom of the window. It shows the version (moved from
  the bottom of the sidebar) on the left, and the GPU badge and preview-backend status, with
  their tooltips, on the right.
- The status bar holds both of the window's bottom corners. The sidebar, tools panel, filmstrip
  and central panel no longer round any.

## Consequences

- Render is in the same place in every view.
- The bar costs a line of height, in exchange for a less crowded header and a fixed place for
  status.
