use thiserror::Error;

/// 全局统一错误类型。
/// 每种变体大致对应原项目一个错误来源域，便于上层按类别处理。
/// 注意：owl-core 不依赖 rusqlite / reqwest 等重量级库，
/// 具体库的错误在各自 crate 里 map 成这里的字符串变体。
#[derive(Debug, Error)]
pub enum OwlError {
    #[error("配置错误: {0}")]
    Config(String),

    #[error("节点未找到: {0}")]
    NodeNotFound(String),

    #[error("节点解析失败: {0}")]
    NodeResolve(String),

    #[error("SSH 连接失败: {0}")]
    SshConnect(String),

    #[error("命令执行失败: {0}")]
    Exec(String),

    #[error("文件传输失败: {0}")]
    Transfer(String),

    #[error("剧本解析失败: {0}")]
    PlaybookParse(String),

    #[error("剧本执行失败: {0}")]
    PlaybookExec(String),

    #[error("数据库错误: {0}")]
    Database(String),

    #[error("HTTP/AI 请求失败: {0}")]
    Http(String),

    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML 序列化错误: {0}")]
    SerdeYaml(#[from] serde_yaml::Error),

    #[error("JSON 序列化错误: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("危险命令已被拦截: {0}")]
    DangerousCommand(String),

    #[error("操作被用户取消")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, OwlError>;
