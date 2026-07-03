# lim — Packaging & Distribution Plan

> Status: Draft (2026-06-30, updated 2026-06-30)
> Owner: lim team
> Related: [architecture.md](architecture.md), [../PROGRESS.md](../PROGRESS.md), [architecture/behavior-trace.md](architecture/behavior-trace.md)

## Target Platforms

Only 64-bit platforms. 32-bit and any platform no longer receiving upstream support are out of scope.

| Platform | Triple | Status |
| :--- | :--- | :--- |
| Linux x86-64 | `x86_64-unknown-linux-gnu` | Phase A |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | Phase A |
| Windows x86-64 | `x86_64-pc-windows-msvc` | Phase A |
| Windows ARM64 | `aarch64-pc-windows-msvc` | Phase A |
| macOS ARM64 | `aarch64-apple-darwin` | Phase A |
| macOS x86-64 | `x86_64-apple-darwin` | Phase A (Intel Mac, EOL but low cost to keep) |

> macOS x86-64 is included because it costs one extra matrix row and `cargo build` works natively on Apple Silicon via Rosetta target. Drop it when upstream Rust or GitHub runners retire Intel support.

## Phasing

### Phase A — Baseline Executables (current target)

Goal: produce a working, optimized binary archive for each target triple.

- Release profile tuning in workspace `Cargo.toml` (LTO / strip / codegen-units / panic=abort)
- CI matrix build on tag push (`v*`)
- Archive layout:
  - unix: `tar.gz` containing `lim`, `novel-cli`, `README.md`, `LICENSE`
  - windows: `zip` containing `lim.exe`, `novel-cli.exe`, `README.md`, `LICENSE`
- GitHub Release upload via `softprops/action-gh-release` or equivalent
- No installer, no package manager integration yet

**Blocker code work** (must happen alongside CI, not before):

1. ~~`crates/lim-tools/src/bash.rs` hard-codes `sh -c` — Windows has no `sh`~~ ✅ **DONE (Phase 3c)**: Replaced by `crates/lim-tools/src/shell.rs` with `ShellBackend` trait — `PosixSh` on Unix, `GitBash`/`Wsl`/`PowerShell` on Windows. See [architecture/behavior-trace.md §3.3](architecture/behavior-trace.md).
2. ~~`crates/lim-executor/src/local.rs` process-group kill is unix-only (`libc::setpgid` / `libc::killpg`); Windows path silently degrades~~ ✅ **DONE**: `kill_process_tree()` is now cross-platform — uses `setpgid`/`killpg` on Unix, `TerminateProcess` on Windows.
3. ~~Decide Windows shell strategy: cmd/powershell translation, OR gate the `bash` tool behind `cfg(unix)` and ship Windows without it~~ ✅ **DONE (Phase 3c)**: Strategy is Git Bash > WSL > PowerShell. `cmd.exe` is intentionally never used. See `crates/lim-core/src/env.rs::detect_windows_shell()`.
4. Verify `reqwest` rustls-tls cross-compiles cleanly to all 5 targets (musl/gnu/msvc) — **TODO**: needs CI matrix validation.

### Phase B — Platform Package Managers (after GUI lands)

Goal: native package manager distribution for the GUI-enabled build.

| Platform | Format | Notes |
| :--- | :--- | :--- |
| macOS | Homebrew tap | `brew install lim` — needs a tap repo + formula autobump |
| Windows | winget manifest | `winget install lim` — needs a manifest PR per release |
| Linux (generic) | AppImage | Single-file portable, distro-agnostic |
| Arch | pacman / AUR | `lim-bin` AUR package, autobump on release |
| Debian | apt / `.deb` | via `cargo-deb` |
| NixOS | nixpkgs / flake | flake output for both binary and source builds |

This phase is intentionally deferred until the GUI front-end exists — package manager distribution is most valuable when end users have a UI to point at.

### Phase C — Self-update (long-term)

Goal: in-process update mechanism.

- Embedded update checker (GitHub Releases API)
- Atomic self-replace (write temp → rename)
- Signature verification (minisign or cosign)
- Only relevant after GUI + package manager presence — pure CLI users generally prefer `brew upgrade` etc.

## CI Design

Located in `.github/workflows/release.yml` (to be created).

- Trigger: `push` to `v*` tags, plus `workflow_dispatch` for manual dry-runs
- Matrix: 6 targets (see table above)
- Steps: `rustup toolchain install` → `cargo build --release --target <triple>` → archive → upload to release
- Cross-compilation strategy:
  - macOS both arches: native on `macos-14` runner (Apple Silicon) using `--target`
  - Linux x86-64: native on `ubuntu-latest`
  - Linux ARM64: cross via `cross` toolchain or `ubuntu-24.04-arm` runner if available
  - Windows both arches: native on `windows-latest` (x86-64) + cross to aarch64-msvc

## Open Questions

- [x] Repository URL — ✅ DONE: `Cargo.toml` declares `repository = "https://github.com/palering/lim"` and git remote `origin` is configured.
- [x] LICENSE file — ✅ DONE: `LICENSE` (MIT) added at repo root, copyright "The lim authors". Matches `Cargo.toml` `license = "MIT"`.
- [x] README.md — ✅ DONE: `README.md` exists at repo root with project intro, quick start, architecture overview, crate inventory, and doc links.
- [ ] Is `novel-cli` shipped in the same archive as `lim`, or released separately? `packs/` are domain samples; bundling them in the main release may bloat the archive for users who only want the runtime. **Current decision**: treat novel-cli as an extension, not an independent CLI package — resolve packaging after current development phase.
- [x] Should `lim` binary embed version metadata at build time (`git describe --tags` → `env!`) so `lim --version` reports the release tag? ✅ DONE: `crates/lim-core/build.rs` runs `git describe --tags --always --dirty=-dirty` and sets `LIM_BUILD_VERSION` env. `LIM_VERSION` prefers git description, falls back to `CARGO_PKG_VERSION` for tarball builds. Consumed by clap `#[command(version = lim_core::LIM_VERSION)]` and `SessionHeader.lim_version`.
- [ ] Do we need musl (statically-linked) Linux builds, or is gnu (glibc) sufficient? musl enables truly portable single-binary distribution on Linux but rules out some native deps.
- [ ] Code signing: macOS (notarization) and Windows (authenticode) — out of scope for Phase A, but should be planned for Phase B.

## What's NOT in Scope

- 32-bit builds (x86, armv7)
- Android / iOS
- Linux musl static builds (unless cross-compilation pain forces it)
- WebAssembly target for the runtime itself (Wasm is only a target for *tools*, not for distributing `lim`)
