# Development Guide

> 面向 AI 和人类开发者的标准开发流程指南。AI agent 读取本文档后应能独立完成 lim 的标准开发任务。

## 项目定位

**lim** 是一个实验性的 AI agent 框架，聚焦 Loop Engineering 与行为可观测性。当前处于早期开发阶段（P4 已完成），API 不稳定。

- **类型**：应用端项目（非库），无下游兼容压力
- **MSRV**：1.95（紧贴 stable，学 bevy 做法）
- **仓库**：private，单人开发，轻量 PR 流程
- **CI**：GitHub Actions，ubuntu-latest，跑 fmt/clippy/build/test

## 架构概览

四层分层，参考 k8s 扩展模型：

| 层 | crate | 职责 |
|---|---|---|
| **Primitives** | `lim-core` | 数据形状、中性 trait，零运行时依赖 |
| **Core** | `lim-llm`、`lim-agent`、`lim-session` | LLM 调用、agent loop、session 持久化 |
| **Harness** | `lim-executor`、`lim-tools`、`lim-skills`、`lim-workflow` | 执行器、内置工具、技能、工作流 |
| **Product** | `apps/cli`、`apps/tui`、`packs/novel` | CLI/TUI 前端、领域应用 |

详细架构见 [architecture.md](architecture.md) 和 [architecture/layers.md](architecture/layers.md)。

## 标准开发流程

### 1. 接到任务后

1. **理解需求**：明确要改什么、为什么改、影响范围
2. **查 memory**：如果是项目相关任务，先查 `/Users/pale/.trae-cn/memory/projects/-Users-pale-my-dev-agent-lim/` 下的 project_memory.md 和最近 topics
3. **读相关代码**：动手前先读现有代码，理解上下文
4. **读规范**：对照 [conventions/basics.md](conventions/basics.md) 的 13 条自检

### 2. 开发

1. **建分支**：`feat/xxx` / `fix/xxx` / `chore/xxx`（从最新 main 切出）
2. **写代码**：遵循 [conventions/basics.md](conventions/basics.md) 和 [conventions/full-guide.md](conventions/full-guide.md)
3. **本地自检**：

   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

4. **提交**：遵循 [conventions/commits.md](conventions/commits.md)，subject 英文，body 至少一份英文

### 3. PR 流程

1. `git push -u origin <branch>`
2. `gh pr create` 开 PR
3. 等 CI 通过（约 1-3 分钟）
4. **合并前检查**：
   - [ ] CI 通过
   - [ ] commit message 符合规范（英文 subject + 至少一份英文 body）
   - [ ] 涉及行为变更的，对照 [behavior-trace.md](architecture/behavior-trace.md) 第 9 节自检可观测性
5. `gh pr merge <N> --squash --delete-branch`
6. `git checkout main && git pull`

### 4. 重要约束（不可违反）

以下约束来自 [AGENTS.md](../AGENTS.md) 和 project memory，违反会导致架构污染：

#### Containment 原则

所有配置、artifacts、sessions、runtime outputs 必须留在项目目录内。**禁止**写入 `$HOME`、`/tmp` 或系统路径。

例外：`cargo test` 写 `std::env::temp_dir()` 是标准 Rust 实践，允许。

#### 行为可观测性

**不成为黑盒**。所有 agent 行为必须有对应的可观测信号：

- 隐式行为必须配 `debug!` / `warn!` log
- 必要时补 `AgentEvent` 变体
- session JSONL 必须能完整回放一次 run

详见 [architecture/behavior-trace.md](architecture/behavior-trace.md)。

#### 关键技术约束

- `ToolCallId` 必须是 String newtype（支持 Anthropic 的 `toolu_...` 格式）
- `ContextEdited` 事件在 session resume 时必须用 `replay_edits()` 恢复
- OpenAI provider 的 tool call streaming 必须为 start 和 delta 事件生成一致的 `ToolCallId`
- Provider endpoint 解析优先级：`full_url` > `base_url + protocol suffix` > `type default + suffix`
- 所有 LLM provider 的 `base_url` 必须是纯 host（不含 `/v1`），`/v1` 放在 protocol suffix 里
- `SessionWriter` 必须用 `truncate(true)` 而非 `append(true)`
- SSE error 事件必须通过 `tx.send(Err(...))` 传播，不能静默丢弃
- 默认 trust level 是 `Workspace`；`Permissive` 需显式 `--trust` flag
- `Permissive` trust level 不能与 `Auto` approval mode 组合，强制 `Always`

#### 分层原则

- **Policy in product layer, mechanism in core layer**：核心层只提供 hooks/channels/traits，产品层决定"做什么"
- **Extension over built-in**：新功能优先通过 hooks/extension points 实现，而非加入 core
- **Crate 边界对齐职责边界**：每个 crate 单一职责
- **Primitive 层零运行时依赖**：`lim-core` 只含数据形状和中性 trait

### 5. 常见陷阱

#### 增量编译缓存导致 clippy 漏检

本地用 nightly + 增量编译时，clippy 可能不重新检查未修改的文件，导致漏检。CI 全新环境会全量编译。

**解决**：重要检查前先 `cargo clean -p <crate>` 再跑 clippy。

#### ring crate 跨平台编译限制

`ring`（reqwest 依赖）含 C 代码，无法在 Linux 上交叉编译到 macOS/Windows。release.yml 在对应平台 runner 上构建，不要在 CI 里加 cross-compile job。

#### dependabot PR 处理

- minor/patch 升级：dependabot 会自动合并到单个 PR（[dependabot.yml](../.github/dependabot.yml) 的 groups 配置）
- major 升级：每个依赖单独 PR，需逐个审查 changelog + 适配代码
- 合并前必须本地 checkout PR 验证：`gh pr checkout <N>` + `cargo clippy` + `cargo test`

#### rust-toolchain.toml 不要 pin 版本

不要创建 `rust-toolchain.toml` pin 特定版本——会导致 rustup 自动安装额外 toolchain。用 `Cargo.toml` 的 `rust-version` 字段声明 MSRV 即可。

### 6. AI 协作流程

当 AI agent 协助开发时：

1. **动手前先问**：涉及 commit/push/PR 等不可逆操作前，必须先列清楚改动内容等用户确认
2. **不要用"可能"**：版本相关问题要查清事实（用 `gh release list`、`rustc --version` 等命令），不要用"可能"等不确定措辞
3. **查最新信息**：写 CI/CD 配置前先用 `gh release list --repo <owner>/<repo>` 查最新版本，不要用过期版本
4. **遵循 containment**：所有临时文件写在项目目录内，不写 `$HOME` 或 `/tmp`
5. **行为可观测**：新增隐式行为必须配 log 或 event

## 文档导航

| 文档 | 用途 |
|---|---|
| [AGENTS.md](../AGENTS.md) | 项目核心原则（containment、可观测性）|
| [CONTRIBUTING.md](../CONTRIBUTING.md) | 贡献流程入口 |
| [conventions/basics.md](conventions/basics.md) | 代码基础准则（13 条）|
| [conventions/full-guide.md](conventions/full-guide.md) | 代码完整规范（14 节）|
| [conventions/commits.md](conventions/commits.md) | Commit message 规范 |
| [architecture.md](architecture.md) | 架构分层与依赖图 |
| [architecture/behavior-trace.md](architecture/behavior-trace.md) | 行为链路与可观测性 |
| [architecture/evolution.md](architecture/evolution.md) | 架构演进路线（P0-P4 状态）|
| [PROGRESS.md](../PROGRESS.md) | 实现进度 |
| [INDEX.md](INDEX.md) | 文档总索引 |

## Changelog

| 日期 | 变更 | 备注 |
|---|---|---|
| 2026-07-03 | 初始版本 v1.0 | 建立面向 AI 的开发流程指南 |

---

## 待办：v0.1 release 前的历史压缩

**触发时机**：项目达到相对满意的完成度，准备打 v0.1.0 tag 前。

**背景**：当前历史 commit 存在混用中英文、P0-P4 / Phase X 内部标签、dependabot 噪音等问题。新规范（[commits.md](conventions/commits.md)）只约束未来 commit，历史问题通过压缩解决。

**执行步骤**：

1. 先打 `v0.0.1-pre-squash` tag 保留旧历史可追溯
2. 按逻辑阶段 squash 成 5-6 个 commit（每个代表一个有意义的阶段边界）：
   - `feat: Phase 1 — minimal agent runtime (MVP)`
   - `feat: Phase 2 — workflow engine and novel-pack`
   - `feat: Phase 3 — LLM provider stack, CLI/TUI, bug fixes`
   - `feat: Phase 4 — Loop Engineering, trust & approval, sandbox design`
   - `chore: CI/CD, dependabot, conventions`（基础设施）
3. 同步更新文档里的 commit hash 引用（PROGRESS.md、docs/archive/、docs/review/）：
   - 改成阶段描述（如 "Phase 1 MVP"）或新 hash
4. force push（确认无他人 clone）
5. 打 `v0.1.0` tag 作为新起点

**压缩后的 commit message 规范**：英文 subject + 双语 body，body 引用 docs/archive/completed-phases.md 作为详细参考。

**风险控制**：
- 单人开发，force push 风险可控
- GitHub 已 merge PR 的记录在 UI 上保留
- pre-squash tag 保证旧历史可追溯
