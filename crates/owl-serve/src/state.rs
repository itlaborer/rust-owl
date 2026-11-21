//! Web 控制台共享状态。

use owl_core::config::db_path;
use owl_core::error::Result;
use owl_node::{LocalNodeStore, NodeResolver, ResolvedNode};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// 所有 handler 共享的应用状态（Clone 便宜：内部皆 Arc）。
#[derive(Clone)]
pub struct AppState {
    pub nodes: Arc<RwLock<LocalNodeStore>>,
    pub resolver: Arc<NodeResolver>,
    /// 可选访问令牌；为 None 时不鉴权
    pub token: Option<String>,
    pub exec_timeout: u64,
    pub db_path: PathBuf,
}

impl AppState {
    pub fn new(token: Option<String>, exec_timeout: u64) -> Self {
        Self {
            nodes: Arc::new(RwLock::new(LocalNodeStore::load_default())),
            resolver: Arc::new(NodeResolver::new()),
            token,
            exec_timeout,
            db_path: db_path(),
        }
    }

    /// 先查内存中的节点表（含刚通过 Web 新增的节点），再回退到 resolver（含 ssh-config）。
    pub fn resolve_node(&self, id_or_name: &str) -> Result<ResolvedNode> {
        if let Some(n) = self.nodes.read().unwrap().get(id_or_name) {
            return Ok(n);
        }
        self.resolver.resolve(id_or_name)
    }

    pub fn open_db(&self) -> Result<owl_history::HistoryDb> {
        owl_history::HistoryDb::open_at(self.db_path.clone())
    }
}
