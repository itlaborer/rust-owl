//! owl-playbook —— YAML 剧本解析与执行编排。
//! 数据结构对齐原项目 internal/control/playbook/parser.go。

pub mod executor;
pub mod model;
pub mod parser;

pub use executor::PlaybookExecutor;
pub use model::{ExecutionMode, ParsedPlaybook, ParsedTask, Playbook, PlaybookTask};
pub use parser::Parser;
