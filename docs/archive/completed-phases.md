# Completed Phases — Detail Archive

> This document preserves implementation details for completed phases.
> See [PROGRESS.md](../../PROGRESS.md) for current status.

## Phase 1: Minimal Agent Runtime

Commit: `36bc1bf` → `5568d5c` → `36bc1bf` | Tests: 44

### Acceptance Criteria (all met)

- [x] FakeProvider runs `user -> assistant tool_call -> tool_result -> assistant final` closed loop
- [x] Partial tool call JSON streaming via StreamAccumulator + event updates (MessageStart/Update/End)
- [x] Schema validation failure returns error as tool_result back to LLM
- [x] `write`/`edit` use atomic commit (`.lim-tmp-<ulid>` + fsync + rename)
- [x] `bash` supports cancellation (setpgid + tokio::select! + killpg SIGTERM → 500ms → SIGKILL)
- [x] JSONL event log replays to identical transcript (assert in CLI)

### Key Patterns Established

- **Dyn-compatible traits**: `ToolFuture = Pin<Box<dyn Future + Send>>` for Tool/Hook/Executor traits
- **EventEnvelope serde**: `#[serde(flatten)]` with `extract_ids()` + `recompute_ids()` to avoid ID/payload conflicts
- **AgentLoop emits MessageEnd** for ALL message roles (user, assistant, tool), not just assistant
- **CLI broadcast pattern**: drop agent before awaiting event logger so all senders close

## Phase 2: Workflow Engine & Novel-Pack

Commit: `12831ad` | Tests: 81 (+37)

### Acceptance Criteria (all met)

- [x] Orchestrator splits chapter into scenes from outline
- [x] Multiple writers generate scene drafts in parallel (DAG level-based `join_all`)
- [x] Reviewer outputs structured report (JSON score with fallback parsing)
- [x] Failed scenes auto-enter rewrite loop, max N attempts (ReviewLoop node)
- [x] Final editor assembles chapter artifact
- [x] All intermediate artifacts traceable via ArtifactStore + session events

### Key Implementation Notes

- **Kahn's algorithm** in `graph.rs` for topological sort — level-based parallel execution
- **NodeKind variants**: AgentRun, ParallelFanOut, FanIn, ReviewLoop, Transform
- **HRTB + Arc<dyn Fn> coercion**: pass `Arc<dyn Fn>` by value, not reference, to avoid E0277
- **`#![allow(clippy::type_complexity)]`** for intentional complex `Arc<dyn Fn>` types
- **ArtifactStore**: file-system based, `Arc<RwLock<HashMap>>` WorkflowContext

## Post-Phase 2 Review Fixes

Source: [implementation-quality-review](../review/2026-06-25-implementation-quality-review.md)

### P0 Fixes (commit `798a460`)

**Bash cancel chain break**: Tool trait updated to accept `CancellationToken` parameter. AgentLoop passes `self.cancel.clone()` to `tool.execute()`. BashTool now propagates it to `executor.run_command()` instead of creating a disconnected token.

**is_error propagation**: AgentMessage gained `is_error: bool` field with `#[serde(default, skip_serializing_if = "is_false")]`. The `tool_result()` constructor stores the flag; `user()`, `assistant()`, `system()` set it to false.

**novel-pack relocation**: Moved from `crates/novel-pack` to `packs/novel`. Created standalone `novel-cli` binary at `packs/novel/src/bin/novel-cli.rs`. Removed Chapter subcommand and novel-pack dependency from lim-cli. Workspace Cargo.toml updated.

### P1 Fixes (commit `fae6317`)

**Image base64**: Added `base64 = "0.22"` crate. The `base64_bytes` serde module in `content.rs` now uses `STANDARD.encode()` / `STANDARD.decode()` instead of raw `serialize_bytes()`. Image blocks serialize as base64 strings in JSON, compatible with OpenAI/Anthropic multimodal APIs.

**OpenAI Provider**: New `openai.rs` module in lim-llm (420 lines, 10 tests).
- Dependencies: `reqwest` (rustls-tls, stream), `tokio-stream`
- SSE streaming via `reqwest.bytes_stream()` + manual line parsing (`data: ` prefix)
- Spawned task writes parsed events to `mpsc` channel, consumed as `BoxStream`
- Message conversion: system/user/assistant/tool roles, multi-part content for images
- Tool schema conversion to OpenAI function format
- Chunk parsing: TextDelta, ToolCallStart, ToolCallDelta, Usage, Done
- Model metadata with per-model defaults (gpt-4o: 128k, gpt-4.1: 1M, o-series: 200k)
- Configurable `base_url` for Azure/proxy

## ContextWindowManager (Phase 3 partial, commit `52039a1`)

Tests: +20 (lim-agent 8→28, total 91→111)

### Architecture

Core principle: full `AgentMessage` history is NEVER modified. Compaction only affects the `LlmMessage` projection sent to the provider.

Pipeline: `messages → estimate_tokens → [over budget?] → split old/recent → summarize → assemble LlmMessage list`

- Token estimation: `text.len() / 4` per content block + 10 overhead per message (~±20% accurate)
- Budget: `(context_window - max_output_tokens) × trigger_ratio` (default: 0.8)
- Split: preserve system messages, keep last N user/assistant exchange pairs
- Boundary adjustment: walk back over consecutive tool-related messages to never split assistant(tool_call) + tool(result) groups
- Summary: non-streaming provider call, injected as User-role message with `[Earlier conversation summary]` prefix

### Rust Pattern: Arc<dyn Provider> Auto-deref

`summarize()` takes `Arc<dyn Provider>` by value (not `&Arc<dyn Provider>`) to enable `provider.stream(request)` via method auto-deref. `Arc<dyn Trait>` does NOT implement `Trait`, but method resolution auto-derefs `Arc<T>` to call methods on the inner type.

## ContextEdit Design (Phase 3 discussion)

### Problem

Three distinct context projection needs share the same data pipeline:

1. **Loop edit** — automatic cleanup during agent loop (prune failed retries, truncate huge tool outputs, strip spent thinking blocks). Transient, per-turn, not persisted.
2. **User edit** — manual selection/editing of context via CLI subcommand or TUI interaction. Persistent, stored in JSONL event log, replayed on session restore.
3. **Compact** — automatic token budget management via ContextWindowManager. Stateless, re-computed each turn.

### Design Decision: Shared Data Structure, Independent Triggers

All three layers operate on `&[AgentMessage]` and produce a modified view. They share:

- `ContextEdit { exclude: Vec<usize>, replace: Vec<(usize, Vec<ContentBlock>)> }` — universal edit descriptor
- `apply_edit(messages, edit) -> Vec<AgentMessage>` — pure function

They differ in trigger, lifetime, and persistence:

| Layer | Trigger | Lifetime | Persisted |
|-------|---------|----------|-----------|
| Loop edit | hooks (after tool execution) | Per-turn | No |
| User edit | CLI subcommand / TUI / conversation | Session | Yes (JSONL) |
| Compact | CWM (before each LLM call) | Per-turn | No |

### Pipeline Order

```
messages (original, immutable)
  → [1] loop edit      (hooks, transient)
  → [2] user edit      (manual, persistent)
  → [3] compact        (CWM, token budget)
  → Provider
```

### Why Not Unify Into One Trait

- Loop edit holds hook state, compact holds token config — different lifecycles
- Edit is discrete user decision, compact is continuous heuristic
- Forced unification creates a god-object with unrelated config
- Better: separate modules, shared data types, agent_loop orchestrates

### Hooks Extension

```rust
pub trait AgentHooks: Send + Sync {
    fn before_tool_call(...) -> HookFuture<Result<BeforeToolDecision>>;
    fn after_tool_call(...) -> HookFuture<Result<()>>;
    fn edit_context(&self, messages: &[AgentMessage])
        -> HookFuture<Option<ContextEdit>>;  // NEW
}
```

Fires after tool results are collected, before the next LLM call. Returns `Option<ContextEdit>` — `None` means no edit.

### Session Persistence

`AgentEvent::ContextEdited { edit: ContextEdit }` variant. `SessionReader::load_with_edits()` replays all `ContextEdited` events to rebuild edit state on session restore.

### CLI / TUI Integration

- CLI: `lim-cli edit-context --session <id>` — load session, show messages, select/edit, write `ContextEdited` event
- TUI: hotkey triggers interactive selector, or `/edit` command in conversation
- Both produce the same `ContextEdit` descriptor

## Rust Edition 2024 Pitfalls Encountered

- `std::env::set_var` is `unsafe` (needs `unsafe` block); `std::env::set_current_dir` is NOT
- `collapsible_if`: Edition 2024 let-chains allow `if let Some(x) = y && condition { }` pattern
- `redundant_closure`: `|x| f(x)` can be replaced with `f` directly when types align
- `useless_conversion`: `.into_iter()` unnecessary when `.zip()` accepts `IntoIterator`
