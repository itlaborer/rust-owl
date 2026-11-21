//! owl-history —— 执行历史数据库。
//! 对应原项目 internal/history，使用 bundled SQLite（rusqlite bundled，
//! 无系统 CGO 依赖，可交叉编译到 x86_64-pc-windows-gnu）。

pub mod db;
pub mod schema;

pub use db::HistoryDb;
