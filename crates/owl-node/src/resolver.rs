//! 节点解析器：local 存储优先，ssh_config 兜底（第一阶段不含 API 源）。
//! 数据结构对齐原项目 `internal/node/resolver.go` 的 ResolvedNode。

use crate::store::LocalNodeStore;
use owl_core::error::{OwlError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// 对齐原项目 ResolvedNode 字段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedNode {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub user: String,
    pub groups: Vec<String>,
    pub labels: HashMap<String, String>,
    pub ssh_key: String,
    pub ssh_password: String,
    pub proxy_jump: String,
    /// "local" / "ssh-config" / "api"
    pub source: String,
}

impl Default for ResolvedNode {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            address: String::new(),
            port: 22,
            user: String::new(),
            groups: vec![],
            labels: HashMap::new(),
            ssh_key: String::new(),
            ssh_password: String::new(),
            proxy_jump: String::new(),
            source: String::new(),
        }
    }
}

pub struct NodeResolver {
    local: LocalNodeStore,
    cache: RwLock<HashMap<String, ResolvedNode>>,
}

impl NodeResolver {
    pub fn new() -> Self {
        Self {
            local: LocalNodeStore::load_default(),
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// 按 id 或 name 解析，命中缓存直接返回。
    /// 回退顺序：local -> ssh_config。
    pub fn resolve(&self, id_or_name: &str) -> Result<ResolvedNode> {
        if let Some(n) = self.cache.read().unwrap().get(id_or_name) {
            return Ok(n.clone());
        }

        let node = self
            .local
            .get(id_or_name)
            .or_else(|| self.from_ssh_config(id_or_name))
            .ok_or_else(|| OwlError::NodeNotFound(id_or_name.into()))?;

        self.cache
            .write()
            .unwrap()
            .insert(id_or_name.to_string(), node.clone());
        Ok(node)
    }

    pub fn resolve_many(&self, ids: &[String]) -> Result<Vec<ResolvedNode>> {
        let mut out = Vec::with_capacity(ids.len());
        let mut errs = vec![];
        for id in ids {
            match self.resolve(id) {
                Ok(n) => out.push(n),
                Err(e) => errs.push(format!("{}: {}", id, e)),
            }
        }
        if out.is_empty() && !errs.is_empty() {
            return Err(OwlError::NodeResolve(errs.join("; ")));
        }
        Ok(out)
    }

    /// 从 ~/.ssh/config 解析（第一阶段给出框架，返回 None）
    fn from_ssh_config(&self, _name: &str) -> Option<ResolvedNode> {
        // TODO(phase2): 解析 ~/.ssh/config 的 Host / HostName / User / Port /
        // IdentityFile / ProxyJump，对齐 internal/ssh/ssh_config.go
        None
    }

    pub fn clear_cache(&self) {
        self.cache.write().unwrap().clear();
    }
}
