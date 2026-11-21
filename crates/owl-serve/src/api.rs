//! Web 控制台 REST API。

use crate::{state::AppState, web, ws};
use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use owl_core::error::OwlError;
use owl_node::ResolvedNode;
use serde::Deserialize;

/// 统一 API 错误响应。
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({"error": self.message})),
        )
            .into_response()
    }
}

impl From<OwlError> for ApiError {
    fn from(e: OwlError) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, e.to_string())
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

// ---------- 请求体 ----------

#[derive(Debug, Deserialize)]
pub struct AddNodeRequest {
    pub name: String,
    pub address: String,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub ssh_key: String,
    #[serde(default)]
    pub ssh_password: String,
}

fn default_user() -> String {
    "root".into()
}
fn default_port() -> u16 {
    22
}
fn default_timeout() -> u64 {
    60
}

#[derive(Debug, Deserialize)]
pub struct NameRequest {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct ExecRequest {
    pub command: String,
    #[serde(default)]
    pub nodes: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub label: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PingRequest {
    pub name: String,
    #[serde(default = "default_ping_timeout")]
    pub timeout: u64,
}
fn default_ping_timeout() -> u64 {
    15
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
}
fn default_limit() -> usize {
    50
}

// ---------- handlers ----------

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "owl-serve",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn list_nodes(State(state): State<AppState>) -> Json<Vec<ResolvedNode>> {
    let mut nodes = state.nodes.read().unwrap().list();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    Json(nodes)
}

async fn add_node(
    State(state): State<AppState>,
    Json(req): Json<AddNodeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.name.trim().is_empty() || req.address.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "name 与 address 不能为空",
        ));
    }
    let node = ResolvedNode {
        id: req.name.clone(),
        name: req.name.clone(),
        address: req.address,
        port: req.port,
        user: req.user,
        groups: req.groups,
        ssh_key: req.ssh_key,
        ssh_password: req.ssh_password,
        source: "local".into(),
        ..Default::default()
    };
    let mut store = state.nodes.write().unwrap();
    store.upsert(node.clone());
    store
        .save()
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.resolver.clear_cache();
    Ok(Json(serde_json::json!({"ok": true, "node": node.name})))
}

async fn delete_node(
    State(state): State<AppState>,
    Json(req): Json<NameRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut store = state.nodes.write().unwrap();
    let removed = store.remove(&req.name);
    if removed {
        store
            .save()
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    state.resolver.clear_cache();
    Ok(Json(serde_json::json!({"ok": removed})))
}

async fn ping_node(
    State(state): State<AppState>,
    Json(req): Json<PingRequest>,
) -> Json<serde_json::Value> {
    let node = match state.resolve_node(&req.name) {
        Ok(n) => n,
        Err(e) => {
            return Json(serde_json::json!({"ok": false, "message": e.to_string()}));
        }
    };
    let exec = owl_ssh::NativeExecutor::new(req.timeout);
    match owl_ssh::Executor::execute(&exec, &node, "true").await {
        Ok(_) => Json(serde_json::json!({"ok": true, "message": format!("{} 连通", node.name)})),
        Err(e) => Json(serde_json::json!({"ok": false, "message": e.to_string()})),
    }
}

async fn exec_command(
    State(state): State<AppState>,
    Json(req): Json<ExecRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.command.trim().is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "command 不能为空"));
    }

    let targets: Vec<ResolvedNode> = if !req.nodes.is_empty() {
        req.nodes
            .iter()
            .filter_map(|id| state.resolve_node(id).ok())
            .collect()
    } else {
        let mut list = state.nodes.read().unwrap().list();
        if !req.groups.is_empty() {
            list.retain(|n| n.groups.iter().any(|g| req.groups.contains(g)));
        }
        if !req.label.is_empty() {
            list.retain(|n| {
                req.label.iter().any(|sel| match sel.split_once('=') {
                    Some((k, v)) => n.labels.get(k).map(|x| x == v).unwrap_or(false),
                    None => n.labels.contains_key(sel),
                })
            });
        }
        if req.groups.is_empty() && req.label.is_empty() && !req.all {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "需指定 nodes / groups / label 或 all",
            ));
        }
        list
    };

    if targets.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "没有匹配到任何节点"));
    }

    let batch = owl_exec::BatchExecutor::new(req.timeout);
    let opts = owl_exec::BatchOptions {
        timeout_secs: req.timeout,
        ..Default::default()
    };
    let results = batch.run(&targets, &req.command, &opts).await?;

    // 落历史库（best-effort）
    if let Ok(db) = state.open_db() {
        for r in &results {
            let _ = db.record_exec(r);
        }
    }

    Ok(Json(serde_json::json!({
        "batch_id": results.first().map(|r| &r.batch_id),
        "total": results.len(),
        "results": results,
    })))
}

async fn history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let db = state
        .open_db()
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rows = db
        .recent(query.limit.min(500))
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({"rows": rows})))
}

// ---------- 鉴权中间件 ----------

async fn auth_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = &state.token else {
        return next.run(req).await;
    };

    let header_ok = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v == expected)
        .unwrap_or(false);
    let query_ok = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")))
        .map(|v| v == expected)
        .unwrap_or(false);

    if header_ok || query_ok {
        next.run(req).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// 构建 axum 路由。
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(web::index_handler))
        .route("/api/health", get(health))
        .route("/api/nodes", get(list_nodes).post(add_node))
        .route("/api/nodes/delete", post(delete_node))
        .route("/api/nodes/ping", post(ping_node))
        .route("/api/exec", post(exec_command))
        .route("/api/history", get(history))
        .route("/api/ws/ssh", get(ws::ws_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
}
