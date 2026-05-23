# Architecture Decision Records

This directory holds thewiki's Architecture Decision Records (ADRs).

An ADR is a short document that captures one significant architectural or
technical decision: what we picked, what the alternatives were, and why we
chose what we did. ADRs are append-only history. When a decision changes, we
write a new ADR that supersedes the old one — we do not rewrite the old one.

## Convention

- Files live at `docs/adr/NNNN-kebab-case-title.md`, four-digit zero-padded
  number, monotonic, never reused.
- Each ADR uses the following sections, in this order:
  - **Status** — `Proposed`, `Accepted`, `Superseded by ADR-XXXX`, or `Deprecated`.
  - **Date** — ISO-8601 (`YYYY-MM-DD`) of the last status change.
  - **Decision-makers** — names or roles. May be left blank until merge.
  - **Context** — the problem, the constraints, what forces the decision.
  - **Decision** — the chosen option and the rationale, tied back to the constraints.
  - **Alternatives considered** — each option with pros, cons, and fit assessment.
  - **Consequences** — positive, negative, and neutral outcomes.
  - **References** — links to docs, benchmarks, issues, prior art.
- Length target: 600–1200 words. Longer is fine when content is load-bearing;
  prose for its own sake is not.
- ADRs are reviewed in the same PR that introduces or supersedes them. Status
  becomes `Accepted` only after merge.

## Index

| #    | Title                                          | Status   | Date       |
| ---- | ---------------------------------------------- | -------- | ---------- |
| 0001 | [Markdown renderer](./0001-markdown-renderer.md) | Accepted | 2026-05-21 |
| 0002 | [Template syntax](./0002-template-syntax.md)     | Accepted | 2026-05-23 |

## Why ADRs

The format follows Michael Nygard's original 2011 proposal
(<https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions>)
and the conventions of <https://adr.github.io/>. ADRs give later contributors
the **why** behind a choice, not just the **what** — which is the information
that disappears fastest from a codebase otherwise.
