# lim 演进路线图

本文档定义 lim 的五阶段演进路径。参考 k8s 的纵深演进（docker → compose → k8s → k3s），每个阶段都有完整的产品形态，不强制升级。

## 一、五阶段总览

| 阶段 | 名称 | 技术分水岭 | k8s 类比 | lim 状态 |
|---|---|---|---|---|
| **0** | Chat | LLM API client + session | curl | 未实现（最简形态参照） |
| **1** | Single Agent | +exec +approval（环境隔离+授权） | docker run | **当前** |
| **2** | Multi/Sub Agent | +scheduler +A2A 通信 | docker-compose | 未实现 |
| **3** | Loop Engineering | +长程任务 +controller +状态账本 | CRD + controller | 未实现（P0 起步） |
| **4** | Daemon/Server | +常驻进程 +独立存储 +网络拓扑 | k3s / k8s | 未实现 |

**关键洞察**：阶段不是严格串行的。Loop Engineering 在阶段 1 就有最小形态（CLI loop），在阶段 3 才完整。每个阶段都能独立交付产品。

## 二、各阶段详解

### 阶段 0：Chat

**最简形态**：CLI 调 LLM API，维护对话 session。

```
$ lim chat "你好"
> 你好！有什么可以帮你的？
```

- 进程：用户启动 → 对话 → 用户退出
- 状态：session JSONL 记录对话
- 无 exec，无工具，无审批
- **价值**：验证 Provider + Session 最小路径

**激活的扩展点**：Provider, SessionWriter

### 阶段 1：Single Agent（当前）

**技术分水岭**：引入 exec + approval。环境隔离和授权是 agent 执行的标志性分界。

```
$ lim "fix the bug in foo.rs"
> [approval] allow write to foo.rs? (y/n)
> [edit] foo.rs
> Done.
```

- 进程：用户启动 → 对话/任务 → 用户退出
- 状态：session JSONL + user_edits + compaction
- 权限：TrustLevel（L0 硬边界）+ ApprovalMode（L1 软审批）
- **当前已完成**：AgentLoop + Tool + Executor + Approval + TUI + Compaction + Session 管理

**激活的扩展点**：Provider, Tool, Executor, AgentHooks（before/after/edit_context）, CompactionStrategy

### 阶段 2：Multi/Sub Agent

**技术分水岭**：多个 AgentLoop 实例协作，需要调度和通信。

```
$ lim "refactor the auth module"
> [spawn researcher] exploring auth module...
> [spawn implementer] applying changes...
> [spawn reviewer] verifying...
> Done.
```

- 沙箱隔离粒度可选：session 级 / agent 级 / user 级
- A2A 通信：从消息拷贝 → channel → MCP 封装 → 完整 A2A 协议
- Scheduler 雏形：简单的任务分配

**待做**：
- `AgentTransport` trait（默认 LocalChannelTransport）
- Scheduler 雏形
- 沙箱隔离粒度可选
- TUI 多任务视图

**激活的扩展点**：AgentTransport, Scheduler, SandboxGranularity

**关键**：AgentLoop 作为节点执行器被 Scheduler 调用。核心循环不改。

### 阶段 3：Loop Engineering（完整）

**技术分水岭**：跨 context window 的长程任务，goal 驱动的自迭代。

```
$ lim loop --goal "fix all clippy warnings" --verifier "cargo clippy"
[loop-001] session 1 starting...
[loop-001] session 1 ended (context 80%)
[loop-001] session 2 starting (state restored)
[loop-001] verifier: PASS
[loop-001] goal achieved
```

- 状态账本：`.lim/loop/progress.md` + `features.json` + `state.json`
- 验证机制：CommandVerifier（确定性优先）+ LlmVerifier（兜底）
- 无进展检测：连续 N 次相同失败 → 升级给人
- Controller 模式（CRD 类比）：固化的任务模板

**待做**：
- ControllerRegistry（CRD 类比）
- Verifier 增强
- AI-native 元工具（AI 控制 loop 本身）

**激活的扩展点**：Verifier 多实现, Controller trait

**关键**：lim 自身功能作为工具暴露给 AI。AI 可以在对话中创建子任务、查询状态、暂停/恢复。

### 阶段 4：Daemon/Server

**技术分水岭**：常驻进程，多任务托管，独立存储。

```
$ lim loop daemon start
$ lim loop submit --goal "..."
task-001 submitted
$ lim loop status
task-001: running (session 3)
$ lim loop attach task-001
```

- daemon 常驻 + 每个任务一个 worker
- 独立存储 + 可选同步到 workspace
- 网络拓扑：默认 P2P 同级组网，预留中心化切换
- 多 workspace 管理

**待做**：
- daemon 进程 + IPC
- DaemonLedger（独立存储 + workspace 同步）
- P2P 网络拓扑
- 中心化调度（预留切换）

**激活的扩展点**：NetworkTopology, StorageBackend

**关键**：LoopRunner 零改动，只换 Ledger 和 Transport。

## 三、Loop Engineering 的三个成熟度

Loop Engineering 不是阶段 3 才开始的，它跨越多个阶段：

| 成熟度 | 阶段 | 形态 | P 级 |
|---|---|---|---|
| **A** | 阶段 1 | CLI loop（前台，`.lim/loop/` 状态，Ctrl+C 中断，`--resume` 恢复） | P0 |
| **B** | 阶段 2-3 | TUI 挂载任务（后台 task，TUI 前台继续对话，`/loop status`） | P1 |
| **C** | 阶段 4 | Daemon/Server 托管（常驻进程，多任务） | P2+ |

**核心结论**：LoopRunner 库的设计要满足三个成熟度都能用，不预留 daemon 专用接口，但保证 `Send + Sync + 无全局状态 + 状态全在 StateLedger`。

## 四、实现优先级

> 2026-07-02 更新：基于 Claude Code 4 层 Loop 模型调研（详见 loop-engineering.md §十二），补充 P3.5/P4/P4+ 规划，标注与 CC 的对照。

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
P0（阶段 1 完成 + Loop 成熟度 A）✅ 已完成
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
- AgentHooks 加 should_stop + max_steps
- StateLedger（WorkspaceLedger）
- LoopRunner（start_goal + resume_goal）
- CLI lim loop --goal / --resume
- CommandVerifier

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
P1（阶段 2 起步 + Loop 成熟度 B）✅ 已完成
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
- AgentTransport trait（LocalChannelTransport）
- Scheduler 雏形
- 沙箱隔离粒度可选
- TUI 多任务视图（/loop status）
- LlmVerifier + 无进展检测

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
P2（阶段 3 完整）✅ 已完成
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
- ControllerRegistry（CRD 类比）
- AI-native 元工具
- 可观测性增强（LoopEvent broadcast）

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
P3（阶段 4 起步）✅ 已完成
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
- daemon 常驻进程骨架（in-process IPC）
- DaemonLedger（独立存储 + workspace 同步）
- P2P 网络拓扑（LocalTopology）+ 中心化预留（CentralizedTopology stub）
- 多 workspace 管理（WorkspaceRegistry）
- CLI loop-daemon 子命令

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
P3.5（product 层补全，基于 CC 调研）✅ 已完成
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
- CLI 暴露 LlmVerifier 入口（--verify llm + --verifier-provider + --verifier-model + --verifier-instructions）
  harness 层 LlmVerifier 已支持独立 provider，纯 product 层补全
- 文档：完成契约写进设计哲学（loop-engineering.md §一 + §12.6）

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
P4（阶段 4 完整 + CC Time-based loop + 多 agent workspace）✅ 已完成
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

P4 拆解为 11 个子任务，按依赖顺序执行。设计原则：
- **架构可回退**：所有新功能通过 wrap/compose 实现，不改 core trait 既有签名
- **XDG 规范**：全局配置三分（config/data/cache）
- **Event 是观测，Status 是控制**：坚守 k8s 哲学，调度决策读 Status 不读 Event

### P4.1 配置系统（XDG 规范）✅
- 全局配置：`~/.config/lim/config.toml`（XDG_CONFIG_HOME）
- 全局数据：`~/.local/share/lim/`（XDG_DATA_HOME）— 含 agents/、sessions/ 全局副本
- 全局缓存：`~/.cache/lim/`（XDG_CACHE_HOME）
- 项目级：`.lim/config.toml`（覆盖全局）+ `.lim/agents/` + `.lim/worktrees/`
- `crates/lim-config/` 新 crate，`ConfigLoader` 支持分层合并（global ← project ← CLI flags）
- **放宽 Containment Principle**：AGENTS.md 预告的 relax 时机

### P4.2 StatusProvider trait ✅
- `crates/lim-core/src/status.rs`：`StatusProvider` trait + `ComponentStatus` + `Condition`
- k8s 风格：`Condition { type, status: True/False/Unknown, last_transition_time, reason, message }`
- `LoopState` 加 `conditions: Vec<Condition>` 字段（`#[serde(default)]` 兜底，不 bump SCHEMA_VERSION）
- 各组件实现 `StatusProvider`：LoopRunner / Scheduler / WorkspaceRegistry / DaemonServer
- 铁律：调度决策读 Status，不读 Event

### P4.3 EventEnvelope 加 agent_id ✅
- `EventEnvelope.agent_id: Option<String>`（`#[serde(default)]`，向后兼容）
- 主 agent 为 `None` 或 `"main"`，sub-agent 为 `"reviewer"` / `"planner"` 等
- 为 multi-agent session 区分铺路

### P4.4 LoopEvent 落盘 + §8.4 黑盒闭合 ✅
- LoopEvent 写到 `.lim/loop/<goal_id>/events.jsonl`（`LoopEventPersister` 后台 task）
- ReviewerVerifier 的 reviewer agent 的 AgentEvent 写到 `.lim/loop/<goal_id>/reviews/review-<N>.jsonl`（`ReviewPersister` 后台 task）
- `Verifier::verify()` 签名扩展接收 `VerifyContext { goal_id, loop_dir }`
- `LoopEvent::VerificationDone` 扩展字段：`verifier_kind` / `verifier_session_id` / `verifier_tokens_used`（`verifier_kind` 已填充，另两个待后续扩展 trait）
- `LoopRunner.with_loop_dir(loop_dir)` 启用事件落盘 + 自动创建 event_tx
- CLI `build_loop_runner` 默认传入 `.lim/loop/` 作为 loop_dir
- 闭合 behavior-trace.md §8.4 已知黑盒点（v1.7 changelog）

### P4.5 角色配置（.lim/agents/<role>.toml）✅
- 项目级：`.lim/agents/<role>.toml`（reviewer.toml / planner.toml / coder.toml）
- 全局级：`~/.config/lim/agents/<role>.toml`（个人角色库）
- 字段：`system_prompt` / `model` / `tools_whitelist` / `max_turns` / `provider`
- **分层设计**：lim-agent 定义中性 `AgentRoleSpec`（harness 层，不依赖 lim-config）；
  CLI 层负责 `AgentRoleConfig` → `AgentRoleSpec` 转换（`role_config_to_spec`）
- `AgentFactory::for_role(spec, provider, registry, ctx, sessions_dir)` 构造方法
- `ReviewerVerifier::from_role_spec(spec, provider, registry, ctx, sessions_dir, instructions)` 构造方法
- `ToolRegistry::filtered_clone(&whitelist)` 实现 tools_whitelist 过滤（共享 Arc<dyn Tool> 实例）
- Model 优先级：CLI flag `--verifier-model` > role config `model` > provider `default_model`
- ReviewerVerifier 加载流程：`load_reviewer_role_config()` 找配置 → `from_role_spec` 构造；
  找不到则 `default_reviewer_spec()` 回退硬编码默认（向后兼容 pre-P4.5）
- 全量验证：382 测试通过，clippy + fmt 干净

### P4.6 Worktree 管理 ✅
- `crates/lim-agent/src/multiagent/worktree.rs`：`WorktreeHandle` + `create_worktree()` / `remove()`
- worktree 存放：`.lim/worktrees/<wt-id>/`（项目级）；`<wt-id>` 为 ULID
- `WorktreeSpec { agent_role, branch, base_branch, auto_cleanup }`（auto_cleanup 默认 true）
- RAII 模式：`Drop` 自动 `git worktree remove --force`（除非 `auto_cleanup=false` 或已 `remove()`）
- `WorktreeHandle::remove()` 显式清理（消费 handle，返回 Result）；Drop 静默 log 错误
- `Mutex<bool>` 防止 double-remove（Drop + remove 并发场景）
- 9 个测试（6 单元 + 3 集成）：spec serde roundtrip、ULID 唯一性、temp git repo create+remove、Drop auto-remove、persistent 保留
- **复用现有** `LocalExecutor::workspace(vec![wt_path])` + `StateLedger::new(wt_path)`

### P4.7 Skill 系统（CC SKILL.md）✅
- **新 crate `crates/lim-skills/`**：`SkillLoader`（发现/解析 SKILL.md）+ `SkillRegistry` + `Skill`/`SkillFrontmatter` schema
- **SKILL.md 格式**：YAML frontmatter（`name` / `description` / `verify` / `verify_args`）+ markdown body
- **加载优先级**（学 OpenClaw）：global (`~/.config/lim/skills/`) → project (`.lim/skills/`)，project 覆盖 global
- **分层设计**：lim-skills 只做 loading（不依赖 lim-agent）；`SkillVerifier` 在 lim-agent 中 impl `Verifier` trait
- **dispatch 逻辑**：
  - `skill.is_programmatic()`（有 `verify` 命令）→ 委托 `CommandVerifier`
  - 否则 → 委托 `LlmVerifier`，skill body 注入为 `extra_instructions`
- `SkillVerifier::from_skill(skill, provider, model)` / `from_registry(registry, name, provider, model)`
- `kind()` 返回 `"skill:command"` 或 `"skill:llm"`（区分 dispatch 路径，可观测）
- 15 个测试（11 loader + 4 SkillVerifier）：frontmatter 解析、verify_args 拼接、project 覆盖 global、parse error 跳过、programmatic pass/fail、non-programmatic LLM

### P4.8 Time-based loop ✅
- **抽象层**：`Timer` trait（`describe()` / `kind()` / `next_trigger()`）+ `TimerError`（`Exhausted` / `InvalidSchedule`）
  文件：`crates/lim-agent/src/loop_runner/timer.rs`
- **简单实现**：`IntervalTimer`（固定间隔触发，支持 `max_runs` 限制，unlimited/limited 两种构造）
- **接入层**：`TimerLoop`（`timer_loop.rs`）集成 Timer + LoopEvent 系统
  - 每次 timer fire 发射 `LoopEvent::TimeTriggered { goal_id, timer_kind, run_number }`
  - 通过闭包 `FnMut(usize) -> Future<GoalResult>` 接入 goal runner（不直接依赖 LoopRunner）
  - terminal result（Success/Failed/Cancelled）停止循环；transient result（BudgetExhausted/NeedsHuman）等下次触发
  - goal_runner 错误不杀死 timer loop，发射 Error 事件后继续
- **事件**：`LoopEvent::TimeTriggered` 变体（`events.rs`）
- 11 个测试（7 timer + 4 timer_loop）
- **待定**：cron 库选择（`cron` crate vs 自解析）、daemon 生命周期（常驻 vs `--max-runs`）

### P4.9 daemon IPC 抽象 ✅
- **抽象层**：`IpcTransport` trait（`send()` / `describe()`）+ `NullTransport`（占位实现）
  文件：`crates/lim-agent/src/loop_runner/ipc.rs`
- **简单实现**：`DaemonHandle` impl `IpcTransport`（in-process mpsc channel，现有实现提取 trait）
- **接入层**：`GoalRunner` trait（`run_goal()` / `name()`）+ `LoggingGoalRunner`（简单实现，只日志）
  文件：`crates/lim-agent/src/loop_runner/goal_runner.rs`
- **DaemonServer 集成**：`with_goal_runner()` 方法，Submit 时 spawn 后台 task 执行 goal
  - goal_runner 完成后根据 GoalResult 更新 ledger state（Success/Failed/NeedsHuman）
  - goal_runner 错误也更新 state 为 Failed
  - 不阻塞 Submit 响应（立即返回 goal_id，后台执行）
- 19 个测试（2 ipc + 3 goal_runner + 14 daemon，含 submit_with_goal_runner_updates_status 集成测试）
- **待定**：Unix socket 路径（`~/.cache/lim/daemon.sock` vs XDG_RUNTIME_DIR）、`attach` 实时 event 流

### P4.10 成本控制（Backoff）✅
- **抽象层**：`BackoffStrategy` trait（`next_delay() -> Option<Duration>` / `reset()` / `attempt()` / `describe()`）
  文件：`crates/lim-agent/src/loop_runner/backoff.rs`
- **简单实现**：`FixedBackoff`（固定 delay，optional max_attempts，unlimited/limited 两种构造）
- **LoopRunner 集成**：`with_backoff()` 方法，`run_goal_loop` 中 VerifyResult::Fail 后调用 `apply_backoff()`
  - `Some(delay)` → sleep 后继续下次 session
  - `None`（exhausted）→ 升级为 NeedsHuman
  - Pass 时 `reset_backoff()`；resume_goal 时也 reset
  - Mutex 包装（`run_goal_loop` 是 `&self`），lock 只在 `next_delay()` 同步调用期间持有
- 与 NoProgressDetector 协同：no_progress threshold 先检查，backoff 在 no_progress 之后
- 7 个测试（backoff 单元）+ 1 个集成测试（`backoff_exhaustion_escalates_to_needs_human`）
- **待定**：ExponentialBackoff + jitter 实现、与 Budget `min_interval` 字段的整合

### P4.11 全量验证 ✅
- cargo test --workspace + clippy + fmt
- 新增测试覆盖：StatusProvider 实现、WorktreeHandle 生命周期、SkillLoader 解析

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
P2.5（多 agent 扩展，可独立于 P4 做）✅ 已完成
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
- Reviewer agent 模式（ReviewerVerifier + fresh context）= CC "第二 agent 代码审查"
  主 agent 完成后 spawn 独立 reviewer，不共享 context，有工具访问可检查实际产物
  CLI: `--verify reviewer` + `--verifier-provider` + `--verifier-model` + `--verifier-instructions`
  实现: `crates/lim-agent/src/loop_runner/verifier.rs` ReviewerVerifier，持 Arc<AgentFactory> 每次 verify() spawn fresh agent

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
P4+（CC Proactive loop）⏳ 远期
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
- Proactive routine 组合（schedule + goal + 动态工作流 + auto mode）= CC Proactive 层
- 中心化调度策略实现（CentralizedTopology 实体化）
- daemon 跨 workspace goal 聚合查询
```

### 与 Claude Code 4 层 Loop 的对照

| CC 层级 | lim 实现路径 | 状态 | 对应 P 级 |
|---|---|---|---|
| Turn-based | `AgentLoop` | ✅ 已有 | P0 |
| Goal-based | `LoopRunner` + `Verifier` | ✅ 已有 | P0-P1 |
| Time-based | `DaemonServer` + 定时器 + Skill | ✅ 已有 | P4 (P4.7/P4.8) |
| Proactive | `ControllerRegistry` + `DaemonServer` + `Scheduler` + 多 workspace | ⏳ 远期 | P4+ |

详见 loop-engineering.md §十二 行业参考。

## 五、workspace 边界

| 阶段 | 状态存储位置 | 理由 |
|---|---|---|
| 阶段 1-3（CLI/TUI） | workspace 下 `.lim/loop/` | Containment Principle，状态跟项目走 |
| 阶段 4（daemon/server） | daemon 独立工作目录 + **可选同步到 workspace** | daemon 有自己的存储，但用户可能需要在 workspace 看现场 |

`StateLedger` 的接口要支持两种 backend：

```rust
trait StateLedgerBackend: Send + Sync {
    async fn load(&self, goal_id: &GoalId) -> Result<LoopState>;
    async fn save(&self, goal_id: &GoalId, state: &LoopState) -> Result<()>;
}

// P0：workspace 本地文件
struct WorkspaceLedger { root: Utf8PathBuf }

// P3：daemon 独立存储 + 同步钩子
struct DaemonLedger {
    daemon_dir: Utf8PathBuf,
    workspace_sync: Option<WorkspaceLedger>,
}
```

## 六、网络拓扑预留

阶段 2 的 A2A 通信抽象要支持未来从单机到多机的演进：

```rust
trait AgentTransport: Send + Sync {
    async fn send(&self, target: AgentId, msg: AgentMessage) -> Result<()>;
    async fn recv(&self) -> Result<AgentMessage>;
}

// P1：进程内 channel（单机）
struct LocalChannelTransport { ... }

// P4：P2P 网络（同级组网）
struct P2PNetworkTransport { ... }

// P4：中心化调度
struct CentralizedTransport { scheduler: SchedulerAddr }
```

默认 P2P 同级组网，不依赖 master 节点，适合本机和小规模局域网。预留扩展能力可切换到中心化模式提高性能。
