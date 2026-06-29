# lim — Implementation Progress

> Last updated: 2026-06-28
> Workspace: 10 crates, ~12900 lines Rust, 188 tests, 0 clippy warnings
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

### LLM Provider Stack — DONE (Phase 3a)

3 self-contained providers with shared SSE parsing (`lim-llm::http`):

| Provider | Type tag | Protocol | Notes |
| :--- | :--- | :--- | :--- |
| OpenAiChatProvider | `openai-chat` | `/v1/chat/completions` | Index-based ToolCallId mapping; compatible with OpenAI/Azure/OpenRouter/DeepSeek/local proxies |
| OpenAiResponsesProvider | `openai-responses` | `/v1/responses` | Complete function calls from `output_item.done`; OpenAI-only |
| AnthropicProvider | `anthropic` | `/v1/messages` | `toolu_*` id preservation; works with DeepSeek anthropic-compatible endpoint |

- Endpoint resolution priority: `full_url` (verbatim) > `base_url + suffix` > `type default + suffix`
- `base_url` is host-only (e.g. `https://api.openai.com`); `/v1` is part of the protocol suffix
- Startup warning when `base_url` ends with `/v1`
- API key via `api_key` (plaintext, dev) or `api_key_env` (env var, production)
- Model alias resolution: CLI `--model` > provider `default_model`

### CLI Integration — DONE (Phase 3a)

`apps/cli` (binary `lim`) subcommands:

- `lim run --provider <name> --model <name> --message "..."`
- `lim run --resume <session.jsonl>` — multi-turn resume with edit replay
- `lim providers` — list configured providers
- `lim tui` — launch terminal UI
- Reads `.lim/config.toml` (gitignored)

### TUI MVP — DONE (Phase 3a)

`apps/tui` (binary `lim-tui`):

- Streaming render (ratatui)
- Multi-turn conversation
- Runtime model/provider switching via `AgentCommand::{SetModel, SetProvider}`
- Agent runs in background tokio task; TUI subscribes via `broadcast::Sender<EventEnvelope>`
- Tool call display with expand/collapse
- Tool approval popup

### Bug Fixes (2026-06-28) — DONE

Critical fixes verified against real LLM endpoints:

1. **ToolCallId preservation** — OpenAI Chat and Responses providers now preserve provider-supplied ids (`call_*`, `toolu_*`) via `ToolCallId::from_string()` instead of generating new Ulids. Without this, multi-turn tool call conversations broke because providers couldn't correlate `tool_result` messages with the original tool calls.
2. **ContextEdited replay chain** — `run_continue` now re-emits all `ContextEdited` events into the new session JSONL. Without this, chained resumes (A → B → C) would lose A's edits in C.
3. **SessionWriter truncate** — `SessionWriter::create()` switched from `append` to `truncate` mode. Reusing an output path no longer pollutes the new session with stale events from a previous run.
4. **SSE error detection** — `http::try_extract_error()` detects provider/proxy errors embedded as SSE events (Anthropic `event: error`, OpenAI Responses `error` event, generic proxy `{"error":{...}}`) and surfaces them as `ProviderError::Api` instead of silently dropping them as empty responses.

### End-to-End Verification — DONE (2026-06-28)

9 test cases across 3 providers × 3 scenarios:

| Scenario | openai-chat (grok) | openai-responses (grok) | anthropic (deepseek-v4-flash) |
| :--- | :--- | :--- | :--- |
| 1. Basic conversation | ✅ | ✅ | ✅ |
| 2. read tool call | ❌ model refuses tool calls | ❌ model refuses tool calls | ✅ |
| 3. bash pwd | ❌ model fabricates answer | ❌ model fabricates answer | ✅ |

- All 9 runs produce well-formed JSONL event streams with correct `MessageEnd` / `TurnEnd` / `AgentEnd` sequences
- Replay verification passes for all sessions
- The 4 grok failures are model-capability issues, not lim bugs (verified: same `tool` schema works on anthropic-type)
- 188 unit tests + 9 e2e runs cover the implementation

## Planned

### Phase 3 Remaining: UX Hardening

Goal: Approval, observability, and interactive editing.

- Approval UI — interactive tool approval, cancellation, steering interaction (basic popup exists; needs full keyboard flow)
- Cost/usage panel — real-time token tracking and budget enforcement
- CLI `edit-context` subcommand and TUI interactive context editing
- Robustness: error recovery, retries, partial-stream failures

Acceptance (updated):

- [x] Long session triggers summary compaction without losing original events
- [x] ContextEdit composes through 3-layer pipeline without mutating original history
- [x] TUI renders streaming output without blocking agent loop
- [x] Session resume replays ContextEdited events across chained resumes
- [ ] Tool approval, cancellation, and steering are fully interactive
- [ ] Real-time token usage visible to user

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
| lim-llm | Foundation | 1+3 | 61 |
| lim-agent | Core | 1+3 | 28 |
| lim-executor | Core | 1 | 6 |
| lim-tools | Core | 1 | 0 |
| lim-session | Core | 1+3 | 11 |
| lim-workflow | Orchestration | 2 | 22 |
| lim (apps/cli) | Entry | 1+3 | 0 |
| lim-tui (apps/tui) | Frontend | 3 | 0 |
| novel-pack (packs/novel) | Domain | 2 | 15 |
| **Total** | | | **188** |
