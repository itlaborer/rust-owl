//! owl-core —— 全局共享类型、配置、错误定义。
//!
//! 对应原 Go 项目的 `internal/common`、`internal/locale`、`internal/i18n`
//! 中「与业务无关、被所有模块共用」的部分。

pub mod config;
pub mod error;
pub mod model;

pub use error::{OwlError, Result};
