//! 跨模块共享的数据模型：目标选择器、执行结果、批次等。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 节点选择方式，对应 CLI 的 --nodes / --groups / --label / --all
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetSelector {
    pub nodes: Vec<String>,
    pub groups: Vec<String>,
    pub labels: Vec<String>, // "env=prod" 形式
    pub all: bool,
}

impl TargetSelector {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.groups.is_empty() && self.labels.is_empty() && !self.all
    }
}

/// 单个节点上的一次执行结果。
/// 原项目中一次 `owl exec` 打 N 个节点 = N 个 Task，共用 batch_id。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub batch_id: String,
    pub node_id: String,
    pub node_name: String,
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
}
