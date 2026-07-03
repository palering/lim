# Contributing to lim

感谢参与 lim 开发！本文档是贡献流程的入口。

## 快速上手

### 环境要求

- Rust stable（≥ MSRV 1.95，见 [Cargo.toml](Cargo.toml)）
- macOS / Linux / Windows
- 可选：`tokei`（代码行统计）、`cargo-nextest`

### 常用命令

```bash
# 质量门
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 构建
cargo build --workspace
cargo build --release --bins

# 运行
cargo run -p lim -- run --message "..."
cargo run -p lim -- tui
```

## 开发流程

lim 采用**轻量 PR 流程**（[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) 详述）：

1. 本地建分支：`feat/xxx` / `fix/xxx` / `chore/xxx`
2. 开发 + 本地自检（fmt / clippy / test）
3. push 并开 PR 到 `main`
4. 等 CI 通过
5. Squash merge

> **当前阶段**：private 仓库，未设正式分支保护，靠纪律约束。v0.1 release 后将启用正式分支保护。

## 规范索引

| 规范 | 文档 | 适用场景 |
|---|---|---|
| 代码基础准则（13 条）| [docs/conventions/basics.md](docs/conventions/basics.md) | 每次 commit 前自检 |
| 代码完整规范（14 节）| [docs/conventions/full-guide.md](docs/conventions/full-guide.md) | 阶段性 review、新模块设计 |
| Commit message 规范 | [docs/conventions/commits.md](docs/conventions/commits.md) | 每次 commit |
| 架构分层与依赖图 | [docs/architecture.md](docs/architecture.md) | 模块边界决策 |
| 行为链路与可观测性 | [docs/architecture/behavior-trace.md](docs/architecture/behavior-trace.md) | PR review 自检 |
| 开发流程指南（含 AI 使用）| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | 标准 workflow |
| 格式化配置 | [rustfmt.toml](rustfmt.toml) | `cargo fmt` 自动应用 |

## Commit 规范摘要

遵循 [Conventional Commits](https://www.conventionalcommits.org/)，**subject 必须英文**，body 至少包含一份英文版本。详见 [docs/conventions/commits.md](docs/conventions/commits.md)。

```
<type>(<scope>): <english subject>

<body，可中文 + 英文双语>
```

## CI

GitHub Actions 配置在 [.github/workflows/](.github/workflows/)：

- **ci.yml**：push to main + PR 触发，跑 fmt / clippy / build / test
- **release.yml**：tag `v*` 触发，跨平台构建 + 发布

Dependabot 配置在 [.github/dependabot.yml](.github/dependabot.yml)，每周一扫描 `github-actions` 和 `cargo` 依赖。
