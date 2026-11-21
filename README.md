# owl-rs

智能分布式运维 CLI 的 Rust 重写版，源自 [go-owl](https://github.com/cangyunye/go-owl)（Go，约 10.5 万行 / 605 个文件）。

采用「核心优先、分批推进」：先把能编译、能跑通的核心链路做实，再逐步补齐 Web / AI / 监控等模块。详见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 当前状态

| 能力 | 状态 | 说明 |
|---|---|---|
| 节点管理 `node add/list/ping` | ✅ 可用 | 本地 JSON 存储，ping 走真实 SSH 连接 |
| 批量执行 `exec` | ✅ 可用 | russh 真实命令执行，多节点并发，表格输出 |
| 文件上传/下载 `file upload/download` | ✅ 可用 | SFTP，断点续传、覆盖策略、mtime 保留 |
| 目录增量同步 `file sync` | ✅ 可用 | rsync 风格 quick-check，只传变化文件 |
| 剧本 `playbook validate` | ✅ 可用 | YAML 解析校验 |
| 剧本 `run` / 会话 / 设置 / 历史 | 🚧 骨架 | 命令已接线，逻辑待实现 |
| AI 助手 `ai` / Web 控制台 `serve` | 🚧 待办 | 规划中的 P2 / P3 |

## 工作区结构

```
owl-rs/
├── Cargo.toml                  # workspace，依赖版本集中管理
├── ARCHITECTURE.md             # 架构方案与 Go→Rust 依赖映射
└── crates/
    ├── owl-core/               # 全局配置、错误、跨模块模型
    ├── owl-node/               # 节点解析与本地存储
    ├── owl-ssh/                # SSH 连接池 + 命令执行器（russh）
    ├── owl-exec/               # 批量执行 + 黑名单
    ├── owl-playbook/           # YAML 剧本解析/执行
    ├── owl-transfer/           # SFTP 传输 + rsync 式增量同步
    ├── owl-history/            # SQLite 历史（rusqlite bundled）
    └── owl-cli/                # clap 命令树入口
```

依赖关系单向无环：`owl-cli → {playbook, transfer, history, ...} → owl-ssh → owl-node → owl-core`。

## 构建

需要 Rust 1.85+（edition 2021）。

```bash
cargo build -p owl-cli
# 产物：target/debug/owl.exe（Windows）/ target/debug/owl（Linux/macOS）
cargo check --workspace
```

### Windows GNU 目标说明

本项目默认走 `x86_64-pc-windows-gnu`（免 MSVC），因此做了两处关键取舍，已写入配置：

- **SSH 加密后端**：`russh` 关闭默认的 `aws-lc-rs`（需要 cmake + C++ + dlltool），改用 `features = ["ring", "flate2", "rsa"]`。ring 用预编译 C + `cc`，只需 MinGW-w64 的 `gcc` / `dlltool`。
- **SQLite**：`rusqlite` 用 `bundled`，免系统 CGO / 数据库库。
- **HTTP**：`reqwest` 用 `native-tls`，避开 `openssl-sys` 编译。

若构建时报 `failed to find tool gcc.exe` / `error calling dlltool`，先安装 MinGW-w64（例如 `winget install BrechtSanders.WinLibs.POSIX.UCRT`）并确保 `gcc`、`dlltool` 在 PATH 中。

## 用法

### 节点管理

```bash
owl node add web01 --address 10.0.0.11 --user root --port 22 --groups prod
owl node list
owl node ping web01
```

节点保存在本地 `nodes.json`。可用 `OWL_HOME` 环境变量隔离配置目录。

### 批量执行命令

```bash
owl exec "uptime" --nodes web01,web02
owl exec "df -h" --groups prod
owl exec "uname -a" --all --timeout 30
```

未指定 `--nodes` / `--groups` / `--all` 时会拒绝执行，避免误操作全网。

### 文件传输

```bash
# 上传（dest 以 / 结尾视为目录）
owl file upload ./app.tar.gz --nodes web01,web02 -d /tmp/

# 下载
owl file download /var/log/app.log --node web01 -d ./logs/

# 目录增量同步：先看计划，不传输
owl file sync ./dist --nodes web01 -d /opt/app --dry-run

# 实际同步：只传变化文件，并删除目标端多余文件
owl file sync ./dist --nodes web01 -d /opt/app --delete

# 反向拉取：远端 → 本地
owl file sync /opt/app --nodes web01 -d ./dist --pull
```

`file sync` 的判定顺序：目标端不存在 → 新增；大小不同 → 更新；mtime 不同（1 秒容差）→ 更新；加 `--checksum` 则再比对内容（流式 FNV-1a）。一致的文件直接跳过。整次同步复用一条 SFTP 会话，不逐文件重连。

支持选项：`--delete`（删目标端多余文件 + 清理空目录）、`--checksum`、`--dry-run`、`--exclude '*.tmp'`（支持 `*` `?`，可多次）、`--ignore-times`、`--no-resume`、`--pull`、`--timeout`。

> **与 rsync 的边界**：这里实现的是 rsync 默认的 **quick-check** 语义（size + mtime），不是 rolling-checksum 块级差量——大文件只改一小段仍走整文件传输（可配合断点续传）。传输后会写回 mtime，因此重复同步是幂等的（第二次全部跳过、0 字节传输）。

### 剧本

```bash
owl playbook validate deploy.yml
owl playbook run deploy.yml --nodes web01 --extra_vars env=prod
```

## 测试

`owl-transfer` 附带一个本地 SFTP 测试服务器 example，可用于端到端回归：

```bash
cargo build -p owl-transfer --example sftp_test_server
./target/debug/examples/sftp_test_server /tmp/remote-root 2222
```

已用该服务器完成 7 个场景的端到端验证：首次推送、幂等重跑、只传变化文件、`--exclude`、`--delete`、`--pull`、pull 幂等。

> Windows Git Bash 注意：MSYS 会把 `--remote /` 的 `/` 转换成路径前缀，跑相关命令时需设置 `MSYS_NO_PATHCONV=1`。

## 路线图

- **P2**：AI 助手（agent loop + tool calling）、剧本条件/循环完整求值、`playbook run` 实体化。
- **P3**：Web 控制台、监控告警闭环、TUI、DuckDB 分析后端。

## License

MIT
