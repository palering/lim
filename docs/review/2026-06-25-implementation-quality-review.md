# lim 项目实现质量审查报告

> 审查日期：2026-06-25
> 审查范围：`crates/` 全部 9 个 crate + `fixtures/` + `lim-cli` 端到端运行验证
> 审查方法：逐文件源码阅读 + `cargo build` / `cargo test` / 实际运行两条 CLI 命令

## 一、总体结论

**实现质量真实扎实，不是占位或敷衍的虚假实现。** 核心路径（agent loop、executor、workflow、session）都有真实业务逻辑和测试覆盖，关键机制（原子提交、进程组取消、DAG 并发、JSONL replay）落地到位。短板集中在 Phase 3+ 未启动的部分，以及少量工具层与 agent 层的断链问题。

## 二、关键证据

### 2.1 零占位代码

在 `crates/` 全量搜索 `todo!()` / `unimplemented!()` / `placeholder` / `FIXME` / `HACK` / `TODO`，**全部无匹配**。没有任何"待实现"标记，这是一个强信号。

### 2.2 构建与测试

- `cargo build --workspace`：✅ 通过
- `cargo test --workspace`：✅ 81 个测试全部通过

| Crate | 测试数 |
| :--- | :--- |
| lim-core | 18 |
| lim-llm | 8 |
| lim-agent | 4 |
| lim-executor | 6 |
| lim-session | 8 |
| lim-workflow | 22 |
| novel-pack | 15 |
| **总计** | **81** |

### 2.3 端到端运行验证

实际运行了两条 CLI 命令，均成功：

1. `lim-cli run --fake fixtures/basic-tool.json --message "请读取 hello.txt"`
   - tool_call 闭环跑通：user → assistant(tool_call: read) → tool_result → assistant(final)
   - replay 校验通过：transcript 消息数 == replay 消息数
   - 16 个事件写入 JSONL

2. `lim-cli chapter --bible ... --outline ... --responses ...`
   - 3 场景并行 ReviewLoop 执行
   - 生成 6 个 artifact（3 scene_draft + 3 review_report）
   - editor 节点合并输出完整章节文本，质量正常

## 三、各层实现质量详评

### 3.1 lim-core — 设计严谨

- **ID 类型**（[id.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-core/src/id.rs#L7-L51)）：宏生成 7 种 newtype（SessionId/BranchId/TurnId/MessageId/ToolCallId/ArtifactId/TraceId），全部基于 Ulid，实现 Display/FromStr/serde，类型隔离到位
- **ContentBlock**（[content.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-core/src/content.rs#L11-L39)）：tagged enum，5 种变体（Text/Thinking/ToolCall/Image/ArtifactRef），符合"最晚转换"设计
- **EventEnvelope**（[event.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-core/src/event.rs#L82-L138)）：用 `#[serde(flatten)]` + `extract_ids` 重计算，避免了 ID 字段与事件 payload 字段冲突——真实考虑过的细节
- **LimError**（[error.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-core/src/error.rs#L7-L43)）：thiserror 分层错误枚举，7 种具体变体 + anyhow::Error 兜底

### 3.2 lim-agent — 真实业务逻辑

[agent_loop.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-agent/src/agent_loop.rs#L119-L235) 不是空壳：

- 双层循环（外层 max_turns，内层 stream 消费）
- 流式累积 → MessageStart/Update/End 事件正确发射
- StopReason::ToolCall 分支触发工具执行，结果回灌
- Schema 校验失败时构造 error ToolResult 回灌给 LLM（[L291-295](file:///Users/pale/my_dev/agent/lim/crates/lim-agent/src/agent_loop.rs#L291-L295)）
- before/after hooks 真实接入（[L298-333](file:///Users/pale/my_dev/agent/lim/crates/lim-agent/src/agent_loop.rs#L298-L333)）
- 4 个单元测试覆盖简单文本、工具调用、schema 失败、未知工具

### 3.3 lim-executor — 事务性实现到位

[local.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-executor/src/local.rs#L51-L105) 的 `write_file_atomic` **真正实现了原子提交**：

1. 父目录自动创建
2. 写入 `.lim-tmp-<ulid>` 临时文件
3. `sync_all()` fsync
4. `tokio::fs::rename` 原子替换

`apply_edit` 同样走临时文件+fsync+rename，且做了**唯一性校验**（[L128-138](file:///Users/pale/my_dev/agent/lim/crates/lim-executor/src/local.rs#L128-L138)：count>1 拒绝执行）。

`run_command` 的取消语义真实落地（[L188-301](file:///Users/pale/my_dev/agent/lim/crates/lim-executor/src/local.rs#L188-L301)）：

- `pre_exec` 里 `setpgid(0,0)` 建进程组
- `tokio::select!` 监听 cancel
- 取消时 `killpg(SIGTERM)` → sleep 500ms → `kill()` 强杀
- stdout/stderr 并发读取避免管道死锁
- 中断结果回灌 `[Command interrupted by user]`

### 3.4 lim-workflow — DAG 真实调度

[graph.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-workflow/src/graph.rs#L66-L126) 的 `topological_order` 是教科书级 Kahn 算法实现，**分层并行 + 环检测**，6 个测试覆盖空图/单节点/链式/菱形/fan-out-fan-in。

[executor.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-workflow/src/executor.rs#L54-L126) 的 `execute` 用 `join_all` 真实并发执行同层节点，ReviewLoop（[L248-396](file:///Users/pale/my_dev/agent/lim/crates/lim-workflow/src/executor.rs#L248-L396)）实现了 writer→reviewer→retry 完整循环，JSON 解析有 fallback，artifact 落盘真实。

### 3.5 lim-session — replay 可验证

[writer.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-session/src/writer.rs) 每行 JSON + flush，[reader.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-session/src/reader.rs) 逐行解析 + `recompute_ids`，[replay.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-session/src/replay.rs) 从 MessageEnd 事件重建 transcript。CLI 运行时 assert replayed.len() == messages.len()，真实闭环。

## 四、发现的短板

### 4.1 工具层与 agent 层取消断链

[bash.rs:67](file:///Users/pale/my_dev/agent/lim/crates/lim-tools/src/bash.rs#L67) 中 `BashTool::execute` 内部 `let cancel = CancellationToken::new();` 自己 new 了一个 token，没有接入 agent loop 的 cancel 信号。这意味着用户取消时，agent loop 虽然会停止消费 stream，但正在执行的 bash 子进程不会被 SIGTERM。

**影响**：取消语义在工具层断链，bash 命令可能继续运行到自然结束。
**修复方向**：Tool trait 的 execute 签名需要接收 CancellationToken，或通过 ToolRegistry 注入。

### 4.2 tool_result 丢弃 is_error 标记

[message.rs:62](file:///Users/pale/my_dev/agent/lim/crates/lim-core/src/message.rs#L62)：

```rust
pub fn tool_result(tool_call_id: ToolCallId, output: impl Into<String>, _is_error: bool) -> Self {
```

`_is_error` 参数被下划线前缀忽略，错误标记没有进入消息层。ToolResult 自身有 is_error 字段，但 AgentMessage 层丢失了这个信息。

**影响**：UI 渲染和 replay 无法区分成功/失败的 tool result。
**修复方向**：在 AgentMessage 增加 `is_error: bool` 字段，或在 ContentBlock 增加 Error 变体。

### 4.3 Image 块未用 base64

[content.rs:84](file:///Users/pale/my_dev/agent/lim/crates/lim-core/src/content.rs#L84) 注释说 "can switch to base64 crate later"，当前用 `serialize_bytes` 序列化 raw bytes。JSON 中会出现字节数组而非 base64 字符串，与主流 LLM API 不兼容。

**影响**：Image 内容块无法直接发送给 OpenAI/Anthropic API。
**修复方向**：引入 `base64` crate，实现真正的 base64 编解码。

### 4.4 orchestrator 简化为直接切片

[orchestrator.rs:7-20](file:///Users/pale/my_dev/agent/lim/crates/novel-pack/src/orchestrator.rs#L7-L20) 的 `split_scenes` 直接从 outline 取 scenes，没有 agent 拆解。代码注释明确标注了 "Phase 2 simplification"，属于诚实的范围收敛，不是敷衍。

### 4.5 未实现的部分（与路线图一致）

- **无真实 LLM provider**：只有 MockProvider，reqwest/eventsource 依赖未引入
- **ContextWindowManager**：未实现，长会话压缩不可用
- **lim-tui**：crate 未创建
- **L1/L2 强隔离**：ContainerExecutor / RemoteExecutor 未实现
- **扩展系统**：JSON-RPC / MCP / Wasm 未实现

这些与 second-draft.md 中 Phase 3+ 路线图一致，不属于虚假实现。

## 五、改进建议优先级

| 优先级 | 项目 | 理由 |
| :--- | :--- | :--- |
| P0 | 修复 bash 工具取消断链 | 取消语义是核心契约，当前实现有安全隐患 |
| P0 | 修复 tool_result is_error 丢失 | 影响 replay 和 UI 正确性 |
| P1 | Image 块改用 base64 | 阻塞多模态支持 |
| P1 | 引入真实 LLM provider（OpenAI/Anthropic） | 当前只能跑 Mock，无法验证真实流式 |
| P2 | ContextWindowManager | 长会话必需，Phase 3 入口 |
| P2 | lim-tui | 交互式使用必需 |
| P3 | L1 沙箱（bwrap/sandbox-exec） | 强隔离，Phase 4 |
| P3 | MCP/JSON-RPC 扩展 | 生态接入，Phase 4 |

## 六、架构分层问题：novel-pack 不应平级放在 crates/

### 6.1 问题定位

当前 workspace 结构（[Cargo.toml](file:///Users/pale/my_dev/agent/lim/Cargo.toml#L3-L13)）：

```
crates/
  lim-core       ← 基础层（runtime 基础设施）
  lim-llm        ← 基础层
  lim-executor   ← 基础层
  lim-agent      ← 核心层（runtime 基础设施）
  lim-tools      ← 核心层
  lim-session    ← 核心层
  lim-workflow   ← 编排层（通用 runtime 能力）
  novel-pack     ← 领域应用层（消费者，不是基础设施）
  lim-cli        ← 入口层
```

**核心矛盾**：novel-pack 是 lim runtime 的**消费者**，不是 runtime 本身。它依赖 lim-workflow + lim-agent + lim-core + lim-llm（[novel-pack/Cargo.toml](file:///Users/pale/my_dev/agent/lim/crates/novel-pack/Cargo.toml)），属于"用 lim 搭出来的应用"。

### 6.2 设计文档原本的定位

[second-draft.md](file:///Users/pale/my_dev/agent/lim/docs/draft/second-draft.md#L213-L238) 第 6.2 节明确写了：

> 小说或剧本应用应该作为 **domain pack 注入**：`novel-pack` ... 核心原则：Writer/Reviewer 只是 workflow 节点配置，**不是 core 类型**。

[second-draft.md](file:///Users/pale/my_dev/agent/lim/docs/draft/second-draft.md#L42-L44) 的 crate 表格里，`lim-workflow` 是通用编排层，而 domain pack 根本不在那张表里——它本就不属于 runtime crate 集合。

### 6.3 平级放在 crates/ 的实际危害

1. **边界模糊**：未来加 screenplay-pack / code-pack / research-pack，crates/ 会膨胀且无法一眼看出谁是 runtime、谁是应用
2. **依赖方向混乱**：novel-pack 依赖 lim-workflow，但 lim-cli 又依赖 novel-pack（[main.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-cli/src/main.rs) 隐式通过 `novel_pack::` 调用），等于入口层直接耦合了一个领域应用
3. **发布语义错位**：lim-core 等是 library crate，novel-pack 也是 library crate，但前者是"给别人用来造 agent 的"，后者是"用 lim 造出来的成品"——两者不应共享同一发布层级
4. **阻碍 xtask 依赖校验**：first-draft.md 第 10.1 节提到要用 xtask 校验依赖方向，如果应用 pack 和 runtime crate 混在一起，校验规则会很难写

### 6.4 建议的目录结构

```
lim/
  crates/          ← runtime 基础设施（lim-*）
    lim-core/
    lim-llm/
    lim-agent/
    lim-executor/
    lim-tools/
    lim-session/
    lim-workflow/
    lim-cli/       ← 或移到根 bin/
  packs/           ← 领域应用包（domain packs）
    novel/
      Cargo.toml
      src/
    # 未来: screenplay/  code/  research/
  fixtures/
  docs/
```

这样：

- `crates/lim-*` 是稳定的 runtime 层，对外发布
- `packs/<name>/` 是基于 runtime 搭的领域应用，可独立演进
- `lim-cli` 作为 runtime 入口，**不应**直接依赖任何 pack；pack 应作为插件/示例被加载（符合 second-draft.md L46 "lim-runtime 负责 skill/domain pack 加载"的定位）

### 6.5 连带需要决策的问题

当前 [lim-cli/src/main.rs](file:///Users/pale/my_dev/agent/lim/crates/lim-cli/src/main.rs#L44-L61) 的 `Chapter` 子命令直接硬编码调用 `novel_pack::build_chapter_workflow`。如果 novel-pack 移出 crates/，这个子命令有三种处理方案：

| 方案 | 做法 | 优点 | 缺点 |
| :--- | :--- | :--- | :--- |
| A | 移到 `packs/novel/` 自己的 binary（`novel-cli`），lim-cli 只保留通用 `run` | 边界最清晰，pack 自带入口 | 用户需安装两个 binary |
| B | lim-cli 通过 feature flag 可选依赖 novel-pack（`--features novel`） | 单 binary 保留 | 引入 feature 复杂度，仍耦合 |
| C | lim-cli 不碰 pack，pack 通过 `lim-runtime` 的 pack loader 动态注册子命令 | 符合远期设计 | 当前没有 lim-runtime，需先实现加载机制 |

**推荐**：短期方案 A（pack 自带 binary），长期演进到方案 C。方案 B 是过渡形态，不建议作为目标。

## 七、结论

lim 项目在 Phase 1-2 范围内是一份**诚实标注范围、核心路径真实可跑**的实现。事务性文件操作、进程组取消、DAG 并发调度、review loop、JSONL replay 这些关键机制都有真实代码和测试支撑，没有 placeholder。

下一步建议优先级：

1. **P0 修复**：bash 取消断链 + tool_result is_error 丢失
2. **P0 架构调整**：novel-pack 移出 crates/ 到 packs/，lim-cli 解耦
3. **P1 启动 Phase 3**：ContextWindowManager + 真实 LLM provider + lim-tui
4. **P2 Phase 4**：L1 沙箱 + MCP/JSON-RPC 扩展
