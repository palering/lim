# lim TODO

> 单一来源汇总所有未完成工作。完成一项就划掉一项；全部清空后删除本文件，避免散落在各文档中需要全局检索。
>
> 维护规则：新增待办追加到对应分类下；完成后标记 ✅ 并保留一周（便于回看），再统一删除。

## v0.1 Release 阻塞项

- [ ] **历史压缩**：打 `v0.0.1-pre-squash` tag 保留旧历史 → 按逻辑阶段 squash 成 5-6 个 commit → 同步更新文档里的 hash 引用。详见 [DEVELOPMENT.md](DEVELOPMENT.md) "待办：v0.1 release 前的历史压缩"
- [x] **版本元数据嵌入** ✅ 已完成（2026-07-03）：`crates/lim-core/build.rs` 运行 `git describe --tags --always --dirty=-dirty`，通过 `cargo:rustc-env=LIM_BUILD_VERSION` 嵌入。`LIM_VERSION` 优先用 git 描述，fallback 到 `CARGO_PKG_VERSION`（无 .git 的 tarball 构建场景）。`lim --version` 和 `SessionHeader.lim_version` 都用此常量，输出如 `lim v0.1.0-3-gabcdef-dirty`。rerun-if-changed 覆盖 `.git/HEAD` / `.git/refs/tags` / `.git/packed-refs`。
- [ ] **musl vs gnu**：是否需要静态链接 Linux 构建（portable single-binary）。详见 [packaging-plan.md](packaging-plan.md)
- [ ] **代码签名**：macOS notarization + Windows authenticode（Phase B 规划）
- [ ] **novel-cli 打包策略**：确认为扩展而非独立 CLI 包，需决定是否随主 release 分发

## 文档同步（低优先级）

- [x] [loop-engineering.md](architecture/loop-engineering.md) §12.7 表格中 Skill 系统（P4.7）和 Time-based loop（P4.8）仍标为未完成，但 evolution.md 已 ✅，需对齐 ✅ 已完成（2026-07-03）：§12.7 表格 P4.7/P4.8/P4.10 标记为 ✅，关键结论更新为"P3.5 + P2.5 + P4.7/P4.8/P4.9/P4.10 全部完成"
- [x] [loop-engineering.md](architecture/loop-engineering.md) §13 已知问题第 2 项（Submit 不 spawn LoopRunner）已被 P4.9 `GoalRunner` 部分解决，需更新状态 ✅ 已完成（2026-07-03）：第 2 项标题加"（部分解决）"，补充 P4.9 GoalRunner 抽象层说明 + 当前状态 + 后续计划

## P4 增强待定项（机制层已完成，产品化细节）

- [ ] **P4.8 Timer**：cron 库选择（`cron` crate vs 自解析）、daemon 生命周期（常驻 vs `--max-runs`）
- [ ] **P4.9 IPC**：Unix socket 路径（`~/.cache/lim/daemon.sock` vs XDG_RUNTIME_DIR）、`attach` 实时 event 流
- [ ] **P4.10 Backoff**：`ExponentialBackoff` + jitter 实现、与 Budget `min_interval` 字段的整合

## loop-engineering.md §13 已知问题（P3 遗留）

- [ ] **1. daemon 无实际 IPC 传输层**：只有 in-process mpsc channel，没有 Unix socket / named pipe / TCP。`lim loop-daemon start` 启动的 server 与其他子命令不通信
- [ ] **2. `DaemonAction::Shutdown` 未实现**：只打印提示，不发送 Shutdown 命令（随 IPC 传输层一起实现）
- [ ] **3. `WorkspaceRegistry` 未接入 daemon 主流程**：daemon 不能跨 goal 聚合查看"某 workspace 下的所有 goal"
- [ ] **4. `CentralizedTopology` 是 pass-through stub**：没有 coordinator 策略（过滤、限流、优先级、持久化日志）

## P4+ Proactive Loop（远期）

- [ ] Proactive routine 组合（schedule + goal + 动态工作流 + auto mode）
- [ ] 中心化调度策略实现（CentralizedTopology 实体化）
- [ ] daemon 跨 workspace goal 聚合查询

## 架构演进候选（受 Pi 文章启发，待评估）

> 来源：[Pi 源码分析](https://mp.weixin.qq.com/s/yk3sWh3dwX-wRRzDbqluQA)。这里只登记值得评估的方向，是否实施需要单独讨论。

- [ ] **评估：双层可干预 AgentLoop**。Pi 的 `steering message`（运行中插入方向调整）+ `follow-up message`（停下时继续追加任务）放进 core loop，而不是 UI 层。lim 当前 AgentLoop 是否需要这种"运行中可干预"能力？
- [x] **Session Tree Phase 1（JSONL 消息树）** ✅ 已完成（2026-07-04）：`AgentMessage` 新增 `parent_id: Option<MessageId>` 字段（`#[serde(default, skip_serializing_if = "Option::is_none")]`），4 个构造器默认 None，新增 `with_parent()` builder。Phase 1 是纯数据层"留种"——字段种下但所有 production 调用点保持 None，线性 Vec 容器不变，LLM 投影不变，不新增 AgentEvent，不 bump SCHEMA_VERSION。旧 JSONL 字节级兼容。Phase 2 将激活 fork/rewind 语义 + BranchCreated/Switched 事件。
- [x] **Compaction Phase 1（结构化摘要 + 文件线索）** ✅ 已完成（2026-07-04）：`SummarizeStrategy` 现在产出 `StructuredSummary`（9 个 LLM 填充字段：goal/constraints/done/in_progress/blocked/key_decisions/next_steps/critical_context + 3 个运行时填充字段：read_files/modified_files/turn_prefix_summary）。LLM prompt 改为 cache-safe `const SUMMARY_INSTRUCTION`（JSON schema 指令），输出经三级 fallback 解析（直接 parse → ```json``` 代码块 → 大括号匹配）。文件级线索由 `extract_file_hints()` 从 `read`/`write`/`edit` 工具调用参数本地确定性提取（不依赖 LLM、不解析 shell）。`CompactionResult` 新增 `summary: Option<StructuredSummary>` + `trigger_reason: CompactionTriggerReason` 字段（AutoThreshold/Manual/OverflowRecovery，Phase 2 用）。解析失败 fallback 到自由文本摘要（保留旧行为）。Phase 2 将实现 Overflow Recovery（`OverflowRecovery` trigger + `turn_prefix_summary` for split turns）。
- [ ] **评估：Overflow Recovery 与阈值压缩分离**。Pi 区分"阈值压缩（预防）"和"overflow recovery（失败后恢复）"，后者用 `_overflowRecoveryAttempted` 防止死循环。lim 当前只有阈值压缩
- [ ] **评估：Extension Hook 层**。Pi 的 extension 能插进 `context`/`before_provider_request`/`before_provider_payload`/`before_agent_start`/`tool_call`/`tool_result`/`session_before_compact`。lim 的 `AgentHooks` 是否需要扩展 hook 面？
- [ ] **评估：工具并行执行 + 结果按源序写回**。Pi 并行执行工具但 tool result messages 按 assistant 原始顺序写回模型。lim 当前工具执行模式如何？
- [ ] **评估：Skills 索引延迟加载**。Pi 启动时只放 skill 索引（name/description/location）进 system prompt，命中任务时模型用 `read` 工具加载完整内容（progressive disclosure）。lim 的 Skill 系统是否需要这种机制？
- [x] **ResourceLoader 沿 cwd 向上查找 AGENTS.md** ✅ 已完成（2026-07-03）：实现了可配置的上下文加载系统。新增 `[context]` 配置段（`ContextFileConfig` / `ContextFileEntry` / `ContextSearch`），支持 4 种查找策略（workspace_root / walk_up / subdirs / fixed）、多文件配置、自定义 label。无硬编码文件名——`AGENTS.md` 只是默认值，用户可完全覆盖。接入 `PromptBuilder.with_project_context`，cache breakpoint 在 Project tier 生效。
