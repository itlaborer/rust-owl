//! owl-serve —— Web 控制台 + WebSSH。
//! 对应原项目 cmd/plugins/serve；用 axum 实现 REST + WebSocket。

pub mod api;
pub mod state;
pub mod web;
pub mod ws;

pub use state::AppState;

use std::net::SocketAddr;

/// 启动 Web 控制台，阻塞直到服务结束。
pub async fn run(
    host: &str,
    port: u16,
    token: Option<String>,
    exec_timeout: u64,
) -> anyhow::Result<()> {
    let state = AppState::new(token.clone(), exec_timeout);
    let app = api::router(state);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("owl Web 控制台已启动: http://{}", addr);
    if let Some(t) = &token {
        tracing::info!(
            "已启用访问令牌鉴权 (Bearer / ?token=)，令牌长度 {}",
            t.len()
        );
    } else {
        tracing::warn!("未设置访问令牌，Web 控制台对所有人开放（仅建议本机使用）");
    }

    axum::serve(listener, app).await?;
    Ok(())
}
