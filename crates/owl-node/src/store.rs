//! 本地节点存储：对应原项目 internal/node/local_source.go。
//! 第一阶段用 ~/.owl/nodes.json 持久化，DB-backed 迁移留待 owl-history 接入。

use crate::resolver::ResolvedNode;
use owl_core::config::owl_home;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LocalNodeStoreError {
    #[error("读取节点文件失败: {0}")]
    Read(String),
    #[error("写入节点文件失败: {0}")]
    Write(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalNodeStore {
    nodes: HashMap<String, ResolvedNode>,
}

impl LocalNodeStore {
    fn path() -> PathBuf {
        owl_home().join("nodes.json")
    }

    pub fn load_default() -> Self {
        let path = Self::path();
        if !path.exists() {
            return Self::default();
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        // save() 序列化的是 self.nodes（HashMap<String, ResolvedNode>），
        // 因此这里必须对称地反序列化成 HashMap，再包回 store，否则解析失败被吞掉导致永远读到空。
        let nodes: HashMap<String, ResolvedNode> =
            serde_json::from_str(&content).unwrap_or_default();
        Self { nodes }
    }

    pub fn get(&self, id_or_name: &str) -> Option<ResolvedNode> {
        if let Some(n) = self.nodes.get(id_or_name) {
            return Some(n.clone());
        }
        // 按 name 再找一次
        self.nodes.values().find(|n| n.name == id_or_name).cloned()
    }

    pub fn list(&self) -> Vec<ResolvedNode> {
        self.nodes.values().cloned().collect()
    }

    pub fn upsert(&mut self, node: ResolvedNode) {
        self.nodes.insert(node.id.clone(), node);
    }

    pub fn remove(&mut self, id_or_name: &str) -> bool {
        self.nodes.remove(id_or_name).is_some()
    }

    pub fn save(&self) -> std::result::Result<(), LocalNodeStoreError> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LocalNodeStoreError::Write(e.to_string()))?;
        }
        let content = serde_json::to_string_pretty(&self.nodes)
            .map_err(|e| LocalNodeStoreError::Write(e.to_string()))?;
        std::fs::write(&path, content).map_err(|e| LocalNodeStoreError::Write(e.to_string()))
    }
}
