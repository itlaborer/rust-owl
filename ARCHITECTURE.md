# owl-rs 架构方案

> `go-owl`（Go，约 10.5 万行 / 605 个 .go）的 Rust 重写设计与骨架。
> 本阶段产出：**架构方案 + 可编译骨架**（第一阶段核心 CLI 范围的接口与结构）。

---

## 1. 重写范围与阶段

采用「核心优先、分批推进」策略，先落地能编译能跑的核心 CLI，再逐步补齐。

| 阶段 | 范围 | 状态 |
|---|---|---|
| **P1** | workspace 骨架、核心数据结构、命令树、配置、节点解析、批量执行框架、剧本解析/执行框架、历史库 schema、黑名单 | ✅ 完成 |
| P2 | russh 真实 SSH/SFTP、连接池打通、剧本条件/循环完整求值 | ✅ SSH/SFTP/增量同步完成；AI 助手待办 |
| **P3（部分）** | Web 控制台（原 cmd/plugins/serve）+ WebSSH | ✅ 已落地（owl-serve） |
| P3（其余） | 监控告警闭环、TUI、DuckDB 分析后端 | 待办 |

P1 覆盖原项目这些模块的结构还原：`internal/node`、`internal/ssh`、`internal/control/{command,blacklist,playbook,transfer,task,async}`、`internal/history`、`internal/common/config`、`cmd/cli/cmd`（命令树）。

---

## 2. Go → Rust 依赖映射

对齐用户既有工具链（`x86_64-pc-windows-gnu` 免 MSVC；rusqlite bundled；reqwest native-tls）。

| 用途 | Go 依赖 | Rust 选型 | 说明 |
|---|---|---|---|
| CLI 框架 | `spf13/cobra` | `clap` (derive) | 子命令/全局参数一一对应 |
| YAML | `gopkg.in/yaml.v3` | `serde_yaml` | 剧本/配置/黑名单 |
| JSON | 标准库 | `serde_json` | 节点存储、剧本 vars |
| SSH | `golang.org/x/crypto/ssh` | `russh` + `russh-keys` | 纯 Rust，无系统依赖，可交叉编译 |
| SFTP | 自实现 | `russh-sftp` | P2 引入 |
| SQLite | `modernc.org/sqlite` | `rusqlite` (bundled) | 免 CGO/系统库，GNU 目标可交叉 |
| 分析库 | `duckdb-go/v2` + Arrow | `duckdb` crate | P3，需 C++/CGO，仅本机 |
| HTTP/AI | 标准 `net/http` | `reqwest` (native-tls + json + stream) | 避开 openssl-sys 编译 |
| 日志 | `go.uber.org/zap` | `tracing` + `tracing-subscriber` | env-filter 控制级别 |
| 异步/并发 | goroutine + sync | `tokio` + `futures` + `dashmap` | 连接池、批量并发 |
| UUID | `google/uuid` | `uuid` (v4, serde) | batch_id / task_id |
| 时间 | `time` | `chrono` (serde) | 对齐 RFC3339 |
| 正则 | `regexp` | `regex` | 变量/条件解析 |
| 表格输出 | 自实现 | `comfy-table` | table/json/simple 三态 |
| TUI | `bubbletea`+`lipgloss` | `ratatui` + `crossterm` | P3 |
| 错误 | `error` | `anyhow`（顶层）+ `thiserror`（库） | 分层错误 |

---

## 3. Crate 拆分

```
owl-rs/
├── Cargo.toml                  # workspace，集中依赖版本
├── crates/
│   ├── owl-core/               # 全局配置、错误、跨模块模型（对应 internal/common, i18n）
│   ├── owl-node/               # 节点解析 local/api/ssh-config（对应 internal/node）
│   ├── owl-ssh/                # SSH 连接池 + 执行器（对应 internal/ssh, control/command）
│   ├── owl-exec/               # 批量执行 + 黑名单（对应 control/command, blacklist, async, task）
│   ├── owl-playbook/           # YAML 剧本解析/执行（对应 control/playbook, pkg/playbook）
│   ├── owl-transfer/           # 文件传输 + 扩散中继（对应 control/transfer）
│   ├── owl-history/            # SQLite 历史（对应 internal/history）
│   ├── owl-serve/              # Web 控制台 REST + WebSSH（对应 cmd/plugins/serve）
│   └── owl-cli/                # clap 命令树入口（对应 cmd/cli）
```

依赖关系（单向，无环）：
```
owl-cli ─┬─> owl-playbook ─> owl-exec ─> owl-ssh ─> owl-node ─> owl-core
         ├─> owl-transfer ─────────────> owl-ssh
         ├─> owl-history ──────────────────────────> owl-core
         └─> (all)
```

**P3 其余预留 crate**（按需加入 workspace）：
- `owl-ai` —— AI 助手（对应 internal/ai，1.5 万行，最复杂，单独一期）
- `owl-monitor` —— 监控告警（对应 internal/monitor，1 万行）
- `owl-tui` —— 终端界面

**已落地：`owl-serve`**（见 §6.8）。

---

## 4. 核心数据结构还原度

骨架里的结构与原项目源码字段**严格对齐**，不是凭空设计：

### 4.1 节点（原 `internal/node/resolver.go`）
```rust
ResolvedNode { id, name, address, port, user, groups, labels,
               ssh_key, ssh_password, proxy_jump, source }
```
三源回退解析：`local -> ssh-config`（P1），`api`（P2，需 Web 控制台）。

### 4.2 剧本（原 `internal/control/playbook/parser.go`）
```rust
Playbook { name, description, version, hosts, vars, execution_mode,
           default, pre_tasks, tasks, post_tasks }
PlaybookTask { name, action, args, when, with_items, loop_control,
               ignore_errors, any_errors_fatal, tags, register,
               changed_when, failed_when, timeout, retry }
```
执行模式：`pipeline`（禁 post_tasks / ignore_errors / any_errors_fatal）与默认 `fail_continue`。
`include` 为展开指令、循环包含检测、`action` 取空格首词 —— 均已还原。

### 4.3 执行模型（原 `CONTEXT.md` 术语表）
一次 `exec` 打 N 个节点 = N 个 `ExecResult`，共享 `batch_id`。Batch 只是 UI 分组概念，无后端实体。均已按此建模。

### 4.4 配置（原 `~/.owl/config.yaml` + `internal/ai/config.go`）
`owl_home()` 支持 `OWL_HOME` 覆盖，DB 路径支持 `OWL_DB_PATH` 覆盖，API key 走环境变量 —— 与原项目一致。

### 4.5 数据库 schema（原 `internal/history/db_sqlite3.go`）
P1 建核心三表：`command_executions` / `nodes` / `sessions`，索引对齐。完整表集（operations、node_communications、file_transfers、aichat、playbook_run_states 等）在 P2/P3 补齐。

### 4.6 文件传输（原 `internal/control/transfer` + `cmd/cli/cmd/file`）
Rust 侧用 **russh-sftp 3.0** 实现真实 SFTP 传输（原项目走 rsync/scp 外部命令 + 自扩散）：

- **链路**：复用 owl-ssh 的 `ConnectionManager` → `SshSession::open_sftp_stream()`（open channel → `request_subsystem("sftp")` → `into_stream()`）→ `SftpSession::new(stream)`。russh-sftp 核心库是 transport-agnostic 的（russh 仅为 dev 依赖），与 russh 0.65 无类型冲突。
- **能力**：分块读写（64 KiB 可配）、`overwrite` / `no-overwrite`、断点续传（远端/本地文件大小比对 + seek 续写）、远端父目录自动创建、单次传输超时、bytes 统计。
- **多节点**：传输层只做单节点；CLI 用 `join_all` 并行编排（对齐原项目 `parallelLimited`）。
- **增量同步（`owl-transfer::sync`）**：在单文件传输之上实现 rsync 默认 quick-check 语义的目录增量同步 —— 递归扫描两端清单（远端 `read_dir` 的目录项自带 stat，无需逐文件 STAT）→ 按「大小 → 修改时间（容差 1s）→ 可选 FNV-1a 内容校验和」生成计划 → 只对 Create/Update 传输、Skip 不传；`--delete` 删除目标端多余文件并清理空目录；`--dry-run` 只出计划；`--exclude` 支持 `*`/`?` 通配符。整次同步复用一条 SFTP 会话（抽出的 `upload_via` / `download_via` 接收 `&SftpSession`），避免逐文件重连。
- **差异**：原项目 `smartUpload` 优先 rsync（需远端 rsync + 免密）并在 rsync 不可用时回落 scp；Rust 版用 SFTP + quick-check 增量，不依赖远端/本地外部命令，跨平台更干净，但**不是** rsync 的 rolling-checksum 块级差量协议（不支持只传文件内变化块）；大文件小改动场景仍走整文件（可配合断点续传）。P2P 自扩散/中继（`diffusion` feature）留待后续阶段。

---

## 5. 命令树（对应 `cmd/cli/cmd/root.go`）

```
owl
├── node    { add | list | ping }        # P1
├── exec    --nodes/--groups/--label/--all --timeout
├── file    { upload | download | sync }  # sync: 增量同步（--pull/--delete/--checksum/--dry-run）
├── playbook { list | validate | run }
├── session                              # P1 占位
├── settings                             # P1 占位
├── history --limit
├── ai      [prompt]                     # P2
└── serve                                # P3
```

---

## 6. 环境搭建与验证说明

### 6.1 本机 Rust 环境（2026-11-21 已修复）

最初环境残缺：只有孤立的 `rustc 1.97.1`，`cargo`/`rustup` 是断链。修复步骤（已执行）：

1. 从 USTC 镜像下载 `rustup-init.exe`（官方源慢，USTC 快）：
   `https://mirrors.ustc.edu.cn/rust-static/rustup/dist/x86_64-pc-windows-gnu/rustup-init.exe`
2. 把残缺 toolchain 目录重命名为 `.broken-bak` 后，用 rustup-init 干净重装：
   ```bash
   RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static \
   RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup \
   rustup-init -y --no-modify-path \
     --default-toolchain stable-x86_64-pc-windows-gnu --profile minimal
   ```
   现为 **rustc/cargo 1.100.0**，target = `x86_64-pc-windows-gnu`。
3. `~/.cargo/config.toml` 配置 USTC crates.io 镜像（sparse 索引）加速下载。

### 6.2 GNU 目标必需的 C 工具链

russh 的 crypto 后端（无论 aws-lc-rs 还是 ring）都含 C 代码，GNU 目标**必须有 MinGW-w64 的 `gcc` + `dlltool`**，否则报
`failed to find tool "gcc.exe"` / `error calling dlltool`。已通过 winget 安装：

```powershell
winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e --accept-package-agreements
# bin 目录（已加入用户 PATH）：
# %LOCALAPPDATA%\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_...\mingw64\bin
```

### 6.3 crypto 后端选择

russh 默认用 `aws-lc-rs`（需 cmake + C++ 工具链，构建重）。已改为 **ring 后端**，避开 cmake/dlltool 的 aws-lc-sys 构建链：

```toml
russh = { version = "0.65", default-features = false, features = ["ring", "flate2", "rsa"] }
```

`reqwest` 选 `native-tls` 而非默认 `rustls`，对齐既有工具链、避开 openssl-sys 编译坑。

### 6.4 构建命令

```bash
cd owl-rs
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=gcc
cargo check --workspace              # 已验证通过（全部 crate）
cargo build -p owl-cli               # 产出 target/debug/owl.exe
cargo build --release
```

### 6.5 验证状态（2026-11-21）
- ✅ `cargo check --workspace` 通过（含 russh-sftp 真实传输 + CLI 接线）。
- ✅ `cargo build -p owl-cli` 成功产出 `owl.exe`（约 165 MB）。二进制 `file upload/download --help` 正常显示。
- ✅ 端到端冒烟：`node add` → `node list` 正确回显；`file upload` 对不可达节点走真实 SSH 连接并超时报错（os error 10060），证明 SFTP 链路非 stub。
- **沙箱构建经验**：本沙箱 rustc 子进程会僵死累积且不可回收，旧 target 目录的 proc-macro DLL 会被锁导致 `Permission denied`。
  解决法：用全新 `CARGO_TARGET_DIR`，`CARGO_BUILD_JOBS=1/2`，`cargo check/build` 反复增量重跑（每次前进一批），最终可完整编译。完整终端环境无此问题，一次 build 即可。
- **已修 bug**：`LocalNodeStore::save()` 写的是 `self.nodes`（裸 HashMap），原 `load_default()` 却按 `LocalNodeStore` 结构反序列化，解析失败被 `unwrap_or_default()` 吞掉 → 节点永远读为空。已改为对称反序列化 HashMap，node/file 命令的节点解析恢复正常。

### 6.6 增量同步追加状态（2026-11-21 晚）
- ✅ 代码完成：`owl-transfer::sync`（递归清单、size/mtime/checksum 计划、--delete、--dry-run、--exclude）、`upload_via`/`download_via` 会话复用重构、`owl file sync` CLI 接线、架构文档更新。
- ✅ `rustfmt --edition 2021 --check` 对三个改动文件无语法错误（仅格式差异）。
- ✅ **编译已恢复并通过（18:51）**：`owl.exe` 163MB 重新产出，`owl-transfer`（含 sync 模块）与 `owl-cli`（含 file sync）均编译成功；`owl file sync --help` 正常显示全部选项（--pull/--delete/--checksum/--dry-run/--exclude 等）。
- **卡死根因与解决**：`svchost.exe` 承载的 **DoSvc（传递优化服务）句柄泄漏至 6.4 万个**，导致系统句柄 24.5 万、`rustc` 新进程卡在内核 `Wait/Executive`。处理：`Restart-Service DoSvc`（句柄 64049→226，系统句柄降至 18.2 万）→ 删除 target 下 16 个被锁的 proc-macro DLL → 分轮 `cargo build` 增量推进至完成。75 个 `HasExited=true` 的 rustc 僵尸对象无法用 taskkill/Stop-Process/Terminate 清除（进程已退出，仅剩内核对象残留），但它们不再阻塞编译，无需重启机器。

### 6.7 端到端增量同步验证（2026-11-21 晚，本地 SFTP 服务）
使用 `crates/owl-transfer/examples/sftp_test_server.rs`（russh + russh-sftp 写的本地 SFTP 测试服务，把 SFTP 操作映射到本地目录）跑真实同步，7 个场景全部通过：

| 场景 | 结果 |
|---|---|
| 首次 push（4 文件含子目录） | 新增 4，49 字节，远端文件正确落盘 ✓ |
| 立即重跑（幂等） | 跳过 4，传输 0 字节 ✓ |
| 改 1 个文件（b.conf 内容变化） | 只更新该文件（大小 16→27，11 字节），其余 3 个跳过 ✓ |
| `--exclude '*.tmp'` | 4 文件只算 3 个 ✓ |
| `--delete`（源端删文件） | 远端多余文件被删除并清理空目录 ✓ |
| `--pull` 反向同步 | 只拉回本地缺失文件（15 字节），方向显示 pull ✓ |
| pull 幂等 | 跳过 3，传输 0 字节 ✓ |

- **验证中发现并修复的真实 bug**：`upload_via` / `download_via` 原来不保留文件 mtime——上传后远端 mtime 是写入时刻、下载后本地 mtime 是下载时刻，导致下次同步把所有文件误判为「修改时间不同」反复重传。修复：上传时 `setstat` 写回源 mtime（best-effort），下载后 `set_modified` 写回远端 mtime。修复后双向幂等均成立。
- **防御性修复**：`sync_dir` 中 `--remote /` 经 `trim_end_matches('/')` 会变空串，部分 SFTP 实现对空路径回落到客户端 cwd；已改为空串时保留 `/`。
- **测试环境注意**：Git Bash/MSYS 会把 `--remote /` 的 `/` 转成 cwd 路径，跑测试须加 `MSYS_NO_PATHCONV=1`（仅测试脚本问题，与 owl 代码无关）。
- 测试服务器保留为 example，后续可做文件传输回归测试：`cargo run -p owl-transfer --example sftp_test_server -- <root_dir> [port]`。

---

## 7. 实现优先级建议（P1 落地顺序）

1. **owl-core** —— 先定死配置/错误/模型，其余 crate 都依赖它。
2. **owl-node** —— 节点存储 + 解析，是 exec/transfer 的输入。
3. **owl-ssh** —— 接 russh，打通真实命令执行（P1 最关键的技术验证点）。
4. **owl-exec** —— 批量并发 + 黑名单门禁。
5. **owl-history** —— 落库执行记录。
6. **owl-playbook** —— 在 exec 之上做编排。
7. **owl-cli** —— 各子命令接真实实现，替换骨架占位。
8. **owl-transfer** —— SFTP 上传下载。

---

## 8. 风险与取舍

- **russh API 版本**：骨架锁定 `russh 0.65` / `russh-keys 0.49`（2026-11 查 crates.io 的最新稳定版）。0.x 版本 API 变动频繁，P2 接入时需按实际拉取版本微调握手与 channel 代码。其余关键版本：clap 4.6 / tokio 1.54 / rusqlite 0.43 / reqwest 0.13 / thiserror 2.0.21。
- **DuckDB / CGO**：分析后端需 C++ 工具链，不能随主 CLI 一起交叉编译，保持 feature 隔离（`owl-history/duckdb`）。
- **AI 模块复杂度**：原 `internal/ai` 1.5 万行（agent loop、73KB tools.go、intent 路由），单独一期，避免拖慢 P1 交付。
- **i18n / 字符编码**：原项目有独立的消息目录 + GBK/Big5 编码处理（`internal/i18n`、`internal/encoding`），Rust 侧 P2 用 `fluent`/`sys-locale` + `encoding_rs` 对齐。

---

## 9. Web 控制台 + WebSSH（owl-serve，2026-11-22 落地）

对应原项目 `cmd/plugins/serve`（3.3 万行 Go），Rust 侧用 axum 精简实现，复用已有的 owl-node / owl-ssh / owl-exec / owl-history，不重复造轮子。

### 9.1 crate 结构
```
owl-serve/
├── src/
│   ├── lib.rs      # run(host, port, token, timeout) 启动入口
│   ├── state.rs    # AppState：节点表(RwLock) + resolver + 可选令牌 + DB 路径
│   ├── api.rs      # axum Router + REST handlers + Bearer/?token 鉴权中间件
│   ├── ws.rs       # /api/ws/ssh WebSocket <-> 交互式 SSH shell 桥接
│   └── web.rs      # 内嵌 assets/index.html 单页 UI
└── assets/index.html  # 自包含前端（概览/节点/批量执行/WebSSH/历史 五视图）
```

### 9.2 交互式 Shell 底座（owl-ssh/shell.rs）
在已认证的 `SshSession` 上新开 session channel → `request_pty` → `request_shell`，
然后 `channel.split()` 成读半/写半，用两个 tokio task 桥接成 mpsc：
- 读半 task：`wait()` 收 `ChannelMsg::{Data, ExtendedData, ExitStatus, ExitSignal, Eof}`，转成 `ShellEvent` 推给 WebSocket。
- 写半 task：从 mpsc 收 `ShellInput::{Data, Resize, Eof}`，调 `data_bytes` / `window_change` / `eof`。
- 上层 WebSocket 只面对字节流 mpsc，不接触 russh 类型。
- API 校验要点（russh 0.65.2）：channel 方法是 request_shell（非 shell）、eof（非 send_eof）、
  pty modes 传 `[(Pty::TTY_OP_END, 0)]`；`channel_open_session()` 返回 `Channel<client::Msg>`。

### 9.3 REST 端点
| 方法 | 路径 | 用途 |
|---|---|---|
| GET | /api/health | 健康检查 + 版本 |
| GET/POST | /api/nodes | 列出 / 新增节点（落 nodes.json，与 CLI 共享） |
| POST | /api/nodes/delete | 删除节点 |
| POST | /api/nodes/ping | 真实 SSH 连通性测试 |
| POST | /api/exec | 批量执行（复用 BatchExecutor + 黑名单 + 落历史库） |
| GET | /api/history?limit= | 查询执行历史 |
| WS | /api/ws/ssh?node=&cols=&rows=&term= | WebSSH 交互终端 |

鉴权：`--token`（或 `OWL_WEB_TOKEN`）启用后，HTTP 走 `Authorization: Bearer` / `?token=`，WS 走 `?token=`；不设则不鉴权（仅建议本机）。

### 9.4 前端
单页 HTML，五个视图，xterm.js + fit addon 走 jsdelivr CDN（离线需替换脚本地址）。
WebSSH 传输约定：客户端 Binary 帧 = 键盘字节，Text 帧 = `{type:resize|eof}` JSON；
服务端 Binary 帧 = 终端输出，Text 帧 = `{type:ready|exit}` JSON。

### 9.5 构建踩坑（本次）
- axum 依赖树大，首次 `cargo check` 会因联网更新 index 卡住（日志停在 manifest warning、CPU 停滞、target 零写入）。
  解法：`cargo check --offline`（依赖已在 registry 缓存），正常推进。
- 历史遗留 116 个 `HasExited=true` 的 rustc 内核对象残留，Stop-Process 清不掉但不消耗 CPU、不阻塞编译，无需处理。
