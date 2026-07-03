# Documentation Index

Project documentation for [lim](../README.md). See [PROGRESS.md](../PROGRESS.md) for current status.

## Repository Structure

```
lim/
  Cargo.toml      # workspace root

  crates/         # 核心 library — 可导出的 crate
    lim-core/
    lim-llm/
    lim-agent/
    lim-executor/
    lim-tools/
    lim-session/
    lim-workflow/

  apps/           # 应用层 — 可运行的前端
    cli/          # lim 主 binary（子命令分发）
    tui/          # TUI 前端（骨架，待实现）

  packs/          # 领域扩展 — 特定领域的 lib + bin
    novel/        # novel-pack（小说创作）

  docs/           # 项目文档（本目录）
```

## Documentation Directories

| Directory | Purpose |
|-----------|---------|
| `archive/` | Completed phase details — technical decisions, patterns established, acceptance criteria. Only contains work that is DONE. |
| `draft/` | Design drafts and unconfirmed ideas — not committed to any phase. May evolve into planned work or be discarded. |
| `research/` | External landscape research — technology surveys of other tools, technical mechanism analyses. Reference material for design decisions. |
| `review/` | Code/implementation reviews — quality assessments and findings from review sessions. |

## Files

- [architecture.md](architecture.md) — Confirmed architectural decisions: frontend/library boundary, directory structure, library exportability.
- [DEVELOPMENT.md](DEVELOPMENT.md) — Standard development workflow guide for AI agents and human contributors.
- [packaging-plan.md](packaging-plan.md) — Cross-platform packaging & distribution plan (Phase A baseline binaries → Phase B package managers → Phase C self-update).
- [TODO.md](TODO.md) — Single source of truth for all uncompleted work. Delete this file once everything is done.

### architecture/

- [architecture/behavior-trace.md](architecture/behavior-trace.md) — 行为链路与可观测性文档：6 条设计原则、Agent Loop / Tool / Provider / Session 链路梳理、已知黑盒点、PR review 与迭代检查清单。**Code review 的关键参考**。

### archive/

- [completed-phases.md](archive/completed-phases.md) — Technical details for each completed phase: key patterns, Rust pitfalls, acceptance criteria. Referenced from PROGRESS.md.

### draft/

- [first-draft.md](draft/first-draft.md) — Initial design draft.
- [second-draft.md](draft/second-draft.md) — Revised design draft.
- [library-polish.md](draft/library-polish.md) — Ideas for external API hardening (ProviderFactory, SessionManager, WorkflowContext typed channels). Not confirmed for implementation.
- [trust-and-approval.md](draft/trust-and-approval.md) — Trust levels + ApprovalMode (5 tiers) + sandbox architecture design. Decouples Executor/Sandbox into separate crates with feature-gated policy. References Codex/Claude Code/Trae/Goose/MXC/LiteBox.

### research/

- [agent-sandbox-landscape.md](research/agent-sandbox-landscape.md) — 2025-2026 survey of sandbox mechanisms across Codex/Claude Code/Trae/Qoder/Cursor/Aider/Goose/OpenHands/Devin/Roo Code. Covers Landlock/bwrap/Seatbelt/microVM/Wasm + Microsoft MXC/LiteBox.
- [system-prompt-persistence.md](research/system-prompt-persistence.md) — How agents keep system prompt instructions effective: per-turn resend + prompt caching, message-layer `<system-reminder>`, tool-layer hard constraints. Claude Code 10-step assembly + Codex 5-layer injection.
- [mxc-feasibility-study.md](research/mxc-feasibility-study.md) — Deep dive on Microsoft MXC as lim-sandbox backend. Architecture near-perfect fit (mxc-sdk library API 1:1 with Executor trait), but 3 hard blockers prevent immediate adoption. Includes adoption conditions + quarterly review checklist.

### review/

- [2026-06-25-implementation-quality-review.md](review/2026-06-25-implementation-quality-review.md) — Phase 1+2 implementation quality review. Source of P0/P1 fixes in completed-phases.md.

## Conventions

- **archive/** is append-only for completed work. Do not add future plans or unconfirmed ideas here.
- **draft/** is for exploration. Ideas live here until confirmed (then move to PROGRESS.md) or discarded.
- **review/** preserves review records. Each review is a timestamped snapshot.
