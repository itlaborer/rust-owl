//! 交互式 Shell（WebSSH 底座）。
//!
//! 在一条已认证的 SSH 会话上开 session channel，请求 PTY + shell，
//! 然后把 channel 拆成读半/写半，用两个 tokio task 桥接成一对 mpsc：
//!   - SSH -> `out_rx`：SSR/STDOUT/退出事件
//!   - `in_tx`  -> SSH：键盘输入 / 窗口尺寸变化
//! 上层（owl-serve 的 WebSocket）只需面对字节流，无需知道 russh 类型。

use crate::connection::SshSession;
use owl_core::error::{OwlError, Result};
use russh::{ChannelMsg, Pty};
use tokio::sync::mpsc;

/// 从 SSH 端收到的一条事件。
#[derive(Debug)]
pub enum ShellEvent {
    /// 终端标准输出（含 shell 回显）
    Data(Vec<u8>),
    /// 标准错误
    Stderr(Vec<u8>),
    /// 远端 shell 退出码
    Exit(i32),
}

/// 发给远端 shell 的控制消息。
#[derive(Debug)]
pub enum ShellInput {
    /// 键盘字节
    Data(Vec<u8>),
    /// 终端尺寸变化 (列, 行)
    Resize(u32, u32),
    /// EOF（Ctrl-D）
    Eof,
}

/// 一条交互式 shell 的句柄。
pub struct ShellChannel {
    /// 接收 SSH 输出
    pub out_rx: mpsc::Receiver<ShellEvent>,
    /// 发送键盘输入 / resize / EOF
    pub in_tx: mpsc::Sender<ShellInput>,
    /// 远端进程退出后该句柄标记结束；drop 时关闭 channel
    cols: u32,
    rows: u32,
}

impl SshSession {
    /// 在会话上开启一个带 PTY 的交互式 shell，并启动双向桥接 task。
    ///
    /// `term` 传终端类型（如 "xterm-256color"），`cols`/`rows` 为初始尺寸。
    pub async fn open_shell(&self, term: &str, cols: u32, rows: u32) -> Result<ShellChannel> {
        let channel = self
            .handle()
            .channel_open_session()
            .await
            .map_err(|e| OwlError::Exec(format!("打开交互式 shell channel 失败: {}", e)))?;

        // 请求伪终端：modes 传空表即可（对端用默认值），TTY_OP_END 作为终止标记。
        let modes = [(Pty::TTY_OP_END, 0u32)];
        channel
            .request_pty(true, term, cols, rows, 0, 0, &modes)
            .await
            .map_err(|e| OwlError::Exec(format!("请求 PTY 失败: {}", e)))?;

        channel
            .request_shell(true)
            .await
            .map_err(|e| OwlError::Exec(format!("请求 shell 失败: {}", e)))?;

        let (read_half, write_half) = channel.split();

        let (out_tx, out_rx) = mpsc::channel::<ShellEvent>(256);
        let (in_tx, mut in_rx) = mpsc::channel::<ShellInput>(256);

        // SSH -> out_rx：读取 channel 消息并转成事件
        tokio::spawn(async move {
            let mut read = read_half;
            while let Some(msg) = read.wait().await {
                match msg {
                    ChannelMsg::Data { data } => {
                        if out_tx.send(ShellEvent::Data(data.to_vec())).await.is_err() {
                            break;
                        }
                    }
                    ChannelMsg::ExtendedData { data, ext } => {
                        if ext == 1
                            && out_tx
                                .send(ShellEvent::Stderr(data.to_vec()))
                                .await
                                .is_err()
                        {
                            break;
                        }
                    }
                    ChannelMsg::ExitStatus { exit_status } => {
                        let _ = out_tx.send(ShellEvent::Exit(exit_status as i32)).await;
                        break;
                    }
                    ChannelMsg::ExitSignal { signal_name, .. } => {
                        let code = -1;
                        tracing::debug!(?signal_name, "远端 shell 被信号终止");
                        let _ = out_tx.send(ShellEvent::Exit(code)).await;
                        break;
                    }
                    ChannelMsg::Eof | ChannelMsg::Close => break,
                    _ => {}
                }
            }
            // out_tx drop -> out_rx 收到 None，WebSocket 端据此关闭
        });

        // in_rx -> SSH：键盘输入 / resize / EOF
        tokio::spawn(async move {
            let write = write_half;
            while let Some(input) = in_rx.recv().await {
                let res = match input {
                    ShellInput::Data(bytes) => write.data_bytes(bytes).await,
                    ShellInput::Resize(c, r) => write.window_change(c, r, 0, 0).await,
                    ShellInput::Eof => {
                        let _ = write.eof().await;
                        let _ = write.close().await;
                        break;
                    }
                };
                if let Err(e) = res {
                    tracing::debug!("写入 shell channel 失败: {}", e);
                    break;
                }
            }
        });

        Ok(ShellChannel {
            out_rx,
            in_tx,
            cols,
            rows,
        })
    }
}

impl ShellChannel {
    pub fn size(&self) -> (u32, u32) {
        (self.cols, self.rows)
    }
}
