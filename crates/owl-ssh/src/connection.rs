//! 连接管理：基于 russh 的真实 SSH 连接 + 连接池。
//! 对齐原项目 internal/ssh/connection_pool.go / connection_manager.go / dial.go。

use dashmap::DashMap;
use owl_core::error::{OwlError, Result};
use owl_node::ResolvedNode;
use russh::client::{self, Config, Handle};
use russh::keys::load_secret_key;
use russh::Preferred;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 连接池 key：(address, port, user, auth 指纹)
type PoolKey = (String, u16, String, String);

/// 客户端事件处理器。check_server_key 默认放行（对应 go-owl 的 InsecureIgnoreHostKey），
/// 后续可接入 known_hosts 校验。
pub(crate) struct ClientHandler;

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        // TODO(security): 接入 ~/.ssh/known_hosts 校验，对齐原项目 host key 策略
        Ok(true)
    }
}

/// 一条已认证的 SSH 会话。
/// Handle 内部是 mpsc Sender，可跨 tokio 任务共享，因此用 Arc 包裹即可。
#[derive(Clone)]
pub struct SshSession {
    pub node: ResolvedNode,
    pub connected: bool,
    handle: Arc<Handle<ClientHandler>>,
}

impl SshSession {
    /// 建立连接并完成认证。认证优先级：私钥（ssh_key）-> 密码（ssh_password）。
    pub async fn connect(node: &ResolvedNode, timeout_secs: u64) -> Result<Self> {
        if node.address.is_empty() {
            return Err(OwlError::SshConnect("节点地址为空".into()));
        }

        let mut config = Config::default();
        config.inactivity_timeout = Some(Duration::from_secs(timeout_secs.max(30)));
        // 优先用现代 KEX，避免老算法协商失败
        config.preferred = Preferred {
            kex: Cow::Owned(vec![
                russh::kex::CURVE25519_PRE_RFC_8731,
                russh::kex::EXTENSION_SUPPORT_AS_CLIENT,
            ]),
            ..Default::default()
        };
        let config = Arc::new(config);

        let addrs = (node.address.as_str(), node.port);
        let mut handle = client::connect(config, addrs, ClientHandler)
            .await
            .map_err(|e| {
                OwlError::SshConnect(format!(
                    "{}@{}:{} - {}",
                    node.user, node.address, node.port, e
                ))
            })?;

        let authed = if !node.ssh_key.is_empty() {
            Self::auth_publickey(&mut handle, &node.user, &node.ssh_key).await?
        } else if !node.ssh_password.is_empty() {
            Self::auth_password(&mut handle, &node.user, &node.ssh_password).await?
        } else {
            return Err(OwlError::SshConnect(format!(
                "节点 {} 未配置私钥或密码",
                node.name
            )));
        };

        if !authed {
            return Err(OwlError::SshConnect(format!(
                "节点 {} 认证失败（用户名或凭据错误）",
                node.name
            )));
        }

        tracing::debug!(node = %node.name, "SSH 连接与认证成功");
        Ok(Self {
            node: node.clone(),
            connected: true,
            handle: Arc::new(handle),
        })
    }

    async fn auth_publickey(
        handle: &mut Handle<ClientHandler>,
        user: &str,
        key_path: &str,
    ) -> Result<bool> {
        let path = PathBuf::from(key_path);
        let key_pair = load_secret_key(&path, None)
            .map_err(|e| OwlError::SshConnect(format!("加载私钥 {:?}: {}", path, e)))?;
        let hash_alg = handle
            .best_supported_rsa_hash()
            .await
            .map_err(|e| OwlError::SshConnect(e.to_string()))?
            .flatten();
        let result = handle
            .authenticate_publickey(
                user,
                russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key_pair), hash_alg),
            )
            .await
            .map_err(|e| OwlError::SshConnect(format!("公钥认证失败: {}", e)))?;
        Ok(result.success())
    }

    async fn auth_password(
        handle: &mut Handle<ClientHandler>,
        user: &str,
        password: &str,
    ) -> Result<bool> {
        let result = handle
            .authenticate_password(user, password)
            .await
            .map_err(|e| OwlError::SshConnect(format!("密码认证失败: {}", e)))?;
        Ok(result.success())
    }

    /// 获取底层 handle 引用，供 executor 开 channel 执行命令
    pub(crate) fn handle(&self) -> &Handle<ClientHandler> {
        &self.handle
    }

    /// 打开 SFTP 子系统并返回双向字节流。
    ///
    /// 流程：open session channel → request_subsystem("sftp") → into_stream。
    /// 返回的 `ChannelStream` 实现 AsyncRead+AsyncWrite，可直接喂给
    /// `russh_sftp::client::SftpSession::new()`。用 impl Trait 隐藏具体类型，
    /// 使 owl-transfer 无需依赖 russh。
    pub async fn open_sftp_stream(
        &self,
    ) -> Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> {
        let channel = self
            .handle()
            .channel_open_session()
            .await
            .map_err(|e| OwlError::Transfer(format!("打开 SFTP channel 失败: {}", e)))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| OwlError::Transfer(format!("请求 sftp 子系统失败: {}", e)))?;
        Ok(channel.into_stream())
    }

    pub async fn close(&self) -> Result<()> {
        self.handle
            .disconnect(russh::Disconnect::ByApplication, "", "English")
            .await
            .map_err(|e| OwlError::SshConnect(format!("断开连接失败: {}", e)))
    }
}

/// 连接池：复用相同 (地址, 端口, 用户, 凭据) 的会话。
pub struct ConnectionManager {
    pool: DashMap<PoolKey, SshSession>,
    timeout_secs: u64,
}

impl ConnectionManager {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            pool: DashMap::new(),
            timeout_secs,
        }
    }

    /// 复用已有会话；没有则新建并放入池中。
    pub async fn acquire(&self, node: &ResolvedNode) -> Result<SshSession> {
        let key = Self::pool_key(node);
        if let Some(s) = self.pool.get(&key) {
            if !s.handle().is_closed() {
                return Ok(s.clone());
            }
        }
        let session = SshSession::connect(node, self.timeout_secs).await?;
        self.pool.insert(key, session.clone());
        Ok(session)
    }

    pub fn close_all(&self) {
        self.pool.clear();
    }

    pub fn len(&self) -> usize {
        self.pool.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }

    fn pool_key(node: &ResolvedNode) -> PoolKey {
        let fingerprint = if !node.ssh_key.is_empty() {
            format!("key:{}", node.ssh_key)
        } else {
            format!("pw:{}", node.ssh_password)
        };
        (
            node.address.clone(),
            node.port,
            node.user.clone(),
            fingerprint,
        )
    }
}
