//! 批量执行器：一次命令打到 N 个节点，并发执行，聚合结果。
//! 对齐原项目：一个 batch_id 下产生 N 个 Task。

use chrono::Utc;
use futures::future::join_all;
use owl_core::error::Result;
use owl_core::model::ExecResult;
use owl_node::ResolvedNode;
use owl_ssh::{Executor, NativeExecutor};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct BatchOptions {
    pub parallel: bool,
    pub timeout_secs: u64,
    pub check_danger: bool,
}

impl Default for BatchOptions {
    fn default() -> Self {
        Self {
            parallel: true,
            timeout_secs: 60,
            check_danger: true,
        }
    }
}

pub struct BatchExecutor {
    exec: NativeExecutor,
    blacklist: super::blacklist::Blacklist,
}

impl BatchExecutor {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            exec: NativeExecutor::new(timeout_secs),
            blacklist: super::blacklist::Blacklist::load(),
        }
    }

    pub async fn run(
        &self,
        nodes: &[ResolvedNode],
        command: &str,
        opts: &BatchOptions,
    ) -> Result<Vec<ExecResult>> {
        if opts.check_danger {
            self.blacklist.check(command)?;
        }
        let batch_id = Uuid::new_v4().to_string();

        // 构造每个节点的 future
        let tasks: Vec<_> = nodes
            .iter()
            .map(|node| self.run_one(node, command, &batch_id))
            .collect();

        let results = if opts.parallel {
            join_all(tasks).await
        } else {
            let mut seq = Vec::new();
            for t in tasks {
                seq.push(t.await);
            }
            seq
        };

        results.into_iter().collect()
    }

    async fn run_one(
        &self,
        node: &ResolvedNode,
        command: &str,
        batch_id: &str,
    ) -> Result<ExecResult> {
        let started = Utc::now();
        let out = self.exec.execute(node, command).await?;
        let finished = Utc::now();
        let duration_ms = (finished - started).num_milliseconds().max(0) as u64;

        Ok(ExecResult {
            batch_id: batch_id.to_string(),
            node_id: node.id.clone(),
            node_name: node.name.clone(),
            command: command.to_string(),
            exit_code: out.exit_code,
            stdout: out.stdout,
            stderr: out.stderr,
            success: out.exit_code == 0,
            started_at: started,
            finished_at: finished,
            duration_ms,
        })
    }
}
