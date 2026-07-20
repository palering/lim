# lim TODO

> 单一来源汇总所有未完成工作。完成一项就划掉一项；全部清空后删除本文件，避免散落在各文档中需要全局检索。
>
> 维护规则：新增待办追加到对应分类下；完成后标记 ✅ 并保留一周（便于回看），再统一删除。

## v0.1 Release 阻塞项

### A. 代码与功能

- [x] **功能测试通过** ✅ 核心路径已验证（2026-07-09）：3 个 provider（openai-chat / openai-responses / anthropic）均正常响应；MockProvider fixture 确定性回放；工具调用（read）；session resume（JSONL 回放 + 上下文延续）；trust level（workspace 阻止 /etc/ 写入）；`lim goal` submit/list；`lim serve` foreground + SIGTERM shutdown + observability 日志；`lim sessions --details` + `lim edit-context --list`。**未覆盖**（需交互/复杂设置，非 v0.1 阻塞）：TUI、Loop Engineering 全流程、Compaction、Steering、错误处理边界 case。
- [ ] **历史压缩**：打 `v0.0.1-pre-squash` tag 保留旧历史 → 按逻辑阶段 squash 成 5-6 个 commit → 同步更新文档里的 hash 引用。详见 [DEVELOPMENT.md](DEVELOPMENT.md) "待办：v0.1 release 前的历史压缩"
- [x] **版本元数据嵌入** ✅ 已完成（2026-07-03）：`crates/lim-core/build.rs` 运行 `git describe --tags --always --dirty=-dirty`，通过 `cargo:rustc-env=LIM_BUILD_VERSION` 嵌入。`LIM_VERSION` 优先用 git 描述，fallback 到 `CARGO_PKG_VERSION`（无 .git 的 tarball 构建场景）。`lim --version` 和 `SessionHeader.lim_version` 都用此常量，输出如 `lim v0.1.0-3-gabcdef-dirty`。rerun-if-changed 覆盖 `.git/HEAD` / `.git/refs/tags` / `.git/packed-refs`。
- [x] **Session Tree Phase 1** ✅ 已完成（2026-07-04）：纯留种，`parent_id` 字段就位，向后兼容
- [x] **Compaction Phase 1** ✅ 已完成（2026-07-04）：结构化摘要 + 文件线索 + cache-safe prompt
- [x] **novel-cli 打包策略** ✅ 已确认（2026-07-09）：release.yml 已将 `novel-cli` 随主 release 一起打包（Unix tar.gz + Windows zip 均包含），作为扩展而非独立 CLI 包分发

### B. 构建与分发

- [ ] **musl vs gnu 决策**：是否需要静态链接 Linux 构建（portable single-binary）。详见 [packaging-plan.md](packaging-plan.md)
- [x] **Release profile 调优** ✅ 已确认（2026-07-09）：`[profile.release]` 已配置 `lto = "fat"` + `codegen-units = 1` + `strip = "symbols"` + `panic = "abort"`
- [x] **CI matrix build** ✅ 已配置（2026-07-09）：`release.yml` tag push 触发 6 平台构建（Linux x86_64/aarch64 gnu、macOS x86_64/aarch64、Windows x86_64/aarch64 msvc）
- [x] **Archive layout** ✅ 已配置（2026-07-09）：每平台产出 `lim-<version>-<target>.tar.gz` / `.zip`，包含 lim + novel-cli 二进制
- [ ] **代码签名**：macOS notarization + Windows authenticode（Phase B 规划，v0.1 可选）

### C. 文档与发布说明

- [x] **README 完善** ✅ 已更新（2026-07-09）：修正配置示例格式（`[providers.<name>]` 表格式）+ 添加 lim-transport/lim-serve crate 说明 + 添加 lim serve/lim goal 命令示例
- [ ] **Release notes**：v0.1.0 changelog（特性清单、已知限制、升级指南）— release.yml 使用 `generate_release_notes: true` 自动生成
- [ ] **CHANGELOG.md**：建立变更日志文件（如尚无）
- [ ] **文档审查**：确保 [INDEX.md](INDEX.md) 列出的所有文档与当前代码状态一致
- [ ] **AGENTS.md / docs 一致性**：项目约定文档反映当前实际

### D. 仓库治理

- [x] **License 确认** ✅ 已完成（2026-07-09）：所有 14 个成员 crate 统一继承 `license.workspace = true`（MIT）+ `repository.workspace = true` + `rust-version.workspace = true` + `authors.workspace = true`
- [x] **.gitignore 审查** ✅ 已确认（2026-07-09）：`/target` / `Cargo.lock` / `.DS_Store` / `/sessions/` / `/.lim/` 均已忽略。**注意**：Cargo.lock 当前被 gitignore，二进制项目通常应提交 Cargo.lock 以确保可复现构建 — 待用户决策
- [x] **Dependabot 配置** ✅ 已确认（2026-07-09）：`.github/dependabot.yml` 配置 github-actions + cargo 双生态周更，minor/patch 分组合并 PR
- [x] **CI 工作流** ✅ 已确认（2026-07-09）：`ci.yml` fmt + clippy (-D warnings) + build + test 全流程，ubuntu-latest 运行
- [ ] **Tag 规范**：建立 `v0.1.0` tag，遵循 SemVer

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

## 待讨论项（zerostack 调研吸收）

> 来源：[docs/research/zerostack.md](research/zerostack.md)。这些是值得吸收的设计点，是否实施需要单独讨论。优先级与阶段是建议，不是承诺。

### v0.1 发布前评估

- [ ] **评估：mimalloc + release profile 调优**。zerostack 用 mimalloc 全局 allocator + `opt-level="z"` + `lto="thin"` + `codegen-units=1` + `strip=true`，binary 12.9MB。lim 的 release profile 已有 LTO/strip，但没用 mimalloc/compact_str/smallvec。**建议**：v0.1 发布前评估加 mimalloc（一行代码改动）和 `opt-level="z"`，测量 binary size 与编译时间影响。
- [ ] **评估：客户端身份声明机制**（已部分实现 2026-07-04）。三个 provider 默认 `User-Agent: lim/<version>`，用户可通过 `extra_headers` 覆盖。修复了 Anthropic 分支不透传 `extra_headers` 的 bug。**待讨论**：是否需要提升为一等字段 `client_identity`，或加知名工具预设档案库？详见 [integration-guide.md "客户端身份声明"](integration-guide.md#7-客户端身份声明与代理供应商兼容)。

### v0.2 候选

- [ ] **讨论：Prompts 模式系统**。zerostack 内置 10 个 prompt 模式（code/plan/review/debug/ask/brainstorm/...），运行时 `/prompt <name>` 切换。lim 已有 `AgentRoleSpec` + `.lim/agents/<role>.toml` 机制，**缺**：预置模板 + TUI `/mode` 命令。**建议**：在 `crates/lim-config/src/builtins/` 下提供预置 role 模板，TUI 加 `/mode <name>` 命令切换。
- [ ] **讨论：Tool call 循环检测（doom-loop）**。zerostack 在 permission checker 里追踪相同 tool call 重复 3+ 次触发警告/拒绝。lim 的 `NoProgressDetector` 是 goal 级别，**没有** tool call 级别循环检测。**建议**：加 `ToolCallLoopDetector` hook，作为 `AgentHooks::before_tool_decision` 的实现，重复 N 次相同工具调用时返回 `Deny`。
- [ ] **讨论：`lim loop --quick` 极简模式**。zerostack 的 `--loop-prompt + --loop-run` 极易上手。lim 的 `LoopRunner` 完整但门槛高——需要写 goal recipe、选 verifier。**建议**：CLI 加 `lim loop --quick --prompt "..." --run "cargo test" --max 10`，内部用 `AlwaysPassVerifier` + `CommandVerifier(--run)` 包装。
- [ ] **讨论：Feature gates**。lim 目前所有功能默认编译。L1 集成用户（只要单次 LLM 调用）不需要 `lim-workflow`/`multiagent`/`loop_runner`。**建议**：在 `apps/cli/Cargo.toml` 加 feature gate（`default = ["full"]`，`minimal = []`），让 L1 集成用户 `cargo install --no-default-features --features minimal lim-cli`。
- [ ] **讨论：`--quick-model` 预设**。zerostack 配置文件定义"快速模型"组合，一键切换 provider+model+context_window。lim 目前 `--provider` + `--model` 分开指定。**建议**：加 `[quick_models]` 配置段，`--quick-model work` 切便宜模型、`--quick-model heavy` 切强模型。

### v0.3+ 候选

- [ ] **讨论：A2A 协议支持**。Google A2A（Agent-to-Agent）协议是跨 agent 通信标准。lim 当前只有自研 `AgentTransport` trait + `LocalChannelTransport`（in-process mpsc），**没有**标准 A2A 协议实现。**建议**：v0.3 与 ACP 一并规划，作为 lim 的"对外通信协议层"。详见 [evolution.md "A2A/ACP 协议层"](architecture/evolution.md#七a2a-acp-协议层)。
- [ ] **讨论：ACP（Agent Communication Protocol）支持**。让编辑器（Zed/VSCode）把 lim 当 agent 后端。`--acp` 启动 JSON-RPC server。**建议**：与 A2A 同期规划，两者处于相似的"对外通信"层级。

## v0.2 交付清单（对外通信协议层 + serve 统一入口）

> 设计详件：[transport-layer.md](architecture/transport-layer.md) + [serve-mode.md](architecture/serve-mode.md)
>
> 一次交付完整对外通信能力，不拆分到多个版本。

### E. lim-transport crate（T1+ 传输 + 协议适配 + 服务发现）

- [x] **E1 跨平台 T1 传输** ✅ 已完成（2026-07-09）：`LocalEndpoint` 抽象 + `UnixStreamTransport` / `TcpStreamTransport` / `NamedPipeClientTransport` + 对应 Server。Linux/macOS Unix socket + TCP loopback 降级，Windows Named Pipe 骨架 + TCP loopback 降级。endpoint URL scheme `auto` / `unix://` / `pipe://` / `tcp://` 全部支持。
- [ ] **E2 T2 TCP 传输**：`TcpTransport` + 心跳 + 断线重连（基础 `TcpStreamTransport` 已实现，心跳/重连待加）
- [x] **E3 T3 A2A HTTP 传输** ✅ 已完成（2026-07-09）：`A2AHttpTransport`（reqwest client）+ `AgentCard` / `A2ATask` / `TaskState` / `Artifact` / `A2AMessage` / `A2APart` 类型 + `A2AAdapter`（impl `ProtocolAdapter`，AgentMessage ↔ A2A Message 互转，Thinking 块跳过，Image 走 base64 FilePart）+ `A2ARequestHandler` trait（server 侧由 lim-serve 实现）+ send_task/get_task/cancel_task/fetch_agent_card/send_message/recv_message API。11 个测试（含 3 个 TCP mock HTTP 集成测试）
- [x] **E4 协议适配层** ✅ 已完成（2026-07-09）：`ProtocolAdapter` trait + `LimNativeAdapter`（serde_json）+ `A2AAdapter`（A2A JSON-RPC）
- [x] **E5 MCP Server 模式** ✅ 已完成（2026-07-09）：`McpServer<H>` 泛型 server + `McpToolHandler` trait（app 层注入具体实现，lim-transport 不依赖 DaemonCommand）+ `StaticToolHandler`（测试用）+ stdio JSON-RPC 2.0 循环（initialize / tools/list / tools/call / ping / shutdown / exit）+ `serve_with_io` 支持任意 AsyncRead/AsyncWrite（测试用 duplex pipe）。8 个测试
- [x] **E6 MCP Client 模式** ✅ 已完成（2026-07-09）：`McpClient`（spawn 子进程 + JSON-RPC 通信管线）+ `McpTool`（impl lim `Tool` trait，category=`Remote`）+ `McpToolProxy`（管理连接 + 批量注册到 ToolRegistry）。2 个集成测试（bash 脚本模拟 MCP server，Unix only）
- [x] **E7 服务发现** ✅ 已完成（2026-07-09）：`ServiceDiscovery` trait + `AgentAddr` enum + `LocalRegistry`（T1 daemon 注册表）。mDNS（T2）+ A2A registry（T3）待后续
- [x] **E8 拓扑自动选择** ✅ 已完成（2026-07-09）：`TopologySelector` + `TopologyConfig` + `ResolvedTopology` + endpoint 自动推断逻辑（无 endpoint→T0, .sock→T1, tcp loopback→T1, tcp remote→T2, http→T3, mcp→T1）
- [x] **E9 配置简化** ✅ 已完成（2026-07-09）：三级配置（零配置 / `from_endpoint()` / 完整显式）+ endpoint 自动推断 + 鉴权 token 传播
- [ ] **E10 跨平台 CI**：三平台（ubuntu/macos/windows）传输层测试矩阵
- [x] **E11 平台条件依赖** ✅ 已完成（2026-07-09）：`[target.'cfg(unix)'.dependencies]` libc + `[target.'cfg(windows)'.dependencies]` windows crate

### F. lim-serve crate（serve 统一入口 + 跨平台运行模式）

- [x] **F1 ServeRunner 核心** ✅ 已完成（2026-07-09）：`ServeOptions` + `ServeRunner::run()` + RunMode 分发（foreground/daemon/system）
- [x] **F2 foreground 模式** ✅ 已完成（2026-07-09）：前台运行 + Ctrl+C + SIGTERM + CancellationToken 三路 shutdown select
- [x] **F3 --daemon 模式** ✅ 已完成（2026-07-09）：spawn detached 子进程（避免 tokio fork 安全问题）+ PID 文件管理（`~/.lim/lim.pid`）+ `stop_daemon` + `is_process_alive`
- [x] **F4 --system 模式** ✅ 已完成（2026-07-09）：`ServiceManager` trait + systemd 用户级 unit 生成 + launchd LaunchAgent plist 生成 + Windows stub
  - Linux: systemd 用户级（`~/.config/systemd/user/lim.service`）✅
  - macOS: launchd LaunchAgent（`~/Library/LaunchAgents/ai.lim.serve.plist`）✅
  - Windows: stub（返回错误，待后续实现）⏳
  - install / uninstall / start / stop / status / restart ✅
- [x] **F5 MCP Server 集成** ✅ 已完成（2026-07-09）：ServeRunner 通过 `with_mcp_handler` 注入 `Arc<dyn McpToolHandler>`，foreground 模式 spawn MCP server task（lim-transport 的 McpServer + Arc blanket impl）
- [x] **F6 A2A Agent 集成** ✅ 已完成（2026-07-09）：`A2AServer`（最小 HTTP/1.1 实现）+ `with_a2a_handler` 注入 + GET agent-card + POST JSON-RPC（tasks/send/get/cancel）+ Bearer auth
- [x] **F7 --goal-daemon 桥接** ✅ 已完成（2026-07-09）：`LoopRunnerGoalRunner` 适配 `GoalRunner` trait + `spawn_daemon_server` 在 foreground 模式启动 + `LoopRunner::run_existing_goal` daemon-friendly 入口
- [x] **F8 聚合模式** ✅ 已完成（2026-07-09）：MCP + A2A + goal-daemon 可在同一进程通过多个 spawned tasks 并行运行
- [x] **F9 配置段** ✅ 已完成（2026-07-09）：`[serve]` / `[serve.goal_daemon]` / `[serve.system]` schema + CLI 集成（config 默认值，flag 覆盖）+ serve-mode.md §8.1 文档对齐

### G. CLI 命令归并

- [x] **G1 `lim serve` 命令** ✅ 已完成（2026-07-09）：clap 子命令 + `--protocol` / `--daemon` / `--system` / `--workspace` / `--config` / `--endpoint` / `--auth-token` 参数 + run_serve dispatch（goal daemon 为常驻能力，无独立 flag，由 `[serve.goal_daemon] enabled` 配置控制）
- [x] **G2 `loop-daemon start` deprecate** ✅ 已完成（2026-07-09）：打印 deprecation 警告指向 `lim serve`（goal daemon 为常驻能力）
- [x] **G3 `lim goal` 子命令** ✅ 已完成（2026-07-09）：`submit/list/status/pause/resume/sync` 作为首选别名，`loop-daemon <action>` 保留为向后兼容别名
- [x] **G4 帮助文档** ✅ 已完成（2026-07-09）：`lim serve --help` + `lim goal --help` 完整说明 + 典型用例（clap 从 doc comments 生成）

### H. 文档与测试

- [ ] **H1 transport-layer.md 完成度审查**：确保设计文档与实现一致
- [ ] **H2 serve-mode.md 完成度审查**：确保设计文档与实现一致
- [ ] **H3 modules.md 更新**：加入 lim-transport 和 lim-serve crate 的模块清单
- [ ] **H4 integration-guide.md 更新**：加入 serve 模式的集成示例
- [ ] **H5 功能测试**：serve 模式全流程测试（三平台 × 三模式 × 多协议组合）
- [ ] **H6 行为可观测性**：serve 启动/停止/协议切换/连接事件记入日志和 JSONL

### v0.2 进度追踪

| 阶段 | 状态 | 备注 |
|---|---|---|
| 设计文档完成 | ✅ | transport-layer.md + serve-mode.md + evolution.md §七更新 |
| lim-transport crate 骨架 | ✅ | Cargo.toml + lib.rs + 9 个模块 + 平台条件依赖 |
| T1 跨平台传输实现 | ✅ | E1/E4/E7/E8/E9/E11 完成。Unix socket + TCP loopback 全功能，Named Pipe 骨架待 Windows CI |
| A2A 协议栈实现 | ✅ | E3 完成。A2AHttpTransport + A2AAdapter + AgentCard/Task 类型 + 11 个测试 |
| MCP 双向实现 | ✅ | E5 MCP Server（McpServer+McpToolHandler，8 测试）+ E6 MCP Client（McpClient+McpTool+McpToolProxy，2 测试） |
| serve 统一入口实现 | ✅ | F1-F9 全部完成（含 Handler 注入 + goal daemon 桥接 + `[serve]` 配置段） |
| 系统服务实现 | ✅ | F4 ServiceManager 三平台完整（Linux systemd + macOS launchd + Windows `sc.exe`） |
| CLI 命令归并 | ✅ | G1-G4 完成（lim serve + lim goal + deprecation + help） |
| 三平台 CI 测试 | ⏳ 待开始 | E10 + H5 |
| 文档与发布说明 | 🔄 进行中 | H1-H2 审查完成，H3 modules.md 已更新，H4/H5 待做 |

**lim-transport 测试总数**：63 个（含 a2a 11 + mcp_server 8 + mcp_client 2 + mcp_protocol 7 + selector 11 + local_endpoint 8 + tcp 3 + unix_socket 4 + discovery 5 + protocol 3 + named_pipe 1）。clippy + fmt 干净。

