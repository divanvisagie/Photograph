# Architecture Decision Records

This project records significant architecture decisions as ADRs, following Michael
Nygard's convention as summarized by Martin Fowler:
https://martinfowler.com/bliki/ArchitectureDecisionRecord.html

The records themselves live in [`docs/adr/`](adr/README.md). This page explains the
process; `docs/adr/README.md` is the index of decisions that have actually been made.

## What is an ADR?

An Architecture Decision Record captures a single significant architecture decision,
along with its context and consequences, at the time it was made. The goal is that
someone reading the codebase months or years later can find out *why* something is
built the way it is, without having to reconstruct the reasoning from commit history
or guesswork.

## What counts as a decision worth recording

Record a decision when it is architecturally significant: it affects structure,
non-functional characteristics (performance, security, portability), dependencies,
interfaces, or construction techniques — and would be non-obvious or costly to
re-derive later. Routine implementation choices, refactors, or anything trivially
reversible don't need a record.

## File layout

- Directory: `docs/adr/`
- One markdown file per decision: `docs/adr/NNNN-short-kebab-title.md`, where `NNNN`
  is a zero-padded, strictly increasing sequence number (`0001`, `0002`, ...).
- `docs/adr/README.md` is the index: every ADR, its number, title, and current status.
- Numbers are never reused, even for rejected or superseded decisions.

## Record template

```markdown
# NNNN. Title

Date: YYYY-MM-DD

## Status

<Proposed | Accepted | Deprecated | Superseded by [NNNN](NNNN-title.md)>

## Context

What is the issue that we're seeing that is motivating this decision or change?
State the forces at play — technical, business, political — as neutrally as possible.

## Decision

What is the change that we're actually proposing or have agreed to?
State it in active voice: "We will ..."

## Consequences

What becomes easier or more difficult as a result of this change? Include both
positive and negative consequences, not just the ones that favor the decision.
```

Keep records short — a paragraph or two per section is normal. An ADR documents a
decision and its reasoning, not a design document.

## Core rules

1. **Immutable once accepted.** Don't edit the Context, Decision, or Consequences of
   an Accepted record to reflect new information. If circumstances change, write a
   new ADR.
2. **Supersede, don't rewrite.** When a new decision replaces an old one:
   - Create a new numbered ADR describing the new decision.
   - Reference the old ADR from the new one's context.
   - Update only the *old* ADR's Status line, to
     `Superseded by [NNNN](NNNN-title.md)`. The rest of the old file stays untouched
     as a historical record of what was decided and why, at the time.
3. **Sequential numbering.** Use the next integer after the highest existing
   `NNNN-*.md` file in `docs/adr/`, zero-padded to 4 digits.
4. **Status values**: `Proposed`, `Accepted`, `Deprecated`, or `Superseded by ...`.
   New records default to `Accepted` unless the decision is still under discussion,
   in which case use `Proposed`.

## Adding a new ADR

1. Check `docs/adr/` for the next sequence number.
2. Write `docs/adr/NNNN-short-kebab-title.md` using the template above, being honest
   about consequences — drawbacks included, not just upside.
3. Add a row to the index table in `docs/adr/README.md`.
4. If this decision supersedes another, update the superseded file's Status line and
   cross-link both directions.

If you're working with Claude Code in this repo, the `.claude/skills/adr/SKILL.md`
skill automates this process end-to-end.
