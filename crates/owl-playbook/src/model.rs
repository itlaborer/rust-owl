//! 剧本数据模型。字段与 YAML tag 严格对齐原项目 parser.go。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Playbook {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub vars: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub execution_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<PlaybookDefaultConfig>,
    #[serde(default)]
    pub pre_tasks: Vec<PlaybookTask>,
    #[serde(default)]
    pub tasks: Vec<PlaybookTask>,
    #[serde(default)]
    pub post_tasks: Vec<PlaybookTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlaybookTask {
    pub name: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub args: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub when: String,
    #[serde(default)]
    pub with_items: Vec<serde_json::Value>,
    #[serde(default)]
    pub loop_control: String,
    #[serde(default)]
    pub ignore_errors: bool,
    #[serde(default)]
    pub any_errors_fatal: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub register: String,
    #[serde(default)]
    pub changed_when: String,
    #[serde(default)]
    pub failed_when: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<TimeoutConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TimeoutConfig {
    #[serde(default)]
    pub connect: String,
    #[serde(default)]
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RetryConfig {
    #[serde(default)]
    pub max: u32,
    #[serde(default)]
    pub interval: String,
    #[serde(default)]
    pub max_interval: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlaybookDefaultConfig {
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub skip_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<TimeoutConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    /// 严格流水线：不允许 post_tasks、ignore_errors、any_errors_fatal
    Pipeline,
    /// 默认：失败继续
    #[default]
    FailContinue,
}

#[derive(Debug, Clone)]
pub struct Condition {
    pub expression: String,
    pub variables: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Loop {
    pub items: Vec<serde_json::Value>,
    pub var_name: String,
}

#[derive(Debug, Clone)]
pub struct ParsedTask {
    pub raw: PlaybookTask,
    pub name: String,
    /// action 取空格前第一个词，如 "shell echo hi" -> "shell"
    pub action: String,
    pub args: HashMap<String, serde_json::Value>,
    pub condition: Option<Condition>,
    pub loop_: Option<Loop>,
}

#[derive(Debug, Clone)]
pub struct ParsedPlaybook {
    pub raw: Playbook,
    pub variables: HashMap<String, serde_json::Value>,
    pub execution_mode: ExecutionMode,
    pub tasks: Vec<ParsedTask>,
    pub pre_tasks: Vec<ParsedTask>,
    pub post_tasks: Vec<ParsedTask>,
}
