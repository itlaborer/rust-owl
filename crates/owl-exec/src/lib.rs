//! owl-exec —— 批量命令/脚本执行控制层。
//! 对应原项目 internal/control/command + blacklist + async + task。

pub mod blacklist;
pub mod batch;

pub use batch::{BatchExecutor, BatchOptions};
pub use blacklist::Blacklist;
