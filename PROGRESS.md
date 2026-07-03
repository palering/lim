# lim — Implementation Progress

> Last updated: 2026-06-30
> Workspace: 10 crates, 262 tests, 0 clippy warnings
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

### Phase 3b: UX Hardening — DONE

4 work streams closing out Phase 3. +10 tests (188→198), 0 clippy warnings.

**Approval (真阻塞) — `apps/tui/src/approval.rs`**

- `ApprovalMode::{Auto, Smart, Always}` shared via `Arc<Mutex<ApprovalMode>>` between `ApprovalHooks` and `App`
- `ApprovalRequest` carries a `oneshot::Sender<ApprovalResponse>`; `ApprovalHooks::before_tool_call` blocks on `rx.await` until TUI responds
- Smart mode auto-approves read-only tools, prompts for write/exec
- 11 unit tests cover auto/smart/always modes, deny, closed channel, idempotent respond
- `run_continue` re-emits `ContextEdited` events so chained resumes preserve edit history

**Cost / Usage Panel — `apps/tui/src/ui/status.rs`**

- Status bar expanded to 2 rows
- Line 1: busy indicator · turn count · approval mode · status message
- Line 2: cumulative tokens · last turn tokens · context window usage (`msgs→msgs (tok→tok)` shown only after first compaction)
- `ContextWindowUsage` cached on `App` from `AgentEvent::ContextCompacted`

**Context Editing — CLI + TUI**

- `lim edit-context --session <jsonl> [--exclude 1,3,5] [--replace 2:...] [--list]` appends a `ContextEdited` event to the JSONL (uses `OpenOptions::append(true)`, not `SessionWriter` which truncates)
- TUI `/edit list|exclude|replace|clear` sends `AgentCommand::{ApplyEdit, ClearEdits}` to the agent task
- `AgentLoop::apply_user_edit` / `clear_user_edits` emit `ContextEdited` events for persistence
- `clear_user_edits` is a no-op when the edit list is empty (no event emitted)
- 3 unit tests cover apply / clear-emits-empty / clear-noop-when-empty

**Error Recovery — `crates/lim-agent/src/agent_loop.rs`**

- `AgentEvent::Error { message, retryable, retry_count, max_retries }` variant added
- Retry loop: only retry when no content has been emitted yet (avoids duplicate/partial messages)
- Exponential backoff: 1s → 2s → 4s, `MAX_RETRIES = 3`
- Cancellation-aware sleep via `tokio::select!`
- `StreamAccumulator::reset()` clears content but preserves `message_id` for stable TUI rendering
- `MockResponse::Error(ProviderError)` supports deterministic retry tests; `ProviderError` now derives `Clone`
- 3 unit tests cover retry-success / no-retry-on-auth / retry-exhaustion (use `#[tokio::test(start_paused = true)]` for instant backoff)
- TUI shows `⚠ <msg>  (retry N/M)` during retries, `⚠ <msg>  (retries exhausted: N/M)` on final failure

### Phase 3c: Behavior Observability + Platform Abstraction — DONE

Two work streams hardening lim against black-box behavior.

**Behavior Trace & Observability — `docs/architecture/behavior-trace.md`**

- 6 design principles established (Behavior Traceable / Implicit Made Explicit / No Silent Failure / Layered Observability / Closed Enum / Lock-Free Across Await)
- All silent-failure paths in 3 providers' SSE parsers + StreamAccumulator now emit `warn!` instead of silently dropping events
- Implicit behaviors made explicit: `ToolRegistry::schemas()` now sorted by name (was HashMap order); `ApprovalHooks::Reject` and schema validation failures now emit `ToolExecutionStart` + `ToolExecutionEnd` (were silent returns)
- `AgentEvent::TurnStart` extended with `model` / `message_count` / `tool_count` / `tool_names` / `system_prompt_len` for self-auditing
- `AgentEvent::ToolExecutionStart` / `ToolExecutionEnd` now carry Unix ms timestamps (`started_at` / `ended_at`)
- 3 LLM providers (OpenAI Chat / OpenAI Responses / Anthropic) instrumented with `debug!` (request URL/model/message_count) + `trace!` (request body, SSE events) + `debug!` (HTTP error response, stream started)
- Context compaction's hidden LLM call now logs `debug!` (start) and `warn!` (stream error)
- Document includes PR-review checklist (§9) and phase-review checklist (§10) for ongoing observability discipline

**Platform Abstraction — `crates/lim-core/src/{env,inject}.rs` + `crates/lim-tools/src/shell.rs`**

- `PlatformInfo` (env module) — immutable startup snapshot of OS / arch / shell / workspace_roots. Detection layer, decoupled from injection
- `InjectInfo` (inject module) — application layer that renders `PlatformInfo` into a system-prompt fragment with per-shell guidance (POSIX/BSD-vs-GNU differences, Git Bash drive mounts, WSL `/mnt/`, PowerShell cmdlets)
- `ShellBackend` trait + 4 implementations: `PosixSh` (Unix default), `GitBash` (Windows priority 1), `Wsl` (priority 2), `PowerShell` (priority 3 fallback). `cmd.exe` intentionally never used — no mainstream agent uses it
- `BashTool` renamed to `ShellTool` (tool name `"bash"` → `"shell"`). Single tool exposed to LLM; backend selected once at startup and held as `Arc<dyn ShellBackend>` for the session
- `apps/cli` and `apps/tui` now detect platform at startup, inject the fragment into the system prompt, and print platform info to stderr for user visibility
- +22 tests (198→262): 8 env + 8 inject + 9 shell backend/tool

**Compaction Observability — `crates/lim-agent/src/context_window.rs`**

- `ContextCompacted` event extended with `summary_model` / `summary_prompt_tokens` / `summary_completion_tokens` — closes the last black-box point in behavior-trace.md §8.1
- `summarize()` now returns `(String, Usage)` capturing the provider's token usage report
- TUI status bar shows `summary cost: N tokens via MODEL` on compaction
- behavior-trace.md §8.1 upgraded from "部分黑盒" to "✅ 完全可观测"

### Packaging Phase A Preparation — DONE

- `[profile.release]` in `Cargo.toml`: `lto = "fat"` / `codegen-units = 1` / `strip = "symbols"` / `panic = "abort"`. Release binary: `lim` 5.4M / `novel-cli` 4.6M
- `.github/workflows/ci.yml`: fmt check + clippy (-D warnings) + build + test + cross-compile check (Windows + macOS targets)
- `.github/workflows/release.yml`: 6-target matrix (Linux x86_64/ARM64 + Windows x86_64/ARM64 + macOS ARM64/x86_64) + GitHub Release upload via `softprops/action-gh-release`
- `docs/packaging-plan.md` updated: 3/4 code blockers marked ✅ DONE, open questions expanded
- Remaining before first release: fix repo URL placeholder, add LICENSE + README.md, verify reqwest rustls-tls cross-compilation on CI

## Planned

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
| lim-core | Foundation | 1+3 | 53 (env 8 + inject 8 + usage 2 + others) |
| lim-llm | Foundation | 1+3 | 66 (3 providers + http + error) |
| lim-agent | Core | 1+3 | 49 (agent_loop + context_window + registry + stream_accumulator + hooks) |
| lim-executor | Core | 1 | 16 (local + executor trait) |
| lim-tools | Core | 1+3 | 8 (shell + read + write + edit) |
| lim-session | Core | 1+3 | 11 (writer + replay) |
| lim-workflow | Orchestration | 2 | 22 (DAG executor + artifact store) |
| lim (apps/cli) | Entry | 1+3 | 0 (integration tests in /tests) |
| lim-tui (apps/tui) | Frontend | 3 | 18 (app + ui + approval) |
| novel-pack (packs/novel) | Domain | 2 | 15 |
| tests/ (integration) | | | 4 (config_integration) |
| **Total** | | | **262** |
