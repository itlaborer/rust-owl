//! 剧本解析器：include 展开 + 任务归一化 + 变量/条件/循环解析。
//! 对齐原项目 parser.go 的 ParseFromFile / expandIncludeTasks / parseTask。

use crate::model::*;
use owl_core::error::{OwlError, Result};
use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct Parser {
    variable_re: Regex,
    base_dir: PathBuf,
    visited_includes: HashSet<PathBuf>,
}

impl Parser {
    pub fn new() -> Self {
        Self {
            variable_re: Regex::new(r"\{\{\s*([^}]+)\s*\}\}").unwrap(),
            base_dir: PathBuf::from("."),
            visited_includes: HashSet::new(),
        }
    }

    pub fn from_file(path: &Path) -> Result<ParsedPlaybook> {
        let mut parser = Self::new();
        parser.base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let content = std::fs::read_to_string(path)
            .map_err(|e| OwlError::PlaybookParse(format!("读取 {:?}: {}", path, e)))?;
        parser.parse(&content)
    }

    pub fn parse(&mut self, content: &str) -> Result<ParsedPlaybook> {
        let raw: Playbook =
            serde_yaml::from_str(content).map_err(|e| OwlError::PlaybookParse(e.to_string()))?;

        let mode = match raw.execution_mode.as_str() {
            "pipeline" => ExecutionMode::Pipeline,
            "" | "fail_continue" => ExecutionMode::FailContinue,
            other => return Err(OwlError::PlaybookParse(format!("未知执行模式: {}", other))),
        };

        // pipeline 模式禁止 post_tasks
        if mode == ExecutionMode::Pipeline && !raw.post_tasks.is_empty() {
            return Err(OwlError::PlaybookParse(
                "pipeline mode does not allow post_tasks".into(),
            ));
        }

        let pre = self.parse_tasks(&raw.pre_tasks)?;
        let tasks = self.parse_tasks(&raw.tasks)?;
        let post = self.parse_tasks(&raw.post_tasks)?;

        if pre.is_empty() && tasks.is_empty() && post.is_empty() {
            return Err(OwlError::PlaybookParse("剧本没有任何任务".into()));
        }

        Ok(ParsedPlaybook {
            variables: raw.vars.clone(),
            raw,
            execution_mode: mode,
            tasks,
            pre_tasks: pre,
            post_tasks: post,
        })
    }

    fn parse_tasks(&mut self, raw: &[PlaybookTask]) -> Result<Vec<ParsedTask>> {
        let mut out = vec![];
        for t in raw {
            // include 是展开指令，不进入最终任务列表
            if t.action.eq_ignore_ascii_case("include") {
                let inc = self.expand_include(t)?;
                out.extend(inc);
                continue;
            }

            let action = t.action.split_whitespace().next().unwrap_or("").to_string();

            let condition = if t.when.is_empty() {
                None
            } else {
                Some(Condition {
                    expression: t.when.clone(),
                    variables: self.extract_vars(&t.when),
                })
            };

            let loop_ = if t.with_items.is_empty() {
                None
            } else {
                Some(Loop {
                    items: t.with_items.clone(),
                    // 原项目固定 "item"
                    var_name: if t.loop_control.is_empty() {
                        "item".into()
                    } else {
                        t.loop_control.clone()
                    },
                })
            };

            out.push(ParsedTask {
                raw: t.clone(),
                name: t.name.clone(),
                action,
                args: t.args.clone(),
                condition,
                loop_,
            });
        }
        Ok(out)
    }

    fn expand_include(&mut self, task: &PlaybookTask) -> Result<Vec<ParsedTask>> {
        let path_str = task
            .args
            .get("playbook")
            .and_then(|v| v.as_str())
            .ok_or_else(|| OwlError::PlaybookParse("include 缺少 playbook 参数".into()))?;
        let path = self.base_dir.join(path_str);
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());

        if self.visited_includes.contains(&canonical) {
            return Err(OwlError::PlaybookParse(format!(
                "circular include detected: {}",
                path_str
            )));
        }
        self.visited_includes.insert(canonical.clone());

        let content = std::fs::read_to_string(&path)
            .map_err(|e| OwlError::PlaybookParse(format!("读取 include {:?}: {}", path, e)))?;
        let raw: Playbook =
            serde_yaml::from_str(&content).map_err(|e| OwlError::PlaybookParse(e.to_string()))?;

        let mut out = vec![];
        out.extend(self.parse_tasks(&raw.pre_tasks)?);
        out.extend(self.parse_tasks(&raw.tasks)?);
        out.extend(self.parse_tasks(&raw.post_tasks)?);

        self.visited_includes.remove(&canonical);
        Ok(out)
    }

    fn extract_vars(&self, s: &str) -> Vec<String> {
        self.variable_re
            .captures_iter(s)
            .filter_map(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}
