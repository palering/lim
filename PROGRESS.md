# lim — Implementation Progress

> Last updated: 2026-06-25
> Workspace: 9 crates, ~8200 lines Rust, 122 tests, 0 clippy warnings
> Details: [docs/archive/completed-phases.md](docs/archive/completed-phases.md)

## Completed

### Phase 1: Minimal Agent Runtime — DONE

Commit `36bc1bf` | 7 crates, 44 tests. Agent loop closed loop with FakeProvider, tool calls, atomic writes, bash cancellation, JSONL replay.

### Phase 2: Workflow Engine & Novel-Pack Spike — DONE

Commit `12831ad` | +lim-workflow +novel-pack, 81 tests. Kahn's DAG executor, parallel scene writing, review loop, artifact store, novel-cli binary.

### Review Fixes (P0 + P1) — DONE

Commits `798a460`, `fae6317`. All review items resolved:

| # | Issue | Resolution |
| :--- | :--- | :--- |
| P0 | Bash cancel chain break | Tool trait accepts CancellationToken |
| P0 | is_error dropped | AgentMessage stores error flag |
| P0 | novel-pack layering | Relocated to packs/novel, standalone CLI |
| P1 | Image block raw bytes | base64 encoding (base64 0.22) |
| P1 | No real LLM provider | OpenAiProvider with SSE streaming |

### ContextWindowManager — DONE

Commit `52039a1` | +20 tests (lim-agent: 8→28). Token estimation, compaction algorithm, summary via provider, AgentLoop integration with ContextCompacted event.

### ContextEdit — 3-Layer Context Projection — DONE

Commit `6064e6c` | +11 tests (lim-core: 19→30). Universal `ContextEdit` descriptor (`exclude` + `replace`), pure `apply_edit()` / `compose_edits()` functions, `AgentEvent::ContextEdited` variant, `AgentHooks::edit_context()` extension. AgentLoop integrates the 3-layer pipeline: loop edit (hooks, transient) → user edit (persistent, JSONL) → compact (CWM, token budget).

## Planned

### Phase 3: Context Compression & TUI (continued)

Goal: Long-running tasks become interactive, visualizable, and sustainable.

Done: ContextWindowManager, ContextEdit (see above).

Remaining:

- lim-tui — ratatui terminal UI with streaming render
- Approval UI — tool approval, cancellation, steering interaction
- Cost/usage panel — real-time token tracking
- CLI `edit-context` subcommand and TUI interactive context editing
- SessionReader replay of `ContextEdited` events on session restore

Acceptance:

- [x] Long session triggers summary compaction without losing original events
- [x] ContextEdit composes through 3-layer pipeline without mutating original history
- [ ] TUI renders streaming output without blocking agent loop
- [ ] Tool approval, cancellation, and steering are interactive

### Phase 4: Strong Isolation & External Tools

Goal: L1/L2 isolation and extension system.

- ContainerExecutor (bwrap / sandbox-exec)
- RemoteExecutor
- Process JSON-RPC / MCP tool protocol
- Wasm tool runtime

Acceptance:

- [ ] Host agent + sandboxed tools mode operational
- [ ] External process tools register schemas and are invoked by agent
- [ ] Wasm tools only access explicitly granted host functions

## Crate Inventory

| Crate | Layer | Phase | Tests |
| :--- | :--- | :--- | :--- |
| lim-core | Foundation | 1+3 | 30 |
| lim-llm | Foundation | 1 | 13 |
| lim-agent | Core | 1+3 | 28 |
| lim-executor | Core | 1 | 6 |
| lim-tools | Core | 1 | 0 |
| lim-session | Core | 1 | 8 |
| lim-cli | Entry | 1 | 0 |
| lim-workflow | Orchestration | 2 | 22 |
| novel-pack (packs/novel) | Domain | 2 | 15 |
| **Total** | | | **122** |
