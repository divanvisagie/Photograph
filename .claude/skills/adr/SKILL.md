---
name: adr
description: Create a new Architecture Decision Record in docs/adr/ following this repo's ADR convention. Use whenever the user asks to write/create/log an ADR, record an architecture decision, backfill decisions from history, or says "document this decision".
---

# Writing an ADR

Photograph records significant, hard-to-reverse architecture decisions as ADRs in `docs/adr/` —
see `docs/adr/README.md` for the full convention (when one is warranted). This skill covers only
the mechanics of adding one.

## Steps

1. Read `docs/adr/README.md`'s index to find the next unused number — never guess or reuse one.
2. Copy `docs/adr/0000-template.md` to `docs/adr/NNNN-short-kebab-title.md` (4-digit, zero-padded).
3. Fill in Context / Decision / Consequences from what was actually discussed — options considered
   and rejected included. Don't invent alternatives that weren't seriously on the table, and don't
   pad Consequences with only upside; the honest costs are the point.
4. Set `Status`: `Proposed` if the user hasn't explicitly signed off yet, `Accepted` if they have.
5. Add a row to the index table in `docs/adr/README.md`.
6. If this decision reverses or replaces an earlier ADR, update that ADR's `Status` to
   `Superseded by [ADR-NNNN](NNNN-title.md)` — never delete or silently rewrite an old one.

## Backfilling from history

When asked to backfill decisions from commit history or existing code, treat each commit's own
message as the source of truth for Context/Decision — don't invent rationale that isn't there.
Preserve the real chronology, including reversals: if a decision was later reversed, write both as
separate numbered ADRs and mark the earlier one `Superseded by` the later one, rather than only
recording the current end state.

Don't write an ADR for something that hasn't actually been decided yet — if the user is still
weighing options, that's a conversation, not a record.
