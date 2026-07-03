# Loop Engineering 落地方案

本文档定义 Loop Engineering 在 lim 的具体实现方案。Loop Engineering 是继 Prompt Engineering → Context Engineering → Harness Engineering 之后的第四代方法论，核心是"设计自驱动循环系统"。

## 一、概念厘清

### Loop Engineering 的本质

引用 Boris Cherny（Claude Code 负责人）："我不再手动提示 Claude 了。我有一堆 loop 在跑，是它们在提示 Claude、决定下一步做什么。我的工作是写这些 loop。"

Loop Engineering 把人从"操作者"变成"规则设计者"：
- **触发**：从人工触发 → 系统自触发
- **判断**：从人判断 → 系统自验证、自迭代
- **持续**：从单次执行 → 跨 context window 持续产出

### Goal-based loop 三要素（完成契约）

> 参考 Claude Code 官方《Getting Started with Loops》及 Feisky 解读，2026-07-02 整理。详见 §十二 行业参考。

**Goal-based loop 三要素 = 触发 + 完成契约（verifier）+ 预算**：
- **触发**：何时启动 loop（手动 prompt / 时间间隔 / 事件）
- **完成契约（verifier）**：把"做完"从主观判断变成可验证的合约——无 verifier 的 loop 退化为 turn-based（人来判断"做完没"）
- **预算**：限制最大消耗（max_sessions / max_tokens）——无预算的 loop 可能无限烧 token

verifier 不是可选组件，是 goal-based loop 闭环的必要条件。CC 的"完成契约"概念印证了这一点：agent 能真正闭环的前提是"做完"可验证。确定性验证（测试数、分数阈值、exit code）优于描述性验证（LLM 自评），因为描述性结果让模型自评很不靠谱。

### 两个层级的 Loop

lim 的 Loop Engineering 是**两个嵌套的 loop**：

```
Goal Loop（跨 session，LoopRunner 管）
└── Turn Loop（单 session 内，AgentLoop 管）
    ├── turn 1: LLM → tool → LLM → tool → stop
    ├── turn 2: ...
    └── session 结束，状态持久化
└── 恢复状态 → 新 session → 继续工作
```

- **Turn Loop** = 现有的 `AgentLoop`，单 context window 内的"LLM→工具→LLM"循环
- **Goal Loop** = 跨 context window 的"跑一个 session → 持久化状态 → 验证 → 决定继续/停止"循环

Anthropic 的实现：initializer agent 跑第一个 session 写好 `feature_list.json` + `claude-progress.txt`，coding agent 跑后续 session 每次读这些文件恢复状态、做一个 feature、写回状态、验证。

### Loop 的 8 个 Primitives

| Primitive | lim 实现 | 层级 |
|---|---|---|
| 1. 循环控制结构 | AgentLoop + MAX_STEPS | Core |
| 2. 验证机制 | Verifier trait | Harness |
| 3. 停止条件 | AgentHooks::should_stop | Core 提供 hook，Product 实现策略 |
| 4. 上下文管理 | ContextWindowManager + compaction | Harness（已有） |
| 5. 工具与反馈 | lim-tools + lim-executor | Product（已有） |
| 6. Human-in-the-Loop | AgentHooks::before_tool_call | Product（已有） |
| 7. 自动化触发与并行 | CLI lim loop / future daemon | Product |
| 8. 记忆/状态持久化 | StateLedger | Harness |

## 二、架构定位

Loop Engineering 不是 lim 架构里的"一层"，而是覆盖在四层之上的能力维度：

```
┌─────────────────────────────────────────────────────────┐
│  Loop Engineering 能力维度（横切）                        │
│  ├─ Verifier（验证机制）        → Product 层策略          │
│  ├─ StopCondition（停止条件）   → Core 层 hook           │
│  ├─ Trigger（自动化触发）       → Product 层组件          │
│  ├─ StateLedger（状态账本）     → Harness 层扩展          │
│  └─ Parallelism（并行隔离）     → Product 层策略          │
├─────────────────────────────────────────────────────────┤
│  Product Layer                                           │
│  apps/cli + apps/tui + lim-tools + 审批 + loop 触发      │
├─────────────────────────────────────────────────────────┤
│  Harness Layer                                           │
│  lim-session + compaction + context_window               │
│  + loop_runner（StateLedger + Verifier + LoopRunner）    │
├─────────────────────────────────────────────────────────┤
│  Core Layer                                              │
│  AgentLoop + AgentHooks（含 should_stop/verify hook）     │
├─────────────────────────────────────────────────────────┤
│  Primitives Layer                                        │
│  lim-core + lim-llm                                      │
└─────────────────────────────────────────────────────────┘
```

## 三、Core 层改动：AgentHooks 补 should_stop + max_steps

AgentLoop 的 Turn Loop 需要"何时停止单个 session"的决策。

### 新增类型

```rust
pub struct StopContext<'a> {
    pub turn_count: usize,
    pub total_tokens: u64,
    pub elapsed: Duration,
    pub last_tool_results: &'a [ToolResult],
}

pub enum StopDecision {
    Continue,
    StopSession(SessionEndReason),
}

pub enum SessionEndReason {
    MaxStepsReached,
    BudgetExhausted,
    NoProgress,
    GoalReached,
    NeedsHuman,
    ContextExhausted,
}
```

### AgentHooks 新增方法

```rust
pub trait AgentHooks: Send + Sync {
    // ... existing methods ...
    
    /// 每个 turn 结束后调用。返回非 Continue 则结束当前 session。
    fn should_stop(&self, _ctx: &StopContext) -> StopDecision {
        StopDecision::Continue
    }
    
    /// 单 session 内的步数硬上限。None = 无限制。
    fn max_steps(&self) -> Option<usize> {
        None
    }
}
```

### AgentLoop 的改动

`run_inner` 的 turn 循环末尾加 `should_stop` 检查：

```rust
// 在 turn 结束后、continue 之前
if let Some(ref hooks) = self.hooks {
    let ctx = StopContext {
        turn_count: turn_num + 1,
        total_tokens: /* accumulated */,
        elapsed: /* elapsed */,
        last_tool_results: &tool_results,
    };
    match hooks.should_stop(&ctx) {
        StopDecision::Continue => {}
        StopDecision::StopSession(reason) => {
            emit SessionEnd { reason };
            break;
        }
    }
}
```

默认 `Continue`，不破坏现有行为。

**注意**：`verify` 不放在 Turn Loop 级别——验证是 Goal Loop 的事，在 session 之间做。

## 四、Harness 层：三个新组件

### 1. StateLedger — 状态持久化

用文件做跨 session 的"工作记忆"（Anthropic 方案的核心）。

```rust
// crates/lim-agent/src/loop_runner/state_ledger.rs

pub struct StateLedger {
    project_root: Utf8PathBuf,
}

impl StateLedger {
    pub async fn load(&self, goal_id: &GoalId) -> Result<LoopState>;
    pub async fn save(&self, goal_id: &GoalId, state: &LoopState) -> Result<()>;
    pub async fn list_goals(&self) -> Result<Vec<GoalId>>;
}

pub struct LoopState {
    pub goal: String,
    pub progress: String,           // agent 自己写的工作笔记（markdown）
    pub features: Vec<FeatureStatus>, // 结构化 feature 清单
    pub turn_count: usize,
    pub total_tokens: u64,
    pub session_count: usize,
    pub last_session_id: Option<SessionId>,
    pub status: GoalStatus,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

pub enum GoalStatus {
    Running,
    Paused,
    Success,
    Failed(String),
    NeedsHuman(String),
}
```

**文件布局**（符合 Containment Principle）：

```
workspace/
├── .lim/
│   └── loop/
│       ├── <goal_id>/
│       │   ├── goal.md           # goal 描述 + 验证标准
│       │   ├── progress.md       # agent 的工作笔记
│       │   ├── features.json     # 结构化 feature 清单
│       │   └── state.json        # LoopRunner 元状态
├── sessions/
│   └── *.jsonl                   # session 事件日志（已有）
└── src/  ...                     # agent 修改的实际代码
```

**为什么用文件而不是 session JSONL**：session JSONL 是事件日志（不可变记录），StateLedger 是工作记忆（可变、可覆盖、agent 可读写）。两者职责不同。

### 2. Verifier — goal 验证

```rust
// crates/lim-agent/src/loop_runner/verifier.rs

pub trait Verifier: Send + Sync {
    async fn verify(&self, state: &LoopState) -> Result<VerifyResult>;
}

pub enum VerifyResult {
    Pass,
    Fail(String),
    NeedsHuman,
}

/// 确定性验证：跑命令，看 exit code（首选）
pub struct CommandVerifier { command: String }

/// LLM-as-judge：让 LLM 看状态判断是否达成（轻量兜底，无工具）
pub struct LlmVerifier { provider: Arc<dyn Provider>, prompt: String }

/// Reviewer agent：spawn 独立 reviewer agent + fresh context + 工具访问
/// 深度审查（CC "第二 agent 代码审查"模式），每次 verify() 跑完整 agent session
pub struct ReviewerVerifier { factory: Arc<AgentFactory>, extra: Option<String> }

/// 组合验证：全过才算过
pub struct AllPassVerifier { verifiers: Vec<Box<dyn Verifier>> }
```

验证器优先级：`CommandVerifier`（确定性）> `LlmVerifier`（轻量 LLM 判断）> `ReviewerVerifier`（深度 agent 审查）。
`CommandVerifier` 骗不了（exit code）；`LlmVerifier` 只看摘要、无工具；`ReviewerVerifier` 有工具可检查实际产物但最贵。

### 3. LoopRunner — Goal Loop 执行器

```rust
// crates/lim-agent/src/loop_runner/runner.rs

pub struct LoopRunner {
    agent_factory: AgentFactory,       // 创建 AgentLoop 的工厂
    ledger: StateLedger,
    verifier: Box<dyn Verifier>,
    budget: Budget,
}

pub struct Budget {
    max_sessions: Option<usize>,
    max_total_tokens: Option<u64>,
    max_total_cost: Option<f64>,
}

impl LoopRunner {
    /// 启动新 goal
    pub async fn start_goal(&self, goal: &str) -> Result<GoalId>;
    
    /// 恢复中断的 goal（P0 关键能力）
    pub async fn resume_goal(&self, goal_id: &GoalId) -> Result<()>;
    
    /// 查询状态
    pub async fn status(&self, goal_id: &GoalId) -> Result<GoalStatus>;
    
    /// 取消
    pub async fn cancel(&self, goal_id: &GoalId) -> Result<()>;
}
```

**Goal Loop 执行流程**：

```
1. 预算检查 → 不够则返回 BudgetExhausted
2. 恢复状态 → StateLedger.load()
3. 构造 session prompt → 基于 state，不是原始 goal
4. 创建 AgentLoop → agent_factory.create(state)
5. 运行一个 session → agent.run(session_prompt)
6. 更新状态 → StateLedger.save()
7. 验证 goal → verifier.verify(state)
   - Pass → 返回 Success
   - Fail → 无进展检测 → 继续/升级
   - NeedsHuman → 返回
8. goto 1
```

**关键设计**：`AgentFactory` 不是 `AgentLoop` 本身——每个 session 创建新的 `AgentLoop` 实例（新 context window），但共享 `ToolRegistry` 和 `Executor`。

## 五、Product 层：CLI 触发器

```rust
// apps/cli: lim loop 命令

// 启动新 goal
$ lim loop --goal "fix all clippy warnings" --verifier "cargo clippy"

// 恢复中断的 goal
$ lim loop --resume <goal_id>

// 查看所有 goal
$ lim loop --list

// 查看某个 goal 状态
$ lim loop --status <goal_id>
```

未来加 cron/event 触发只是套一层调度，LoopRunner 本身不变。

## 六、与现有组件的关系

| 现有组件 | 关系 |
|---|---|
| `AgentLoop` | Turn Loop 执行器，LoopRunner 调用它跑单个 session。不改核心逻辑，只在 turn 末尾加 `should_stop` 检查 |
| `AgentHooks` | 补 `should_stop` + `max_steps`。LoopRunner 不直接用 AgentHooks——它构造 AgentLoop 时注入 hooks |
| `SessionWriter` | 不变。每个 session 正常写 JSONL |
| `ContextWindowManager` | 不变。compaction 在 Turn Loop 内生效 |
| `ToolRegistry` | 共享。所有 session 共用同一套工具 |
| `Executor` | 共享。所有 session 共用同一个执行器 |

**最重要的一点**：Loop Engineering 不需要重构现有核心。它是在现有架构之上加一层。

## 七、workflow 的定位

workflow 是实现 loop 的手段之一，不是独立的架构层：

- **静态 DAG workflow**（lim-workflow 当前）：适合"已知步骤的 loop"
- **动态 workflow**（LLM 路由）：适合"未知步骤的 loop"
- **Agent loop + 工具**：最轻量的 loop

这三种都是 Loop Engineering 的实现路径。它们都属于 Harness/Product 层，都是"如何使用核心循环来完成任务"的策略。

lim-workflow 当前是静态版本（代码路由）。未来升级成动态版本只是把 router 从代码换成 LLM，结构不变。

## 八、AI-native 自省

lim 的核心功能应该作为工具暴露给 AI，让 AI 能在对话中控制 loop：

```rust
// 未来实现的元工具（AI 可调用）
trait LoopControlTool: Tool {
    // AI 可以在对话中创建子任务
    // AI 可以查询任务状态
    // AI 可以暂停/恢复/取消任务
    // AI 可以修改 goal 或验证标准
}
```

这意味着 lim 的工具集不只是"读文件/写文件/跑命令"，还包括"控制 lim 自身"。聊天本身也是一种 loop，AI 可以随时从"对话模式"切换到"任务模式"。

## 九、实现优先级

```
P0  AgentHooks 加 should_stop + max_steps         （Core，小改动）
P0  StateLedger（WorkspaceLedger）                 （Harness，新模块）
P0  LoopRunner（start_goal + resume_goal）          （Harness，新模块）
P0  CommandVerifier                                （Harness，新模块）
P0  CLI lim loop --goal / --resume                 （Product，入口）

P1  LlmVerifier + 无进展检测                        （Harness，增强）
P1  TUI loop 状态查看                               （Product，UI）

P2  ControllerRegistry（CRD 类比）                  （Harness，固化模板）
P2  AI-native 元工具                               （Product，AI 自省）

P3  daemon 形态                                    （Product，常驻进程）
P3  DaemonLedger                                   （Harness，独立存储）
```

P0 就能跑起来一个基本的 Loop Engineering：`lim loop --goal "fix all clippy warnings" --verifier "cargo clippy"`。

## 十、设计约束

LoopRunner 库的设计要满足三个成熟度（CLI / TUI 挂载 / daemon）都能用：

1. `Send + Sync` — 未来可在线程/进程里跑
2. 无全局状态 — 每个 LoopRunner 实例独立
3. 状态全在 StateLedger — 不依赖进程内存

这三点保证未来 daemon 化时零改动。

## 十一、可观测性

Loop Engineering 的可观测性需要在 session 事件流之外增加 loop 级信号：

- **LoopState 变更**：每次 save 时 `debug!` log
- **验证结果**：每次 verify 时 `info!` log（Pass/Fail/NeedsHuman）
- **session 边界**：LoopRunner 跨 session 时 `info!` log
- **预算消耗**：每次 session 结束更新预算 `debug!` log
- **无进展检测**：触发时 `warn!` log

这些信号不进 session JSONL（那是 Turn Loop 的记录），而是进 tracing log 和 StateLedger 的 state.json。

P1+ 补充：以上信号现在也通过 `LoopEvent` broadcast channel 发送（见 `crates/lim-agent/src/loop_runner/events.rs`）。`LoopRunner::with_event_sender` 注入 sender，订阅者可写 `.lim/loop/<goal_id>/events.jsonl`、喂给 TUI 仪表盘，或对接外部可观测性系统。事件覆盖：`GoalStarted` / `GoalResumed` / `SessionStarted` / `SessionEnded` / `VerificationDone` / `BudgetChecked` / `NoProgressDetected` / `GoalFinished` / `Error`。

## 十二、行业参考：Claude Code 4 层 Loop 模型与 lim 对照

> 本节整理 Claude Code 官方博客《Getting Started with Loops》(2026-06-30) 及 Feisky 解读文章的关键框架，对照 lim 当前实现与规划，标注重合、差距与改进方向。资料来源：
> - https://claude.com/blog/getting-started-with-loops
> - https://mp.weixin.qq.com/s/DVjVwFs7EPIr9F-39-BKQw

### 12.1 Claude Code 的 4 层 Loop 分类

Claude Code 团队按**"你愿意交出去什么"**把 loop 分成 4 层，递进关系是交出去越多、自动化越高、成本越高：

| 层级 | 你交出去的 | 触发方式 | 停止条件 | 适用场景 | CC 原语 |
|---|---|---|---|---|---|
| **Turn-based** | 验证步骤 | 用户 prompt | Claude 判断完成或需更多上下文 | 短任务、探索性、一次性 | Skill（SKILL.md） |
| **Goal-based** | 停止条件 | 手动 prompt（实时） | goal 达成 OR 最大 turn 数 | 有可验证退出条件的任务 | `/goal` |
| **Time-based** | 触发时机 | 时间间隔 | 用户取消 OR 工作完成 | 周期性工作、监控外部系统 | `/loop`、`/schedule` |
| **Proactive** | 整个决策流程 | 事件/计划，无人实时 | 每个任务 goal 达成退出；routine 运行到关闭 | 长期定型的 well-defined 工作流 | 以上全部 + 动态工作流 + auto mode |

### 12.2 CC 关键工程实践

1. **Skill 编码验证步骤**：把"怎么确认做对了"写成 `SKILL.md`，让 agent 自己跑端到端验证（启动 dev server → 点击 → 截图 → 检查 console → 性能追踪）。越是量化的检查，agent 越容易自验证。

2. **确定性停止条件**：`/goal` 的停止条件必须可验证（测试通过数、分数阈值、Lighthouse 分数）。描述性结果让模型自评很不靠谱——跑 Lighthouse 拿到 92 分这种确定性结果才能准确判断。

3. **独立 evaluator 模型**：每次 Claude 尝试停止，有**独立的评估模型**检查条件是否满足，没满足就打回去继续。evaluator 与主 agent 分离，避免主 agent 的推理偏见影响判断。Feisky 称之为"完成契约"——把"做完"从主观判断变成可验证的合约，agent 才能真正闭环。

4. **质量保障四件套**：保持 codebase 干净（agent 跟随现有模式）→ 给 agent 验证自己工作的方式（skills）→ 文档易达 → 用第二个 agent 做代码审查（fresh context，less biased）。不要只修单个问题，要编码进系统改进所有未来迭代。

5. **成本控制**：选对原语和模型（90% 任务 turn-based 够了）→ 确定性工作写脚本（跑脚本比推理便宜）→ 先小规模试跑（动态工作流能起几十 agent）→ 间隔匹配变化频率（PR 一小时一条 review 别用 5 分钟轮询）→ 看消耗明细（`/usage`/`/goal`/`/workflows`）。

6. **Proactive 层风险最高**：动态工作流 + `/schedule` + auto mode 组合对模型性能和上下文要求极高，最烧 token。Feisky 建议先从 `/goal` 开始——找每天重复做的事，问能不能把"做完了"写成可验证的一句话。

### 12.3 重合点分析（lim 规划已覆盖且方向一致）

重合度约 80%。lim 的 Loop Engineering 规划与 CC 框架在核心方向上完全一致：

| CC 概念 | lim 对应 | 重合度 | 说明 |
|---|---|---|---|
| Goal-based loop (`/goal`) | `LoopRunner` + `Verifier` + `Budget` + `NoProgressDetector` | 完全重合 | lim P0-P1 已实现 CC 的 goal-based 全部要素：停止条件、turn cap、无进展升级 |
| 确定性停止条件优先 | `CommandVerifier`（exit code）优先于 `LlmVerifier` | 完全重合 | lim 文档明确"确定性优先"，CC 也强调"测试数/分数阈值最好用" |
| 预算控制 | `Budget` (max_sessions + max_tokens) | 重合 | CC 的"explicit turn caps" = lim 的 `max_sessions` |
| 无进展检测 | `NoProgressDetector`（连续相同失败→NeedsHuman） | 重合+超越 | lim 比 CC 多了显式 no-progress 机制，CC 文章未提及 |
| Time-based 是未来方向 | evolution.md 阶段 4 daemon | 方向重合 | lim 规划了 daemon 模式，CC 的 `/loop 5m` 是同样的时间触发 |
| Proactive 是最高形态 | evolution.md 阶段 4 + ControllerRegistry | 方向重合 | CC 的"组合 routine" = lim 的 ControllerRegistry + daemon |
| AI-native 自省 | §八 + P2 `register_loop_control_tools` | 完全重合 | CC 的"agent 控制 loop 本身" = lim 的元工具 `loop_list_goals` 等 |
| 跨 context window 持续 | Goal Loop（LoopRunner 跨 session） | 完全重合 | CC 的"initializer + coding agent" = lim 的 `LoopState` + `build_session_prompt` |
| 状态持久化 | `StateLedger` / `DaemonLedger` | 完全重合 | CC 的 `progress.txt` = lim 的 `state.json` + `progress.md` |

### 12.4 lim 当前实现对照与差距

| CC 层级 | lim 对应 | 状态 | 差距分析 |
|---|---|---|---|
| Turn-based | `AgentLoop`（每轮用户驱动） | ✅ 已实现 | **无 Skill 系统**：lim 有 hooks 但没有 `SKILL.md` 机制让用户编码验证步骤。agent 自验证靠 verifier trait，不是用户可编辑的 skill 文档 |
| Goal-based | `LoopRunner` + `Verifier` + `Budget` + `NoProgressDetector` | ✅ P1 已实现 | **CLI 未暴露 LlmVerifier 入口**：harness 层 `LlmVerifier::new(provider, model, extra)` 已接受独立 `Arc<dyn Provider>`，trait 设计上 evaluator 是独立的。但 CLI `build_loop_runner` 只接受 `--verify <command>`，用户无法选 LlmVerifier 或配置独立 evaluator provider。这是 **product 层暴露不足**，不是 harness 层设计缺陷 |
| Time-based | `DaemonLedger`（独立存储）+ `DaemonServer`（in-process） | ⚠️ 部分 | **无时间触发**：lim 的 daemon 是手动 submit，没有 `/loop 5m` 这种时间间隔触发原语。`DaemonServer` 持有 ledger 但没有定时器 |
| Proactive | `ControllerRegistry` + `WorkspaceRegistry` + `NetworkTopology` | ⚠️ 骨架 | **无 routine 组合**：lim 有 daemon 骨架但没有"schedule + goal + 动态工作流 + auto mode"的组合 routine 模型 |

### 12.5 基于架构原则的改进方向

lim 的核心架构原则是 **"policy in product, mechanism in core"** + **"extension over built-in"**。CC 的概念映射到 lim 时必须遵循这条原则——CC 是 product 一体化（内置原语），lim 是 trait + 组合。

#### 改进 1：完善 product 层入口（高优先级，机制已有缺暴露）

**问题**：harness 层 `LlmVerifier` 已支持独立 provider，但 CLI `build_loop_runner` 只接受 `--verify <command>`，用户无法选 LlmVerifier。

**改进**（遵循"policy in product"）：

```rust
// apps/cli/src/main.rs - Loop 命令增加选项
Loop {
    /// Verifier 类型: "cargo test" (command) 或 "llm" (LLM judge)
    #[arg(long)]
    verify: Option<String>,
    /// 独立 evaluator 的 provider 实例名（默认复用主 provider）
    #[arg(long)]
    verifier_provider: Option<String>,
    /// 独立 evaluator 的 model 名
    #[arg(long)]
    verifier_model: Option<String>,
}
```

harness 层零改动，纯 product 层补入口。这印证 CC 的"独立 evaluator 模型"——lim 的 trait 早就支持了，只是 product 没暴露。

**所属 P 级**：P3.5（P3 已完成，这是 P3 收尾的 product 层补全）

#### 改进 2：把"完成契约"写进设计哲学（高优先级，文档层）

**改进**：在 §一 概念厘清补充 goal-based loop 三要素：

> **Goal-based loop 三要素 = 触发 + 完成契约（verifier）+ 预算**
> - 无 verifier 的 loop 退化为 turn-based（人来判断"做完没"）
> - 无预算的 loop 可能无限烧 token
> - 无触发的 loop 不存在
>
> verifier 不是可选组件，是 goal-based loop 闭环的必要条件。CC 的"完成契约"概念印证了这一点。

**所属 P 级**：文档改动，随下次 review 落地

#### 改进 3：Skill 系统（中优先级，新 crate）

**问题**：CC 的 `SKILL.md` 让用户编码验证步骤，lim 的 `Verifier` trait 是程序化的，普通用户难写。

**改进**（遵循"extension over built-in" + "crate 边界对齐职责边界"）：

- 新增 `crates/lim-skills/`：加载 `.lim/skills/*.md` → 解析 frontmatter → 注入 system prompt + 可选验证脚本
- Skill 是 **Verifier 的一种实现**（`SkillVerifier`），不是替代 verifier trait
- Skill 文档可包含验证命令（`verify: cargo test`），SkillVerifier 解析后委托给 CommandVerifier
- Skill 也可包含验证步骤描述（markdown），由 LlmVerifier 读取后执行端到端验证

Skill 文档示例：

```markdown
---
name: verify-frontend-change
description: Verify any UI change end-to-end before declaring it done.
verify: npm test
---
# 前端改动验证流程
1. 启动 dev server，打开改动页面
2. 点击按钮、填表单，确认预期行为
3. 检查浏览器 console：不能有新的 error 或 warning
4. 用 Chrome Devtools MCP 跑一次性能追踪
```

这样 lim 既有程序化 verifier（给开发者），又有人类可编辑 skill（给用户），两者通过 trait 统一。

**所属 P 级**：P4（新 crate，独立交付）

#### 改进 4：Time-based loop（中优先级，P4 daemon 扩展）

**改进**（遵循"渐进演进" + "LoopRunner 零改动"）：

- P3 daemon 骨架已有 `DaemonServer` + `DaemonLedger`
- P4 在 `DaemonCommand::Submit` 增加 `every: Option<Duration>` 字段
- daemon spawn `tokio::time::interval` task，周期性触发 `LoopRunner::start_goal`
- Budget 增加 `min_interval` + backoff 策略（无变化时拉长间隔）

```rust
// P4 daemon routine 示例
$ lim loop-daemon submit --goal "check PR reviews" --every 5m --workspace /path/to/repo
$ lim loop-daemon submit --goal "triage bug reports" --schedule "0 9 * * 1-5"  # cron
```

不需要改 LoopRunner（"LoopRunner 零改动，只换 Ledger 和 Transport"原则）。

**所属 P 级**：P4

#### 改进 5：Reviewer agent 模式（中优先级，P2 多 agent 扩展）✅ 已完成

**改进**（遵循"core 提供机制，product 决定策略"）：

- harness 层 `AgentFactory::create_session()` 已有 spawn fresh agent 能力
- product 层新增 `ReviewerVerifier`：每次 `verify()` spawn 独立 reviewer agent，fresh context + 工具访问
- reviewer 不共享主 agent 的 session（fresh），可以读文件、跑命令检查实际产物
- 输出 `PASS` / `FAIL: <reason>` / `NEEDS_HUMAN`（与 LlmVerifier 一致）
- 可作为 `AllPassVerifier` 的子 verifier 组合使用

```rust
// P2.5 reviewer 模式 — 实际实现
let reviewer = ReviewerVerifier::new(
    Arc::new(reviewer_factory),  // 独立 factory（可配置不同 model）
    extra_instructions,           // Option<String>
);
let verifier = AllPassVerifier::new(vec![
    Box::new(CommandVerifier::new("cargo test")),
    Box::new(reviewer),
]);
```

CLI 暴露：`--verify reviewer` + `--verifier-provider` + `--verifier-model` + `--verifier-instructions`

**所属 P 级**：P2.5 ✅ 已完成（`crates/lim-agent/src/loop_runner/verifier.rs`）

#### 改进 6：成本控制原语细化（低优先级）

**改进**：Budget 增加 `min_interval` 字段；daemon routine 支持 backoff（无变化时拉长间隔）。

**所属 P 级**：P4（随 Time-based loop 一起）

### 12.6 lim 与 CC 的设计哲学差异

lim 的 Loop Engineering 与 Claude Code 的核心差异在于**抽象层级**：

- **Claude Code**：product 一体化，`/goal`/`/loop`/`/schedule` 是内置原语，Skill 是用户可编辑的验证文档，evaluator 是内置的独立模型。用户在 product 层组合。
- **lim**：core 提供 trait（`Verifier`/`LedgerBackend`/`AgentTransport`/`NetworkTopology`），product 层（CLI/TUI/daemon）组合实现。`LoopRunner` 是 harness 层的 goal-based loop 执行器，不绑定具体 verifier 实现。

这意味着 lim 的 4 层 loop 不是内置原语，而是**通过组合 core trait 实现的 product 形态**：

| CC 层级 | lim 实现路径 | 状态 |
|---|---|---|
| Turn-based | `AgentLoop` | ✅ 已有 |
| Goal-based | `LoopRunner` + `Verifier` | ✅ 已有 |
| Time-based | `DaemonServer` + 定时器 | P4 |
| Proactive | `ControllerRegistry` + `DaemonServer` + `Scheduler` + 多 workspace | P4+ |

这种分离让 lim 可以支持 CC 之外的模式（如 k8s-style 声明式 controller、多 agent 拓扑），但代价是 product 层需要做更多组合工作。

### 12.7 后续规划汇总

基于 CC 调研，lim Loop Engineering 的后续规划如下：

| P 级 | 改进项 | 层级 | 依赖 | 优先级 |
|---|---|---|---|---|
| P3.5 | CLI 暴露 LlmVerifier 入口 + 独立 evaluator provider 配置 | Product | 无（harness 已支持） | 高 ✅ 已完成 |
| P3.5 | 文档：完成契约写进 §一 设计哲学 | 文档 | 无 | 高 ✅ 已完成 |
| P4 | Skill 系统（`crates/lim-skills/` + `SkillVerifier`） | Harness/Product | Verifier trait | 中 |
| P4 | Time-based loop（daemon 定时器 + `--every` + cron schedule） | Product | DaemonServer | 中 |
| P4 | 成本控制细化（Budget `min_interval` + backoff） | Harness | Budget | 低 |
| P2.5 | Reviewer agent 模式（`ReviewerVerifier` + fresh context） | Product | AgentFactory | 中 ✅ 已完成 |
| P4+ | Proactive routine 组合（schedule + goal + 动态工作流 + auto mode） | Product | 全部上述 | 低 |

**关键结论**：lim 的架构原则（policy in product, mechanism in core）与 CC 的概念完全兼容。CC 的 4 层 loop 在 lim 中不是内置原语，而是通过组合 core trait 实现的 product 形态。P3.5 + P2.5 已完成，剩余差距在 Skill 系统（P4 新 crate）和 Time-based loop（P4 daemon 定时器）。

## 十三、P1–P3 实现状态与已知问题

> 本节追踪 Loop Engineering P1–P3 的落地情况与已知缺陷，供后续 review/迭代参考。最后更新：2026-07-02。

### P1（已完成）

| 组件 | 文件 | 状态 |
|---|---|---|
| LlmVerifier + AllPassVerifier 组合验证器 | `loop_runner/verifier.rs` | ✅ |
| NoProgressDetector（连续相同失败计数→NeedsHuman） | `loop_runner/progress.rs` | ✅ |
| LoopRunner 接入 hooks + 可配置 no-progress 阈值 | `loop_runner/runner.rs` | ✅ |
| TUI `/loop status|list|resume` 命令 | `apps/tui/src/app/commands.rs` | ✅ |
| AgentTransport trait + LocalChannelTransport | `multiagent/transport.rs` | ✅ |
| Scheduler 雏形（spawn/cancel/join） | `multiagent/scheduler.rs` | ✅ |

### P2（已完成）

| 组件 | 文件 | 状态 |
|---|---|---|
| ControllerRegistry + FixClippy/WriteTests/Custom 控制器 | `loop_runner/controller.rs` | ✅ |
| AI-native 元工具 `loop_list_goals` / `loop_goal_status` / `loop_pause_goal` / `loop_resume_goal` | `crates/lim-tools/src/loop_control.rs` | ✅ |
| LoopEvent broadcast 事件类型 | `loop_runner/events.rs` | ✅ |
| `register_loop_control_tools` 在 CLI `build_loop_runner` 中注册 | `apps/cli/src/main.rs` | ✅ |

### P3（已完成，含已知问题）

| 组件 | 文件 | 状态 |
|---|---|---|
| `LedgerBackend` trait（抽象存储层） | `loop_runner/ledger.rs` | ✅ |
| `DaemonLedger` 独立存储 + `bind_workspace` + `sync_to_workspace` | `loop_runner/ledger.rs` | ✅ |
| `DaemonCommand`/`DaemonResponse`/`DaemonHandle`/`DaemonServer` in-process IPC | `loop_runner/daemon.rs` | ✅ |
| `NetworkTopology` trait + `LocalTopology`（P2P）+ `CentralizedTopology`（stub） | `multiagent/topology.rs` | ✅ |
| `WorkspaceRegistry` + `Workspace` + `WorkspaceId` | `multiagent/workspace.rs` | ✅ |
| `LoopRunner::with_event_sender`（emit `LoopEvent`） | `loop_runner/runner.rs` | ✅ |
| CLI `lim loop-daemon start/submit/list/status/pause/resume/sync/shutdown` 子命令 | `apps/cli/src/main.rs` | ✅ |
| `StateLedger` 改为 `Clone` 以共享给 loop-control tools | `loop_runner/state.rs` | ✅ |

### 已知问题（待后续 review/迭代处理）

#### 1. daemon 无实际 IPC 传输层

`DaemonServer` 只支持 in-process mpsc channel，没有 Unix socket / named pipe / TCP 传输。`lim loop-daemon start` 启动的 server 与其他 `loop-daemon <action>` 子命令不通信——后者直接操作 `DaemonLedger` 存储。

**影响**：`start` 子命令目前仅用于演示/集成测试；生产场景应直接用其他子命令操作同一存储。

**后续**：P4 增加 Unix socket 传输，`start` 派生 server 进程，其他子命令通过 socket 发送 `DaemonCommand`。

#### 2. daemon `Submit` 不 spawn LoopRunner

`DaemonServer::handle_command(Submit)` 只记录 goal 到 ledger，不构造 `AgentFactory`/`Verifier`/`Budget` 来 spawn `LoopRunner`。实际执行需要用户 `sync` 后到 workspace 跑 `lim loop --resume <ID>`。

**影响**：daemon 模式无法"提交即执行"，目前只是"提交即登记"。

**后续**：daemon 持有 `AgentFactory` + `Verifier` 工厂，submit 后 spawn 后台 task 跑 `LoopRunner`。

#### 3. `DaemonAction::Shutdown` 未实现

CLI `lim loop-daemon shutdown` 只打印提示，不发送 `Shutdown` 命令（因为没有 IPC 通道）。

**后续**：随 IPC 传输层一起实现。

#### 4. `WorkspaceRegistry` 未接入 daemon 主流程

`WorkspaceRegistry` 已实现 register/get/list/unregister，但 `DaemonServer` 没有持有 `WorkspaceRegistry`——daemon 只用 `DaemonLedger.workspace_binding` 单 goal 维度的绑定元数据。

**影响**：daemon 不能跨 goal 聚合查看"某 workspace 下的所有 goal"，需要遍历 `list_goals` 再逐个查 binding。

**后续**：`DaemonServer` 持有 `Arc<WorkspaceRegistry>`，`Submit` 时自动 register workspace，新增 `DaemonCommand::ListByWorkspace`。

#### 5. `CentralizedTopology` 是 pass-through stub

`CentralizedTopology` 内部仍用 `RoutingHub` 直接转发，没有 coordinator 策略（过滤、限流、优先级、持久化日志）。

**影响**：trait 抽象已验证（P2P + 中心化两种形态都能实现 `NetworkTopology`），但没有实际中心化调度能力。

**后续**：替换为带策略的 coordinator；或在 P4 用真正的远程调度器实现。

#### 6. `register_loop_control_tools` 只注册到 CLI，未注册到 TUI

`lim loop` CLI 已注册 4 个 loop-control 工具；TUI 模式（`lim tui`）的 agent 会话还没注册它们。TUI `/loop` slash 命令工作正常，但 agent 自身不能在 TUI 会话里调用 `loop_list_goals` 等工具。

**后续**：在 TUI 启动时构造 `Arc<StateLedger>` 并调用 `register_loop_control_tools`。

#### 7. `LoopEvent` 未写入 `.lim/loop/<goal_id>/events.jsonl`

`LoopRunner::with_event_sender` 只把事件发到 broadcast channel。CLI `build_loop_runner` 还没创建订阅者把事件落盘。

**影响**：事件只在内存中存在（如果没人订阅就直接丢弃）；无法事后回放。

**后续**：CLI/TUI 在构造 `LoopRunner` 时 spawn 一个订阅 task，把 `LoopEvent` 序列化为 JSONL 追加到 `.lim/loop/<goal_id>/events.jsonl`。

#### 8. `progress.rs` 的 `NoProgressDetector` 非线程安全跨 session

`NoProgressDetector` 内部状态（连续失败计数）不是 `Mutex`/`AtomicUsize` 保护，靠 `&self` 借用串行调用。当前 `LoopRunner` 单线程跑 session 不会有问题，但如果未来 daemon 并发跑同一 goal 的多个 session 会数据竞争。

**后续**：要么文档化"同 goal 不并发"约束，要么改成 `AtomicUsize`。

#### 9. `TrustLevel` 加 `Serialize/Deserialize` 是为了 `Workspace`

P3 给 `lim_core::TrustLevel` 加了 `serde::Serialize/Deserialize` derive，纯粹是为了让 `Workspace` struct 可序列化。这是跨层耦合的让步（primitive 层本应零运行时依赖，但 serde 不算运行时依赖，可接受）。

**后续**：如果 `Workspace` 改用字符串存储 trust level（或独立 enum），可撤销 primitive 层的 derive。

#### 10. `loop_control` 工具的 pause/resume 只改 ledger 状态

`loop_pause_goal` / `loop_resume_goal` 只 flip `LoopState.status`，不能中断正在运行的 session（session 由 `AgentLoop::run` 持有，工具在 session 内执行，无法 cancel 自己）。

**影响**：pause 是"下一次 session 之前人工检查"的信号，不是即时停止。

**后续**：如果需要即时停止，需要 `LoopRunner` 持有 `CancellationToken` 并在每轮 session 之间检查 ledger 状态。

### 验证状态

- `cargo test --workspace`：357 passed / 0 failed / 6 ignored
- `cargo clippy --all-targets --workspace -- -D warnings`：clean
- `cargo fmt --all --check`：clean
- lim-agent 单元测试：122 passed（含 1 个新增 `event_sender_emits_lifecycle_events`）
- 端到端冒烟测试：`lim loop-daemon submit` → `list` → `sync` → workspace `.lim/loop/` 可见 ✅
