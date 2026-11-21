//! owl-node —— 节点管理与解析。
//! 对应原项目 internal/node：local / api / ssh-config 三源回退解析。

pub mod resolver;
pub mod store;

pub use resolver::{NodeResolver, ResolvedNode};
pub use store::{LocalNodeStore, LocalNodeStoreError};
