# Documentation Index

Project documentation for [lim](../README.md). See [PROGRESS.md](../PROGRESS.md) for current status.

## Directory Structure

| Directory | Purpose |
|-----------|---------|
| `archive/` | Completed phase details — technical decisions, patterns established, acceptance criteria. Only contains work that is DONE. |
| `draft/` | Design drafts and unconfirmed ideas — not committed to any phase. May evolve into planned work or be discarded. |
| `review/` | Code/implementation reviews — quality assessments and findings from review sessions. |

## Files

### archive/

- [completed-phases.md](archive/completed-phases.md) — Technical details for each completed phase: key patterns, Rust pitfalls, architecture decisions. Referenced from PROGRESS.md.

### draft/

- [first-draft.md](draft/first-draft.md) — Initial design draft.
- [second-draft.md](draft/second-draft.md) — Revised design draft.
- [library-polish.md](draft/library-polish.md) — Ideas for external API hardening (ProviderFactory, SessionManager, WorkflowContext typed channels). Not confirmed for implementation.

### review/

- [2026-06-25-implementation-quality-review.md](review/2026-06-25-implementation-quality-review.md) — Phase 1+2 implementation quality review. Source of P0/P1 fixes in completed-phases.md.

## Conventions

- **archive/** is append-only for completed work. Do not add future plans or unconfirmed ideas here.
- **draft/** is for exploration. Ideas live here until confirmed (then move to PROGRESS.md) or discarded.
- **review/** preserves review records. Each review is a timestamped snapshot.
