# lim — Project Conventions

## Containment Principle (Phase 1–2)

All configuration, artifacts, sessions, and runtime outputs SHOULD stay within the project directory during build, test, and normal execution.

Concretely:

- `target/` — Cargo build output, gitignored.
- `sessions/` — JSONL event logs and workflow artifacts, gitignored.
- `tests/fixtures/` — test input data, tracked in git.
- `.lim-tmp-*` — transient atomic-write temps, created in the target file's parent directory and cleaned up by rename.
- `.lim/` — project-local runtime state (loop/daemon/worktrees/config), gitignored.

The only exceptions (relaxed since P4 / lim-config 引入):

- `cargo test` writing to `std::env::temp_dir()` (e.g. `/tmp/lim-test-*`), standard Rust test practice with per-test cleanup.
- `~/.config/lim/` — XDG global config (lim-config, P4 引入)
- `~/.lim/lim.pid` — daemon PID file（lim-serve `--daemon` 模式）
- `~/.lim/logs/` — daemon 日志目录（lim-serve `--daemon` 模式）

项目本地状态（`.lim/loop/`、`.lim/daemon/`、`.lim/worktrees/`、`sessions/`）仍在项目内。系统服务安装文件（systemd unit / launchd plist）写入用户级 XDG 目录，不污染项目树。

## Code Conventions

完整规范分两层，按场景使用：

- **日常开发**遵循 [docs/conventions/basics.md](docs/conventions/basics.md)（13 条基础准则）
- **阶段性 review** 按完整规范 [docs/conventions/full-guide.md](docs/conventions/full-guide.md) 执行（14 节）

入口索引与工具命令见 [docs/conventions/README.md](docs/conventions/README.md)。规范动态迭代，新增条目在该文档末尾维护 changelog。

代码格式化配置见 [rustfmt.toml](rustfmt.toml)。

## Behavior Trace & Observability

行为链路与可观测性是 lim 的核心设计目标：**不成为黑盒**。所有 agent 行为必须有对应的可观测信号（Event 或 Log），session JSONL 必须能完整回放一次 run。

设计原则、行为链路梳理、已知黑盒点、PR review 与迭代检查清单详见 [docs/architecture/behavior-trace.md](docs/architecture/behavior-trace.md)。该文档是 code review 的关键参考：

- **PR review**：对照第 9 节"可观测性清单"自检
- **阶段性 review**：对照第 10 节"迭代检查清单"审计当前实现
- **新增隐式行为**：必须配 `debug!` / `warn!` log，必要时补 `AgentEvent` 变体
