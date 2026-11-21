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

    /// 从 ~/.ssh/config 解析指定 Host（含 `Host *` 兜底默认值）。
    /// 支持别名精确匹配、逗号分隔别名；忽略通配符模式（交给 local 存储显式配置）。
    fn from_ssh_config(&self, name: &str) -> Option<ResolvedNode> {
        let path = dirs::home_dir()?.join(".ssh").join("config");
        let content = std::fs::read_to_string(path).ok()?;
        let blocks = parse_ssh_config(&content);

        let mut matched: Option<&SshHostBlock> = None;
        let fallback = blocks.iter().find(|b| b.patterns.iter().any(|p| p == "*"));
        for b in &blocks {
            if b.patterns.iter().any(|p| p == name) {
                matched = Some(b);
                break;
            }
        }
        let b = matched.or(fallback)?;
        // Host * 兜底若没有 HostName 则没有实际地址，视为未命中
        if b.hostname.is_none() {
            return None;
        }

        Some(ResolvedNode {
            id: name.to_string(),
            name: name.to_string(),
            address: b.hostname.clone().unwrap(),
            port: b.port.unwrap_or(22),
            user: b.user.clone().unwrap_or_else(|| {
                std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .unwrap_or_default()
            }),
            ssh_key: b.identity_file.clone().unwrap_or_default(),
            proxy_jump: b.proxy_jump.clone().unwrap_or_default(),
            source: "ssh-config".into(),
            ..Default::default()
        })
    }

    pub fn clear_cache(&self) {
        self.cache.write().unwrap().clear();
    }
}

/// ~/.ssh/config 中的一个 Host 块
#[derive(Debug, Default)]
struct SshHostBlock {
    patterns: Vec<String>,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity_file: Option<String>,
    proxy_jump: Option<String>,
}

/// 轻量解析 SSH config：按 `Host` 分块，块内取关键字首个值（SSH 自身也是首个生效）。
fn parse_ssh_config(content: &str) -> Vec<SshHostBlock> {
    let mut blocks: Vec<SshHostBlock> = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("").to_ascii_lowercase();
        let value = parts.next().map(str::trim).filter(|v| !v.is_empty());
        let value = match value {
            Some(v) => v,
            None => continue,
        };
        match key.as_str() {
            "host" => {
                // Host 行开启新块；逗号分隔多个别名
                let patterns = value
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                blocks.push(SshHostBlock {
                    patterns,
                    ..Default::default()
                });
            }
            "hostname" | "user" | "port" | "identityfile" | "proxyjump" => {
                if let Some(b) = blocks.last_mut() {
                    // 只取每个关键字的第一个值（SSH 语义）
                    match key.as_str() {
                        "hostname" => {
                            if b.hostname.is_none() {
                                b.hostname = Some(value.to_string());
                            }
                        }
                        "user" => {
                            if b.user.is_none() {
                                b.user = Some(value.to_string());
                            }
                        }
                        "port" => {
                            if b.port.is_none() {
                                b.port = value.parse().ok();
                            }
                        }
                        "identityfile" => {
                            if b.identity_file.is_none() {
                                b.identity_file = Some(expand_tilde(value));
                            }
                        }
                        "proxyjump" => {
                            if b.proxy_jump.is_none() {
                                b.proxy_jump = Some(value.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    blocks
}

/// 展开 ~ 前缀为 home 目录
fn expand_tilde(s: &str) -> String {
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            let rest = s.strip_prefix("~/").unwrap_or("");
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ssh_config() {
        let cfg = r#"
Host web01
    HostName 10.0.0.11
    User root
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

Host prod-*
    HostName 10.0.1.5

Host *
    User ops
    Port 2200
"#;
        let blocks = parse_ssh_config(cfg);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].patterns, vec!["web01"]);
        assert_eq!(blocks[0].hostname.as_deref(), Some("10.0.0.11"));
        assert_eq!(blocks[0].port, Some(2222));
        assert!(blocks[0]
            .identity_file
            .as_ref()
            .unwrap()
            .contains("id_ed25519"));
        assert_eq!(blocks[2].patterns, vec!["*"]);
        assert_eq!(blocks[2].user.as_deref(), Some("ops"));
    }
}
