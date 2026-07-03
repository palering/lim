# lim 架构与规划审查报告

> 审查日期：2026-07-02
> 审查人：Claude Fable 5
> 审查范围：架构设计、文档完整性、演进规划、技术风险评估
> 审查方法：文档审查 + 代码规模统计 + 依赖分析
> 
> **修订记录**：
> - v1.0 (2026-07-02 初稿)
> - v1.1 (2026-07-02 修正)：修正 Loop 完成度（P0-P2 → P0-P3）、删除重复设计、调整时间估算

## 一、总体评估

### 1.1 项目定位

**核心定位**：Rust-native agent runtime

- ✅ 继承 Pi 分层架构（Primitives → Core → Harness → Product）
- ✅ 借鉴 Claude Code Loop Engineering（4 层 loop 模型）
- ✅ 参考 k8s 扩展模型（CRD + controller）
- ✅ `AgentLoop`（Turn Loop）vs `LoopRunner`（Goal Loop）清晰映射 Pi 的执行模型

**超越 Pi 的特性**：
- ✅ Behavior-trace 6 原则（可观测性强化）
- ✅ Time-based loop（Pi 只有 goal-based）
- ✅ Loop Engineering P0-P3 完整实现（DaemonServer + IPC + NetworkTopology）
- ✅ Session JSONL 完整回放能力

### 1.2 实现完成度

**当前状态**：Phase 3 完成，Loop Engineering P0-P3 完成，Phase 4 准备阶段

| Phase | 状态 | 完成内容 |
|---|---|---|
| Phase 1 | ✅ 完成 | AgentLoop + Tool + Executor + Session |
| Phase 2 | ✅ 完成 | Workflow engine (lim-workflow) |
| Phase 3a-3c | ✅ 完成 | LLM providers + TUI + UX hardening + Platform abstraction |
| **Loop P0-P3** | ✅ 完成 | LoopRunner + DaemonServer + IPC + NetworkTopology（357 测试通过）|
| Phase 4 | ⏳ 准备中 | Sandbox + MCP + Loop P3.5+ |

**代码规模**：
- 10 crates，12,809 行代码（不含注释/空行）
- 262 单元测试，0 clippy warnings
- 3 个 LLM providers（Anthropic + OpenAI Chat + OpenAI Responses）

### 1.3 架构质量评价

**优秀之处**：
1. ✅ **分层严格**：依赖方向清晰，`lim-core` 零运行时依赖
2. ✅ **可观测性设计**：behavior-trace.md 的 6 条原则是同类项目少见的严谨设计
3. ✅ **工程纪律**：13 条基础准则 + Containment Principle 执行到位
4. ✅ **扩展性**：`AgentHooks` 统一策略入口，Policy/Capability 正交分离

**值得继续保持**：
- Session JSONL 完整回放能力
- Event + Log 双层可观测
- 闭合枚举错误类型（无 catch-all）
- 锁不跨 await（P6 原则）

## 二、关键风险与建议

### 2.1 Sandbox 依赖风险（高优先级）

**现状分析**：
- trust-and-approval.md 计划等待 Microsoft MXC 成熟
- MXC 当前状态：alpha + "not a security boundary" 警告
- 采纳条件：schema 稳定 + crates.io 发布 + 移除警告（预估 2-3 年）

**风险**：Phase 4 被 MXC 无限期阻塞

**建议**：✅ **将 self-built 从 fallback 提升为 primary plan**

#### 推荐架构

```rust
// crates/lim-sandbox/src/lib.rs

/// Sandbox 后端抽象（不依赖具体实现）
pub trait SandboxBackend: Send + Sync {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxHandle>;
}

pub struct SandboxConfig {
    pub workspace_roots: Vec<Utf8PathBuf>,
    pub network_access: NetworkPolicy,
    pub env_vars: HashMap<String, String>,
    pub max_memory_mb: Option<u64>,
    pub timeout: Option<Duration>,
}

pub struct SandboxHandle {
    backend: Box<dyn SandboxBackend>,
    id: SandboxId,
}
```

#### 实现优先级

```
P0 (Phase 4 启动): NullSandbox
  - 直接透传到 LocalExecutor（无隔离）
  - 用于快速打通 SandboxBackend trait 调用链
  - 预估 1-2 天

P1 (Phase 4 MVP): 自研薄层（primary plan）
  - Linux: landlock(2) + seccomp-bpf (via libseccomp crate)
  - macOS: sandbox_init() 直接 FFI (Seatbelt profile)
  - Windows: CreateRestrictedToken + SetTokenInformation
  - 预估 2-3 周（考虑三平台各有坑：ABI 版本、deprecated 警告、API 取舍）
  - 参考 Codex 的 linux-sandbox crate 设计

P2 (Phase 4 优化): MXC shim（when ready）
  - 封装 mxc-sdk (library mode) 或 mxc CLI (process mode)
  - 采纳条件：schema 稳定 + crates.io 发布 + 移除警告
  - 预估 2-3 天（shim 层很薄）
  - 作为 P1 的可选替代 backend（用户可选）
```

**关键决策**：
- ✅ P1 自研薄层不阻塞 Phase 4（MXC 不 ready 也能发布）
- ✅ MXC 作为优化项（满足条件后作为可选 backend）
- ✅ 用户可选 backend（CLI `--sandbox-backend mxc|native|null`）

**架构图**：

```
lim-tools (read/write/shell)
    ↓
lim-executor::Executor trait
    ↓
SandboxedExecutor (持有 SandboxHandle)
    ↓
lim-sandbox::SandboxBackend trait
    ↓ 多实现
├─ NullSandbox (P0, 无隔离)
├─ LandlockSandbox (P1, Linux)
├─ SeatbeltSandbox (P1, macOS)
├─ TokenSandbox (P1, Windows)
└─ MxcSandbox (P2, 全平台优化)
```

### 2.2 Loop Engineering 深化设计（中优先级）

**现状分析**：
- ✅ Loop P0-P3 已完成（LoopRunner + StateLedger + Verifier + DaemonServer + IPC）
- ✅ `AllPassVerifier` 已实现组合验证器
- ⏳ P3.5 待补全：CLI 暴露 LlmVerifier 入口 + 完成契约写进设计哲学

**建议**：扩展现有 `AllPassVerifier` 而非新增概念

#### 与 AllPassVerifier 的关系

**当前实现**：`AllPassVerifier` 已支持组合多个 verifier（所有都 Pass 才 Pass）

**扩展方向**：
```rust
// crates/lim-agent/src/loop_runner/verifier.rs

pub struct AllPassVerifier {
    pub required: Vec<Box<dyn Verifier>>,
    pub optional: Vec<Box<dyn Verifier>>,  // 新增：跑但不阻塞
}

impl Verifier for AllPassVerifier {
    async fn verify(&self, state: &LoopState) -> VerifyResult {
        // 1. required 全过才继续
        for v in &self.required {
            match v.verify(state).await? {
                VerifyResult::Pass => continue,
                other => return other,
            }
        }
        
        // 2. optional 跑一遍但不阻塞（结果可记录到 LoopState）
        for v in &self.optional {
            let _ = v.verify(state).await;
        }
        
        VerifyResult::Pass
    }
}
```

**Self-assessment**：将 `LlmVerifier` 作为 required 的最后一个 verifier，而非单独概念

#### Skill 系统（P4 规划）

统一为 **Skill 文档一种格式**（避免与 contract.toml 重复）：

```markdown
---
name: fix-clippy
description: Fix all clippy warnings
---

# Fix Clippy Warnings

## Verification

```toml
[[required]]
type = "command"
command = "cargo clippy --all-targets -- -D warnings"
timeout = "5m"

[[required]]
type = "command"
command = "cargo test --all"
timeout = "10m"
```

## Budget
- Max sessions: 10
- Max tokens: 1M
```

**实现**：
- `SkillVerifier` 解析 Skill 文档
- 委托给 `CommandVerifier` 或 `LlmVerifier`
- CLI `lim loop --skill fix-clippy`

### 2.3 发布前阻塞项（高优先级）

**必须在 first release 前解决**（< 1 天工作量）：

1. ❌ **LICENSE 文件缺失**
   - Cargo.toml 声明 `license = "MIT"` 但文件不存在
   - release archive 预期包含 LICENSE

2. ❌ **README.md 缺失**
   - release archive 预期包含 README.md
   - 建议章节：What is lim / Quick Start / Architecture / Development

3. ❌ **仓库 URL 占位符**
   - Cargo.toml: `https://github.com/user/lim` 必须替换为真实仓库
   - 所有文档链接检查

4. ⚠️ **CI matrix 跨平台验证**
   - 需验证 6 个目标 triple 的编译（特别是 Windows ARM64 + Linux ARM64）
   - reqwest rustls-tls 跨平台验证

## 三、演进路线（修正版）

基于 Loop P0-P3 已完成 + MXC 策略 + Skill 系统规划：

### 立即可做（< 1 周）

**发布前阻塞项**（< 1 天）
- LICENSE + README.md + 仓库 URL
- CI matrix 6 平台验证

**Loop P3.5**（< 1 天，harness 层已支持）
- CLI 暴露 `--verify llm` 入口
- 完成契约写进设计哲学（loop-engineering.md）

**Loop P2.5**（2-3 天，可选）
- ReviewerVerifier（spawn 独立 reviewer agent）
- fresh context 验证模式

### Q3 2026: Phase 4 准备（2 个月）

**Week 1-2: Sandbox 抽象层**
- lim-sandbox crate + SandboxBackend trait
- NullSandbox（P0，透传 LocalExecutor）
- 打通 SandboxedExecutor 调用链

**Week 3-6: Sandbox 自研薄层（P1 primary）**
- LandlockSandbox（Linux，landlock + seccomp）
- SeatbeltSandbox（macOS，sandbox_init FFI）
- TokenSandbox（Windows，Restricted Token）
- 跨平台集成测试 + 文档
- **预估 3-4 周**（考虑三平台坑点 + buffer）

**Week 7-8: 预留 buffer / 文档补全**

### Q4 2026: Phase 4 交付（2 个月）

**Week 1-3: MCP tool protocol**
- JSON-RPC over stdio（crates/lim-mcp/）
- ExternalTool wrapper（注册到 ToolRegistry）
- 参考 Claude Desktop 的 MCP client 实现
- **预估 2-3 周**（MVP，完整实现需 3-4 周）

**Week 4-5: Skill 系统（P4）**
- SkillRegistry 加载 `.lim/skills/*.md`
- Skill 文档解析（frontmatter + markdown）
- CLI `lim loop --skill <name>`
- 扩展 AllPassVerifier 支持 optional verifiers

**Week 6-7: Phase 4 集成测试**
- Sandbox + MCP + Skill 端到端验证
- 文档补全（Sandbox 使用指南 + MCP tool 开发指南）

**Week 8: Phase 4 release**
- 打包 Phase A（6 平台 binary）
- 发布 blog（Rust-native agent runtime 定位）
- 准备 RustConf 2027 talk proposal

### Q1 2027: Phase 4 完整 + Phase 5 探索

**P4（Time-based loop）**
- DaemonServer IPC 完整实现（Unix socket 替代 in-process）
- LoopTrigger::Cron|Interval
- LoopEvent 落盘（.lim/loop/<goal_id>/events.jsonl）

**P5 探索（Proactive loop）**
- 动态工作流（goal → sub-goals 自动分解）
- 多 workspace 聚合（daemon 跨项目管理）
- 中心化调度（CentralizedTopology 实体化）

**MXC backend（when ready）**
- 季度检查 MXC 采纳条件（schema 稳定 + crates.io + 移除警告）
- 满足条件时实现 MxcSandbox（2-3 天）
- 作为可选 backend（不替代自研薄层）

## 四、技术实现细节

### 4.1 Landlock（Linux）实现参考

```rust
// crates/lim-sandbox/src/backends/landlock.rs

use landlock::{Access, AccessFs, Ruleset, RulesetAttr, ABI};

pub struct LandlockSandbox {
    workspace_roots: Vec<Utf8PathBuf>,
}

impl SandboxBackend for LandlockSandbox {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxHandle> {
        let abi = ABI::V3; // Kernel 5.19+
        
        let mut ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))?
            .create()?;
        
        // workspace_roots 可读写
        for root in &self.workspace_roots {
            ruleset = ruleset
                .add_rule(Access::from_read(root))?
                .add_rule(Access::from_write(root))?;
        }
        
        // /tmp 可读写，其他路径只读
        ruleset = ruleset
            .add_rule(Access::from_read("/tmp"))?
            .add_rule(Access::from_write("/tmp"))?;
        
        for ro_path in ["/usr", "/lib", "/lib64", "/etc"] {
            if std::path::Path::new(ro_path).exists() {
                ruleset = ruleset.add_rule(Access::from_read(ro_path))?;
            }
        }
        
        // 启用 landlock（影响当前进程及子进程）
        ruleset.restrict_self()?;
        
        Ok(SandboxHandle::new(/* ... */))
    }
}
```

**关键点**：
- Landlock 是 Linux 5.19+ 的内核特性（不需要外部依赖）
- 路径级别的访问控制（比 seccomp 简单）
- 配合 seccomp-bpf 限制 syscall（用 `libseccomp` crate）

### 4.2 Seatbelt（macOS）实现参考

```rust
// crates/lim-sandbox/src/backends/seatbelt.rs

extern "C" {
    fn sandbox_init(
        profile: *const libc::c_char,
        flags: u64,
        errorbuf: *mut *mut libc::c_char,
    ) -> libc::c_int;
}

pub struct SeatbeltSandbox {
    workspace_roots: Vec<Utf8PathBuf>,
}

impl SandboxBackend for SeatbeltSandbox {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxHandle> {
        // Seatbelt profile（SBPL 语法）
        let profile = format!(
            r#"
            (version 1)
            (allow default)
            (deny file-write* (subpath "/"))
            {}
            (allow file-read* file-write* (subpath "/tmp"))
            "#,
            self.workspace_roots
                .iter()
                .map(|p| format!(r#"(allow file-read* file-write* (subpath "{}"))"#, p))
                .collect::<Vec<_>>()
                .join("\n")
        );
        
        let profile_c = CString::new(profile)?;
        let mut errorbuf: *mut libc::c_char = std::ptr::null_mut();
        
        unsafe {
            let ret = sandbox_init(profile_c.as_ptr(), 0, &mut errorbuf);
            if ret != 0 {
                let err = CString::from_raw(errorbuf).into_string()?;
                return Err(LimError::sandbox_init(err));
            }
        }
        
        Ok(SandboxHandle::new(/* ... */))
    }
}
```

**关键点**：
- macOS 11+ 的 `sandbox_init()` 仍然可用（虽然文档说 deprecated）
- SBPL（Sandbox Profile Language）描述访问规则
- 配合 `sandbox-exec` 命令行工具（二选一）

### 4.3 Windows Token（Windows）实现参考

```rust
// crates/lim-sandbox/src/backends/windows_token.rs

use windows::Win32::Security::*;
use windows::Win32::System::Threading::*;

pub struct TokenSandbox {
    workspace_roots: Vec<Utf8PathBuf>,
}

impl SandboxBackend for TokenSandbox {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxHandle> {
        unsafe {
            let mut token: HANDLE = std::mem::zeroed();
            
            // 创建 Restricted Token
            CreateRestrictedToken(
                GetCurrentProcessToken(),
                DISABLE_MAX_PRIVILEGE,
                0, std::ptr::null(),
                0, std::ptr::null(),
                0, std::ptr::null(),
                &mut token,
            )?;
            
            // 设置 Integrity Level（Low）
            SetTokenInformation(
                token,
                TokenIntegrityLevel,
                /* ... */,
            )?;
            
            // 创建进程时使用 restricted token
        }
        
        Ok(SandboxHandle::new(/* ... */))
    }
}
```

**关键点**：
- Restricted Token（限制权限）+ Low Integrity Level（限制写入）
- 不如 Linux/macOS 细粒度（Windows 的沙箱是 AppContainer，需要 UWP）
- 但足够阻止大部分危险操作

## 五、行动清单

### 立即（< 1 周）

**发布前阻塞项**（< 1 天）
- [ ] LICENSE 文件（MIT 协议展开）
- [ ] README.md（What is lim / Quick Start / Architecture / Development）
- [ ] 仓库 URL 替换（Cargo.toml + 所有文档链接）
- [ ] CI matrix 补充 6 平台验证

**Loop P3.5**（< 1 天）
- [ ] CLI 暴露 `--verify llm` 入口
- [ ] 完成契约写进设计哲学（loop-engineering.md）

**Loop P2.5**（2-3 天，可选）
- [ ] ReviewerVerifier（spawn 独立 reviewer agent）
- [ ] fresh context 验证模式

### Q3 2026（2 个月）

**Week 1-2**
- [ ] lim-sandbox crate + SandboxBackend trait
- [ ] NullSandbox 实现（P0）
- [ ] 打通 SandboxedExecutor 调用链

**Week 3-6**
- [ ] LandlockSandbox（Linux，landlock + seccomp）
- [ ] SeatbeltSandbox（macOS，sandbox_init FFI）
- [ ] TokenSandbox（Windows，Restricted Token）
- [ ] 跨平台集成测试 + 文档

**Week 7-8**
- [ ] 预留 buffer / 文档补全

### Q4 2026（2 个月）

**Week 1-3**
- [ ] MCP tool protocol（crates/lim-mcp/）
- [ ] ExternalTool wrapper
- [ ] 参考 Claude Desktop MCP client 实现

**Week 4-5**
- [ ] SkillRegistry 实现
- [ ] Skill 文档解析（frontmatter + markdown）
- [ ] CLI `lim loop --skill <name>`
- [ ] 扩展 AllPassVerifier 支持 optional verifiers

**Week 6-7**
- [ ] Sandbox + MCP + Skill 端到端验证
- [ ] 文档补全（使用指南 + 开发指南）

**Week 8**
- [ ] Phase 4 release（6 平台打包）
- [ ] 发布 blog（Rust-native agent runtime 定位）
- [ ] RustConf 2027 talk proposal

## 六、战略决策确认

### 6.1 项目定位

✅ **Rust-native agent runtime**（继承 Pi + 借鉴 Claude Code + 参考 k8s）
- Pi 的分层理念 + Rust 的类型安全
- Claude Code 的 Loop Engineering（4 层 loop 模型）
- k8s 的扩展模型（CRD + controller）
- 在可观测性、Time-based loop、多 agent 拓扑上超越 Pi

### 6.2 Sandbox 策略

✅ **P1 自研薄层 primary，MXC 作为 P2 优化**
- 不等 MXC 成熟（避免 Phase 4 被阻塞）
- MXC 满足条件后作为可选 backend
- 用户可通过 CLI 选择 backend（`--sandbox-backend mxc|native|null`）

### 6.3 Loop Engineering

✅ **扩展 AllPassVerifier，统一 Skill 系统**
- 不新增 CompletionContract（与 AllPassVerifier 重复）
- 扩展 AllPassVerifier 支持 optional verifiers
- Skill 文档统一格式（frontmatter + markdown），不单独用 contract.toml
- SkillVerifier 委托给 CommandVerifier/LlmVerifier

### 6.4 发布节奏

✅ **Phase 4 first public release（Q4 2026）**
- Phase A 打包（6 平台 binary）
- 定位清晰（Rust-native agent runtime）
- 文档完整（LICENSE + README + Architecture）

## 七、与 Pi 的对照

| Pi 概念 | lim 实现 | 状态 | 备注 |
|---|---|---|---|
| Turn loop | `AgentLoop` | ✅ Phase 1 | 完整映射 |
| Multi-agent | `AgentTransport` + `Scheduler` | ✅ P1 | 已完成 |
| Long-running task | `LoopRunner` + `StateLedger` | ✅ P0-P3 | 已完成 |
| Verification | `Verifier` trait | ✅ P1 | trait 已有 |
| Verifier 组合 | `AllPassVerifier` | ✅ P1 | 已有，待扩展 optional |
| Daemon | `DaemonServer` + IPC | ✅ P3 | 已完成 |

**lim 超越 Pi 的特性**：
- ✅ Behavior-trace 6 原则（可观测性强化）
- ✅ Time-based loop（P4 规划，Pi 无）
- ✅ Skill 系统（P4 规划，Pi 无）
- ✅ Session JSONL 完整回放
- ✅ Loop P0-P3 完整实现（DaemonServer + NetworkTopology + 357 测试）

## 八、总结

### 核心评价

**这是一个设计优秀、执行严谨的 Rust-native agent runtime 实现**

- 架构设计：四层分离 + 可观测性设计领先同类项目
- 工程质量：357 测试 + 0 clippy warnings + 严格分层
- 完成度：Phase 3 已完成，Loop P0-P3 已完成，Phase 4 准备充分

### 三个关键决策

1. ✅ **Sandbox 不等 MXC**（P1 自研薄层，2-3 周）
2. ✅ **Loop 扩展 AllPassVerifier**（不新增 CompletionContract）
3. ✅ **统一 Skill 系统**（不单独用 contract.toml）

### 最终建议

**立即行动**（< 1 周）：
- LICENSE + README.md + 仓库 URL（阻塞 release）
- Loop P3.5（CLI `--verify llm`，< 1 天）
- Loop P2.5（ReviewerVerifier，2-3 天，可选）

**Q3 重点**（2 个月）：
- Sandbox 抽象层 + 自研薄层（3-4 周，不等 MXC）

**Q4 目标**（2 个月）：
- MCP + Skill 系统 + Phase 4 release

---

**审查结论**：项目架构扎实，演进路线清晰，建议按此方案推进。Loop P0-P3 已完成是重要里程碑，Phase 4 的 Sandbox + MCP + Skill 三大件可按规划推进。
