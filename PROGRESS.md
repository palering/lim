# lim — Implementation Progress

> Last updated: 2026-07-03
> Workspace: 12 crates, 432 tests, 0 clippy warnings
> Roadmap: [docs/architecture/evolution.md](docs/architecture/evolution.md)
> Behavior trace: [docs/architecture/behavior-trace.md](docs/architecture/behavior-trace.md)

## Current Phase

**P4 完成** — 阶段 4（Loop Engineering 完整 + CC Time-based loop + 多 agent workspace）全部落地。P0-P4.11 全部 ✅，P3.5 ✅。下一步是 v0.1 release 前的历史压缩（见 [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)）和 packaging 收尾（见 [docs/packaging-plan.md](docs/packaging-plan.md)）。

## Completed Phases

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

Commit `52039a1` | Token estimation, compaction algorithm, summary via provider, AgentLoop integration with ContextCompacted event.

### ContextEdit — 3-Layer Context Projection — DONE

Commit `6064e6c` | Universal `ContextEdit` descriptor (`exclude` + `replace`), pure `apply_edit()` / `compose_edits()` functions, `AgentEvent::ContextEdited` variant, `AgentHooks::edit_context()` extension. AgentLoop integrates the 3-layer pipeline: loop edit (hooks, transient) → user edit (persistent, JSONL) → compact (CWM, token budget).

### LLM Provider Stack — DONE (Phase 3a)

3 self-contained providers with shared SSE parsing (`lim-llm::http`):

| Provider | Type tag | Protocol | Notes |
| :--- | :--- | :--- | :--- |
| OpenAiChatProvider | `openai-chat` | `/v1/chat/completions` | Index-based ToolCallId mapping; compatible with OpenAI/Azure/OpenRouter/DeepSeek/local proxies |
| OpenAiResponsesProvider | `openai-responses` | `/v1/responses` | Complete function calls from `output_item.done`; OpenAI-only |
| AnthropicProvider | `anthropic` | `/v1/messages` | `toolu_*` id preservation; works with DeepSeek anthropic-compatible endpoint |

- Endpoint resolution priority: `full_url` (verbatim) > `base_url + suffix` > `type default + suffix`
- `base_url` is host-only (e.g. `https://api.openai.com`); `/v1` is part of the protocol suffix
- API key via `api_key` (plaintext, dev) or `api_key_env` (env var, production)
- Model alias resolution: CLI `--model` > provider `default_model`

### CLI Integration — DONE (Phase 3a)

`apps/cli` (binary `lim`) subcommands:

- `lim run --provider <name> --model <name> --message "..."`
- `lim run --resume <session.jsonl>` — multi-turn resume with edit replay
- `lim providers` — list configured providers
- `lim tui` — launch terminal UI
- `lim sessions [--details <path>]` — list/inspect saved sessions
- `lim edit-context --session <jsonl> [--exclude 1,3,5] [--replace 2:...] [--list]`
- `lim loop --goal "..." --verify "<cmd>"` — Loop Engineering
- `lim loop-daemon <action>` — daemon mode
- Reads `.lim/config.toml` (gitignored) + global `~/.config/lim/config.toml` (XDG)

### TUI MVP — DONE (Phase 3a)

`apps/tui` (binary `lim-tui`):

- Streaming render (ratatui)
- Multi-turn conversation
- Runtime model/provider switching via `AgentCommand::{SetModel, SetProvider}`
- Agent runs in background tokio task; TUI subscribes via `broadcast::Sender<EventEnvelope>`
- Tool call display with expand/collapse
- Tool approval popup (Auto/Smart/Always modes)
- Context editing via `/edit list|exclude|replace|clear`
- Cost panel: cumulative tokens, last-turn tokens, context window usage, compaction cost

### Phase 3b: UX Hardening — DONE

4 work streams: Approval (Auto/Smart/Always), Cost/Usage Panel, Context Editing (CLI + TUI), Error Recovery (exponential backoff retry).

### Phase 3c: Behavior Observability + Platform Abstraction — DONE

- 6 design principles in [behavior-trace.md](docs/architecture/behavior-trace.md)
- All silent-failure paths now emit `warn!` instead of silently dropping events
- `PlatformInfo` + `InjectInfo` + `ShellBackend` trait (PosixSh / GitBash / Wsl / PowerShell)
- `BashTool` → `ShellTool` (tool name `"shell"`, backend selected at startup)
- `ContextCompacted` event extended with `summary_model` / `summary_prompt_tokens` / `summary_completion_tokens` — closes §8.1 black-box

### Packaging Phase A Preparation — DONE

- `[profile.release]`: `lto = "fat"` / `codegen-units = 1` / `strip = "symbols"` / `panic = "abort"`
- `.github/workflows/ci.yml`: fmt + clippy (-D warnings) + build + test + cross-compile check
- `.github/workflows/release.yml`: 6-target matrix + GitHub Release upload
- `.github/dependabot.yml`: weekly updates (Cargo + Actions), minor+patch grouped, major separate
- MSRV: `rust-version = "1.95"` in Cargo.toml (follows Bevy's stable-tracking strategy)
- See [packaging-plan.md](docs/packaging-plan.md) for remaining release blockers

### P0-P3: Loop Engineering Foundations — DONE

- **P0** (阶段 1 完成 + Loop 成熟度 A): AgentHooks should_stop/max_steps, StateLedger, LoopRunner (start_goal/resume_goal), CLI `lim loop`, CommandVerifier
- **P1** (阶段 2 起步 + Loop 成熟度 B): AgentTransport trait (LocalChannelTransport), Scheduler 雏形, LlmVerifier + 无进展检测
- **P2** (阶段 3 完整): ControllerRegistry (CRD 类比), AI-native 元工具, LoopEvent broadcast
- **P3** (阶段 4 起步): daemon 常驻进程骨架 (in-process IPC), DaemonLedger, P2P 网络拓扑, 多 workspace 管理, CLI `loop-daemon`

### P2.5: Reviewer Agent — DONE

`ReviewerVerifier` + fresh context + tool access = CC "第二 agent 代码审查" pattern. CLI: `--verify reviewer`.

### P3.5: Product Layer Completion — DONE

- CLI 暴露 LlmVerifier 入口 (`--verify llm` + `--verifier-provider/model/instructions`)
- 完成契约写进设计哲学 ([loop-engineering.md](docs/architecture/loop-engineering.md) §一 + §12.6)

### P4: Time-based Loop + Multi-agent Workspace — DONE

11 sub-tasks, all ✅:

| Task | Description |
| :--- | :--- |
| P4.1 | XDG config system (`~/.config/lim/`, `~/.local/share/lim/`, `~/.cache/lim/`) + `crates/lim-config/` |
| P4.2 | `StatusProvider` trait + `ComponentStatus` + `Condition` (k8s-style, Event vs Status separation) |
| P4.3 | `EventEnvelope.agent_id` field (multi-agent session disambiguation) |
| P4.4 | LoopEvent 落盘 + §8.4 黑盒闭合 (`LoopEventPersister` + `ReviewPersister`) |
| P4.5 | 角色配置 `.lim/agents/<role>.toml` + `AgentFactory::for_role` + `ToolRegistry::filtered_clone` |
| P4.6 | Worktree 管理 (`WorktreeHandle` RAII, `.lim/worktrees/<id>/`) |
| P4.7 | Skill 系统 (`crates/lim-skills/`, `SkillLoader`, `SkillVerifier` delegates to Command/Llm) |
| P4.8 | Time-based loop (`Timer` trait + `IntervalTimer` + `TimerLoop` + `LoopEvent::TimeTriggered`) |
| P4.9 | daemon IPC 抽象 (`IpcTransport` trait + `GoalRunner` trait + `DaemonServer::with_goal_runner`) |
| P4.10 | 成本控制 (`BackoffStrategy` trait + `FixedBackoff` + `LoopRunner::with_backoff`) |
| P4.11 | 全量验证 (cargo test + clippy + fmt) |

### B-1/B-2: Behavior Observability Cleanup — DONE (2026-07-03)

- **B-1**: `SessionEventPersister` 持久化主 agent `AgentEvent` 到 `.lim/loop/<goal_id>/session-<N>.jsonl` (previously `event_rx` was discarded)
- **B-2**: `Verifier` trait returns `VerifyOutcome { result, metadata: VerifyMetadata { session_id, tokens_used } }`; `LoopEvent::VerificationDone` now populated from metadata
- §8.4 black-box points in [behavior-trace.md](docs/architecture/behavior-trace.md) fully closed

## Planned

### v0.1 Release

Pre-release cleanup (see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)):

- History squash: tag `v0.0.1-pre-squash`, collapse to 5-6 logical commits, update doc hash references
- Packaging blockers: fix repo URL placeholder, add LICENSE + README.md (see [packaging-plan.md](docs/packaging-plan.md) Open Questions)
- novel-cli packaging strategy: treat as extension, not independent CLI package

### P4+: Proactive Loop (远期)

- Proactive routine 组合 (schedule + goal + 动态工作流 + auto mode)
- 中心化调度策略实现 (CentralizedTopology 实体化)
- daemon 跨 workspace goal 聚合查询

## Crate Inventory

| Crate | Layer | Tests |
| :--- | :--- | :--- |
| lim-core | Foundation | 57 |
| lim-llm | Foundation | 66 (+4 integration) |
| lim-agent | Core | 170 |
| lim-executor | Core | 16 |
| lim-tools | Core | 13 |
| lim-session | Core | 24 |
| lim-config | Foundation | 12 |
| lim-skills | Foundation | 11 |
| lim-workflow | Orchestration | 22 |
| lim (apps/cli) | Entry | 0 |
| lim-tui (apps/tui) | Frontend | 22 |
| novel-pack (packs/novel) | Domain | 15 |
| **Total** | | **432** |
