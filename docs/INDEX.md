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
    lim-transport/   # T1+ 传输层 + 协议适配 + 服务发现 (v0.2)
    lim-serve/       # serve 统一入口 + 跨平台运行模式 (v0.2)

  apps/           # 应用层 — 可运行的前端
    cli/          # lim 主 binary（子命令分发）
    tui/          # TUI 前端（ratatui + crossterm）

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
- [integration-guide.md](integration-guide.md) — **外部程序集成指南**：按复杂度演进（L1 单次调用 → L2 Agent+工具 → L3 多 agent 编排），以小说编写工具为贯穿案例。面向 GUI/Web 应用开发者。
- [functional-testing.md](functional-testing.md) — 功能测试指南：覆盖全流程的测试用例（基础对话/工具调用/TUI/Loop Engineering/Compaction/Steering/错误处理），按章节顺序执行可验证 v0.1 release 前的功能完整性。
- [packaging-plan.md](packaging-plan.md) — Cross-platform packaging & distribution plan (Phase A baseline binaries → Phase B package managers → Phase C self-update).
- [TODO.md](TODO.md) — Single source of truth for all uncompleted work. Delete this file once everything is done.

### architecture/

- [architecture/overview.md](architecture/overview.md) — lim 架构总览：四层分层（Product/Harness/Core/Primitives）+ 10 条设计原则 + 扩展点组织。所有后续架构决策的参照系。
- [architecture/modules.md](architecture/modules.md) — 模块清单与迭代路线：每个 crate 的子模块、核心类型、职责，以及 v0.1/v0.2/长期迭代方向。overview.md 的展开。
- [architecture/execution-flow.md](architecture/execution-flow.md) — 执行链路与数据流转：启动链路、单 turn 执行、Loop Engineering、Compaction、Session 持久化、配置数据流的运行时视角。
- [architecture/behavior-trace.md](architecture/behavior-trace.md) — 行为链路与可观测性文档：6 条设计原则、Agent Loop / Tool / Provider / Session 链路梳理、已知黑盒点、PR review 与迭代检查清单。**Code review 的关键参考**。
- [architecture/compaction.md](architecture/compaction.md) — Compaction 设计细节：ContextWindowManager 三层管线、SummarizeStrategy 工作流、StructuredSummary schema。
- [architecture/loop-engineering.md](architecture/loop-engineering.md) — Loop Engineering 完整设计：Goal Loop、Verifier、Controller、Ledger、Daemon、Budget、Backoff、Timer。
- [architecture/evolution.md](architecture/evolution.md) — 演进路线：P0-P4 各阶段目标与完成情况。
- [architecture/layers.md](architecture/layers.md) — 分层职责与扩展点详解：每层职责、组件、"谁定义/谁实现/谁调用"关系。
- [architecture/transport-layer.md](architecture/transport-layer.md) — 分层传输架构设计：T0-T3 四层传输光谱、跨平台 T1（Unix socket / Named Pipe / TCP loopback）、协议适配层（ProtocolAdapter）、A2A 协议统一、MCP 双向、服务发现、拓扑自动选择。lim-transport crate 的实现依据。
- [architecture/serve-mode.md](architecture/serve-mode.md) — `lim serve` 统一入口设计：运行模式三层（foreground / --daemon / --system）、跨平台系统服务（systemd / launchd / Windows Service）、协议+能力正交组合、loop-daemon 归并。lim-serve crate 的实现依据。

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
- [zerostack.md](research/zerostack.md) — 2026-07 survey of zerostack (gi-dellav/zerostack v1.6.0). Single-crate agent based on rig 0.39, ~12k LoC, mimalloc, doom-loop detection, 10 prompt modes, ACP support. Identifies 7 design points worth absorbing and confirms lim's strengths in layering, observability, and embeddability.

### review/

- [2026-06-25-implementation-quality-review.md](review/2026-06-25-implementation-quality-review.md) — Phase 1+2 implementation quality review. Source of P0/P1 fixes in completed-phases.md.

## Conventions

- **archive/** is append-only for completed work. Do not add future plans or unconfirmed ideas here.
- **draft/** is for exploration. Ideas live here until confirmed (then move to PROGRESS.md) or discarded.
- **review/** preserves review records. Each review is a timestamped snapshot.
