# 分层传输架构

> 状态：已部分实现（T0-T3 传输本体 + MCP 双向 + A2A 协议栈 + 服务发现 + 拓扑选择均已完成。mDNS/registry 跨节点发现为 in-memory stub，待 v0.2+ 接入真实实现）
>
> 相关文档：[serve-mode.md](serve-mode.md)（serve 统一入口）、[evolution.md](evolution.md) §七（A2A/ACP 演进）、[modules.md](modules.md)（lim-transport crate）

## 一、设计目标

lim 的 multi/sub agent 当前只有进程内 mpsc 通信（T0），需要扩展到跨进程、跨节点、跨网络的协作能力，同时保持编排层 API 稳定。

核心目标：

| 目标 | 说明 |
|---|---|
| **伸缩升降级** | 同一套编排 API，底层传输可从 in-process → cross-process → cross-node → cross-network 逐级升级 |
| **自动检测 + 手动配置** | 默认根据场景自动选传输，高级用户可强制指定 |
| **协议无关** | 编排层（Scheduler/Topology）不关心底层是 mpsc / Unix socket / TCP / A2A |
| **跨平台** | T1 跨进程传输在 Linux/macOS/Windows 上都有首选 + 降级方案 |
| **A2A 统一** | 一套 A2A 协议栈同时覆盖 agent 互联 + 编辑器集成（不单独实现 ACP） |

## 二、四层传输光谱

```
┌─────────────────────────────────────────────────────────────┐
│                    编排层（不变）                              │
│   Scheduler / NetworkTopology / Workflow / LoopRunner        │
│   ↑ 只依赖 AgentTransport trait，不关心底层                    │
├─────────────────────────────────────────────────────────────┤
│ T0  In-Process      │ T1  Cross-Process     │ T2  Cross-Node │
│ 同进程 mpsc          │ 同机多进程             │ 同局域网多节点  │
│ LocalChannelTransport│ UnixSocketTransport   │ TcpTransport   │
│ 已实现 ✅            │ 已实现 ✅              │ 已实现 ✅       │
├─────────────────────┼──────────────────────┼────────────────┤
│ T3  Cross-Network   │                       │                │
│ 跨互联网/跨云         │                       │                │
│ A2AHttpTransport    │                       │                │
│ 已实现 ✅            │                       │                │
└─────────────────────┴───────────────────────┴────────────────┘
```

四层的取舍：

| 层 | 延迟 | 吞吐 | 复杂度 | 适用场景 |
|---|---|---|---|---|
| **T0 In-Process** | <1μs | 极高 | 最低 | 一个程序内多 agent 协作（当前主场景） |
| **T1 Cross-Process** | ~10μs | 高 | 低 | 同机多个 lim 实例 / GUI + lim daemon / 多个垂直应用 |
| **T2 Cross-Node** | ~1ms | 中 | 中 | 同机房多节点 / 开发团队协作 |
| **T3 Cross-Network** | ~50ms+ | 低 | 高 | 跨网/跨云/与外部 agent 系统互联 |

**升降级原则**：默认用最低复杂度的传输满足需求。当需求升级时（从 T0 到 T1），**编排层代码零改动**，只换 transport 实现。

## 三、Crate 边界

```
lim-core (AgentMessage, AgentId)
   ↑
lim-transport (T1+ 传输 + 协议适配 + 服务发现)
   ↑
lim-agent (AgentTransport trait, LocalChannelTransport T0, Scheduler, NetworkTopology)
   ↑
lim-serve (组装层，把 transport + agent + loop_runner 组合成 lim serve)
   ↑
cli/tui (命令入口)
```

**关键边界**：

- **`AgentTransport` trait 留 `lim-agent`**：T0 实现在那里，trait 跟着实现走
- **`lim-transport` 只装 T1+ 实现**：Unix socket / Named Pipe / TCP / A2A HTTP + 协议适配器 + 服务发现
- **`lim-agent` 不依赖 `lim-transport`**：app 层（CLI/集成方）把 lim-transport 的实现注入 lim-agent 的 trait object

```rust
// app 层组装
let transport: Box<dyn AgentTransport> = match config.tier {
    Tier::T0 => Box::new(LocalChannelTransport::new(hub, rx)),
    Tier::T1 => Box::new(lim_transport::UnixSocketTransport::connect(...).await?),
    Tier::T3 => Box::new(lim_transport::A2AHttpTransport::connect(...).await?),
};
scheduler.spawn(SpawnOptions { /* transport 注入 */ }).await?;
```

这样 L1/L2 集成用户不需要拉 `lim-transport`，保持零网络依赖。

## 四、跨平台 T1 设计

T1 是跨进程同机通信，三大平台的本地 IPC 能力：

| 平台 | 原生首选 | tokio 支持 | 备注 |
|---|---|---|---|
| Linux | Unix Domain Socket | ✅ | `AF_UNIX`，`XDG_RUNTIME_DIR` 约定 |
| macOS | Unix Domain Socket | ✅ | 同上，`$TMPDIR` 约定 |
| Windows 10+ | Named Pipe | ✅ | `tokio::net::windows::named_pipe` |
| Windows 老版 | TCP loopback | ✅ | Win10 1803 之前无 `AF_UNIX` |

### 4.1 LocalEndpoint 抽象

```rust
/// Platform-aware local IPC endpoint.
#[derive(Debug, Clone)]
pub enum LocalEndpoint {
    /// Unix domain socket path (Linux/macOS preferred)
    Unix(PathBuf),
    /// Windows named pipe name (Windows preferred)
    /// e.g. r"\\.\pipe\lim-daemon"
    NamedPipe(String),
    /// TCP loopback (cross-platform fallback)
    TcpLoopback(SocketAddr),
}

impl LocalEndpoint {
    /// Auto-detect the best endpoint for the current platform.
    pub fn auto(app_name: &str) -> Self {
        #[cfg(unix)]
        { Self::Unix(default_unix_socket_path(app_name)) }
        #[cfg(windows)]
        { Self::NamedPipe(format!(r"\\.\pipe\{}", app_name)) }
    }
}
```

### 4.2 首选 + 降级矩阵

| 平台 | T1 首选 | T1 降级 | endpoint 配置 |
|---|---|---|---|
| Linux | Unix socket (`$XDG_RUNTIME_DIR`) | TCP loopback | `auto` 或 `unix://` |
| macOS | Unix socket (`$TMPDIR`) | TCP loopback | `auto` 或 `unix://` |
| Windows 10+ | Named Pipe (`\\.\pipe\`) | TCP loopback | `auto` 或 `pipe://` |
| Windows 老版 | TCP loopback | — | `tcp://127.0.0.1:port` |
| 任何受限环境 | TCP loopback | — | `tcp://127.0.0.1:port` |

**保证**：所有平台至少有一套可用方案，TCP loopback 是终极保底。

### 4.3 平台路径规范

| 平台 | 首选路径 | 降级路径 |
|---|---|---|
| Linux | `$XDG_RUNTIME_DIR/lim/daemon.sock` | `/tmp/lim-{uid}/daemon.sock` |
| macOS | `$TMPDIR/lim/daemon.sock` | `/tmp/lim/daemon.sock` |
| Windows | `\\.\pipe\lim-daemon` | `tcp://127.0.0.1:0` |

Linux 用 `XDG_RUNTIME_DIR`：systemd/logind 管理的运行时目录，用户登录时创建、登出时清理，文件权限自动 0700。

macOS 用 `$TMPDIR`：通常是 `/var/folders/xx/yy.../T/`，用户隔离。

Windows 用 Named Pipe：原生 IPC 机制，支持 ACL 权限控制。

### 4.4 endpoint URL scheme

| scheme | 平台 | 含义 |
|---|---|---|
| `auto` | 全部 | 自动选平台首选（默认） |
| `unix://` | Unix | Unix domain socket 路径 |
| `pipe://` | Windows | Named Pipe 名称 |
| `tcp://` | 全部 | TCP loopback（保底） |

### 4.5 自动降级机制

```rust
impl LocalSocketServer {
    /// Bind with automatic fallback.
    /// Tries the primary endpoint first; if it fails (e.g. Unix socket
    /// unsupported on old Windows), falls back to TCP loopback.
    pub async fn bind_with_fallback(app_name: &str) -> Result<(Self, LocalEndpoint)> {
        let primary = LocalEndpoint::auto(app_name);
        match Self::bind(&primary).await {
            Ok(server) => Ok((server, primary)),
            Err(e) => {
                tracing::warn!(primary = ?primary, error = %e,
                    "primary local endpoint failed, falling back to TCP loopback");
                let fallback = LocalEndpoint::TcpLoopback("127.0.0.1:0".parse().unwrap());
                Ok((Self::bind(&fallback).await?, fallback))
            }
        }
    }
}
```

### 4.6 不引入 `interprocess` crate

tokio 1.x 原生支持 Unix socket 和 Windows Named Pipe，不需要第三方库。自研抽象代码量约 300 行，完全可控，零新依赖。

## 五、协议适配层

协议和传输是正交的。A2A 协议可以跑在任何传输层上：

| 传输层 | A2A 跑法 | LimNative 跑法 | 用途 |
|---|---|---|---|
| T0 mpsc | 不需要（直接传 AgentMessage） | 直接传对象 | lim agent 间（最快） |
| T1 Unix socket / Named Pipe | A2A JSON-RPC over socket 帧 | serde_json 帧 | 同机跨进程 |
| T2 TCP | A2A JSON-RPC over TCP | serde_json 帧 | 同局域网多节点 |
| T3 HTTP(S) | A2A JSON-RPC over HTTP（标准 A2A） | — | 跨网 / 外部 agent 系统 |

### 5.1 ProtocolAdapter trait

```rust
#[async_trait]
pub trait ProtocolAdapter: Send + Sync {
    /// Serialize an AgentMessage to the protocol's wire format.
    async fn encode(&self, msg: &AgentMessage) -> Result<Vec<u8>>;
    /// Deserialize from the protocol's wire format.
    async fn decode(&self, data: &[u8]) -> Result<AgentMessage>;
    /// Protocol name for diagnostics (e.g. "lim-native", "a2a", "mcp").
    fn name(&self) -> &str;
}
```

### 5.2 两个实现（不是三个）

| 适配器 | 何时用 | 对端要求 |
|---|---|---|
| `LimNativeAdapter` | 通信对端也是 lim（任意传输层） | lim |
| `A2AAdapter` | 通信对端是外部 agent 系统或编辑器 | 任意 A2A 兼容 |

**没有 `AcpAdapter`**——ACP 已于 2026 年 4 月合并入 A2A，编辑器集成走 A2A Client 角色，不单独实现。

## 六、A2A 协议统一

### 6.1 A2A 覆盖的场景

A2A 协议定义了三个角色：

```
┌──────────┐   A2A    ┌──────────┐   A2A    ┌──────────┐
│  Client  │ ────────►│  Agent   │◄──────── │  Agent   │
│ (编辑器)  │          │  (lim)   │          │ (外部)    │
└──────────┘          └──────────┘          └──────────┘
```

- **Client 角色**：编辑器（Zed/VSCode）、CLI 前端、GUI 应用 → 发起 task、接收流式响应
- **Agent 角色**：lim agent → 处理 task、调用工具、返回结果
- **Agent ↔ Agent**：lim agent 与其他 A2A 兼容 agent 互操作

**一套 A2A 协议栈同时覆盖**：
- lim agent ↔ 外部 agent 系统（agent 间互联）
- 编辑器 ↔ lim agent（编辑器集成，原 ACP 场景）
- CLI/GUI ↔ lim daemon（客户端 ↔ agent）

### 6.2 lim 需要实现的 A2A 能力

| 能力 | lim 作为 Agent | lim 作为 Client |
|---|---|---|
| AgentCard 发布 | ✅ `/.well-known/agent-card.json` | — |
| Task 接收与处理 | ✅ `tasks/send` + `tasks/sendSubscribe`（流式） | — |
| Task 发起 | — | ✅ 调用外部 A2A agent |
| 工具调用结果返回 | ✅ `TaskStateUpdate` + `TaskArtifact` | — |
| 状态推送 | ✅ SSE / WebSocket 流式 | ✅ 接收流式 |

### 6.3 编辑器集成用例

```
Zed 编辑器 (A2A Client)
    │
    │ A2A JSON-RPC over Unix socket (T1) 或 HTTP (T3)
    │
lim daemon (A2A Agent)
    │
    │ 内部用 T0 mpsc 编排
    │
lim sub-agents (researcher / writer / editor)
```

Zed 不需要专门的 ACP 支持，只要 lim 暴露 A2A Agent 端点，Zed 作为 A2A Client 连接即可。

## 七、MCP 双向

MCP 是**工具调用协议**，不是 agent 间通信协议。它在架构中的位置与 A2A 不同：

```
AgentLoop
├── ToolRegistry
│   ├── 内置工具（read/write/edit/shell/glob/grep）
│   ├── 自定义工具（用户通过 Tool trait 注册）
│   └── MCP 工具（通过 MCP Client 连接外部工具服务器）← 对外调用
│
└── AgentTransport（agent 间通信）
    ├── T0 LocalChannelTransport
    ├── T1 UnixSocketTransport + LimNativeAdapter
    └── T1/T3 A2A Transport + A2AAdapter
```

MCP 在 **Tool 层**，不在 Transport 层。MCP 有两个方向：

### 7.1 MCP Server 模式（对外暴露）

`lim serve --protocol mcp` 把 lim 自己包装成 MCP server，被其他 agent 当工具调用：

| 工具名 | 对应 lim 内部能力 | 说明 |
|---|---|---|
| `lim_run` | AgentLoop::run_once | 一次性任务执行（最常用） |
| `lim_start_session` | AgentLoop + Session 持久化 | 启动可 resume 的会话 |
| `lim_continue_session` | Session resume | 继续已有会话 |
| `lim_compact` | compact_now | 手动压缩 |
| `lim_list_tools` | ToolRegistry::list | 列出可用工具 |
| `lim_get_status` | Session/Goal 状态 | 查询当前状态 |

**用例**：Claude Code / Cursor 等支持 MCP client 的 agent 不需要改代码就能调 lim。

### 7.2 MCP Client 模式（对外调用）

lim 的 `ToolRegistry` 加一个 `McpToolProxy`，把外部 MCP 工具服务器的工具包装成 lim 的 `Tool` trait 实现。AgentLoop 调用 MCP 工具和调用内置工具完全一样。

```toml
# 配置外部 MCP 工具服务器
[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
```

### 7.3 MCP 与 A2A 的关系

| 协议 | 定位 | 层级 | 场景 |
|---|---|---|---|
| **A2A** | agent 间通信 + 编辑器集成 | Transport 层 | lim ↔ agent / lim ↔ 编辑器 |
| **MCP** | 工具调用 | Tool 层 | lim ↔ 工具服务器 |
| ~~ACP~~ | 不单独实现 | — | A2A Client 角色覆盖 |

## 八、服务发现

T0 不需要服务发现（RoutingHub 直接查 HashMap）。T1+ 需要服务发现机制：

| 层级 | 服务发现方式 |
|---|---|
| T0 | `RoutingHub` 内部 HashMap（已实现） |
| T1 | daemon 维护 agent 注册表，Unix socket 查询 |
| T2 | mDNS / DNS-SD 广播（同局域网自动发现） |
| T3 | 目录服务 / A2A Agent Registry（HTTP） |

```rust
#[async_trait]
pub trait ServiceDiscovery: Send + Sync {
    async fn register(&self, id: &AgentId, addr: &AgentAddr) -> Result<()>;
    async fn lookup(&self, id: &AgentId) -> Result<Option<AgentAddr>>;
    async fn list(&self) -> Result<Vec<(AgentId, AgentAddr)>>;
    async fn deregister(&self, id: &AgentId) -> Result<()>;
}

pub enum AgentAddr {
    InProcess,
    UnixSocket(PathBuf),
    Tcp(SocketAddr),
    Http(Url),
}
```

## 九、拓扑自动选择

`TopologySelector` 根据配置和场景自动选传输：

```rust
pub struct TopologyConfig {
    pub tier: TopologyTier,        // auto / t0 / t1 / t2 / t3
    pub protocol: ProtocolKind,    // auto / lim_native / a2a / mcp
    pub bind: Option<SocketAddr>,
    pub peers: Vec<SocketAddr>,
    pub discovery: Option<DiscoveryConfig>,
}

pub enum TopologyTier {
    Auto,              // 自动检测
    T0InProcess,
    T1CrossProcess,
    T2CrossNode,
    T3CrossNetwork,
}
```

**自动检测逻辑**（`Auto` 模式）：

```
1. 检测是否有 daemon socket 文件（~/.lim/daemon.sock）
   └─ 有 → T1CrossProcess（通过 daemon 路由）
2. 检测配置文件是否有 [topology.peers]
   └─ 有 → T2CrossNode
3. 否则 → T0InProcess（当前默认）
```

## 十、配置简化

### 10.1 三级配置

```toml
# ─── 默认：零配置 ───
# 不写 [topology] = T0 in-process，开箱即用

# ─── 常见：一行端点 + 鉴权 ───
[topology]
endpoint = "~/.lim/daemon.sock"       # 或 "https://lim-cluster.example.com"
auth_token = "${LIM_AUTH_TOKEN}"

# ─── 高级：完整显式 ───
[topology]
tier = "t2_cross_node"
protocol = "lim_native"

[topology.bind]
addr = "0.0.0.0:7878"

[topology.discovery]
method = "mdns"
service_name = "lim-agent"

[topology.auth]
method = "mtls"
cert_file = "~/.lim/cert.pem"
key_file = "~/.lim/key.pem"
```

### 10.2 endpoint 自动推断

| endpoint 模式 | 推断 tier | 推断 protocol | 推断 discovery |
|---|---|---|---|
| 无 endpoint | T0 | — | RoutingHub |
| `~/.lim/daemon.sock` / `.sock` 结尾 | T1 | lim_native | daemon 注册表 |
| `tcp://192.168.1.10:7878` | T2 | lim_native | 配置 peers + mDNS |
| `http://` / `https://` | T3 | a2a | A2A registry |
| `a2a://` | T3 | a2a | A2A registry |
| `mcp://` | T1/T3 | mcp | 配置 peers |

### 10.3 鉴权默认策略

按 tier 自动选：

- T0：无鉴权（同进程，信任边界内）
- T1：Unix socket 文件权限（`0600`，同用户）
- T2：可选 token，默认不强制
- T3：强制 token 或 mTLS

```toml
# 最简：环境变量 token
[topology]
endpoint = "https://lim-cluster.example.com"
auth_token = "${LIM_AUTH_TOKEN}"     # 自动用 Bearer token

# 高级：mTLS
[topology.auth]
method = "mtls"
cert_file = "~/.lim/cert.pem"
key_file = "~/.lim/key.pem"
ca_file = "~/.lim/ca.pem"
```

### 10.4 对应到集成指南的 L0-L3

| 集成复杂度 | 拓扑配置 | 用户需要写什么 |
|---|---|---|
| L0/L1/L2（单进程内） | T0，零配置 | 什么都不写 |
| L3a/L3b/L3c（单进程多 agent） | T0，零配置 | 什么都不写 |
| 多进程同机 | T1 | `endpoint = "auto"` |
| 多节点同网 | T2 | `endpoint = "tcp://0.0.0.0:7878"` |
| 跨网 A2A | T3 | `endpoint = "https://..."` + `auth_token` |

**90% 的用户只需要前两行**，这是设计目标。

## 十一、lim-transport crate 结构

```
crates/lim-transport/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── protocol.rs          ← ProtocolAdapter trait + LimNativeAdapter + A2AAdapter
│   ├── local_endpoint.rs    ← LocalEndpoint + 跨平台路径 + 自动降级
│   ├── local_socket.rs      ← LocalSocketTransport + LocalSocketServer
│   ├── unix_socket.rs       ← #[cfg(unix)] Unix socket 实现
│   ├── named_pipe.rs        ← #[cfg(windows)] Named Pipe 实现
│   ├── tcp.rs               ← TcpTransport (T2)
│   ├── a2a_http.rs          ← A2AHttpTransport (T3) + A2A Agent/Client
│   ├── mcp_server.rs        ← MCP Server 模式（lim 作为 MCP server）
│   ├── mcp_client.rs        ← MCP Client 模式（lim 调外部 MCP 工具）
│   ├── discovery.rs         ← ServiceDiscovery trait
│   ├── mdns.rs              ← mDNS 发现 (T2)
│   ├── registry.rs          ← 目录服务发现 (T3)
│   └── selector.rs          ← TopologySelector + 自动检测
└── tests/
    ├── local_socket_test.rs
    ├── a2a_test.rs
    └── mcp_test.rs
```

### Cargo.toml 平台条件依赖

```toml
[dependencies]
lim-core = { path = "../lim-core" }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"

# Unix 平台依赖
[target.'cfg(unix)'.dependencies]
libc = "0.2"

# Windows 平台依赖
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_System_Services",
    "Win32_System_Threading",
] }
```

**结果**：Linux/macOS binary 不会包含 Windows crate，反之亦然。每个平台的 binary 只包含自己需要的代码。

## 十二、关键设计决策

### 12.1 为什么不直接上 A2A？

in-process 能覆盖 80% 的实际场景。小说编写工具的编排者/写手/编辑，全在一个进程内用 mpsc 协作就够。直接上 A2A 会引入网络序列化、服务发现、连接管理的复杂度，对 80% 的用户是过度设计。

### 12.2 为什么把协议适配器和传输层分离？

协议和传输是正交的：
- A2A 协议可以跑在 HTTP（T3）上，也可以跑在 Unix socket（T1）上
- MCP 协议可以跑在 stdio（T1）上，也可以跑在 SSE（T3）上
- lim-native 可以跑在 mpsc（T0）/ Unix socket（T1）/ TCP（T2）上

分离后可以自由组合：`UnixSocketTransport + A2AAdapter` = 同机用 A2A 协议通信。

### 12.3 为什么 ACP 不单独实现？

IBM ACP（Agent Communication Protocol）已于 2026 年 4 月正式归档合并入 A2A。Zed ACP（Agent Client Protocol）虽然名义上独立，但 A2A 协议定义的 Client 角色完全覆盖编辑器集成场景。一套 A2A 协议栈足够，避免维护两套协议。

### 12.4 为什么 MCP 在 Tool 层不在 Transport 层？

MCP 是工具调用协议（agent ↔ 工具服务器），不是 agent 间通信协议。它的调用模式是"agent 决定调工具 → 工具执行 → 结果返回"，与 lim 的 `Tool` trait 一致。放在 Tool 层可以让 AgentLoop 调用 MCP 工具和调用内置工具完全一样。

### 12.5 为什么 T0 实现留 lim-agent？

in-process transport（`LocalChannelTransport` / `RoutingHub`）本质是 lim-agent 编排能力的一部分，和 `Scheduler` / `AgentLoop` 紧密配合，没有网络、没有序列化、没有服务发现。搬到 `lim-transport` 反而会让 `lim-agent` 反向依赖 `lim-transport`，层级倒置。

T0 留 lim-agent，T1+ 进 lim-transport——这是正确的分界线。

## 十三、风险与待讨论点

### 风险

1. **T1 daemon 单点**：daemon 挂了所有跨进程通信断。缓解：daemon 重启后 agent 自动重连
2. **T2 mDNS 不可靠**：局域网复杂时发现失败。缓解：fallback 到手动配置 peers
3. **T3 A2A 规范演进**：A2A 还在演进，可能 breaking change。缓解：协议适配器版本化
4. **消息大小**：大消息（如完整代码文件）跨网传输慢。缓解：消息分片 + 引用传递（传 file path 而非内容）

### 待讨论

1. **消息格式**：lim-native 用 serde_json 还是更紧凑的二进制（bincode/postcard）？跨网时二进制省带宽
2. **安全性**：T2/T3 需要 TLS + 身份认证。认证机制选什么（token / mTLS / OAuth）？
3. **MCP 版本**：MCP 规范也在演进，支持哪个版本？
