//! 全局配置：对应原项目 `~/.owl/config.yaml`。
//! 路径解析、默认配置、AI 配置都集中在这里。

use crate::error::{OwlError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 配置文件根目录 ~/.owl （可用 OWL_HOME 覆盖）
pub fn owl_home() -> PathBuf {
    if let Ok(p) = std::env::var("OWL_HOME") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".owl")
}

pub fn config_path() -> PathBuf {
    owl_home().join("config.yaml")
}

pub fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("OWL_DB_PATH") {
        return PathBuf::from(p);
    }
    owl_home().join("owl.db")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ai: AiConfig,
    pub settings: SettingsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// openai / anthropic / qwen(dashscope) / deepseek
    pub provider: String,
    pub model: String,
    /// 留空时从环境变量 OWL_API_KEY / OPENAI_API_KEY / DASHSCOPE_API_KEY 读取
    pub api_key: String,
    pub base_url: String,
    /// 秒
    pub timeout: u64,
    /// 工具循环最大轮数，0 = 默认 10
    pub max_turns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsConfig {
    pub output: OutputConfig,
    pub default: DefaultConfig,
    pub target: TargetConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub format: String, // table / json / simple
    pub color: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultConfig {
    pub timeout: String, // e.g. "60s"
    pub parallel: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetConfig {
    pub groups: String,
    pub label: String,
    pub nodes: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ai: AiConfig::default(),
            settings: SettingsConfig::default(),
        }
    }
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            model: "gpt-4o".into(),
            api_key: first_env(&["OWL_API_KEY", "OPENAI_API_KEY", "DASHSCOPE_API_KEY"]),
            base_url: std::env::var("OWL_BASE_URL").unwrap_or_default(),
            timeout: 120,
            max_turns: 10,
        }
    }
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            output: OutputConfig::default(),
            default: DefaultConfig::default(),
            target: TargetConfig::default(),
        }
    }
}
impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            format: "table".into(),
            color: true,
        }
    }
}
impl Default for DefaultConfig {
    fn default() -> Self {
        Self {
            timeout: "60s".into(),
            parallel: true,
        }
    }
}
impl Default for TargetConfig {
    fn default() -> Self {
        Self {
            groups: String::new(),
            label: String::new(),
            nodes: String::new(),
        }
    }
}

impl Config {
    /// 从 ~/.owl/config.yaml 加载；文件不存在时返回默认配置。
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| OwlError::Config(format!("读取 {:?}: {}", path, e)))?;
        serde_yaml::from_str(&content).map_err(|e| OwlError::Config(e.to_string()))
    }
}

fn first_env(keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default()
}
