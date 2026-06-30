# MXC as lim-sandbox Backend — Feasibility Study

> Research date: 2026-06-30
> Scope: Whether Microsoft MXC can be used as lim-sandbox backend, integration paths, and adoption timing.
> Related: [../draft/trust-and-approval.md](../draft/trust-and-approval.md), [agent-sandbox-landscape.md](agent-sandbox-landscape.md)

## TL;DR

MXC's architecture is **near-perfect fit** for lim: Rust 72.5%, MIT, cross-platform, `mxc-sdk` library crate (PR #556, 2026-06-25) provides in-process `Sandbox` handle with `kill/wait/timeout/stdio` API that maps 1:1 to lim's `Executor::run_command`. **However, three hard blockers prevent immediate adoption**:

1. README red-banner warning: "no MXC profiles should be treated as security boundaries currently"
2. Schema `0.7.0-alpha` (semver §4 allows arbitrary breakage); macOS seatbelt backend stable only 18 days
3. `mxc-sdk` Rust crate only 5 days old (PR #556), NOT published to crates.io; git-dep doesn't support subdir

**Decision**: Wait for MXC to mature, then adopt as lim-sandbox backend (~300 lines `MxcExecutor`). Until then, lim uses minimal self-built sandbox if/when needed (sandbox priority deferred per project decision).

## MXC Repository Snapshot (2026-06-30)

| Item | Value |
|---|---|
| Repo | `github.com/microsoft/mxc`, public, MIT License |
| Current version | v0.7.0 (2026-06-13), marked **Pre-release** |
| Last commit | 2026-06-27 (4 days ago), 57 commits, **highly active** |
| Rust toolchain | pinned 1.93, workspace `version = "0.7.0"`, `edition = "2021"` |
| Schema | stable `0.7.0-alpha`, dev `0.8.0-dev` — still alpha |
| Pre-built binaries | ✅ v0.7.0 release ships signed production binaries for 3 platforms: `wxc-exec.exe` (Win), `lxc-exec` (Linux), `mxc-exec-mac` (macOS) |
| TypeScript SDK | `@microsoft/mxc-sdk` on npm, Public Preview |
| Rust crate on crates.io | ❌ **Not published**. `crates.io/crates/mxc-sdk` returns "Crate not found" |
| Cargo workspace | 33 sub-crates: `core/wxc`, `core/wxc_common`, `core/lxc`, `core/mxc_darwin`, `core/mxc-sdk` (new), `core/mxc_pty`; backends `appcontainer/windows_sandbox/seatbelt/bubblewrap/lxc/wslc/hyperlight/nanvix/isolation_session` |

## Critical New Discovery: `mxc-sdk` Library Crate

PR #556 (2026-06-25, 5 days ago) added a **Rust library crate** `mxc-sdk` (lib name `mxc_sdk`) — not just a CLI wrapper. Provides in-process sandbox start with streaming stdio, no shell-out needed.

**Public API**:
- `build_request(&SandboxPolicy) → SandboxRequest`
- `spawn_sandbox → Sandbox` handle
- `Sandbox::take_stdin/stdout/stderr`
- `Sandbox::try_wait`, `Sandbox::id`, `Sandbox::kill` (process-tree termination)
- `Sandbox::wait → WaitOutcome::{Exited(i32), TimedOut}`
- `Sandbox::wait_with_output`

**Limitation**: Library mode only supports `ProcessContainer`/`Seatbelt`/`Bubblewrap` one-shot process backends. LXC/VM/microvm returns `UnsupportedContainment`.

## Three-Platform Invocation

### Binary shell-out mode

| Platform | Binary | Invocation | Default backend | Notes |
|---|---|---|---|---|
| Windows | `wxc-exec.exe config.json` or `--config-base64 <b64>` | `processcontainer` (AppContainer/BaseContainer, Win11 24H2=26100+) | `--debug`, `--dry-run`, `--log-file` |
| Linux | `./lxc-exec config.json` or `--config-base64` | `bubblewrap` (needs bwrap in PATH); `lxc` needs root + lxc-utils | `--delete --containername <id>` cleanup |
| macOS | `./mxc-exec-mac --experimental config.json` | `seatbelt` (stable since v0.7.0). **No sandbox-exec CLI**, direct `sandbox_init()` private API | `--experimental` flag in README examples |

### API capabilities

- **stdin/stdout/stderr**: ✅ piped via `tokio::process::Command::piped()` (shell-out) or `take_stdin/stdout/stderr` (library)
- **cancellation**: ✅ Library: `Sandbox::kill()` process-tree termination (Unix SIGKILL process group; Windows job-object). Shell-out: kill MXC binary, child processes exit with it
- **timeout**: ✅ Two ways: `process.timeout` in config (ms, 0=none); library `wait_with_timeout` + `WaitOutcome::TimedOut`
- **exit codes**: Standard i32. `WaitOutcome::Exited(i32)` (library) or MXC binary exit code (shell-out)

## Config Schema (0.7.0-alpha)

```json
{
  "version": "0.7.0-alpha",
  "containerId": "my-container",
  "containment": "process",
  "lifecycle": { "destroyOnExit": true, "preservePolicy": false },
  "process": {
    "commandLine": "python app.py",
    "cwd": "/workspace",
    "env": ["MY_VAR=value"],
    "timeout": 30000
  },
  "filesystem": {
    "readwritePaths": ["/tmp/output"],
    "readonlyPaths":  ["/data"],
    "deniedPaths":    ["/Users/me/.ssh"]
  },
  "network": {
    "defaultPolicy": "block",
    "enforcementMode": "firewall",
    "proxy": { "localhost": 8080 },
    "allowedHosts":  ["api.github.com"],
    "blockedHosts":  ["evil.com"]
  },
  "seatbelt": {
    "profileOverride": null, "guiAccess": false,
    "launchMethod": "exec", "nestedPty": true, "keychainAccess": false
  }
}
```

### Mapping to lim concepts

| lim concept | MXC field | Notes |
|---|---|---|
| workspace_roots (writable) | `filesystem.readwritePaths` | string[] |
| read-only paths | `filesystem.readonlyPaths` | string[] |
| Disable network | `network.defaultPolicy: "block"` | macOS: only reliable isolation (blockedHosts unsupported on macOS) |
| **Command blocklist** | ❌ **MXC doesn't support** | MXC is path/network/UI sandbox, no command allowlist. lim's `DefaultPolicy::dangerous_commands` must check BEFORE calling MXC — this composes well with lim's existing `PermissionPolicy::check_command` |
| Env var filtering | ✅ Automatic. seatbelt backend always `env_clear()`, host env never inherited; `PATH` defaults to `/usr/bin:/bin:/usr/sbin:/sbin`, only `process.env` overrides | Equivalent to lim's desired "allowlist-style" env filtering |

## Integration Paths (3 options)

Since MXC Rust crate is not on crates.io, and `mxc-sdk` depends on `wxc_common.workspace = true` (path dep linking entire workspace), Cargo stable git-dependency doesn't support subdir. Options narrow to:

| Path | Workload | Pros | Cons |
|---|---|---|---|
| **A. PATH detect + shell out binary** | ~300 lines | Zero compile coupling; MXC independently upgradable; matches "call binary to reduce workload" goal | User must install MXC separately, breaks lim `cargo install lim` single-binary experience; version drift; per-call config→base64 serialization |
| **B. Vendor pre-built binaries into lim distribution** | ~300 lines + packaging | Single `cargo install lim` works; signed binaries | Vendor 3 platforms × 2 archs = 6 binaries (~10-30MB each, package bloat); macOS codesign/notarize; license attribution; version pinned |
| **C. git submodule + source build (path dep)** | ~300 lines + build config | Rust-native in-process (use `mxc-sdk` library API, not shell-out); patchable; closest to lim architecture | Pulls entire MXC workspace as build dep; Rust 1.93 toolchain alignment; Linux needs `liblxc-dev` (but bwrap doesn't); macOS needs Xcode CLT; build time↑ |

**Recommended if adopting MXC**: **Path C (library mode)** — lim is Rust-native, library mode avoids per-call process spawn + JSON serialization overhead, and `Sandbox` handle's kill/wait/timeout maps almost 1:1 to lim `Executor::run_command`. **Fallback A** (shell-out) for quick validation or CI. **Avoid B** (vendor binaries) — packaging complexity exceeds benefit.

## Risks & Limitations

### "Not a security boundary" — biggest risk

README top warning (verbatim):
> There are known cases where the current policies generated by the MXC SDK in this repository are **overly permissive** and will be addressed before this is made more generally available. Security researcher partnership while MXC matures is welcome, however **no MXC profiles should be treated as security boundaries currently**.

**Implication for lim**: Cannot use MXC as sole defense against malicious LLM output. Must layer lim's own `PermissionPolicy` + approval layer on top. This composes with lim's existing `policy.rs` note "L0 permission policy — reduces accidental misuse but is NOT a sandbox": MXC provides L1+ OS sandbox, but currently untrusted.

### macOS `sandbox_init` private API stability

`docs/macos-support/seatbelt-backend.md` explicitly states:
- `sandbox_init()` marked deprecated in headers since macOS 10.8, but Apple continues to ship it;自家 apps + Chromium use it
- **Risk level identical to Codex / Claude Code / Trae** (all use `sandbox-exec`/`sandbox_init`). Not MXC-specific; shared debt of macOS CLI sandbox ecosystem
- MXC works on macOS 11+; profile passed to `sandbox_init` via `pre_exec` as string, no temp file (fits lim containment principle)

### Windows Insider Preview requirement

**Clarification**: Only `isolation_session` backend needs Insider Preview 26300.8553. For lim's main stable backend `processcontainer`, only **Win11 24H2 (26100)** is needed — generally released. So lim works on mainstream Windows 11, no Insider needed. VM backends (`windows_sandbox`/`microvm`/`wslc`) are experimental, lim should not depend on them.

### Schema 0.7.0-alpha compatibility commitment

- Still `0.x-alpha`, semver §4 allows arbitrary breakage per release
- Parser only compares major.minor (pre-release label ignored), so 0.7.0 config works on 0.7.x parser, but 0.8.0 may reject
- MXC has clear migration process docs (PR N dual-read → PR N+1 update → PR N+2 remove fallback), showing team values compatibility — but no commitment at this stage
- seatbelt promoted from experimental to stable only in v0.7.0 — **macOS support stable for only 18 days**

### Migration path if MXC goes unmaintained

MXC is essentially a thin wrapper over OS primitives: seatbelt wraps `sandbox_init`, bwrap wraps `bwrap` CLI, ProcessContainer wraps AppContainer/BaseContainer API. **If MXC stops, lim's fallback**:
1. Fork relevant backend crate (MIT licensed, legal), self-maintain; or
2. Shell out to `sandbox-exec`/`bwrap` directly (Codex/Claude Code approach, ~200 lines/platform); or
3. Switch back to lim self-built sandbox.

**Conclusion: adopting MXC doesn't cause deep lock-in** — it wraps the same OS APIs lim would call anyway. Migration cost is controlled.

## Architecture Fit with lim

### Mode match

MXC is **spawn subprocess mode** (whether shell-out binary or `mxc-sdk` library's `spawn_sandbox`), **not library-in-process mode** (sandboxed code still runs as separate process). This exactly matches lim `Executor::run_command` semantics.

### Executor trait mapping (almost 1:1)

```rust
// lim crates/lim-executor/src/executor.rs:110
fn run_command(&self, command: CommandSpec, cancel: CancellationToken) -> ExecutorFuture<Result<CommandOutput>>;
```

| lim `CommandSpec` | MXC config | MXC library API |
|---|---|---|
| `command` + `args` | `process.commandLine` | `SandboxRequest` |
| `cwd` | `process.cwd` | same |
| `env: HashMap` | `process.env: ["K=V"]` | `set_env(Vec<(K,V)>)` |
| `timeout_secs` | `process.timeout` (ms) | `wait_with_timeout` |
| `cancel: CancellationToken` | — | `tokio::select! { sandbox.wait() | cancel.cancelled() }; sandbox.kill()` |
| Returns `CommandOutput{stdout,stderr,exit_code}` | — | `wait_with_output() → Output{outcome, stdout, stderr}` |

`ExecutorCapabilities { is_sandboxed: true }` returns true for `MxcExecutor`. **Zero-friction integration**.

### CancellationToken mapping

lim's existing `local.rs:255` pattern `tokio::select! { child.wait() | cancel.cancelled() }` directly reuses: swap `child.wait()` for `sandbox.wait()`, swap `libc::killpg` for `sandbox.kill()` (MXC does process-tree termination internally, cross-platform consistent, even Windows uses job-object).

## Adoption Conditions (when to integrate MXC)

lim should integrate `MxcExecutor` as `Executor` trait's future impl when **ALL THREE** conditions are met:

1. **Schema leaves alpha**: stable `1.0` (or at least `0.x` non-alpha with documented compat commitment)
2. **`mxc-sdk` Rust crate published to crates.io** (or stable signed binaries with documented release cadence)
3. **README removes "not a security boundary" warning** — meaning Microsoft considers it production-grade for security-sensitive use cases

**Quarterly review checklist** (until adoption):
- [ ] Check MXC version bump and release notes
- [ ] Check if "not a security boundary" warning removed
- [ ] Check if `mxc-sdk` published to crates.io
- [ ] Check schema version (still alpha?)
- [ ] Check macOS seatbelt backend stability claims
- [ ] Test sample `MxcExecutor` prototype against lim's `Executor` trait

## Code Size Estimates

| Path | Lines (rough) |
|---|---|
| `MxcExecutor` (shell-out, post-MXC-1.0 adoption) | ~300-400 (config builder + binary discovery + spawn + stdio + cancel + error mapping) |
| `MxcExecutor` (library mode, `mxc-sdk` crate) | ~200-300 (no base64 serialization/process mgmt, direct `Sandbox` handle) |
| Self-built SandboxedExecutor (macOS seatbelt + Linux bwrap two platforms) | ~650-900 |
| Self-built full platform (incl. Windows restricted token) | ~1700-2600 |

## Sources

- [microsoft/mxc GitHub repo](https://github.com/microsoft/mxc)
- [MXC v0.7.0 release with signed binaries](https://github.com/microsoft/mxc/releases/tag/v0.7.0)
- [MXC PR #556: mxc-sdk library crate](https://github.com/microsoft/mxc/pull/556)
- [MXC docs/schema.md](https://github.com/microsoft/mxc/blob/main/docs/schema.md)
- [MXC docs/macos-support/seatbelt-backend.md](https://github.com/microsoft/mxc/blob/main/docs/macos-support/seatbelt-backend.md)
- [crates.io: mxc-sdk not found](https://crates.io/crates/mxc-sdk)
- [npm: @microsoft/mxc-sdk](https://www.npmjs.com/package/@microsoft/mxc-sdk)
