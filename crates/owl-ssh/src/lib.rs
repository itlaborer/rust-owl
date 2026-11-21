//! owl-ssh —— SSH 连接与执行层。
//! 对应原项目 internal/ssh + internal/control/command/ssh_executor。
//! 使用纯 Rust 的 russh，无系统 libssh 依赖，可交叉编译到 GNU 目标。

pub mod connection;
pub mod executor;
pub mod shell;

pub use connection::{ConnectionManager, SshSession};
pub use executor::{CommandOutput, Executor, NativeExecutor};
pub use shell::{ShellChannel, ShellEvent, ShellInput};
