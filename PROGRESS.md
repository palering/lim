# lim — Implementation Progress

> Last updated: 2026-06-25
> Workspace: 9 crates, ~6600 lines Rust, 91 tests, 0 clippy warnings
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

## Planned

### Phase 3: Context Compression & TUI

Goal: Long-running tasks become interactive, visualizable, and sustainable.

- ContextWindowManager — summarize older turns, preserve raw JSONL
- lim-tui — ratatui terminal UI with streaming render
- Approval UI — tool approval, cancellation, steering interaction
- Cost/usage panel — real-time token tracking

Acceptance:

- [ ] Long session triggers summary compaction without losing original events
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
| lim-core | Foundation | 1 | 19 |
| lim-llm | Foundation | 1 | 13 |
| lim-agent | Core | 1 | 8 |
| lim-executor | Core | 1 | 6 |
| lim-tools | Core | 1 | 0 |
| lim-session | Core | 1 | 8 |
| lim-cli | Entry | 1 | 0 |
| lim-workflow | Orchestration | 2 | 22 |
| novel-pack (packs/novel) | Domain | 2 | 15 |
| **Total** | | | **91** |
