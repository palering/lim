# lim — Project Conventions

## Containment Principle (Phase 1–2)

All configuration, artifacts, sessions, and runtime outputs MUST stay within the project directory. Nothing escapes to `$HOME`, `/tmp`, or any system path during build, test, or normal execution.

Concretely:

- `target/` — Cargo build output, gitignored.
- `sessions/` — JSONL event logs and workflow artifacts, gitignored.
- `tests/fixtures/` — test input data, tracked in git.
- `.lim-tmp-*` — transient atomic-write temps, created in the target file's parent directory and cleaned up by rename.

The only exception is `cargo test` writing to `std::env::temp_dir()` (e.g. `/tmp/lim-test-*`), which is standard Rust test practice with per-test cleanup.

This constraint will be relaxed when lim ships a real config system (e.g. `~/.config/lim/`) or persistent data store. Until then, if any code path creates files outside the project tree, it is a bug.
