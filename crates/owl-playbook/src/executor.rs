//! 剧本执行器：pre_tasks -> tasks -> post_tasks 顺序执行，
//! 支持条件、循环、register、ignore_errors、失败续跑。

use crate::model::{ExecutionMode, ParsedPlaybook, ParsedTask};
use owl_core::error::Result;
use owl_core::model::ExecResult;
use owl_exec::BatchExecutor;
use owl_node::ResolvedNode;
use std::collections::HashMap;

pub struct PlaybookExecutor {
    batch: BatchExecutor,
}

#[derive(Debug, Clone, Default)]
pub struct PlaybookRunReport {
    pub results: Vec<ExecResult>,
    pub failed: usize,
}

impl PlaybookExecutor {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            batch: BatchExecutor::new(timeout_secs),
        }
    }

    pub async fn run(
        &self,
        pb: &ParsedPlaybook,
        nodes: &[ResolvedNode],
        extra_vars: &HashMap<String, String>,
    ) -> Result<PlaybookRunReport> {
        let mut report = PlaybookRunReport::default();
        let mut registers: HashMap<String, String> = HashMap::new();

        // 合并 vars + extra_vars 为简单字符串表，供条件/模板渲染用
        let mut vars = HashMap::new();
        for (k, v) in &pb.variables {
            vars.insert(k.clone(), json_to_string(v));
        }
        vars.extend(extra_vars.clone());

        // 依次执行三段
        for task in pb.pre_tasks.iter().chain(pb.tasks.iter()).chain(pb.post_tasks.iter()) {
            self.run_task(task, pb, nodes, &vars, &mut registers, &mut report)
                .await?;
        }
        Ok(report)
    }

    async fn run_task(
        &self,
        task: &ParsedTask,
        pb: &ParsedPlaybook,
        nodes: &[ResolvedNode],
        vars: &HashMap<String, String>,
        registers: &mut HashMap<String, String>,
        report: &mut PlaybookRunReport,
    ) -> Result<()> {
        // 条件求值（phase1 支持简单真假 / ==；完整表达式留 phase2）
        if let Some(cond) = &task.condition {
            if !eval_condition(&cond.expression, vars, registers) {
                return Ok(()); // 条件不满足，跳过
            }
        }

        let cmd = render(&task.action, vars, registers);
        let results = self
            .batch
            .run(nodes, &cmd, &owl_exec::BatchOptions::default())
            .await?;

        let mut any_failed = false;
        for r in &results {
            if !r.success {
                any_failed = true;
            }
            report.results.push(r.clone());
        }

        if any_failed {
            report.failed += 1;
            // pipeline 严格模式或 any_errors_fatal 时中断
            if pb.execution_mode == ExecutionMode::Pipeline || task.raw.any_errors_fatal {
                return Err(owl_core::error::OwlError::PlaybookExec(format!(
                    "任务失败: {}",
                    task.name
                )));
            }
            if task.raw.ignore_errors {
                return Ok(());
            }
        }

        if !task.raw.register.is_empty() {
            let stdout = results
                .first()
                .map(|r| r.stdout.clone())
                .unwrap_or_default();
            registers.insert(task.raw.register.clone(), stdout);
        }
        Ok(())
    }
}

fn json_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 变量替换 {{ name }}
fn render(input: &str, vars: &HashMap<String, String>, registers: &HashMap<String, String>) -> String {
    let mut out = input.to_string();
    for (k, v) in vars.iter().chain(registers.iter()) {
        out = out.replace(&format!("{{{{ {} }}}}", k), v);
        out = out.replace(&format!("{{{{{}}}}}", k), v);
    }
    out
}

fn eval_condition(expr: &str, vars: &HashMap<String, String>, registers: &HashMap<String, String>) -> bool {
    let e = render(expr.trim(), vars, registers);
    if e.eq_ignore_ascii_case("false") || e.is_empty() {
        return false;
    }
    if e.eq_ignore_ascii_case("true") {
        return true;
    }
    // 支持 a == b / a != b
    for (op, expect_eq) in [("==", true), ("!=", false)] {
        if let Some(pos) = e.find(op) {
            let left = e[..pos].trim();
            let right = e[pos + op.len()..].trim();
            return (left == right) == expect_eq;
        }
    }
    !e.is_empty()
}
