# Agent Sandbox Landscape — 2025–2026 Research

> Research date: 2026-06-30
> Scope: How mainstream AI coding agents implement execution sandboxing and isolation.
> Related: [../draft/trust-and-approval.md](../draft/trust-and-approval.md), [../architecture.md](../architecture.md)

## TL;DR

A paradigm shift occurred in 2025–2026: from **Docker containers** → **lightweight OS primitives** (namespace / Landlock / Seatbelt). Anthropic's official data: sandboxing reduced permission prompts by **84%**. bubblewrap starts in < 50ms vs Docker's 2–10s — for coding agents that frequently spawn sandboxed bash sessions, Docker's latency is unacceptable.

## Sandbox Mechanism Matrix

| Tool | Linux | macOS | Windows | Cloud / cross-platform |
|---|---|---|---|---|
| **Codex (OpenAI)** | Landlock + seccomp + bwrap | Seatbelt (`sandbox-exec`) | Restricted Token + ACL (`windows-sandbox-rs`) | Docker recommended |
| **Claude Code (Anthropic)** | bubblewrap (bwrap) | Seatbelt (`sandbox-exec`) | Weak / not public | Custom proxy for cloud |
| **Trae** | bwrap + User Namespace | `sandbox-exec` | Self-built sandbox SDK | TRAE Work cloud env |
| **Qoder (Alibaba)** | Cloud isolated sandbox | Cloud isolated sandbox | Cloud isolated sandbox | Patch-as-output + temp branch |
| **Cursor** | None (approval only) | None | None | Cloud Sandbox Agent |
| **Aider** | None | None | None | Docker devcontainer |
| **Goose (Block/AAIF)** | None (Inspector stack) | None | None | Docker `set_container()` |
| **OpenHands** | Docker container | Docker container | Docker container | Runtime image |
| **Devin (Cognition)** | Task-level cloud VM | n/a | n/a | Closed-source cloud VM |
| **Roo Code** | None (approval + regex) | None | None | None |

## Key Tool Deep Dive

### Codex (OpenAI) — Most complete local OS-level sandbox

**Code location**: `codex-rs/linux-sandbox/`, `codex-rs/sandboxing/`, `codex-rs/windows-sandbox-rs/`, `codex-rs/bwrap/`, `codex-rs/execpolicy/`, `codex-rs/network-proxy/`.

**Linux three-layer defense** (default on, no root required):
- **Landlock** (Linux 5.13+): unprivileged process declares accessible paths via ABI-versioned rulesets
- **seccomp**: syscall filtering — allows `read/write/open/stat/mmap`, rejects `mount/umount/reboot/init_module`; network access disabled via `install_network_seccomp_filter_on_current_thread()`
- **bubblewrap (bwrap)**: immutable filesystem + independent PID namespace

**macOS**: `sandbox-exec` with profile — `/usr/lib`, `/System`, `/Applications` read-only; cwd read-write; `~/.ssh`, `~/.aws`, `/etc`, `/var` denied; outbound network disabled by default.

**Windows** (`windows-sandbox-rs`, ~30 Rust files): elevated sandbox creates two local users (`CodexSandboxOffline` / `CodexSandboxOnline`) with random DPAPI-encrypted passwords, ACL-protected system dirs, restricted-token fork for child processes.

**Permission matrix** (3 sandbox modes × 4 approval policies):

| sandbox_mode | read | write | network |
|---|---|---|---|
| `read-only` | full FS | denied | denied |
| `workspace-write` (default) | full FS | cwd + `writable_roots` + tmp only | default off, configurable |
| `danger-full-access` | full | full | full |

| approval_policy | behavior |
|---|---|
| `untrusted` | approve all except known-safe read-only commands |
| `on-request` (default) | silent within sandbox boundary, prompt on boundary touch |
| `on-failure` | no pre-check, prompt only after failure |
| `never` | never prompt (CI/CD) |

**Safe command allowlist** (`is_known_safe_command`): `cat/cd/echo/grep/ls/pwd/wc`, `git status/log/diff/show/branch`, restricted `find` (no `-exec/-delete`), restricted `ripgrep` (no `--pre/--hostname-bin`).

**Protected subpaths** (even within workspace): `.git/`, `.hg/`, `.svn/`, `.codex/`, `.agents/`.

**Env var policy**: `inherit = all|core|none`; auto-mask variables whose names contain `KEY/SECRET/TOKEN`.

### Claude Code (Anthropic) — Lightweight OS primitives + network proxy

**Core philosophy**: shift from "permission-based" (per-step approval) → "boundary-based" (autonomy within boundary). Official data: sandboxing reduced permission prompts by **84%**.

**Filesystem isolation**:
- Linux: **bubblewrap** (bwrap) — process-level namespace, startup < 50ms, near-zero resource overhead
- macOS: **Seatbelt** (`sandbox-exec`) — kernel-level namespace isolation
- bash tool can only read/write cwd; boundary enforced by OS, applies to all child processes (even `gcc`/`npm build`)

**Network isolation**: agent can only reach a proxy server via **unix domain socket**; all outbound traffic goes through the proxy; proxy enforces domain allowlist, prompts for unauthorized requests. Both boundaries must coexist — missing network isolation leaks SSH keys; missing FS isolation enables escape.

**Cloud version** (Claude Code on the Web): git credentials and signing keys never enter sandbox; custom proxy validates scoped credential + checks push targets configured branch (zero-trust).

**Open source**: released as research preview at `anthropics/claude-code-sandboxing` (npm package `@anthropic-ai/sandbox-runtime`).

**Default**: opt-in (`/sandbox` command / `/permissions` config); three modes `default/auto-accept/sandbox`, `shift+tab` to switch.

### Trae — Cross-platform Chinese solution

| Platform | Implementation |
|---|---|
| macOS | `sandbox-exec`, auto-configured |
| Windows (native) | Self-built sandbox SDK |
| Windows (WSL2) / Linux | Remote SSH + bwrap + User Namespace |

**Linux special handling**: checks User Namespace; if disabled, temporarily `sysctl -w kernel.unprivileged_userns_clone=1` + `kernel.apparmor_restrict_unprivileged_userns=0`; persists to `/etc/sysctl.d/trae-sandbox.conf`.

**Permission model** (key design: read-write inherits user permissions; **when read-write and read-only conflict, read-only wins**):
- Read-only: `.vscode`, `/` (default: not explicitly writable = read-only)
- Read-write: project dir (except `.trae`, `.vscode`, `.git`), tmp, cache, `~/.local/{lib,bin,share}`, toolchain dirs

**`sandbox.json` custom config** (`~/.trae-cn/sandbox.json`):
```json
{
  "filesystem": { "readWrite": [], "readOnly": [] },
  "network": { "default": "allow", "allow": [], "deny": [] }
}
```
Path priority: longer = more precise; readWrite + readOnly conflict → readOnly wins. Network match supports `[ip/mask]:port`, `domain:port`, `*.domain` (Windows only).

**Command strategy**: allowlist → outside sandbox; not in allowlist → inside sandbox, on failure ask to retry outside; high-risk (`rm -rf`) → intercept + 3 options (skip / allowlist / sandbox).

**CLI permission modes**: `default` / `plan` / `bypass_permissions`. Tool priority: `allowed_tools` > `disallowed_tools` > `ask_tools` (disallowed highest). Default allow: `cat/find/grep/head/ls/rg/tail`, `awk/cut/diff/sort/uniq/wc`, `git diff/log/show/status`, `cd/date/echo/env/pwd/which`. Default prompt: `rm -rf /`, `rm -rf ~`, `rm -rf $HOME`, `rm -rf /Users`, `go clean -modcache`.

### Goose (Block → AAIF) — Microkernel + Inspector stack + LLM adversarial review

**No OS-level sandbox**, but unique defense-in-depth:

**Inspector stack** (every tool call passes through):
```
SecurityInspector → EgressInspector → AdversaryInspector
                → PermissionInspector → RepetitionInspector
```

**AdversaryInspector**: if `~/.config/goose/adversary.md` exists, makes an **independent LLM call** to review `shell`/`automation_script` tools (ALLOW/BLOCK). Fail-open policy (review call fails → allow, debatable).

**4 GooseModes**: `Auto` (default auto-approve) / `Approve` (ask every time) / `SmartApprove` (prompt only sensitive; reads MCP `ToolAnnotations.read_only_hint`, uses `PermissionJudge` LLM classifier when unsure) / `Chat` (no tools).

**Extension isolation**: malware check before load; 31 env var blocklist (`PATH/LD_LIBRARY_PATH/PYTHONPATH/NODE_OPTIONS`); stdio extensions can `set_container()` for Docker; secrets in system keyring by default (macOS Keychain / Windows Credential Manager / Linux Secret Service).

## 2024–2026 New Trends

### 1. Docker → lightweight OS primitives

The most significant paradigm shift. Anthropic chose bubblewrap/Seatbelt over Docker for explicit engineering reasons:

| Approach | Startup | Resource | Granularity |
|---|---|---|---|
| bubblewrap/Seatbelt | < 50ms | near-zero | process-level |
| Docker container | 2–10s | hundreds of MB | system-level |

Peking University's containerless approach: Chroot + Mount Namespaces + Terminal Isolation, trains SWE Agent without Docker.

### 2. Microsoft MXC (Build 2026) — OS-level Agent sandbox

**MXC = Microsoft Execution Container**, open-sourced at Build 2026. Kernel-level enforcement, not dependent on Agent alignment.

**Four isolation tiers**:

| Level | Implementation | Use case |
|---|---|---|
| Process-level | process boundary isolation, resource access limit | low-risk operations |
| Session-level | user session isolation | medium permissions |
| VM-level | independent VM, includes WSL | high-risk operations |
| Windows 365 cloud | fully cloud, zero local data | highest security |

Demo: OpenClaw Agent tried to batch-delete 94 desktop images; MXC read-only sandbox intercepted at OS kernel layer. **Agent execution permission enforced by OS kernel, not Agent self-alignment.**

Protocol and interface definitions open-sourced on GitHub. This is Microsoft's answer to "how to give Agent system-level execution capability safely."

### 3. Microsoft LiteBox — Rust-based Library OS

Announced by James Morris (Microsoft Linux Emerging Tech team). **Rust-written "Library OS"** for sandboxing.

**Core idea**: package OS services into the application/runtime itself, rather than exposing apps to the full host kernel interface, reducing attack surface. Focuses on easy interop between "north" shims and "south" platforms. Supports both kernel and non-kernel scenarios.

Positioning: a library OS approach to sandboxing, distinct from namespace/Seatbelt (OS primitive) and container (full isolation). Relevant to lim because it's Rust-native and library-form (embeddable).

### 4. macOS sandbox-exec deprecation risk

- Apple man page explicitly marks **DEPRECATED**
- Still functional in current macOS; Codex/Claude Code/Trae all use it (CLI tools don't go through App Store, don't require App Sandbox + entitlements)
- **Risk**: Apple may remove in future; signed GUI apps must use App Sandbox (`com.apple.security.app-sandbox` entitlement) + Endpoint Security framework (requires `com.apple.developer.endpoint-security.client` entitlement, strict review, mainly for security vendors)
- CLI tools can rely on `sandbox-exec` short-term; long-term need to watch for removal or migrate to cloud microVM

### 5. Landlock adoption

- Linux 5.13+, unprivileged process creates FS access rules
- **Codex is the only coding agent publicly using Landlock as core** (standalone `linux-sandbox` crate, `landlock.rs`)
- Trae Linux uses bwrap + User Namespace (not Landlock directly)
- Degrades to warn when Landlock unavailable

### 6. microVM rise (Firecracker lineage)

Cloud agent execution's "new infrastructure":
- **E2B**: AWS Firecracker microVM, independent Linux kernel + memory per sandbox, ~15M sandbox sessions/month
- **Microsandbox** (Zerocore AI, open source): VM-level isolation + <200ms startup + Rust/Python/TS/Go SDK
- **Daytona**: $24M Series A, >2M isolated workloads/month
- **Tencent Cube Sandbox**: April 2026 open source, "industry's first hardware-level isolation + sub-100ms startup"
- **gVisor**: Google user-space kernel, EKS has `agent-sandbox` deployment (Kata Containers similar)

### 7. Wasm / WASI

- WASI 0.3.0 targeted Feb 2026, "component model standardization final stage means WebAssembly will gradually replace containers"
- But "industry didn't choose familiar Docker, turned to lower-level tech" — here "lower-level" means namespace/Landlock, **not Wasm**
- Wasm sandbox currently used for **code execution environments** (Code Interpreter, untrusted code fragments), not entire coding agent
- 2026 WASI 1.0 maturity may change this; not yet mainstream for agent harness

### 8. Harness Engineering paradigm (2026 keyword)

"2025 was the year of Agent, 2026 is the year of Harness." Core shifts:
- permission-based (per-step approval) → boundary-based (autonomy within boundary)
- Container-level → OS Primitive-level
- Security and autonomy no longer zero-sum — better sandbox releases more autonomy (Claude Code: sandbox reduced prompts 84%)

## Engineering Recommendations for lim

1. **Local CLI agent**: prefer **Landlock + seccomp + bwrap** (Linux) + **Seatbelt** (macOS), referencing Codex's `linux-sandbox` crate. Avoid Docker startup overhead.
2. **macOS compliance**: `sandbox-exec` usable short-term (CLI tools don't require App Sandbox); long-term watch Apple removal plans; consider Endpoint Security (requires entitlement review) or migrate to cloud microVM.
3. **Cloud agent execution**: Firecracker microVM (E2B pattern) + gVisor/Kata for enterprise isolation; Docker bind mount + read-only for lightweight scenarios.
4. **Permission model**: reference Codex 3×4 matrix (sandbox_mode × approval_policy); distinguish read/write/exec/network; workspace_roots via `writable_roots` allowlist extension.
5. **Network isolation**: Claude Code's unix domain socket + proxy + domain allowlist is more flexible than pure seccomp socket blocking (useful for partial network like `pip install`).
6. **Don't rely on approval alone**: community consensus "over-reliance on human-in-the-loop worsens security" (approval fatigue); use **approval + OS sandbox + failure-escalated retry** triple.
7. **Wasm sandbox**: post-2026 WASI 1.0 maturity may suit untrusted code fragment execution; entire agent harness migration to Wasm not yet timed.
8. **Microsoft MXC/LiteBox**: monitor for cross-platform inspiration — LiteBox's Rust library-OS approach is architecturally aligned with lim's embeddable crate model.

## Sources

- [Claude Code Sandboxing: OS-level isolation for security and autonomy balance](http://m.toutiao.com/group/7642873781194654223/) — Anthropic 84% data, bwrap/Seatbelt selection logic
- [Dissecting OpenAI Codex: engineering design of a local coding agent](http://m.toutiao.com/group/7649651848315585065/) — orchestrator pipeline, three-platform sandbox crates
- [Codex CLI security mechanism: sandbox policy and permission control](https://blog.csdn.net/gitblog_00993/article/details/150624532) — 3×4 matrix, Landlock code
- [Codex sandbox deep dive: how OS-level code isolation works](https://blog.csdn.net/weixin_43571227/article/details/162327890) — three-layer defense, seatbelt config
- [Codex CLI five defense lines breakdown](https://blog.csdn.net/weixin_63132747/article/details/161523786) — Windows elevated sandbox
- [TRAE official docs - sandbox](https://docs.trae.cn/ide_sandbox) — macOS/Windows/Linux implementation, sandbox.json
- [TRAE official docs - command blocklist](https://docs.trae.cn/enterprise_command-blocklist) — enterprise text-match blocklist
- [TRAE official docs - tool permissions](https://docs.trae.cn/cli_tool-permission) — CLI permission tiers
- [Goose source architecture: AI Agent as microkernel](http://m.toutiao.com/group/7652944016144695817/) — Inspector stack, AdversaryInspector
- [Cursor Automations: resident agent execution engine](http://m.toutiao.com/group/7636570883095659023/) — Cloud Sandbox Agent
- [Sandbox revolution: Firecracker microVM for AI Agent](http://m.toutiao.com/group/7630049623419159055/) — E2B/microVM trend
- [WASI 1.0: WebAssembly everywhere in 2026](http://m.toutiao.com/group/7599222854286082612/) — WASI 0.3.0 timeline
- [Tencent Cube Sandbox open source](http://m.toutiao.com/group/7631199032001577508/) — 2026 microVM
- [When sandbox-exec is no longer recommended](https://cloud.tencent.com/developer/ask/sof/115922704/answer/137846860) — macOS DEPRECATED confirmation
- [Microsoft Build 2026: MXC sandbox technical analysis](https://blog.csdn.net/Leinwin/article/details/161655813) — MXC four tiers
- [Microsoft open-sources LiteBox: Rust-based sandbox Library OS](http://m.toutiao.com/group/7619547344479994403/) — LiteBox design
- [Phoronix: Microsoft's LiteBox as Rust-based Sandboxing Library OS](http://www.linuxeden.com/a/157224) — James Morris announcement
