//! 危险命令黑名单：对齐原项目 internal/control/blacklist/checker.go。
//! 默认黑名单 + ~/.owl/blacklist.yaml 自定义。

use owl_core::config::owl_home;
use owl_core::error::{OwlError, Result};
use serde::Deserialize;

const DEFAULT_BLOCKED: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    ":(){:|:&};:",
    ">/dev/sda",
    "dd if=/dev/zero of=/dev/sda",
    "mkfs",
    "chmod -R 777 /",
];

#[derive(Debug, Deserialize, Default)]
struct BlacklistFile {
    blocked: Vec<String>,
}

pub struct Blacklist {
    blocked: Vec<String>,
}

impl Blacklist {
    pub fn load() -> Self {
        let mut blocked: Vec<String> = DEFAULT_BLOCKED.iter().map(|s| s.to_string()).collect();
        let path = owl_home().join("blacklist.yaml");
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(file) = serde_yaml::from_str::<BlacklistFile>(&content) {
                blocked.extend(file.blocked);
            }
        }
        Self { blocked }
    }

    /// 命中黑名单返回 Err(DangerousCommand)
    pub fn check(&self, command: &str) -> Result<()> {
        let normalized = command.trim();
        for pat in &self.blocked {
            if normalized.contains(pat.as_str()) {
                return Err(OwlError::DangerousCommand(format!(
                    "命中黑名单规则: {}",
                    pat
                )));
            }
        }
        Ok(())
    }
}
