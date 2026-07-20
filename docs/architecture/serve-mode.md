# lim serve 统一入口设计

> 状态：已实现（v0.1 已交付核心：foreground/daemon/system 三模式 + MCP/A2A/goal daemon 协议组合 + `[serve]` 配置段 + Handler 注入。v0.2 补全 IPC 跨进程 goal 提交等）
>
> 相关文档：[transport-layer.md](transport-layer.md)（分层传输架构）、[loop-engineering.md](loop-engineering.md)（goal daemon）

## 一、设计目标

把所有"持续运行"的模式统一成 `lim serve` 一个入口，替代当前散落的 `loop-daemon` 命令，并为 v0.2 的 MCP/A2A 协议层提供统一承载。

核心目标：

| 目标 | 说明 |
|---|---|
| **统一入口** | 所有"持续运行"场景（MCP server / A2A agent / goal daemon）收敛到 `lim serve` |
| **协议 + 能力正交** | 协议层（MCP/A2A）和能力层（goal daemon/agent runtime）可自由组合 |
| **运行模式可选** | foreground / --daemon / --system 三种模式，每个协议组合都适用 |
| **跨平台** | 后台托管和系统服务在 Linux/macOS/Windows 上都有原生实现 |

## 二、统一后的命令体系

### 2.1 命令一览

```bash
# ─── 前台模式（默认，终端打印日志） ───
lim serve                              # 默认：all 协议 + goal daemon
lim serve --protocol mcp               # 只开 MCP server（测试用）
lim serve --protocol a2a               # 只开 A2A agent（测试用）
lim serve --protocol none              # 只跑 goal daemon

# ─── 后台托管模式 ───
lim serve --daemon                     # 后台启动，关闭终端不影响
lim serve --daemon --protocol mcp      # 后台 + 单协议（测试用）

# ─── 系统服务模式 ───
lim serve --system install             # 安装为开机自启
lim serve --system uninstall           # 卸载
lim serve --system start               # 启动（等同 systemctl start）
lim serve --system stop                # 停止
lim serve --system status              # 查看状态
lim serve --system restart             # 重启
```

### 2.2 现有命令归并

| 现有命令 | 统一后 | 说明 |
|---|---|---|
| `lim loop-daemon start` | `lim serve` | goal daemon 是 serve 的常驻能力（无需 flag） |
| `lim loop-daemon submit` | `lim goal submit`（独立保留） | 提交 goal，不需要 serve 运行 |
| `lim loop-daemon list/status/sync` | `lim goal list/status/sync`（独立保留） | 查询，不需要 serve 运行 |
| `lim loop-daemon stop` | `lim serve --system stop` 或 `lim serve --daemon stop` | 停止 serve 实例 |
| (规划的) `lim mcp serve` | `lim serve --protocol mcp` | MCP Server（测试用） |
| (规划的) `lim a2a serve` | `lim serve --protocol a2a` | A2A Agent（测试用） |

**关键**：`goal submit/list/status/sync` 等查询命令独立保留，不依赖 serve 运行——它们直接操作 daemon ledger 存储。只有 `start`（运行 goal runner）归并到 serve。goal daemon 本身是 serve 的常驻能力，默认开启，无需任何 flag。

### 2.3 与现有命令的关系

```
lim 命令体系（v0.2 后）
├── 一次性
│   ├── lim run -p "..."              ← 一次性 CLI（保留）
│   └── lim goal submit/list/...      ← goal 查询（保留，直接操作存储）
│
├── 交互式
│   └── lim tui                       ← 交互式 TUI（保留）
│
└── 持续运行
    └── lim serve [options]           ← 统一入口（新）
        │
        ├── 运行模式
        │   ├── (默认) foreground
        │   ├── --daemon              ← 后台托管
        │   └── --system <action>     ← 系统服务
        │
        ├── 协议层（对外暴露）
        │   ├── (默认) all            ← MCP + A2A 全开
        │   ├── --protocol mcp        ← 仅 MCP（测试用）
        │   ├── --protocol a2a        ← 仅 A2A（测试用）
        │   └── --protocol none       ← 无协议（仅 goal daemon）
        │
        └── 能力层（对内，常驻）
            ├── goal daemon           ← Loop Engineering goal 管理（默认开）
            ├── --workspace <path>    ← 绑定工作区
            └── --config <path>       ← 配置文件
```

## 三、运行模式三层

### 3.1 foreground（默认）

前台运行，终端实时打印日志。适合开发调试。

```bash
lim serve
# 默认：all 协议（MCP + A2A）+ goal daemon
# 日志直接输出到 stderr，Ctrl+C 退出
```

### 3.2 --daemon（后台托管）

后台启动，关闭终端不影响。适合日常使用。

lim 采用 **respawn** 模式（spawn detached child + exec）而非传统的 `fork + setsid`。原因：lim 使用 tokio 多线程 runtime，`fork` 只保留调用线程、其他线程直接消失，会损坏 runtime 状态（锁泄漏、信号管道断裂等）。respawn 方式 spawn 一个全新的子进程重新 `exec` 自己的二进制，子进程从 `main()` 重新初始化，状态干净。

| 平台 | 实现方式 | 关闭终端后存活 |
|---|---|---|
| Linux | `spawn` detached child + `pre_exec` 调 `setsid` | ✅ |
| macOS | 同 Linux（POSIX `setsid`） | ✅ |
| Windows | `CreateProcess` with `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP` | ✅ |

**通用逻辑**：

1. 主进程（带 `--daemon`）调用 `daemonize(args, log_dir)`
2. `daemonize` 用 `Command::new(current_exe)` spawn 子进程，参数为 `serve <args>`（**不带** `--daemon`）
3. 子进程 I/O 重定向到日志文件（`~/.lim/logs/lim.log` + `.err.log`）
4. Unix: 子进程在 `exec` 前通过 `pre_exec` 调用 `setsid` 创建新会话、脱离控制终端
5. Windows: 子进程用 `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` 创建标志
6. 父进程把子进程 PID 写入 `~/.lim/lim.pid`
7. 父进程 `std::mem::forget(child)`（避免 Drop 杀死子进程）后退出

**停止**：`lim serve --daemon stop` 读 PID 文件发 SIGTERM（Unix）/ `TerminateProcess`（Windows）。

### 3.3 --system（系统服务）

安装为系统服务，开机自启。适合长期运行的场景。

| 平台 | 服务管理器 | 安装位置 | 用户级 vs 系统级 |
|---|---|---|---|
| Linux | systemd | `~/.config/systemd/user/lim.service` | 默认用户级（`systemctl --user`） |
| macOS | launchd | `~/Library/LaunchAgents/ai.lim.serve.plist` | 用户级 LaunchAgent |
| Windows | Windows Service | `sc.exe create lim ...` | 默认用户级（不要求管理员） |

**用户级优先**的原因：lim 是开发工具，不需要系统级权限，用户级服务不需要 root/admin 安装。

## 四、跨平台系统服务实现

### 4.1 ServiceManager trait

```rust
/// Daemon lifecycle manager (cross-platform).
pub trait ServiceManager: Send + Sync {
    /// Install as a system service.
    fn install(&self, config: &ServiceConfig) -> Result<()>;
    /// Uninstall the system service.
    fn uninstall(&self) -> Result<()>;
    /// Start the service.
    fn start(&self) -> Result<()>;
    /// Stop the service.
    fn stop(&self) -> Result<()>;
    /// Get current status.
    fn status(&self) -> Result<ServiceStatus>;
    /// Restart the service.
    fn restart(&self) -> Result<()> { self.stop()?; self.start() }
}

pub struct ServiceConfig {
    pub name: String,           // "lim"
    pub description: String,
    pub exec_path: PathBuf,     // lim binary path
    pub args: Vec<String>,      // ["serve"]  (默认 all 协议 + goal daemon)
    pub working_dir: Option<PathBuf>,
    pub log_dir: PathBuf,       // ~/.lim/logs/
    pub auto_restart: bool,
}

pub enum ServiceStatus {
    Running { pid: u32, started_at: SystemTime },
    Stopped,
    Failed { error: String },
    Unknown,
}
```

### 4.2 平台实现

```rust
// 工厂函数
pub fn service_manager() -> Result<Box<dyn ServiceManager>> {
    #[cfg(target_os = "linux")]
    { Ok(Box::new(systemd::SystemdServiceManager::new()?)) }

    #[cfg(target_os = "macos")]
    { Ok(Box::new(launchd::LaunchdServiceManager::new()?)) }

    #[cfg(target_os = "windows")]
    { Ok(Box::new(windows::WindowsServiceManager::new()?)) }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    { Err(LimError::Config("system service not supported on this platform".into())) }
}

// ─── Linux systemd ───
#[cfg(target_os = "linux")]
mod systemd {
    pub struct SystemdServiceManager { /* ... */ }
    // 生成 ~/.config/systemd/user/lim.service（用 generate_unit 函数）
    // 调用 systemctl --user daemon-reload/enable/start/stop
    // status 通过 systemctl --user is-active + show --property=MainPID
}

// ─── macOS launchd ───
#[cfg(target_os = "macos")]
mod launchd {
    pub struct LaunchdServiceManager { /* ... */ }
    // 生成 ~/Library/LaunchAgents/ai.lim.serve.plist（用 generate_plist 函数）
    // 调用 launchctl load/unload/start/stop
    // status 通过 launchctl list 解析 PID
}

// ─── Windows Service ───
#[cfg(target_os = "windows")]
mod windows {
    pub struct WindowsServiceManager { /* ... */ }
    // 通过 sc.exe 调用 SCM（install/uninstall/start/stop/status）
    // 非 windows crate 直调，避免 ServiceMain + rust 生命周期管理的复杂性
}
```

**实现状态**：Linux systemd ✅ / macOS launchd ✅ / Windows `sc.exe` ✅

### 4.3 生成的 service 单元文件

**Linux systemd**（`~/.config/systemd/user/lim.service`）：

```ini
[Unit]
Description=lim agent server
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/lim serve
Restart=on-failure
RestartSec=5
StandardOutput=append:%h/.lim/logs/lim.log
StandardError=append:%h/.lim/logs/lim.err.log

[Install]
WantedBy=default.target
```

**macOS launchd**（`~/Library/LaunchAgents/ai.lim.serve.plist`）：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>ai.lim.serve</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/lim</string>
    <string>serve</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/Users/you/.lim/logs/lim.log</string>
  <key>StandardErrorPath</key>
  <string>/Users/you/.lim/logs/lim.err.log</string>
</dict>
</plist>
```

**Windows Service**：通过 `windows` crate 注册到 SCM，ServiceMain 启动 lim serve 内部逻辑。

## 五、后台托管跨平台实现

### 5.1 daemonize 函数（respawn 模式）

lim 的 daemon 模式采用 **respawn** 而非传统 `fork + setsid`。原因见 §3.2：tokio 多线程 runtime 与 `fork` 不兼容。

```rust
/// Spawn a detached child process running `lim serve <args>` and write
/// its PID to the PID file. `args` should be the serve arguments
/// **without** `--daemon`. The child re-executes the current binary
/// with `serve` prepended.
pub fn daemonize(args: &[String], log_dir: &Path) -> Result<u32> {
    let exe = std::env::current_exe()?;
    let stdout_path = log_dir.join("lim.log");
    let stderr_path = log_dir.join("lim.err.log");

    // 打开日志文件（append 模式）
    let stdout = OpenOptions::new().create(true).append(true).open(&stdout_path)?;
    let stderr = OpenOptions::new().create(true).append(true).open(&stderr_path)?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve");
    for a in args { cmd.arg(a); }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    // Unix: 子进程 exec 前调 setsid 创建新会话、脱离控制终端
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // Windows: DETACHED_PROCESS + CREATE_NEW_PROCESS_GROUP
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    let child = cmd.spawn()?;
    let pid = child.id();

    // 写 PID 到 ~/.lim/lim.pid
    let pid_file = PidFile::new(&PidFile::default_path()?);
    pid_file.write(pid)?;

    // 关键:forget child,避免 Drop kill 子进程
    std::mem::forget(child);
    Ok(pid)
}
```

**为什么不用 fork + setsid**:

1. **tokio runtime 损坏**:tokio 多线程 runtime 有多个 worker/IO/timer 线程。`fork` 只保留调用线程,其他线程直接消失,runtime 状态不一致(锁泄漏、信号管道断裂)。
2. **锁状态损坏**:fork 时若有线程持锁(如 glibc malloc/printf 内部锁),子进程里这些锁永远不释放,再 lock 就死锁。
3. **tokio 官方警告**:tokio 文档明确说 fork 与多线程 runtime 不兼容。

**respawn 的优势**:

- 子进程从 `main()` 重新初始化,状态干净(无遗留 runtime/锁/线程)
- 跨平台:Unix `posix_spawn` + Windows `CreateProcess` 统一抽象
- 与 `--system` 模式复用同一套 `to_serve_args()` 序列化逻辑
- 唯一代价:子进程重新加载二进制 + 初始化,对长期运行的 daemon 可忽略

**respawn 模式的 PID 文件语义**:PID 文件记录的是子进程(实际运行的 daemon)的 PID,不是父进程。父进程 spawn 后立即退出,`stop_daemon` 读 PID 文件向子进程发信号。

### 5.2 PID 文件管理

```rust
/// ~/.lim/lim.pid 文件管理
pub struct PidFile {
    path: PathBuf,  // ~/.lim/lim.pid
}

impl PidFile {
    pub fn write(&self, pid: u32) -> Result<()>;
    pub fn read(&self) -> Result<Option<u32>>;
    pub fn remove(&self) -> Result<()>;
    /// 检查 PID 对应的进程是否还活着
    pub fn is_running(&self) -> bool;
}
```

## 六、lim-serve crate 结构

```
crates/lim-serve/
├── Cargo.toml
├── src/
│   ├── lib.rs                 ← crate 入口 + re-exports
│   ├── options.rs             ← ServeOptions / RunMode / ProtocolKind / ServiceConfig / SystemAction + to_serve_args()
│   ├── runner.rs              ← ServeRunner：协议编排 + 运行模式分发 + 信号处理
│   ├── a2a_server.rs          ← A2A HTTP server（最小 HTTP/1.1 实现）
│   ├── daemon.rs              ← --daemon 后台托管（respawn 模式，跨平台）
│   ├── pid_file.rs            ← PID 文件管理（~/.lim/lim.pid）
│   └── service_manager.rs     ← ServiceManager trait + systemd/launchd 平台实现 + factory
└── tests/                     ← （当前测试内联在各模块的 #[cfg(test)] mod tests 中）
```

**关键设计**:

- **Builder 模式注入 handler**:`ServeRunner::new(options).with_mcp_handler(h).with_a2a_handler(h).run()`
- **三路 shutdown**:Ctrl+C + SIGTERM + 外部 CancellationToken,`tokio::select!`(SIGTERM 提取为自由函数避免 `#[cfg]` 在 select 分支内)
- **to_serve_args()**:`ServeOptions` 序列化为 CLI args,daemon respawn 和 system service ExecStart 复用同一套逻辑
- **MCP/A2A handler 注入**:`Arc<dyn McpToolHandler>` / `Arc<dyn A2ARequestHandler>`,通过 lim-transport 的 blanket impl 支持 trait object

### 6.1 ServeRunner 核心逻辑

```rust
/// Configuration for `lim serve`.
pub struct ServeOptions {
    pub protocol: ProtocolSet,         // all（默认）/ mcp / a2a / none
    pub mode: RunMode,                 // foreground / daemon / system
    pub workspace: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub bind_endpoint: Option<String>, // endpoint URL（auto / unix:// / tcp:// / http://）
    pub auth_token: Option<String>,    // A2A / 网络协议鉴权
}
// 注：goal daemon 是 serve 的常驻能力，无对应 flag

pub enum ProtocolSet {
    None,                               // 仅 goal daemon
    Single(ProtocolKind),               // 单协议（测试用）
    All,                                // 全部协议（默认）
}

pub enum ProtocolKind {
    Mcp,                                // MCP server (stdio)
    A2a,                                // A2A agent (HTTP)
}

pub enum RunMode {
    Foreground,
    Daemon,
    System(SystemAction),
}

pub enum SystemAction {
    Install,
    Uninstall,
    Start,
    Stop,
    Status,
    Restart,
}

/// The main serve runner.
pub struct ServeRunner {
    options: ServeOptions,
    mcp_handler: Option<Arc<dyn McpToolHandler>>,
    a2a_handler: Option<Arc<dyn A2ARequestHandler>>,
    goal_runner: Option<Arc<dyn GoalRunner>>,
    shutdown: CancellationToken,
}

impl ServeRunner {
    pub fn new(options: ServeOptions) -> Self { /* ... */ }
    pub fn with_mcp_handler(mut self, handler: Arc<dyn McpToolHandler>) -> Self { /* ... */ }
    pub fn with_a2a_handler(mut self, handler: Arc<dyn A2ARequestHandler>) -> Self { /* ... */ }
    pub fn with_goal_runner(mut self, runner: Arc<dyn GoalRunner>) -> Self { /* ... */ }
    pub fn shutdown_token(&self) -> CancellationToken { /* ... */ }

    pub async fn run(self) -> Result<()> {
        match self.options.mode.clone() {
            RunMode::Foreground => self.run_foreground().await,
            RunMode::Daemon => self.run_daemon().await,
            RunMode::System(action) => self.run_system_action(action).await,
        }
    }

    async fn run_foreground(self) -> Result<()> {
        // 1. 按 protocol spawn 各 server task（MCP via McpServer / A2A via A2AServer）
        // 2. spawn_daemon_server()：启动 DaemonServer（含 LoopRunnerGoalRunner 桥接）
        // 3. wait_for_shutdown()：Ctrl+C + SIGTERM + CancellationToken 三路 select
        // 4. cancel + join 所有 task
    }

    async fn run_daemon(self) -> Result<()> {
        // 1. 检查是否已有 daemon 运行（daemon_pid()）
        // 2. daemonize(to_serve_args(), log_dir) respawn 子进程
        // 3. 打印 PID 后退出
    }

    async fn run_system_action(self, action: SystemAction) -> Result<()> {
        let manager = crate::service_manager::service_manager()?;
        match action {
            SystemAction::Install => {
                let config = ServiceConfig::from_current_exe(&self.options, log_dir)?;
                manager.install(&config)?;
            }
            SystemAction::Uninstall => manager.uninstall()?,
            SystemAction::Start => manager.start()?,
            SystemAction::Stop => manager.stop()?,
            SystemAction::Status => { println!("{}", manager.status()?); }
            SystemAction::Restart => manager.restart()?,
        }
        Ok(())
    }
}
```

### 6.2 Cargo.toml

```toml
[dependencies]
lim-core = { workspace = true }
lim-agent = { workspace = true }
lim-transport = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }

# Unix 平台依赖：libc（setsid 等 POSIX 调用）
[target.'cfg(unix)'.dependencies]
libc = "0.2"

# Windows 平台依赖：windows crate（SCM API，当前为 stub）
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_System_Services",
    "Win32_System_Threading",
    "Win32_System_Environment",
] }

[dev-dependencies]
tempfile = "3"
```

**结果**：Linux/macOS binary 不会包含 Windows crate，反之亦然。每个平台的 binary 只包含自己需要的代码。

## 七、协议 + 能力正交组合

### 7.1 典型组合

| 场景 | 命令 | 说明 |
|---|---|---|
| 开发调试 | `lim serve` | 前台，看日志 |
| 日常使用 | `lim serve --daemon` | 后台托管 |
| 开机自启 | `lim serve --system install` | 系统服务 |
| Claude Code 调 lim | `lim serve --protocol mcp --daemon` | MCP Server 后台 |
| Zed 编辑器调 lim | `lim serve --protocol a2a --daemon` | A2A Agent 后台 |
| 团队共享 lim 实例 | `lim serve --protocol a2a --system install` | 系统服务 |
| Loop Engineering 长期跑 | `lim serve --system install` | goal daemon 系统服务 |
| 全功能 lim 节点 | `lim serve --system install` | all 协议 + goal daemon 系统服务 |

### 7.2 一个进程多角色

`lim serve`（默认 all 协议 + goal daemon）是一个进程，同时：

- **对外暴露**：MCP Server（被其他 agent 调）+ A2A Agent（被编辑器/外部 agent 调）
- **对内管理**：goal daemon（运行 Loop Engineering goal）

这是设计的关键——一个进程可以同时承担多个角色，协议和能力是正交维度。

## 八、配置示例

### 8.1 serve 配置段

`[serve]` 配置段为 `lim serve` 提供默认值，CLI flag 覆盖配置值。当整个 `[serve]`
段缺失时，使用内置默认（`all` 协议、foreground 模式、`.lim/daemon` 存储）。

```toml
# .lim/config.toml

[serve]
# 默认协议（CLI --protocol 可覆盖）
#   "all"   — MCP + A2A 全开（默认）
#   "mcp"   — 仅 MCP（测试用）
#   "a2a"   — 仅 A2A（测试用）
#   "none"  — 仅 goal daemon
protocol = "all"
# 默认运行模式（CLI --daemon / --system 可覆盖）
#   "foreground" — 前台运行（默认）
#   "daemon"     — 后台托管
# 注：--system 是一次性动作（install/uninstall/...），不是 run mode，不可在此配置
mode = "foreground"

[serve.goal_daemon]
# goal daemon 是 serve 的常驻能力，默认开启
enabled = true
# daemon ledger 存储根（遵循 containment 原则，留在项目目录内）
storage = ".lim/daemon"

[serve.system]
# 系统服务安装元数据（lim serve --system install 使用）
service_name = "lim"
auto_restart = true
log_dir = ".lim/logs"
```

**未实现的子段**：`[serve.mcp]` / `[serve.a2a]` 的细粒度配置
（transport / tool_prefix / expose / endpoint / auth_token）暂未落地，这些
参数目前通过 CLI flag（`--endpoint`、`--auth-token`）控制。后续版本会补全。

### 8.2 用户日常一句话

```bash
# 安装为系统服务（一次性）
lim serve --system install

# 之后开机自启，无需关心
# 手动控制：
lim serve --system start
lim serve --system stop
lim serve --system status
```

## 九、闭环：daemon 设计的收敛

### 9.1 之前 daemon 的散点

```
现状（散点）：
├── lim loop-daemon start/stop/submit/list   ← goal 管理，in-process
├── lim tui                                   ← 交互式，前台
├── lim run -p "..."                          ← 一次性
└── (规划的) lim mcp serve / lim a2a serve    ← 对外暴露，未实现
```

### 9.2 统一后的闭环

```
统一后：
lim run -p "..."                              ← 一次性 CLI（保留）
lim tui                                       ← 交互式 TUI（保留）
lim goal submit/list/status/sync              ← goal 查询（保留，直接操作存储）
lim serve [options]                           ← 所有"持续运行"统一入口
  │
  ├── 运行模式
  │   ├── (默认) foreground
  │   ├── --daemon
  │   └── --system install/start/stop/...
  │
  ├── 协议层（对外暴露）
  │   ├── (默认) all            ← MCP + A2A 全开
  │   ├── --protocol mcp        ← 仅 MCP（测试用）
  │   ├── --protocol a2a        ← 仅 A2A（测试用）
  │   └── --protocol none       ← 仅 goal daemon
  │
  └── 能力层（对内）
      ├── --workspace <path>
      └── --config <path>
```

**闭环的关键**：

1. **`loop-daemon start` 不再独立**——它的能力（goal 管理）变成 `lim serve` 的常驻能力
2. **MCP/A2A server 不再独立**——它们是 `lim serve --protocol X` 的协议选择
3. **前台/后台/系统服务是正交维度**——任何协议组合都可以用任何运行模式
4. **一个进程可以同时承担多个角色**——`lim serve`（默认 all + goal daemon）

### 9.3 迁移策略

v0.2 发布时：

- `lim serve` 新命令上线，完整功能
- `lim loop-daemon start` 标记为 deprecated，内部转发到 `lim serve`
- `lim loop-daemon submit/list/status/sync` 重命名为 `lim goal submit/list/status/sync`（旧名保留别名一个版本）
- v0.3 移除 deprecated 别名

## 十、CI 测试矩阵

```yaml
# .github/workflows/ci.yml
strategy:
  matrix:
    include:
      - os: ubuntu-latest
        serve_test: [foreground, daemon, system_service]
      - os: macos-latest
        serve_test: [foreground, daemon, system_service]
      - os: windows-latest
        serve_test: [foreground, daemon, system_service]
```

**测试覆盖**：

- 三平台 foreground 模式启动 + 优雅关闭
- 三平台 --daemon 后台启动 + stop
- 三平台 --system install/uninstall/start/stop/status
- MCP Server 模式功能测试
- A2A Agent 模式功能测试
- goal daemon 模式功能测试（默认开启）
- 聚合模式（all 协议 + goal daemon）功能测试

## 十一、关键设计决策

### 11.1 为什么统一到 serve 而不是保留独立命令？

独立命令（`lim mcp serve` / `lim a2a serve` / `lim loop-daemon start`）会导致：
- 用户需要记住多个命令
- 协议组合（MCP + A2A）需要跑多个进程
- 运行模式（foreground/daemon/system）每个命令都要实现一遍

统一到 `lim serve` 后：
- 一个命令，`--protocol` 和 `--mode` 正交组合
- 一个进程可以同时承担多角色
- 运行模式只实现一遍

### 11.2 为什么用户级服务优先？

lim 是开发工具，不需要系统级权限。用户级服务（`systemctl --user` / LaunchAgent / 用户级 Windows Service）：
- 不需要 root/admin 安装
- 用户登出时可以选择是否继续运行
- 卸载简单，不残留系统配置

### 11.3 为什么 goal 查询命令独立保留？

`goal submit/list/status/sync` 是查询和提交操作，不需要 serve 运行。它们直接操作 daemon ledger 存储（JSON 文件），是离线操作。只有 `start`（运行 goal runner）需要 serve 进程。

这保留了"先提交 goal，再启动 serve 执行"的工作流。

### 11.4 为什么不引入 `windows-service` crate？

`windows-service` crate 封装了 Windows Service API，但 lim 的 Windows Service 实现较简单（注册 ServiceMain + 启动 serve 逻辑），直接用 `windows` crate 的 Win32 API 足够，避免额外依赖。`windows` crate 是微软官方维护的，覆盖所有 Win32 API。
