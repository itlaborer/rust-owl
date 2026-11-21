//! WebSSH：WebSocket <-> 交互式 SSH Shell 双向桥接。

use crate::state::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use owl_ssh::{ShellEvent, ShellInput};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    /// 节点名或 id
    pub node: String,
    #[serde(default = "default_cols")]
    pub cols: u32,
    #[serde(default = "default_rows")]
    pub rows: u32,
    #[serde(default = "default_term")]
    pub term: String,
}
fn default_cols() -> u32 {
    120
}
fn default_rows() -> u32 {
    32
}
fn default_term() -> String {
    "xterm-256color".into()
}

/// GET /api/ws/ssh?node=...&cols=...&rows=...&term=...
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(query): Query<WsQuery>,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state, query))
}

async fn handle_socket(socket: WebSocket, state: AppState, query: WsQuery) {
    let cols = query.cols.clamp(20, 400);
    let rows = query.rows.clamp(5, 200);

    // 解析节点并建立真实 SSH 会话
    let node = match state.resolve_node(&query.node) {
        Ok(n) => n,
        Err(e) => {
            let _ = socket;
            tracing::warn!("WebSSH 节点解析失败: {}", e);
            return;
        }
    };

    let session = match owl_ssh::SshSession::connect(&node, 30).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("WebSSH SSH 连接失败: {}", e);
            return;
        }
    };

    let mut shell = match session.open_shell(&query.term, cols, rows).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("WebSSH 打开 shell 失败: {}", e);
            let _ = session.close().await;
            return;
        }
    };

    let (mut ws_sink, mut ws_stream) = socket.split();
    let in_tx = shell.in_tx.clone();

    // 客户端 -> SSH：浏览器键盘输入（Binary 帧）与控制消息（Text JSON）
    let input_task = tokio::spawn(async move {
        while let Some(result) = ws_stream.next().await {
            match result {
                Ok(Message::Binary(bytes)) => {
                    if in_tx.send(ShellInput::Data(bytes.to_vec())).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Text(text)) => {
                    if let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&text) {
                        match cmd.get("type").and_then(|v| v.as_str()) {
                            Some("resize") => {
                                let c = cmd
                                    .get("cols")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(cols as u64)
                                    as u32;
                                let r = cmd
                                    .get("rows")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(rows as u64)
                                    as u32;
                                if in_tx.send(ShellInput::Resize(c, r)).await.is_err() {
                                    break;
                                }
                            }
                            Some("eof") => {
                                let _ = in_tx.send(ShellInput::Eof).await;
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    // 告知前端 shell 就绪
    let _ = ws_sink
        .send(Message::Text(
            serde_json::json!({"type": "ready", "node": node.name, "cols": cols, "rows": rows})
                .to_string()
                .into(),
        ))
        .await;

    // SSH -> 客户端：终端输出
    let mut exit_code: Option<i32> = None;
    while let Some(event) = shell.out_rx.recv().await {
        let msg = match event {
            ShellEvent::Data(bytes) => Message::Binary(bytes.into()),
            ShellEvent::Stderr(bytes) => Message::Binary(bytes.into()),
            ShellEvent::Exit(code) => {
                exit_code = Some(code);
                Message::Text(
                    serde_json::json!({"type": "exit", "code": code})
                        .to_string()
                        .into(),
                )
            }
        };
        if ws_sink.send(msg).await.is_err() {
            break;
        }
        if exit_code.is_some() {
            break;
        }
    }

    input_task.abort();
    let _ = session.close().await;
    tracing::debug!(node = %node.name, ?exit_code, "WebSSH 会话结束");
}
