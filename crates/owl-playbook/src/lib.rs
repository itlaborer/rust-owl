//! owl-playbook —— YAML 剧本解析与执行编排。
//! 数据结构对齐原项目 internal/control/playbook/parser.go。

pub mod model;
pub mod parser;
pub mod executor;

pub use model::{Playbook, PlaybookTask, ExecutionMode, ParsedPlaybook, ParsedTask};
pub use parser::Parser;
pub use executor::PlaybookExecutor;
