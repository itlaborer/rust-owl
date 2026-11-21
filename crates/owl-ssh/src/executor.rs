//! 执行器：基于 russh channel 真实执行命令，收集 stdout/stderr/exit code。
//! 对齐原项目 internal/ssh/native_executor.go：超时控制 + 流处理。

use crate::connection::ConnectionManager;
use async_trait::async_trait;
use owl_core::error::{OwlError, Result};
use owl_node::ResolvedNode;
use russh::ChannelMsg;
use std::time::Duration;
use tokio::time::timeout;

#[derive(Debug, Clone, Default)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// 执行器 trait，统一同步/异步实现（对应原项目 Executor 接口）。
#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(&self, node: &ResolvedNode, command: &str) -> Result<CommandOutput>;
}

pub struct NativeExecutor {
    conn: ConnectionManager,
    timeout_secs: u64,
}

impl NativeExecutor {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            conn: ConnectionManager::new(timeout_secs),
            timeout_secs,
        }
    }

    /// 关闭所有复用连接
    pub fn close_all(&self) {
        self.conn.close_all();
    }
}

#[async_trait]
impl Executor for NativeExecutor {
    async fn execute(&self, node: &ResolvedNode, command: &str) -> Result<CommandOutput> {
        let session = self.conn.acquire(node).await?;

        let fut = exec_on_session(&session, command);
        match timeout(Duration::from_secs(self.timeout_secs), fut).await {
            Ok(res) => res,
            Err(_) => Err(OwlError::Exec(format!(
                "命令执行超时（{}s）: {} @ {}",
                self.timeout_secs, node.name, command
            ))),
        }
    }
}

/// 在已建立的会话上开 channel 执行命令并收集输出。
async fn exec_on_session(
    session: &crate::connection::SshSession,
    command: &str,
) -> Result<CommandOutput> {
    let mut channel = session
        .handle()
        .channel_open_session()
        .await
        .map_err(|e| OwlError::Exec(format!("打开 SSH channel 失败: {}", e)))?;

    channel
        .exec(true, command)
        .await
        .map_err(|e| OwlError::Exec(format!("发送 exec 请求失败: {}", e)))?;

    let mut out = CommandOutput::default();
    loop {
        let msg = channel
            .wait()
            .await
            .ok_or_else(|| OwlError::Exec("SSH channel 意外关闭".into()))?;
        match msg {
            ChannelMsg::Data { data } => {
                out.stdout.push_str(&String::from_utf8_lossy(&data));
            }
            ChannelMsg::ExtendedData { data, ext } => {
                // ext == 1 是 stderr（SSH 协议约定）
                if ext == 1 {
                    out.stderr.push_str(&String::from_utf8_lossy(&data));
                }
            }
            ChannelMsg::ExitStatus { exit_status } => {
                out.exit_code = exit_status as i32;
                // 收到 exit status 后可能还有残留数据，继续读到 channel 关闭
            }
            ChannelMsg::ExitSignal {
                signal_name,
                error_message,
                ..
            } => {
                out.exit_code = -1;
                out.stderr
                    .push_str(&format!("[signal {:?}] {}", signal_name, error_message));
            }
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
    }

    Ok(out)
}
