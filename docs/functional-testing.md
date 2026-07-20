# lim 功能测试指南

本指南提供一系列测试用例，覆盖 lim 的全流程功能。按顺序完成这些测试后，可以确认核心功能正常，进入 v0.1 release 准备阶段。

> **使用方式**：按章节顺序执行，每个测试用例包含「准备」「操作」「预期」三部分。如发现问题，记录到 [TODO.md](TODO.md) 的"测试发现"段。

## 前置准备

### 环境

```bash
# 1. 进入项目目录
cd /path/to/lim

# 2. 构建二进制
cargo build --release
# 二进制位于 target/release/lim

# 3. 加入 PATH（可选，方便后续命令）
export PATH="$PWD/target/release:$PATH"

# 4. 验证版本
lim --version
# 预期：lim <git-describe 输出>（如 lim v0.1.0-3-gabcdef-dirty）
```

### Provider 配置

在项目根目录创建 `.lim/config.toml`（gitignored）：

```toml
default_provider = "anthropic"

[providers.anthropic]
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-20250514"

[providers.openai]
type = "openai-chat"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"
```

设置 API key：

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
export OPENAI_API_KEY="sk-..."
```

验证配置：

```bash
lim providers
# 预期：列出 anthropic 和 openai 两个 provider
```

### 测试目录

```bash
# 创建独立测试目录，避免污染主项目
mkdir -p /tmp/lim-test
cd /tmp/lim-test
# 在这里也放一份 .lim/config.toml（或用全局配置 ~/.config/lim/config.toml）
```

---

## 一、基础单次对话（`lim run`）

### 测试 1.1：最简单的 Hello World

**目的**：验证 provider 调用链路、streaming、事件持久化基本可用。

```bash
mkdir -p /tmp/lim-test
cd /tmp/lim-test

lim run --provider anthropic --message "回复一个字：好"
```

**预期**：
- 终端打印 transcript（agent 响应）
- `sessions/` 目录下生成 `<session_id>.jsonl` 文件
- 文件第一行是 `SessionHeader`，最后一行是 `AgentEnd`

**验证**：

```bash
# 查看最新 session
ls -lt sessions/ | head -3

# 检查事件序列
cat sessions/<latest>.jsonl | jq '.event'
# 预期事件序列：
# "session_header"
# "agent_start"
# "turn_start"
# "message_start"
# "message_update" (多个)
# "message_end"
# "turn_end"
# "agent_end"
```

### 测试 1.2：使用 MockProvider（无真实 API 调用）

**目的**：在没有 API key 或需要确定性输出时验证链路。

准备 fixture 文件 `/tmp/lim-test/fixture.json`：

```json
[
  {"type": "text", "text": "你好，我是 mock agent。"},
  {"type": "text", "text": "这是第二次响应。"}
]
```

```bash
lim run --fake /tmp/lim-test/fixture.json --message "Hello"
```

**预期**：
- 输出 "你好，我是 mock agent。"
- 第二次 `lim run --fake` 会输出 "这是第二次响应。"

### 测试 1.3：Session resume

**目的**：验证 session JSONL 回放、消息历史延续。

```bash
# 第一次对话
lim run --provider anthropic --message "记住我的名字叫测试者" --output /tmp/lim-test/s1.jsonl

# resume 继续对话
lim run --provider anthropic --resume /tmp/lim-test/s1.jsonl --message "我叫什么名字？"
# 预期：agent 能回答"测试者"
```

**验证**：

```bash
# 检查 resume 后的 session 包含原始消息 + 新消息
cat sessions/<new_id>.jsonl | jq 'select(.event == "message_end") | .event.message.role'
# 预期：User, Assistant, User, Assistant, User, Assistant（按顺序）
```

### 测试 1.4：OpenAI provider

**目的**：验证 OpenAI 调用链路。

```bash
lim run --provider openai --model gpt-4o --message "回复一个字：好"
# 预期：正常响应
```

---

## 二、工具调用（`lim run` + tools）

### 测试 2.1：read 工具

**目的**：验证 agent 能调用 `read` 工具读文件。

```bash
mkdir -p /tmp/lim-test
echo "Hello from test file" > /tmp/lim-test/test.txt

lim run --provider anthropic --message "请读取 /tmp/lim-test/test.txt 文件并告诉我内容"
```

**预期**：
- agent 调用 `read` 工具
- 输出包含 "Hello from test file"
- session JSONL 包含 `ToolExecutionStart` 和 `ToolExecutionEnd` 事件

**验证**：

```bash
cat sessions/<latest>.jsonl | jq 'select(.event == "tool_execution_start") | .event.tool_name'
# 预期："read"

cat sessions/<latest>.jsonl | jq 'select(.event == "tool_execution_end") | .event.result'
# 预期：包含文件内容
```

### 测试 2.2：write + edit 工具

**目的**：验证文件写入和编辑。

```bash
lim run --provider anthropic --message "在 /tmp/lim-test/ 下创建一个 hello.rs 文件，内容是 fn main() { println!(\"hello\"); }"
```

**预期**：
- agent 调用 `write` 工具创建文件
- 验证文件存在且内容正确

```bash
cat /tmp/lim-test/hello.rs
# 预期：fn main() { println!("hello"); }
```

继续编辑：

```bash
lim run --provider anthropic --message "把 /tmp/lim-test/hello.rs 里的 hello 改成 hi"
# 预期：agent 调用 edit 工具，文件内容变为 println!("hi")
```

### 测试 2.3：shell 工具

**目的**：验证 shell 命令执行。

```bash
lim run --provider anthropic --message "运行 ls /tmp/lim-test/ 命令并告诉我看到什么文件"
```

**预期**：
- agent 调用 `shell` 工具
- 输出包含 `hello.rs`、`test.txt` 等

### 测试 2.4：Trust Level（workspace 限制）

**目的**：验证默认 workspace trust 拒绝跨 workspace 写入。

```bash
# 默认 workspace trust，尝试写 workspace 外
lim run --provider anthropic --message "在 /etc/ 下创建一个文件 lim-test-marker" 2>&1 | tee /tmp/lim-test/trust-test.log
# 预期：agent 调用 write 工具被拒绝（或写入失败），因为 /etc/ 不在 workspace 内
```

```bash
# 切换 permissive trust
lim run --trust permissive --provider anthropic --message "在 /tmp/lim-test-external/ 下创建一个文件 marker.txt 内容是 ok"
# 预期：写入成功（permissive 允许任意路径）
```

---

## 三、TUI 交互

### 测试 3.1：启动 TUI 并对话

```bash
cd /tmp/lim-test
lim tui --provider anthropic
```

**操作**：
1. 在输入框输入 "帮我列出当前目录文件"
2. 按 Enter 发送
3. 观察流式输出

**预期**：
- TUI 界面正常渲染
- 输入后看到流式响应
- 状态栏显示 token 使用量

### 测试 3.2：Slash 命令

在 TUI 中依次尝试：

```
/help               # 显示帮助
/tokens             # 查看 token 使用
/model gpt-4o       # 切换模型
/provider openai    # 切换 provider
/sessions           # 列出历史 session
/compact            # 手动触发 compaction
/exit               # 退出
```

**预期**：每个命令都能正常执行，无报错。

### 测试 3.3：工具审批

```bash
lim tui --provider anthropic
# 输入："在 /tmp/lim-test/ 下创建一个文件 approval-test.txt"
```

**预期**：
- agent 调用 `write` 工具时弹出审批弹窗
- 选择 "Allow" 后工具执行
- 文件创建成功

---

## 四、Session 管理

### 测试 4.1：列出 sessions

```bash
# 多跑几次 lim run 产生多个 session
lim run --provider anthropic --message "test 1" > /dev/null
lim run --provider anthropic --message "test 2" > /dev/null

lim sessions --limit 5
# 预期：列出最近 5 个 session
```

### 测试 4.2：查看 session 详情

```bash
lim sessions --details sessions/<session_id>.jsonl
# 预期：显示 session 的完整 transcript（User/Assistant 消息列表）
```

### 测试 4.3：编辑 session 上下文

```bash
# 列出消息
lim edit-context --session sessions/<id>.jsonl --list

# 排除某些消息（不实际写入，只预览）
lim edit-context --session sessions/<id>.jsonl --exclude "1,3" --list

# 实际写入 ContextEdited 事件
lim edit-context --session sessions/<id>.jsonl --exclude "2"

# resume 编辑后的 session
lim run --resume sessions/<id>.jsonl --provider anthropic --message "总结我们之前的对话"
# 预期：agent 看不到被 exclude 的消息
```

---

## 五、Loop Engineering（`lim loop`）

### 测试 5.1：最简单的 goal loop（AlwaysPass verifier）

**目的**：验证 LoopRunner 基本流程。

```bash
cd /tmp/lim-test
mkdir -p loop-test
cd loop-test

lim loop --provider anthropic --goal "在当前目录创建一个 README.md 文件，内容是 Hello World"
# 不加 --verify，使用 AlwaysPassVerifier，单 session 即完成
```

**预期**：
- agent 创建 README.md
- goal 状态为 `Completed`
- `.lim/loop/<goal_id>/` 目录生成

**验证**：

```bash
# 查看 goal 列表
lim loop --list

# 查看 goal 详情
lim loop --status <goal_id>
# 预期：状态为 Completed

# 检查 session 事件持久化
ls .lim/loop/<goal_id>/
# 预期：session-0.jsonl, events.jsonl

# 检查 session JSONL 完整性
cat .lim/loop/<goal_id>/session-0.jsonl | jq '.event' | head -5
# 预期：session_header, agent_start, ...
```

### 测试 5.2：Command verifier（`cargo test`）

**目的**：验证 verify 失败 → 下一 session 重试。

```bash
cd /tmp/lim-test
mkdir -p cargo-test-demo
cd cargo-test-demo
cargo init --name demo

# 写一个故意失败的测试
cat > src/lib.rs << 'EOF'
pub fn add(a: i32, b: i32) -> i32 {
    a - b  // 故意写错
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_add() {
        assert_eq!(add(1, 2), 3);
    }
}
EOF

# 启动 loop，verifier 为 cargo test
lim loop --provider anthropic --goal "修复 src/lib.rs 让 cargo test 通过" --verify "cargo test" --max-sessions 3
```

**预期**：
- Session 1：agent 尝试修复，但可能修错或修对
- Verify：`cargo test`，exit code 决定 Pass/Fail
- 如果 Fail：进入 Session 2 继续修复
- 直到 Pass 或达到 max-sessions

**验证**：

```bash
# 查看所有 session 的验证结果
cat .lim/loop/<goal_id>/events.jsonl | jq 'select(.event == "verification_done") | {session_id, result}'
# 预期：每次 verify 的结果（Pass/Fail）

# 查看最终 goal 状态
lim loop --status <goal_id>
```

### 测试 5.3：LLM verifier

**目的**：验证 LLM-as-judge 验证器。

```bash
cd /tmp/lim-test
mkdir -p llm-verify-test
cd llm-verify-test
echo "TODO" > TODO.md

lim loop --provider anthropic --goal "把 TODO.md 内容改成 'DONE'" --verify llm --max-sessions 2
```

**预期**：
- agent 修改 TODO.md
- LLM verifier 独立调用 LLM 评估是否达成 goal
- 验证结果在 `LoopEvent::VerificationDone` 中

**验证**：

```bash
cat .lim/loop/<goal_id>/events.jsonl | jq 'select(.event == "verification_done") | {verifier_kind, verifier_tokens_used}'
# 预期：verifier_kind = "llm"，tokens_used > 0
```

### 测试 5.4：Reviewer verifier

**目的**：验证独立 reviewer agent + 工具访问 + review 持久化。

```bash
cd /tmp/lim-test
mkdir -p reviewer-test
cd reviewer-test
echo "fn main() { println!(\"hello\") }" > main.rs

lim loop --provider anthropic --goal "在 main.rs 里加一行注释说明这是 hello world 程序" --verify reviewer --max-sessions 1
```

**预期**：
- 主 agent 修改 main.rs
- Reviewer agent spawn 独立 session，用 `read` 工具检查 main.rs
- Review session 持久化到 `.lim/loop/<goal_id>/reviews/review-0.jsonl`

**验证**：

```bash
ls .lim/loop/<goal_id>/reviews/
# 预期：review-0.jsonl

# 检查 reviewer 的工具调用
cat .lim/loop/<goal_id>/reviews/review-0.jsonl | jq 'select(.event == "tool_execution_start") | .event.tool_name'
# 预期："read"

# 检查 verification metadata
cat .lim/loop/<goal_id>/events.jsonl | jq 'select(.event == "verification_done") | {verifier_kind, verifier_session_id, verifier_tokens_used}'
# 预期：verifier_kind = "reviewer"，verifier_session_id 非空
```

### 测试 5.5：Budget 限制

**目的**：验证 max_sessions / max_tokens 预算耗尽时正确停止。

```bash
cd /tmp/lim-test
mkdir -p budget-test
cd budget-test

# 故意给一个无法完成的 goal + 严格预算
lim loop --provider anthropic --goal "生成 1 到 100 万的质数表" --verify "test ! -f primes.txt" --max-sessions 2
```

**预期**：
- 2 个 session 后停止
- goal 状态为 `BudgetExhausted` 或 `NeedsHuman`

### 测试 5.6：Goal resume

**目的**：验证中断的 goal 可恢复。

```bash
# 先启动一个会跑很久的 goal
lim loop --provider anthropic --goal "重构整个项目" --verify "cargo test" --max-sessions 10 &
LOOP_PID=$!
sleep 10
kill $LOOP_PID  # 模拟中断

# 查看未完成 goal
lim loop --list
# 预期：有一个 Running 状态的 goal

# resume
lim loop --resume <goal_id>
# 预期：从上次中断处继续
```

---

## 六、Daemon 模式（`lim serve` / `lim goal`）

> **注意**：`lim loop-daemon start` 已废弃，推荐使用 `lim serve`（goal daemon 为 serve 的常驻能力，默认开启）。goal 管理命令（submit/list/status/pause/resume/sync）的首选别名是 `lim goal <action>`，`lim loop-daemon <action>` 保留为向后兼容。

### 测试 6.1：daemon 基本流程

```bash
cd /tmp/lim-test
mkdir -p daemon-test
cd daemon-test

# 启动 serve（前台，goal daemon 默认开启）
# 开一个新的终端窗口运行：
lim serve --protocol none
# 预期：打印 "Mode: foreground" + "Protocol: none" + "Goal daemon: enabled (built-in)"
# 进程持续运行，等待 goal 提交

# 在另一个终端提交 goal
lim goal submit --goal "在当前目录创建 marker.txt" --workspace .
# 预期：返回 goal_id

# 列出 goals
lim goal list
# 预期：包含刚提交的 goal

# 查看状态
lim goal status <goal_id>

# 暂停
lim goal pause <goal_id> --reason "testing"

# 恢复
lim goal resume <goal_id>

# 关闭 serve：在 serve 终端按 Ctrl+C
# 预期：优雅退出
```

**注意**：当前 daemon 是 in-process 模式（goal 提交通过 in-process channel，非 Unix socket IPC）。`lim serve --protocol none` 只启动 goal daemon，不暴露 MCP/A2A 协议。

### 测试 6.2：`lim serve` 配置默认值

**目的**：验证 `[serve]` 配置段为 `--protocol` / `--daemon` 提供默认值。

```bash
cd /tmp/lim-test
mkdir -p serve-config-test
cd serve-config-test

# 配置 [serve] 段
cat > .lim/config.toml << 'EOF'
default_provider = "anthropic"

[providers.anthropic]
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-20250514"

[serve]
protocol = "none"    # 只启动 goal daemon
mode = "foreground"

[serve.goal_daemon]
enabled = true
storage = ".lim/daemon"
EOF

# 不带 --protocol 启动，应使用配置默认值 "none"
lim serve &
SERVE_PID=$!
sleep 2

# 预期输出包含：
# Mode:     foreground
# Protocol: none
# Goal daemon: enabled (built-in)

# 关闭
kill -TERM $SERVE_PID
wait $SERVE_PID 2>/dev/null
# 预期：优雅退出（收到 SIGTERM）
```

### 测试 6.3：`lim serve` 配置禁用 goal daemon

**目的**：验证 `[serve.goal_daemon] enabled = false` 能禁用 goal daemon。

```bash
cd /tmp/lim-test/serve-config-test

# 修改配置禁用 goal daemon
cat > .lim/config.toml << 'EOF'
[serve.goal_daemon]
enabled = false
EOF

lim serve --protocol none &
SERVE_PID=$!
sleep 2

# 预期输出包含：
# Goal daemon: disabled

kill -TERM $SERVE_PID
wait $SERVE_PID 2>/dev/null
```

### 测试 6.4：`lim serve` SIGTERM 优雅退出

**目的**：验证 foreground 模式下 SIGTERM 能触发 CancellationToken。

```bash
cd /tmp/lim-test
mkdir -p serve-shutdown-test
cd serve-shutdown-test

lim serve &
SERVE_PID=$!
sleep 2

# 发送 SIGTERM
kill -TERM $SERVE_PID
wait $SERVE_PID
EXIT_CODE=$?

# 预期：exit code = 0（优雅退出，非 137/SIGKILL）
echo "Exit code: $EXIT_CODE"
```

---

## 七、Compaction

### 测试 7.1：触发自动 compaction

**目的**：验证长对话触发 compaction + StructuredSummary 生成。

```bash
cd /tmp/lim-test
mkdir -p compaction-test
cd compaction-test

# 创建一个会触发 compaction 的长对话（小 context window）
# 注意：这需要修改 .lim/config.toml 或用 CLI 参数
# 当前 ContextConfig 默认 context_window=128000，需要很长的对话才会触发

# 方法 1：用脚本生成长对话
for i in $(seq 1 50); do
    echo "User: 这是第 $i 条消息，请记住"
    echo "Assistant: 好的，我记住了第 $i 条消息"
done > /tmp/lim-test/long-conversation.txt

lim run --provider anthropic --message "请阅读以下长对话并总结：$(cat /tmp/lim-test/long-conversation.txt)"
```

**验证**（如触发 compaction）：

```bash
cat sessions/<latest>.jsonl | jq 'select(.event == "context_compacted") | {messages_before, messages_after, tokens_before, tokens_after, summary_model}'
# 预期：messages_after < messages_before，summary_model 非空
```

### 测试 7.2：手动 compaction（TUI）

```bash
lim tui --provider anthropic
# 进行几轮对话后
/compact
# 预期：状态栏显示 compaction 结果（messages before/after, token cost）
```

---

## 八、Steering（运行中注入消息）

### 测试 8.1：TUI 中 steering

**目的**：验证运行中注入消息。

```bash
lim tui --provider anthropic
# 1. 发送一个需要长时间思考的消息，如 "列出 1 到 100 的所有质数并解释算法"
# 2. 在 agent 思考过程中，立即发送第二条消息 "只列出前 10 个就够了"
# 3. 观察 agent 是否调整了输出
```

**预期**：
- 第二条消息作为 SteeringMessage 注入
- 状态栏显示 "Steering: 1"（注入的消息数量）
- agent 输出可能调整为只列前 10 个

**验证**：

```bash
cat sessions/<latest>.jsonl | jq 'select(.event == "steering_message_applied") | {count, contents}'
# 预期：count = 1，contents 包含注入的消息
```

---

## 九、Context 文件加载（AGENTS.md 等）

### 测试 9.1：AGENTS.md 自动加载

**目的**：验证项目上下文文件被注入到 system prompt。

```bash
cd /tmp/lim-test
mkdir -p context-test
cd context-test

# 创建 AGENTS.md
cat > AGENTS.md << 'EOF'
# Project Conventions

- 这个项目里所有文件名必须用中文
- 回答时必须用文言文风格
EOF

lim run --provider anthropic --message "创建一个文件记录测试结果"
# 预期：agent 创建的文件名是中文（如 测试结果.txt），且回答用文言文风格
```

### 测试 9.2：自定义 context 配置

**目的**：验证 `[context]` 配置段支持自定义文件。

```bash
cd /tmp/lim-test/context-test

# 创建自定义 context 文件
cat > .lim/config.toml << 'EOF'
default_provider = "anthropic"

[providers.anthropic]
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-20250514"

[context]
files = [
  { path = "STYLE.md", label = "style-guide" },
  { path = "../shared-rules.md", search = "fixed" }
]
EOF

cat > STYLE.md << 'EOF'
# Style Guide
- 用英文回答
- 简洁明了
EOF

echo "# Shared Rules" > ../shared-rules.md
echo "- 全局规则" >> ../shared-rules.md

lim run --provider anthropic --message "hello"
# 预期：agent 用英文回答（受 STYLE.md 影响）
```

---

## 十、Provider 切换

### 测试 10.1：运行时切换 provider（TUI）

```bash
lim tui --provider anthropic
# 1. 用 anthropic 对话几轮
# 2. /provider openai
# 3. 继续对话
# 预期：切换后用 OpenAI 模型响应，对话历史保留
```

### 测试 10.2：运行时切换 model

```bash
lim tui --provider anthropic
# 1. /model claude-sonnet-4-20250514
# 2. 对话
# 3. /model gpt-4o  # 需要先 /provider openai
# 4. 继续对话
```

---

## 十一、错误处理与恢复

### 测试 11.1：Provider 错误重试

**目的**：验证 rate limit / 网络错误时的重试行为。

```bash
# 用无效 API key 触发 auth 错误（不可重试）
WRONG_KEY=invalid lim run --provider anthropic --message "test" 2>&1 | tee /tmp/lim-test/error-test.log
# 预期：报 auth 错误，不重试，退出

# 模拟 rate limit（需要真实场景，难手动触发）
# 可观察日志：RUST_LOG=lim_agent=debug lim run ...
```

### 测试 11.2：Session JSONL 损坏恢复

```bash
# 备份一个 session
cp sessions/<id>.jsonl /tmp/lim-test/corrupted.jsonl

# 故意损坏
echo "invalid json line" >> /tmp/lim-test/corrupted.jsonl

# 尝试 resume
lim run --resume /tmp/lim-test/corrupted.jsonl --provider anthropic --message "test" 2>&1 | tee /tmp/lim-test/corrupt-test.log
# 预期：跳过损坏行或报错，但不会 panic
```

### 测试 11.3：取消（Ctrl+C）

```bash
# 启动一个长任务
lim run --provider anthropic --message "写一篇 1000 字的文章" &
RUN_PID=$!
sleep 3
kill -INT $RUN_PID  # Ctrl+C
# 预期：优雅退出，session JSONL 包含已发生的事件
```

---

## 十二、综合场景测试

### 测试 12.1：完整的 coding 任务

**目的**：端到端验证 agent 完成真实 coding 任务。

```bash
cd /tmp/lim-test
mkdir -p coding-e2e
cd coding-e2e
cargo init --name e2e

lim loop --provider anthropic \
  --goal "在 src/lib.rs 实现 add(a, b) 和 multiply(a, b) 函数，并写完整的单元测试" \
  --verify "cargo test" \
  --max-sessions 3
```

**预期**：
- agent 创建 lib.rs 实现 + 测试
- `cargo test` 通过
- goal 状态 Completed

### 测试 12.2：多工具协作

```bash
lim run --provider anthropic --message "
1. 在 /tmp/lim-test/multi-tool/ 创建一个项目目录
2. 在里面创建 Cargo.toml（bin 项目，name=demo）
3. 创建 src/main.rs 打印 hello
4. 运行 cargo build 验证
5. 运行 ./target/debug/demo 验证输出
"
# 预期：agent 串联使用 write + shell 工具完成全流程
```

### 测试 12.3：长对话 + resume + compaction

```bash
# 1. 进行长对话
lim run --provider anthropic --output /tmp/lim-test/long.jsonl --message "
请帮我设计一个简单的 TODO 应用，包含：
1. 数据结构
2. CRUD 操作
3. 持久化
详细说明每个部分
"

# 2. resume 继续追问
lim run --resume /tmp/lim-test/long.jsonl --provider anthropic --message "把上面的设计用 Rust 实现"

# 3. 再 resume
lim run --resume /tmp/lim-test/long-2.jsonl --provider anthropic --message "给实现的代码加单元测试"
# 预期：agent 能记住前两次对话的上下文
```

---

## 测试完成检查清单

完成所有测试后，确认以下项：

- [ ] **基础对话**：`lim run` 能正常调用 Anthropic 和 OpenAI，输出合理
- [ ] **Mock 模式**：`--fake` fixture 能确定性回放
- [ ] **Session resume**：`--resume` 能延续对话上下文
- [ ] **工具调用**：read/write/edit/shell 都能正常调用
- [ ] **Trust level**：workspace 默认限制跨目录写入，permissive 允许
- [ ] **TUI**：启动、对话、slash 命令、审批弹窗都正常
- [ ] **Session 管理**：list/details/edit-context 正常
- [ ] **Loop Engineering**：goal loop + 各种 verifier（AlwaysPass/Command/LLM/Reviewer）
- [ ] **Budget 限制**：max_sessions 耗尽时正确停止
- [ ] **Goal resume**：中断后可恢复
- [ ] **Daemon**：`lim serve --protocol none` + `lim goal submit/list/status/pause/resume` 基本流程
- [ ] **Serve 配置**：`[serve]` 配置段为 `--protocol`/`--daemon` 提供默认值
- [ ] **Serve 禁用 goal daemon**：`[serve.goal_daemon] enabled = false` 生效
- [ ] **Serve SIGTERM**：foreground 模式 SIGTERM 触发优雅退出（exit 0）
- [ ] **Compaction**：长对话触发自动 compaction，StructuredSummary 生成
- [ ] **Steering**：TUI 中注入消息能影响 agent 行为
- [ ] **Context 文件**：AGENTS.md 等被注入 system prompt
- [ ] **Provider 切换**：TUI 中运行时切换 provider/model
- [ ] **错误处理**：无效 key、JSONL 损坏、Ctrl+C 都不 panic
- [ ] **综合场景**：coding 任务 + 多工具协作 + 长对话 resume

## 发现问题怎么办

1. **记录到 [TODO.md](TODO.md)** 的"测试发现"段
2. **严重问题**（panic、数据丢失、核心功能不可用）→ 立即修复，阻塞 v0.1
3. **中等问题**（体验差、边界 case）→ 评估是否阻塞 v0.1
4. **轻微问题**（文案、UI 细节）→ 记录，v0.2 修复

## 测试通过后的下一步

进入 v0.1 release 准备阶段，详见 [TODO.md](TODO.md) "v0.1 Release 阻塞项" 段：

1. 历史压缩（git squash）
2. musl vs gnu 决策
3. 代码签名
4. novel-cli 打包策略
5. Release notes 撰写
6. Tag & publish
