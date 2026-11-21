//! owl-rs CLI 入口。命令树对齐原项目 cmd/cli/cmd/root.go。

use anyhow::Result;
use clap::{Parser, Subcommand};
use owl_node::{LocalNodeStore, NodeResolver, ResolvedNode};
use owl_ssh::Executor as _;
use std::collections::HashMap;

#[derive(Parser)]
#[command(
    name = "owl",
    version = "0.1.0",
    about = "智能分布式运维工具 (Rust 重写版)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// 输出格式: table / json / simple
    #[arg(long, global = true, default_value = "table")]
    format: String,
}

#[derive(Subcommand)]
enum Commands {
    /// 节点管理（增删改查、分组、标签、导入导出）
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
    /// 批量命令 / 脚本执行
    Exec {
        /// 要执行的命令
        command: String,
        #[arg(long)]
        nodes: Vec<String>,
        #[arg(long)]
        groups: Vec<String>,
        #[arg(long)]
        label: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
    /// 文件传输（上传 / 下载）
    File {
        #[command(subcommand)]
        action: FileAction,
    },
    /// YAML 剧本编排
    Playbook {
        #[command(subcommand)]
        action: PlaybookAction,
    },
    /// 交互式会话
    Session,
    /// 系统设置
    Settings,
    /// 执行历史
    History {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// AI 助手（phase2）
    Ai {
        /// 自然语言查询
        prompt: Option<String>,
    },
    /// 启动 Web 控制台 + WebSSH
    Serve {
        /// 监听地址
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// 监听端口
        #[arg(long, short, default_value_t = 8080)]
        port: u16,
        /// 访问令牌（也可用 OWL_WEB_TOKEN 环境变量）；设置后需 Bearer 鉴权
        #[arg(long)]
        token: Option<String>,
        /// 批量执行默认超时（秒）
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum NodeAction {
    /// 添加节点
    Add {
        name: String,
        #[arg(long)]
        address: String,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long)]
        groups: Vec<String>,
    },
    /// 列出节点
    List,
    /// 检查连通性
    Ping { name: String },
}

#[derive(Subcommand)]
enum FileAction {
    /// 目录增量同步（rsync 风格：只传变化的文件）
    Sync {
        /// 本地目录
        local: String,
        /// 远端目录
        #[arg(long, short = 'd')]
        remote: String,
        #[arg(long)]
        nodes: Vec<String>,
        /// 反向同步：远端 → 本地
        #[arg(long)]
        pull: bool,
        /// 删除目标端多余文件（危险操作）
        #[arg(long)]
        delete: bool,
        /// 大小与修改时间相同时仍校验内容
        #[arg(long)]
        checksum: bool,
        /// 只生成计划，不实际传输
        #[arg(long)]
        dry_run: bool,
        /// 忽略修改时间，仅按大小判断
        #[arg(long)]
        ignore_times: bool,
        /// 关闭断点续传
        #[arg(long)]
        no_resume: bool,
        /// 排除模式（支持 * ? 通配符，可多次）
        #[arg(long)]
        exclude: Vec<String>,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// 上传本地文件到节点
    Upload {
        local: String,
        #[arg(long)]
        nodes: Vec<String>,
        /// 远端目标路径（文件为完整路径，目录则以 / 结尾）
        #[arg(long, short = 'd', default_value = "/tmp/")]
        dest: String,
        /// 拒绝覆盖已存在的远端文件
        #[arg(long)]
        no_overwrite: bool,
        /// 禁用断点续传（默认开启）
        #[arg(long)]
        no_resume: bool,
    },
    /// 从节点下载文件
    Download {
        remote: String,
        #[arg(long)]
        node: String,
        /// 本地目标路径（目录或文件）
        #[arg(long, short = 'd', default_value = ".")]
        dest: String,
        /// 多个节点下载时按节点名建子目录
        #[arg(long)]
        subdir: bool,
        #[arg(long)]
        no_overwrite: bool,
        #[arg(long)]
        no_resume: bool,
    },
}

#[derive(Subcommand)]
enum PlaybookAction {
    /// 列出剧本
    List,
    /// 校验剧本语法
    Validate { path: String },
    /// 执行剧本
    Run {
        path: String,
        #[arg(long)]
        nodes: Vec<String>,
        #[arg(long)]
        extra_vars: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志（对齐原项目 zap）
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    dispatch(cli).await
}

async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Node { action } => match action {
            NodeAction::List => {
                let store = LocalNodeStore::load_default();
                let mut table = comfy_table::Table::new();
                table.set_header(["ID", "名称", "地址", "端口", "用户", "分组"]);
                for n in store.list() {
                    table.add_row([
                        n.id,
                        n.name,
                        n.address,
                        n.port.to_string(),
                        n.user,
                        n.groups.join(","),
                    ]);
                }
                println!("{table}");
            }
            NodeAction::Add {
                name,
                address,
                user,
                port,
                groups,
            } => {
                let mut store = LocalNodeStore::load_default();
                let node = ResolvedNode {
                    id: name.clone(),
                    name: name.clone(),
                    address,
                    port,
                    user,
                    groups,
                    source: "local".into(),
                    ..Default::default()
                };
                store.upsert(node);
                store.save()?;
                println!("✓ 节点 {name} 已保存");
            }
            NodeAction::Ping { name } => {
                let resolver = NodeResolver::new();
                let node = resolver.resolve(&name)?;
                // 用一次真实的 SSH 连接验证连通性（命令为 true）
                let exec = owl_ssh::NativeExecutor::new(15);
                match exec.execute(&node, "true").await {
                    Ok(_) => println!("✓ {} ({}@{}) 连通", node.name, node.user, node.address),
                    Err(e) => println!("✗ {} 不可达: {e}", node.name),
                }
            }
        },
        Commands::Exec {
            command,
            nodes,
            groups,
            label,
            all,
            timeout,
        } => {
            let resolver = NodeResolver::new();
            // 目标节点：优先 --nodes；否则按分组/标签/全部从本地存储筛选
            let targets = if !nodes.is_empty() {
                resolver.resolve_many(&nodes)?
            } else {
                let store = LocalNodeStore::load_default();
                let mut list = store.list();
                if !groups.is_empty() {
                    list.retain(|n| n.groups.iter().any(|g| groups.contains(g)));
                }
                if !label.is_empty() {
                    // --label 支持 key=value（精确）或 key（存在即可）
                    list.retain(|n| {
                        label.iter().any(|sel| match sel.split_once('=') {
                            Some((k, v)) => n.labels.get(k).map(|x| x == v).unwrap_or(false),
                            None => n.labels.contains_key(sel),
                        })
                    });
                }
                if groups.is_empty() && label.is_empty() && !all {
                    // 未指定目标时给出提示，避免误操作全网
                    anyhow::bail!("请指定 --nodes / --groups / --label 或 --all");
                }
                list
            };
            if targets.is_empty() {
                anyhow::bail!("没有匹配到任何节点");
            }

            let batch = owl_exec::BatchExecutor::new(timeout);
            let opts = owl_exec::BatchOptions {
                timeout_secs: timeout,
                ..Default::default()
            };
            let results = batch.run(&targets, &command, &opts).await?;

            // 执行结果落历史库（best-effort，写失败不影响命令本身）
            match owl_history::HistoryDb::open() {
                Ok(db) => {
                    for r in &results {
                        if let Err(e) = db.record_exec(r) {
                            tracing::warn!("写入执行历史失败: {e}");
                        }
                    }
                }
                Err(e) => tracing::warn!("打开历史库失败: {e}"),
            }

            let mut table = comfy_table::Table::new();
            table.set_header(["节点", "退出码", "状态", "耗时(ms)", "输出"]);
            for r in &results {
                let output = if r.stdout.is_empty() {
                    &r.stderr
                } else {
                    &r.stdout
                };
                let preview: String = output.chars().take(120).collect();
                table.add_row([
                    r.node_name.clone(),
                    r.exit_code.to_string(),
                    if r.success {
                        "✓".into()
                    } else {
                        "✗".into()
                    },
                    r.duration_ms.to_string(),
                    preview,
                ]);
            }
            println!("{table}");
        }
        Commands::File { action } => {
            use owl_transfer::sync::{SyncAction, SyncDirection, SyncOptions};
            use owl_transfer::Transfer as _;

            match action {
                FileAction::Sync {
                    local,
                    remote,
                    nodes,
                    pull,
                    delete,
                    checksum,
                    dry_run,
                    ignore_times,
                    no_resume,
                    exclude,
                    timeout,
                } => {
                    let resolver = NodeResolver::new();
                    let targets = if nodes.is_empty() {
                        anyhow::bail!("目录同步需指定 --nodes");
                    } else {
                        resolver.resolve_many(&nodes)?
                    };

                    let opts = SyncOptions {
                        direction: if pull {
                            SyncDirection::Pull
                        } else {
                            SyncDirection::Push
                        },
                        delete,
                        checksum,
                        dry_run,
                        ignore_times,
                        resume: !no_resume,
                        excludes: exclude,
                        timeout_secs: timeout,
                        ..Default::default()
                    };
                    let transfer = owl_transfer::SftpTransfer::new(timeout);

                    let mut summary = comfy_table::Table::new();
                    summary.set_header([
                        "节点", "方向", "新增", "更新", "跳过", "删除", "字节", "状态",
                    ]);
                    let mut detail = comfy_table::Table::new();
                    detail.set_header(["节点", "文件", "动作", "原因"]);
                    let mut detail_rows = 0usize;

                    for n in &targets {
                        if delete {
                            tracing::warn!("节点 {} 启用了 --delete，将删除目标端多余文件", n.name);
                        }
                        match transfer.sync_dir(n, &local, &remote, &opts).await {
                            Ok(r) => {
                                summary.add_row([
                                    r.node_name.clone(),
                                    opts.direction.as_str().into(),
                                    r.created.to_string(),
                                    r.updated.to_string(),
                                    r.skipped.to_string(),
                                    r.deleted.to_string(),
                                    r.bytes.to_string(),
                                    if r.success {
                                        if dry_run {
                                            "计划".into()
                                        } else {
                                            "✓".into()
                                        }
                                    } else {
                                        format!("✗ {}", r.errors.len())
                                    },
                                ]);
                                for c in r
                                    .changes
                                    .iter()
                                    .filter(|c| c.action != SyncAction::Skip)
                                    .take(80)
                                {
                                    detail.add_row([
                                        r.node_name.clone(),
                                        c.rel.clone(),
                                        c.action.as_str().into(),
                                        c.reason.clone(),
                                    ]);
                                    detail_rows += 1;
                                }
                                for e in &r.errors {
                                    println!("  ! [{}] {}", r.node_name, e);
                                }
                            }
                            Err(e) => {
                                summary.add_row([
                                    n.name.clone(),
                                    opts.direction.as_str().into(),
                                    "0".into(),
                                    "0".into(),
                                    "0".into(),
                                    "0".into(),
                                    "0".into(),
                                    format!("✗ {}", e),
                                ]);
                            }
                        }
                    }
                    println!("{summary}");
                    if detail_rows > 0 {
                        println!("\n变更明细（最多显示 80 条）:");
                        println!("{detail}");
                    }
                }
                FileAction::Upload {
                    local,
                    nodes,
                    dest,
                    no_overwrite,
                    no_resume,
                } => {
                    let resolver = NodeResolver::new();
                    let targets = if nodes.is_empty() {
                        anyhow::bail!("上传需指定 --nodes");
                    } else {
                        resolver.resolve_many(&nodes)?
                    };

                    // 目标路径：以 / 结尾视为目录，拼接文件名
                    let file_name = std::path::Path::new(&local)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| anyhow::anyhow!("无法从本地路径提取文件名"))?;
                    let remote_path = if dest.ends_with('/') {
                        format!("{}{}", dest, file_name)
                    } else {
                        dest.clone()
                    };

                    let opts = owl_transfer::TransferOptions {
                        no_overwrite,
                        resume: !no_resume,
                        ..Default::default()
                    };
                    let transfer = owl_transfer::SftpTransfer::new(opts.timeout_secs);

                    let tasks: Vec<_> = targets
                        .iter()
                        .map(|n| transfer.upload_with(n, &local, &remote_path, &opts))
                        .collect();
                    let results = futures::future::join_all(tasks).await;

                    let mut table = comfy_table::Table::new();
                    table.set_header(["节点", "状态", "字节", "说明"]);
                    for r in results {
                        match r {
                            Ok(r) => {
                                table.add_row([
                                    r.node_name,
                                    if r.success {
                                        "✓".into()
                                    } else {
                                        "✗".into()
                                    },
                                    r.bytes.to_string(),
                                    r.message,
                                ]);
                            }
                            Err(e) => {
                                table.add_row(["?".into(), "✗".into(), "0".into(), e.to_string()]);
                            }
                        }
                    }
                    println!("{table}");
                }
                FileAction::Download {
                    remote,
                    node,
                    dest,
                    subdir,
                    no_overwrite,
                    no_resume,
                } => {
                    let resolver = NodeResolver::new();
                    let targets = resolver.resolve_many(&[node.clone()])?;
                    let opts = owl_transfer::TransferOptions {
                        no_overwrite,
                        resume: !no_resume,
                        ..Default::default()
                    };
                    let transfer = owl_transfer::SftpTransfer::new(opts.timeout_secs);

                    // 本地路径：以 / 结尾或已存在目录视为目录，拼接远端文件名
                    let file_name = remote.rsplit('/').next().unwrap_or("downloaded");
                    let is_dir = dest.ends_with('/') || std::path::Path::new(&dest).is_dir();
                    let base_dir = if is_dir {
                        dest.trim_end_matches('/').to_string()
                    } else {
                        std::path::Path::new(&dest)
                            .parent()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|| ".".into())
                    };

                    let mut table = comfy_table::Table::new();
                    table.set_header(["节点", "状态", "字节", "本地路径", "说明"]);
                    for n in &targets {
                        // --subdir：按节点名建子目录，避免多节点同名文件互相覆盖
                        let local_path = if subdir {
                            let d = std::path::Path::new(&base_dir).join(&n.name);
                            let _ = std::fs::create_dir_all(&d);
                            d.join(file_name).to_string_lossy().into_owned()
                        } else if is_dir {
                            std::path::Path::new(&base_dir)
                                .join(file_name)
                                .to_string_lossy()
                                .into_owned()
                        } else {
                            dest.clone()
                        };
                        match transfer.download_with(n, &remote, &local_path, &opts).await {
                            Ok(r) => {
                                table.add_row([
                                    r.node_name,
                                    if r.success {
                                        "✓".into()
                                    } else {
                                        "✗".into()
                                    },
                                    r.bytes.to_string(),
                                    r.local_path,
                                    r.message,
                                ]);
                            }
                            Err(e) => {
                                table.add_row([
                                    n.name.clone(),
                                    "✗".into(),
                                    "0".into(),
                                    local_path.clone(),
                                    e.to_string(),
                                ]);
                            }
                        }
                    }
                    println!("{table}");
                }
            }
        }
        Commands::Playbook { action } => match action {
            PlaybookAction::List => {
                // 列出 owl_home/playbooks 下的 .yml/.yaml 剧本
                let dir = owl_core::config::owl_home().join("playbooks");
                let mut table = comfy_table::Table::new();
                table.set_header(["文件", "剧本名"]);
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for ent in rd.flatten() {
                        let p = ent.path();
                        let is_yaml = p
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s == "yml" || s == "yaml")
                            .unwrap_or(false);
                        if !is_yaml {
                            continue;
                        }
                        let name = owl_playbook::Parser::from_file(&p)
                            .map(|pb| pb.raw.name)
                            .unwrap_or_else(|_| p.to_string_lossy().into_owned());
                        table.add_row([p.to_string_lossy().into_owned(), name]);
                    }
                }
                println!("{table}");
            }
            PlaybookAction::Validate { path } => {
                let pb = owl_playbook::Parser::from_file(std::path::Path::new(&path));
                match pb {
                    Ok(p) => println!("剧本校验通过: {} ({} 个主任务)", p.raw.name, p.tasks.len()),
                    Err(e) => println!("剧本校验失败: {}", e),
                }
            }
            PlaybookAction::Run {
                path,
                nodes,
                extra_vars,
            } => {
                let pb = owl_playbook::Parser::from_file(std::path::Path::new(&path))?;
                let resolver = NodeResolver::new();
                let targets = if nodes.is_empty() {
                    anyhow::bail!("执行剧本需指定 --nodes");
                } else {
                    resolver.resolve_many(&nodes)?
                };
                // --extra_vars key=value 解析为变量表
                let mut vars = HashMap::new();
                for kv in &extra_vars {
                    match kv.split_once('=') {
                        Some((k, v)) => {
                            vars.insert(k.to_string(), v.to_string());
                        }
                        None => anyhow::bail!("--extra_vars 需为 key=value 形式: {kv}"),
                    }
                }

                let executor = owl_playbook::PlaybookExecutor::new(60);
                let report = executor.run(&pb, &targets, &vars).await?;

                // 剧本结果也落历史库
                if let Ok(db) = owl_history::HistoryDb::open() {
                    for r in &report.results {
                        let _ = db.record_exec(r);
                    }
                }

                let mut table = comfy_table::Table::new();
                table.set_header(["节点", "命令", "退出码", "状态", "耗时(ms)"]);
                for r in &report.results {
                    table.add_row([
                        r.node_name.clone(),
                        r.command.clone(),
                        r.exit_code.to_string(),
                        if r.success {
                            "✓".into()
                        } else {
                            "✗".into()
                        },
                        r.duration_ms.to_string(),
                    ]);
                }
                println!("{table}");
                println!(
                    "\n剧本 {} 执行完成：{} 条结果，{} 个任务失败",
                    pb.raw.name,
                    report.results.len(),
                    report.failed
                );
            }
        },
        Commands::Session => println!("(session) phase1 骨架"),
        Commands::Settings => println!("(settings) phase1 骨架"),
        Commands::History { limit } => match owl_history::HistoryDb::open() {
            Ok(db) => match db.recent(limit) {
                Ok(rows) => {
                    let mut table = comfy_table::Table::new();
                    table.set_header(["时间", "节点", "命令", "退出码", "状态"]);
                    for r in &rows {
                        table.add_row([
                            r.started_at
                                .with_timezone(&chrono::Local)
                                .format("%m-%d %H:%M:%S")
                                .to_string(),
                            r.node_name.clone(),
                            {
                                let c: String = r.command.chars().take(50).collect();
                                c
                            },
                            r.exit_code.to_string(),
                            if r.success {
                                "✓".into()
                            } else {
                                "✗".into()
                            },
                        ]);
                    }
                    println!("{table}");
                }
                Err(e) => println!("读取历史失败: {e}"),
            },
            Err(e) => println!("打开历史库失败: {e}"),
        },
        Commands::Ai { prompt } => println!("ai: {:?} (phase2)", prompt),
        Commands::Serve {
            host,
            port,
            token,
            timeout,
        } => {
            // 命令行 token 优先，其次环境变量
            let token = token
                .or_else(|| std::env::var("OWL_WEB_TOKEN").ok().filter(|s| !s.is_empty()));
            println!("Web 控制台启动中 → http://{}:{}", host, port);
            owl_serve::run(&host, port, token, timeout).await?;
        }
    }
    Ok(())
}
