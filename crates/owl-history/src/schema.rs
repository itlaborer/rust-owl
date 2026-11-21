//! 数据库 schema —— 对齐原项目 internal/history/db_sqlite3.go 的核心表。
//! 这里建第一阶段最小可用表集：执行记录 + 节点 + 会话。

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS command_executions (
    id           TEXT PRIMARY KEY,
    batch_id     TEXT NOT NULL,
    node_id      TEXT NOT NULL,
    node_name    TEXT,
    command      TEXT,
    exit_code    INTEGER DEFAULT 0,
    stdout       TEXT,
    stderr       TEXT,
    success      INTEGER DEFAULT 1,
    duration_ms  INTEGER DEFAULT 0,
    created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_exec_batch   ON command_executions(batch_id);
CREATE INDEX IF NOT EXISTS idx_exec_node    ON command_executions(node_id);
CREATE INDEX IF NOT EXISTS idx_exec_created ON command_executions(created_at);

CREATE TABLE IF NOT EXISTS nodes (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    address    TEXT,
    port       INTEGER DEFAULT 22,
    user_name  TEXT,
    groups     TEXT,
    labels     TEXT,
    status     TEXT,
    updated_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);

CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    node_id     TEXT,
    status      TEXT,
    created_at  TEXT NOT NULL
);
"#;
