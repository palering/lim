# lim — Trust & Approval Design (Draft)

> Status: Draft (2026-06-30)
> Owner: lim team
> Related: [../research/agent-sandbox-landscape.md](../research/agent-sandbox-landscape.md), [../research/system-prompt-persistence.md](../research/system-prompt-persistence.md), [../architecture.md](../architecture.md), [../packaging-plan.md](../packaging-plan.md)

## Goal

Replace the current `LocalExecutor::permissive()` default (no restrictions) with a layered trust + approval model that:

1. Has a safe default (`workspace` trust, not `permissive`)
2. Decouples **trust enforcement** (hard boundary) from **approval prompting** (soft UX)
3. Is pluggable — downstream crates can disable the entire policy module and implement their own
4. Keeps `Executor` and `Sandbox` in **separate crates** to avoid tight coupling
5. Preserves prompt caching (system prompt static, dynamic info via message layer)

## Two Orthogonal Concepts

Current code conflates two things:

| Concept | Nature | Existing impl | Problem |
|---|---|---|---|
| **Trust** (hard boundary) | Can the system allow this op at all? | `DefaultPolicy` fields empty = permissive | Not enforced |
| **ApprovalMode** (soft prompt) | Does the user approve each op? | `Auto/Smart/Always` implemented | Not linked to Trust |

**Key principle**: Trust is a **hard boundary**; ApprovalMode is a **soft prompt**. Even with `ApprovalMode=Auto`, `Trust=Workspace` refuses out-of-workspace writes. Both must pass to proceed. They are orthogonal.

## Trust Levels

Three levels (L2 is Phase 4, interface reserved now):

| Level | Name | Paths | Commands | Network | Use case |
|---|---|---|---|---|---|
| **L0** | Permissive | all | all | all | dev/debug only |
| **L1** | Workspace | within `workspace_roots` | blocklist (`rm -rf /`, `sudo`, `curl\|sh`, etc.) | allowed | **default production** |
| **L2** | Sandbox | workspace + tmp | via bwrap/Seatbelt/restricted-token isolation | default off | untrusted code |

## ApprovalMode (5 tiers)

Extends the current 3 (`Auto/Smart/Always`) with intermediate tiers for finer UX. Each mode defines the set of tool categories auto-approved; everything else prompts.

```rust
pub enum ApprovalMode {
    /// Auto-approve all (only allowed when Trust=Workspace/Sandbox)
    Auto,
    /// Read-only auto (read); write+script prompt
    AllowReadOnly,
    /// Non-script auto (read/write/edit); script (bash) prompt
    AllowNonScript,
    /// Non-network auto; network prompt
    AllowNonNetwork,
    /// Prompt for all
    Always,
}
```

**Tool category** declared by each tool via `Tool::category()` with default `ReadOnly`:

```rust
pub enum ToolCategory {
    ReadOnly,   // read, ls, grep
    Write,      // write, edit
    Script,     // bash, powershell
    Network,    // fetch (future)
    Remote,     // MCP / remote tools (future)
}
```

Tools override as needed; default is safest (read-only).

## Trust × ApprovalMode Linkage

**"Must have at least one layer of insurance"** — core constraint. Matrix:

| ApprovalMode \ Trust | Permissive | Workspace | Sandbox |
|---|---|---|---|
| Auto | ❌ forbidden combo | ✅ write ops go path check, OOB reject | ✅ same as Workspace + sandbox |
| AllowReadOnly / AllowNonScript / AllowNonNetwork | ❌ forbidden | ✅ | ✅ |
| Always | ✅ only allowed combo with Permissive | ✅ | ✅ |

**Enforcement**: `Trust=Permissive` locks `ApprovalMode` to `Always`; requires multi-step explicit confirmation to unlock. Runtime ApprovalMode downgrade to Auto/Smart while Trust=Permissive is rejected with a warning.

**Multi-step explicit confirmation** (to unlock Permissive+Auto danger combo): two consecutive confirms + type specific string (e.g. `I understand`), logged to session JSONL.

**Evaluation order** (per tool call):
1. `denylist.contains(name)` → reject (highest priority)
2. Trust level check (path / command / network against workspace_roots + blocklist) → reject if OOB
3. ApprovalMode + ToolCategory decision → prompt or auto-allow
4. `allowlist.contains(name)` → allow

## Crate Architecture (decoupled)

**Key decision**: `Executor` and `Sandbox` in **separate crates**. `Executor` defines the execution interface; `Sandbox` is an independent crate that wraps any `Executor` with isolation. Policy lives in its own feature-gated module.

```
crates/
├── lim-executor/              # Executor trait + LocalExecutor (no sandbox, no policy by default)
│   ├── Cargo.toml             [features] default = ["policy"]
│   │                                       policy = []
│   ├── src/
│   │   ├── lib.rs
│   │   ├── executor.rs        # Executor trait (never depends on policy/sandbox)
│   │   ├── local.rs           # LocalExecutor (calls policy if present, else permissive)
│   │   └── policy/            # ← under feature = "policy"
│   │       ├── mod.rs         # TrustLevel / AccessPolicy / PolicyDecision
│   │       ├── path.rs        # workspace_roots check
│   │       ├── command.rs     # blocklist match
│   │       └── network.rs     # domain allowlist (Phase 4)
│
├── lim-sandbox/               # NEW crate, independent of lim-executor
│   ├── Cargo.toml             depends-on: lim-executor (trait only)
│   ├── src/
│   │   ├── lib.rs             # SandboxExecutor wraps Box<dyn Executor>
│   │   ├── trait.rs           # Sandbox trait (isolate + exec)
│   │   ├── linux/             # Landlock + seccomp + bwrap (Phase 4)
│   │   ├── macos/             # Seatbelt / sandbox-exec (Phase 4)
│   │   └── windows/           # Restricted token + ACL (Phase 4)
│
└── lim-agent/                 # Agent layer; defines ToolGuard trait
    └── src/hooks.rs           # ToolGuard trait (neutral, not policy-specific)
```

### Why separate `lim-sandbox` crate

- **Decoupling**: downstream may want executor without sandbox, or sandbox with a custom executor
- **Optional**: `lim-sandbox` is an optional dep; CLI/TUI opt-in via feature
- **Pluggable backends**: future Wasm/microVM backends live here without bloating lim-executor
- **Reference alignment**: Codex has separate `linux-sandbox` / `windows-sandbox-rs` / `bwrap` crates; same separation principle

### Why policy is a feature, not a crate

- Policy checks need `Executor` internals (path/command); tighter coupling than sandbox
- Keeping it in lim-executor with a feature flag lets downstream `default-features = false` to fully exclude
- A separate `lim-policy` crate would create circular deps (policy needs Executor types, Executor is where checks are called)

## ToolGuard Trait (neutral interface)

`ApprovalHooks::before_tool_call` does NOT depend on `AccessPolicy` directly. It takes a trait `ToolGuard`:

```rust
// in lim-agent (or lim-core)
pub trait ToolGuard: Send + Sync {
    fn check(&self, op: &Operation) -> PolicyDecision;
}

pub enum PolicyDecision {
    Allow,
    Prompt,
    Reject(String),
}
```

`AccessPolicy` implements `ToolGuard`. Downstream can implement `MyCustomGuard`. ApprovalHooks holds `Arc<dyn ToolGuard>`, not `AccessPolicy`. **Fully removable**: downstream `default-features = false` + own `ToolGuard` impl (e.g. `AlwaysAllow`) + inject ApprovalHooks. lim-executor policy code never compiles.

## Operation Type (neutral)

Lives in `lim-core` (most neutral; tools and hooks both need it):

```rust
pub enum Operation {
    ReadFile { path: Utf8PathBuf },
    WriteFile { path: Utf8PathBuf },
    EditFile { path: Utf8PathBuf },
    RunCommand { command: String, args: Vec<String> },
    NetworkAccess { host: String, port: Option<u16> },
}
```

Tools construct `Operation` before execute, pass to `ToolGuard::check`.

## Sandbox Backends (Phase 4)

Based on three supplementary research investigations (BPF, ASRT/MXC reusability, Wasm), lim adopts a **self-built thin layer + platform-native primitive crate combo**. None of the existing sandbox projects can be used as a hard dependency; all serve as design references.

### Why not directly depend on existing implementations

| Project | Actual situation | Why not hard-depend |
|---|---|---|
| Anthropic `sandbox-runtime` | Real name `anthropic-experimental/sandbox-runtime`, **TypeScript/Node.js** package (`@anthropic-ai/sandbox-runtime` v0.0.62), Apache-2.0 | Node.js runtime distribution cost conflicts with lim's Containment Principle; Rust FFI unrealistic; only spawn CLI mode works, but Node runtime still needed |
| Microsoft MXC | `microsoft/mxc`, **Rust 72.5%**, MIT, but v0.7.0-alpha, README explicitly states "no MXC profiles should be treated as security boundaries currently" | Not yet stable, no crate published to crates.io, only ~1 month old (Build 2026 public release) |
| BPF (Tetragon/Tracee) | Technically capable of per-process-tree file access interception + real-time block | Linux-only; requires root/CAP_BPF (mainstream distros `unprivileged_bpf_disabled=1`); conflicts with Containment Principle; Codex/Claude Code both chose not to use BPF |
| Wasm (Wasmtime) | WASI 1.0 not yet production-ready (June 2026) | **Fatal**: WASI has no `fork`/`exec`, bash tool cannot be Wasm-ized — this is lim's core capability; no mainstream agent uses Wasm as harness sandbox |

### Self-built path: platform-native primitives

```
crates/lim-sandbox/
├── trait.rs           # SandboxBackend trait
├── linux.rs           # landlock 0.4.5 + libseccomp 0.4.0 (cargo add)
├── macos.rs           # sandbox_init() direct call (nix/libc crate, NOT sandbox-exec CLI)
└── windows.rs         # windows 0.62 crate's AppContainer API (Win32_Security_Isolation)
```

| Platform | Backend | Startup | Reference |
|---|---|---|---|
| Linux | `landlock` crate + `libseccomp` crate combo | < 50ms | Codex `linux-sandbox` crate (Apache-2.0, code reading reference) |
| macOS | `sandbox_init()` direct call via `nix`/`libc` crate, dynamic TinyScheme profile generation | < 50ms | MXC `seatbelt_common` crate design (MIT, design blueprint); NOT calling `sandbox-exec` CLI to avoid external binary dependency |
| Windows | `windows` crate AppContainer API | native | MXC `appcontainer_common` reference |

**Key design decisions**:
- **macOS does NOT call `sandbox-exec` CLI** — directly uses `sandbox_init()` in `Command::pre_exec` (MXC's approach, more robust, avoids external binary dependency)
- **Linux uses crate combination not vendoring** — `landlock` 0.4.5 (11.2M downloads, MIT/Apache-2.0) + `libseccomp` 0.4.0 (1.66M downloads) already do the heavy lifting; no need to vendor Codex's entire `linux-sandbox` crate
- **Network isolation** (Phase 4): reference ASRT's proxy model (HTTP proxy + SOCKS5 + domain allowlist), simplified to "all deny + allowlist domains" local proxy in Phase 1-2

### BPF as optional Linux observability layer (Phase 3+)

Not the default backend. Role: observability / audit / advanced enforcement. Integration method: detect if Tetragon/Tracee daemon is running; if so, register policy via its API; lim itself does not load BPF programs. Use `matchNamespaces` (not `matchBinaries`, to avoid shebang trap) for process-tree filtering.

### Wasm as L2 extension plugin runtime (future, 12-18 months)

Not the main sandbox. Reserved for "user uploads .wasm tool plug-and-play" scenario, borrowing Extism's pattern. Trigger condition: WASI 1.0 stable + component model mature + real user extension demand. Until then, technical pre-research only, no mainline investment.

**Architecture relationship**:
```
L0 policy (current policy.rs)  →  L1 OS sandbox (lim-sandbox: bwrap/Seatbelt/Landlock)  →  L2 optional Wasm plugin runtime (future)
```
Wasm is L2 extension layer, orthogonal to L1. If lim doesn't accept user .wasm plugins, pure OS sandbox suffices, no double layer needed.

## System Prompt Layer (3-layer persistence)

Per research, system prompt alone is insufficient. lim uses three layers:

```
┌─ L1: System Prompt static segment (cache-friendly, stable) ──┐
│  "Prefer searching files under workspace"                   │
│  "All artifacts must stay within project dir"              │
│  (from AGENTS.md / .lim/AGENTS.md)                         │
└───────────────────────────────────────────────────────────┘
┌─ L2: Message-layer dynamic injection (per-turn, no system  ─┐
│  prompt edit to preserve cache)                             │
│  <system-reminder>                                          │
│  Current workspace: /path/to/project                        │
│  Current trust level: workspace                             │
│  </system-reminder>                                         │
└───────────────────────────────────────────────────────────┘
┌─ L3: Tool-layer hard constraint (holds even if LLM forgets) ─┐
│  write_file(path) → check path.startswith(workspace)       │
│  bash(cmd) → check cmd not in blocklist                     │
│  OOB → reject + emit Error                                  │
└───────────────────────────────────────────────────────────┘
```

**Context compaction**: reference Codex — preserve `encrypted_content`-style "latent understanding" after compaction; key instructions (workspace/trust) must appear in compacted messages. lim's `ContextWindowManager` must ensure workspace + trust directives survive compaction.

## Configuration

### CLI flag

```
lim run --trust permissive|workspace|sandbox   # default: workspace
```

`permissive` prints a warning; `sandbox` requires `lim-sandbox` feature compiled in.

### config.toml

```toml
[executor]
trust = "workspace"                    # project-level default
workspace_roots = ["."]                # explicit; empty = auto-detect (git root or cwd)
blocked_commands = ["rm -rf /", "sudo"]  # appended to hardcoded defaults

[approval]
mode = "AllowNonScript"                # default ApprovalMode
allowlist = ["read", "ls", "grep"]     # explicit allow
denylist = ["curl|sh"]                # explicit deny (highest priority)

[sandbox]                              # Phase 4, optional
backend = "auto"                       # auto | bwrap | seatbelt | none
network = "off"                        # off | proxy | allow
```

### TUI runtime commands

- `/trust <level>` — switch trust (only upgrade allowed; downgrade-to-permissive requires multi-confirm)
- `/approval <mode>` — switch ApprovalMode (subject to Trust×Approval combo rules)
- `/allow <tool>` — add to allowlist
- `/deny <tool>` — add to denylist

### workspace_roots resolution

Priority: config explicit > git root (`git rev-parse --show-toplevel`) > cwd.

Git root preferred (matches developer mental model); falls back to cwd for non-git projects. Also settable via "open directory" in future GUI.

### Remote tools (Phase 4)

When remote tools (MCP/Remote) configured, TUI prompts at startup:
```
Remote tools detected. Select trust level:
  [1] Sandbox (recommended, default isolated)
  [2] Workspace (testing, easier debugging)
  [3] Don't enable remote tools
```
Selection persisted to session; runtime changeable. **Default for remote = Sandbox**; testing phase may use Workspace.

## Implementation Phasing

### Phase 1 (minimal change, kill permissive default)

- `LocalExecutor::workspace(roots)` constructor as new default
- `DefaultPolicy` actually checks `workspace_roots` (read/write/edit pre-call path prefix match)
- `apps/cli/main.rs:330,507` → `LocalExecutor::workspace(vec![cwd])`
- Add `--trust` flag; `permissive` opt-in with warning
- ApprovalMode unchanged

### Phase 2 (command blocklist + linkage)

- `DefaultPolicy.dangerous_commands` actually enforced (bash pre-call match)
- `ApprovalHooks::before_tool_call` queries current Trust; OOB → `BeforeToolDecision::Reject`
- TUI shows `⚠ blocked by trust policy: <reason>`
- Expand ApprovalMode to 5 tiers + ToolCategory

### Phase 3 (ToolGuard abstraction)

- Extract `ToolGuard` trait to lim-agent/hooks.rs
- `AccessPolicy` implements `ToolGuard`
- `ApprovalHooks` holds `Arc<dyn ToolGuard>`
- `Operation` type in lim-core
- Feature-gate policy module

### Phase 4 (Sandbox crate)

- New `crates/lim-sandbox/`
- `Sandbox` trait + `SandboxExecutor`
- Linux: Landlock + seccomp + bwrap
- macOS: Seatbelt
- Windows: Restricted Token + ACL
- Integrate with Trust=L2

## References to Other Tools

This design draws from:

| Tool | Borrowed idea |
|---|---|
| **Codex (OpenAI)** | 3×4 sandbox_mode × approval_policy matrix; Landlock+seccomp+bwrap Linux combo; safe command allowlist; protected subpaths (`.git/`); env var masking; `windows-sandbox-rs` restricted token; separate sandbox crates |
| **Claude Code (Anthropic)** | boundary-based vs permission-based philosophy; bwrap/Seatbelt OS primitives over Docker; unix domain socket + proxy for network; `<system-reminder>` message-layer injection; cache-safe forking for compaction; 6 permission modes. **Note**: the widely-cited `anthropics/claude-code-sandboxing` repo does NOT exist (GitHub 404); the real project is `anthropic-experimental/sandbox-runtime` — TypeScript/Node.js package, Apache-2.0, not Rust, not suitable as lim hard-dependency |
| **Trae** | sandbox.json filesystem readWrite/readOnly with "readOnly wins on conflict"; path priority (longer = more precise); command strategy (allowlist → outside sandbox, else inside + failure retry); enterprise command blocklist text-match |
| **Goose** | Inspector stack (SecurityInspector → EgressInspector → AdversaryInspector → PermissionInspector → RepetitionInspector); AdversaryInspector LLM-as-judge for risky tools; SmartApprove reading `ToolAnnotations.read_only_hint`; 31 env var blocklist; secret keyring default |
| **Codex (system prompt)** | 5-layer input injection (permissions → developer → AGENTS.md → env → user); role priority (system > developer > user > assistant); auto-compact with `encrypted_content` latent understanding |
| **Claude Code (system prompt)** | 10-step SystemPromptBuilder with static/dynamic boundary; `CLAUDE.md` at prompt end for Recency; dynamic info via `<system-reminder>` not system prompt edit; "don't add/remove tools mid-conversation, model state as tools" |
| **Microsoft MXC** | `microsoft/mxc`, Rust 72.5% + MIT, but v0.7.0-alpha, self-declared "not a security boundary yet". 4-tier isolation (process → session → VM → cloud); `seatbelt_common` crate design (TinyScheme profile via `sandbox_init()` not CLI) is the most valuable blueprint for lim's macOS backend |
| **Microsoft LiteBox** | Rust Library OS approach; package OS services into runtime; reduced host interface attack surface; embeddable crate form — architecturally aligned with lim's embeddable crate model |
| **E2B / Firecracker** | microVM for cloud agent execution; independent kernel + memory per sandbox; ~15M sessions/month proves scalability; overkill for lim's local CLI use case |
| **BPF (Tetragon/Tracee)** | Technically the most precise: per-process-tree file-access interception + real-time block via `matchNamespaces` + override return. But Linux-only + requires root/CAP_BPF + conflicts with lim Containment Principle. Codex/Claude Code both chose NOT to use BPF — strong engineering signal. Reserved as optional Linux observability layer (Phase 3+) |
| **Wasm (Wasmtime)** | WASI 1.0 not production-ready (June 2026). Fatal: no `fork`/`exec`, bash tool cannot be Wasm-ized. No mainstream agent uses Wasm as harness sandbox. Reserved for future L2 plugin runtime (12-18 months), borrowing Extism pattern for "user uploads .wasm tool" scenario |

## Open Questions

- [ ] Should `novel-cli` and `lim` share the same trust config, or does each pack have its own? Leaning toward shared (trust is runtime-level, not domain-level).
- [ ] How to handle `git` operations that span workspaces (e.g. `git -C /other/repo`)? Block by default, allow via explicit `writable_roots`?
- [ ] Should `ToolCategory` be a bitflag (so a tool can be `Write | Network`)? Leaning toward single category for simplicity; tools declare their primary category.
- [ ] Does compaction need to preserve `<system-reminder>` tags verbatim, or just the workspace/trust info? Leaning toward preserving the info (tags are transport, not content).
