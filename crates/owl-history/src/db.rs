//! 历史库封装：rusqlite Connection（bundled SQLite）。

use crate::schema::SCHEMA;
use chrono::Utc;
use owl_core::config::db_path;
use owl_core::error::{OwlError, Result};
use owl_core::model::ExecResult;
use rusqlite::{params, Connection};
use std::path::PathBuf;

pub struct HistoryDb {
    conn: Connection,
}

impl HistoryDb {
    /// 打开（或创建）历史库并建表。路径可用 OWL_DB_PATH 覆盖。
    pub fn open() -> Result<Self> {
        Self::open_at(db_path())
    }

    pub fn open_at(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| OwlError::Database(format!("创建目录 {:?}: {}", parent, e)))?;
        }
        let conn = Connection::open(&path)
            .map_err(|e| OwlError::Database(format!("打开 {:?}: {}", path, e)))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| OwlError::Database(e.to_string()))?;
        Ok(Self { conn })
    }

    /// 内存库，便于测试
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| OwlError::Database(e.to_string()))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| OwlError::Database(e.to_string()))?;
        Ok(Self { conn })
    }

    pub fn record_exec(&self, r: &ExecResult) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn
            .execute(
                "INSERT INTO command_executions
                 (id, batch_id, node_id, node_name, command, exit_code, stdout, stderr, success, duration_ms, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    id,
                    r.batch_id,
                    r.node_id,
                    r.node_name,
                    r.command,
                    r.exit_code,
                    r.stdout,
                    r.stderr,
                    r.success as i32,
                    r.duration_ms as i64,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|e| OwlError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<ExecResult>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT batch_id, node_id, node_name, command, exit_code, stdout, stderr, success, created_at
                 FROM command_executions ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|e| OwlError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([limit as i64], |row| {
                Ok(ExecResult {
                    batch_id: row.get(0)?,
                    node_id: row.get(1)?,
                    node_name: row.get(2)?,
                    command: row.get(3)?,
                    exit_code: row.get(4)?,
                    stdout: row.get(5)?,
                    stderr: row.get(6)?,
                    success: row.get::<_, i32>(7)? != 0,
                    started_at: parse_dt(&row.get::<_, String>(8)?),
                    finished_at: parse_dt(&row.get::<_, String>(8)?),
                    duration_ms: 0,
                })
            })
            .map_err(|e| OwlError::Database(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| OwlError::Database(e.to_string()))
    }
}

fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
