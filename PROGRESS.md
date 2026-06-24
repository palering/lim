# lim — Implementation Progress

> Last updated: 2026-06-25
> Workspace: 9 crates, ~6000 lines Rust, 81 tests, 0 clippy warnings

## Phase 1: Minimal Agent Runtime — DONE

Commit: `36bc1bf` | Tests: 44

Acceptance criteria from [second-draft.md §10]:

- [x] FakeProvider runs `user -> assistant tool_call -> tool_result -> assistant final` closed loop
- [x] Partial tool call JSON streaming via StreamAccumulator + event updates (MessageStart/Update/End)
- [x] Schema validation failure returns error as tool_result back to LLM
- [x] `write`/`edit` use atomic commit (temp file + fsync + rename)
- [x] `bash` supports cancellation with interrupted tool_result recording
- [x] JSONL event log replays to identical transcript (assert verified in CLI)

Crates delivered: lim-core, lim-llm, lim-agent, lim-executor, lim-tools, lim-session, lim-cli.

## Phase 2: Workflow Engine & Novel-Pack Spike — DONE

Commit: `12831ad` | Tests: 81 (+37)

Acceptance criteria from [second-draft.md §10]:

- [x] Orchestrator splits chapter into scenes (from outline)
- [x] Multiple writers generate scene drafts in parallel (DAG level-based join_all)
- [x] Reviewer outputs structured report (JSON score with fallback parsing)
- [x] Failed scenes auto-enter rewrite loop, max N attempts (ReviewLoop node)
- [x] Final editor assembles chapter artifact
- [x] All intermediate artifacts traceable in session event log + ArtifactStore

Additional deliverables: Kahn's topological sort, WorkflowContext (Arc<RwLock<HashMap>>), ArtifactStore (file-system), novel-cli standalone binary.

## Post-Phase 2 Review Fixes — DONE

Commit: `798a460` | From: [implementation-quality-review](docs/review/2026-06-25-implementation-quality-review.md)

| Priority | Issue | Status |
| :--- | :--- | :--- |
| P0 | Bash tool cancel chain break — CancellationToken not propagated | Fixed: Tool trait accepts CancellationToken, passed from AgentLoop through to executor |
| P0 | tool_result is_error dropped — error flag not stored in AgentMessage | Fixed: AgentMessage gains `is_error: bool` field, tool_result() stores it |
| P0 | novel-pack misplaced in crates/ — architectural layering violation | Fixed: relocated to packs/novel, standalone novel-cli binary, lim-cli decoupled |
| P1 | Image block not using base64 — incompatible with LLM APIs | Open |
| P1 | No real LLM provider — only MockProvider available | Open |

## Phase 3: Context Compression & TUI — Planned

Goal: Long-running tasks become interactive, visualizable, and sustainable.

Scope:

- ContextWindowManager — summarize older turns when approaching token limit, preserve raw JSONL
- Branch summary — inject compressed context without losing original events
- lim-tui — ratatui-based interactive terminal UI
- Approval UI — tool execution approval, cancellation, steering/follow-up interaction
- Cost/usage panel — real-time token and cost tracking

Acceptance criteria:

- [ ] Long session triggers summary compaction without losing original JSONL events
- [ ] TUI renders streaming output without blocking the agent loop
- [ ] Tool approval, cancellation, and steering/follow-up are interactive

Dependencies: Real LLM provider (P1) — needed for genuine streaming data to drive TUI rendering.

### Phase 3 Suggested Order

1. Real LLM provider (OpenAI or Anthropic streaming via reqwest + eventsource)
2. Image block base64 fix (unblocks multimodal)
3. ContextWindowManager + branch summary
4. lim-tui crate skeleton + event consumption
5. Interactive approval UI in TUI
6. Cost/usage panel

## Phase 4: Strong Isolation & External Tools — Future

Goal: Ship L1/L2 isolation and extension system.

Scope:

- ContainerExecutor (bwrap / sandbox-exec)
- RemoteExecutor
- Process JSON-RPC / MCP tool protocol
- Wasm tool runtime (explicit host function grants only)

Acceptance criteria:

- [ ] Host agent + sandboxed tools mode operational
- [ ] Full-container mode operational
- [ ] External process tools can register schemas and be invoked by agent
- [ ] Wasm tools can only access explicitly granted host functions

## Crate Inventory

| Crate | Layer | Phase | Tests |
| :--- | :--- | :--- | :--- |
| lim-core | Foundation | 1 | 18 |
| lim-llm | Foundation | 1 | 4 |
| lim-agent | Core | 1 | 8 |
| lim-executor | Core | 1 | 6 |
| lim-tools | Core | 1 | 0 |
| lim-session | Core | 1 | 8 |
| lim-cli | Entry | 1 | 0 |
| lim-workflow | Orchestration | 2 | 22 |
| novel-pack (packs/novel) | Domain | 2 | 15 |
| **Total** | | | **81** |
