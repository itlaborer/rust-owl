//! owl-transfer —— 基于 SFTP 的文件上传/下载与 rsync 式增量同步。
//! 对应原项目 internal/control/transfer + cmd/cli/cmd/file。
//! 传输层只做单节点；多节点并行由调用方（CLI）用 join_all 编排。
//! P2P 扩散/中继由 `diffusion` feature 在后续阶段启用。

pub mod sync;

use async_trait::async_trait;
use owl_core::error::{OwlError, Result};
use owl_node::ResolvedNode;
use owl_ssh::ConnectionManager;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use std::io::SeekFrom;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::time::timeout;

/// 单次文件传输结果（对齐原项目 transfer.TransferResult）。
#[derive(Debug, Clone)]
pub struct TransferResult {
    pub node_name: String,
    pub local_path: String,
    pub remote_path: String,
    pub bytes: u64,
    pub success: bool,
    /// 人类可读的说明（成功时含速度/是否续传，失败时为错误原因）。
    pub message: String,
}

/// 传输选项（对齐原项目 UploadOptions / DownloadOptions 的公共部分）。
#[derive(Debug, Clone)]
pub struct TransferOptions {
    /// 覆盖已存在的远端/本地文件（默认 true）
    pub overwrite: bool,
    /// 拒绝覆盖已存在文件（优先级高于 overwrite）
    pub no_overwrite: bool,
    /// 断点续传：从已传输字节处继续
    pub resume: bool,
    /// 自动创建远端父目录
    pub create_dirs: bool,
    /// 分块大小（字节），默认 64 KiB
    pub chunk_size: usize,
    /// 传输超时（秒）
    pub timeout_secs: u64,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            overwrite: true,
            no_overwrite: false,
            resume: true,
            create_dirs: true,
            chunk_size: 64 * 1024,
            timeout_secs: 300,
        }
    }
}

#[async_trait]
pub trait Transfer: Send + Sync {
    async fn upload(
        &self,
        node: &ResolvedNode,
        local: &str,
        remote: &str,
    ) -> Result<TransferResult> {
        self.upload_with(node, local, remote, &TransferOptions::default())
            .await
    }

    async fn download(
        &self,
        node: &ResolvedNode,
        remote: &str,
        local: &str,
    ) -> Result<TransferResult> {
        self.download_with(node, remote, local, &TransferOptions::default())
            .await
    }

    async fn upload_with(
        &self,
        node: &ResolvedNode,
        local: &str,
        remote: &str,
        opts: &TransferOptions,
    ) -> Result<TransferResult>;

    async fn download_with(
        &self,
        node: &ResolvedNode,
        remote: &str,
        local: &str,
        opts: &TransferOptions,
    ) -> Result<TransferResult>;
}

/// SFTP 传输器。持有连接池，复用 owl-ssh 已建立的 SSH 会话。
pub struct SftpTransfer {
    conn: ConnectionManager,
    default_timeout_secs: u64,
}

impl SftpTransfer {
    pub fn new(default_timeout_secs: u64) -> Self {
        Self {
            conn: ConnectionManager::new(default_timeout_secs),
            default_timeout_secs,
        }
    }

    /// 建立到节点的 SFTP 会话。crate 内可供 sync 模块复用同一条会话。
    pub(crate) async fn sftp_session(&self, node: &ResolvedNode) -> Result<SftpSession> {
        let session = self.conn.acquire(node).await?;
        let stream = session.open_sftp_stream().await?;
        let sftp = SftpSession::new(stream)
            .await
            .map_err(|e| OwlError::Transfer(format!("初始化 SFTP 会话失败: {}", e)))?;
        Ok(sftp)
    }

    pub fn close_all(&self) {
        self.conn.close_all();
    }
}

#[async_trait]
impl Transfer for SftpTransfer {
    async fn upload_with(
        &self,
        node: &ResolvedNode,
        local: &str,
        remote: &str,
        opts: &TransferOptions,
    ) -> Result<TransferResult> {
        let result = TransferResult {
            node_name: node.name.clone(),
            local_path: local.to_string(),
            remote_path: remote.to_string(),
            bytes: 0,
            success: false,
            message: String::new(),
        };

        let timeout_secs = effective_timeout(opts.timeout_secs, self.default_timeout_secs);
        let fut = async {
            let sftp = self.sftp_session(node).await?;
            let out = upload_via(&sftp, local, remote, opts).await;
            sftp.close().await.ok();
            out
        };

        match timeout(Duration::from_secs(timeout_secs), fut).await {
            Ok(Ok((bytes, _resumed, msg))) => Ok(TransferResult {
                bytes,
                success: true,
                message: msg,
                ..result
            }),
            Ok(Err(e)) => Ok(TransferResult {
                message: e.to_string(),
                ..result
            }),
            Err(_) => Ok(TransferResult {
                message: format!("上传超时（{}s）", timeout_secs),
                ..result
            }),
        }
    }

    async fn download_with(
        &self,
        node: &ResolvedNode,
        remote: &str,
        local: &str,
        opts: &TransferOptions,
    ) -> Result<TransferResult> {
        let result = TransferResult {
            node_name: node.name.clone(),
            local_path: local.to_string(),
            remote_path: remote.to_string(),
            bytes: 0,
            success: false,
            message: String::new(),
        };

        let timeout_secs = effective_timeout(opts.timeout_secs, self.default_timeout_secs);
        let fut = async {
            let sftp = self.sftp_session(node).await?;
            let out = download_via(&sftp, remote, local, opts).await;
            sftp.close().await.ok();
            out
        };

        match timeout(Duration::from_secs(timeout_secs), fut).await {
            Ok(Ok((bytes, _resumed, msg))) => Ok(TransferResult {
                bytes,
                success: true,
                message: msg,
                ..result
            }),
            Ok(Err(e)) => Ok(TransferResult {
                message: e.to_string(),
                ..result
            }),
            Err(_) => Ok(TransferResult {
                message: format!("下载超时（{}s）", timeout_secs),
                ..result
            }),
        }
    }
}

fn effective_timeout(requested: u64, default_secs: u64) -> u64 {
    if requested == 0 { default_secs } else { requested }
}

/// 在一条已建立的 SFTP 会话内上传单个文件。
/// 抽成独立函数供 upload_with 与 sync 引擎复用，避免增量同步时逐文件重连。
pub(crate) async fn upload_via(
    sftp: &SftpSession,
    local: &str,
    remote: &str,
    opts: &TransferOptions,
) -> Result<(u64, bool, String)> {
    let mut lf = tokio::fs::File::open(local)
        .await
        .map_err(|e| OwlError::Transfer(format!("打开本地文件 {} 失败: {}", local, e)))?;
    let local_meta = lf
        .metadata()
        .await
        .map_err(|e| OwlError::Transfer(format!("读取本地文件大小失败: {}", e)))?;
    let file_size = local_meta.len();
    // 保留源文件 mtime：增量同步靠 size+mtime 判定，若传输端不写回时间戳，
    // 下次同步会把所有文件误判为「修改时间不同」而重复传输。
    let source_mtime_secs = local_meta.modified().ok().and_then(|t| {
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as u32)
    });

    if opts.create_dirs {
        if let Some(parent) = remote_parent(remote) {
            ensure_remote_dir(sftp, parent).await?;
        }
    }

    let exists = sftp
        .try_exists(remote)
        .await
        .map_err(|e| OwlError::Transfer(format!("检查远端文件失败: {}", e)))?;

    if exists && opts.no_overwrite {
        return Err(OwlError::Transfer(format!(
            "远端文件已存在且指定了 no-overwrite: {}",
            remote
        )));
    }

    let mut offset = 0u64;
    let mut resumed = false;

    if exists && opts.resume {
        let mut rf = sftp
            .open(remote)
            .await
            .map_err(|e| OwlError::Transfer(format!("打开远端文件取大小失败: {}", e)))?;
        offset = rf
            .seek(SeekFrom::End(0))
            .await
            .map_err(|e| OwlError::Transfer(format!("获取远端文件大小失败: {}", e)))?;
        rf.close().await.ok();
        resumed = offset > 0;
        if offset >= file_size {
            lf.seek(SeekFrom::Start(0)).await.ok();
            return Ok((0u64, resumed, "已存在且大小匹配，跳过".to_string()));
        }
        lf.seek(SeekFrom::Start(offset))
            .await
            .map_err(|e| OwlError::Transfer(format!("本地文件 seek 失败: {}", e)))?;
    }

    // 打开远端文件：resume 用 WRITE（配合 seek 到 offset），否则 CREATE|TRUNCATE
    let flags = if resumed && offset > 0 {
        OpenFlags::WRITE
    } else {
        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE
    };
    let mut rf = sftp
        .open_with_flags(remote, flags)
        .await
        .map_err(|e| OwlError::Transfer(format!("打开远端文件失败 {}: {}", remote, e)))?;
    if resumed && offset > 0 {
        rf.seek(SeekFrom::Start(offset))
            .await
            .map_err(|e| OwlError::Transfer(format!("远端文件 seek 失败: {}", e)))?;
    }

    let mut buf = vec![0u8; opts.chunk_size.max(4096)];
    let mut written = 0u64;
    loop {
        let n = lf
            .read(&mut buf)
            .await
            .map_err(|e| OwlError::Transfer(format!("读取本地文件失败: {}", e)))?;
        if n == 0 {
            break;
        }
        rf.write_all(&buf[..n])
            .await
            .map_err(|e| OwlError::Transfer(format!("写入远端文件失败: {}", e)))?;
        written += n as u64;
    }
    rf.flush()
        .await
        .map_err(|e| OwlError::Transfer(format!("刷新远端文件失败: {}", e)))?;
    // 显式 close 等待远端确认，避免写入被静默丢弃
    rf.close()
        .await
        .map_err(|e| OwlError::Transfer(format!("关闭远端文件失败: {}", e)))?;

    // 写回源文件 mtime（best-effort）：让支持 setstat 的服务端保留时间戳，
    // 保证下一次增量同步能正确判定「两端一致」。不支持的服务端忽略即可。
    if let Some(mtime) = source_mtime_secs {
        let mut attrs = russh_sftp::protocol::FileAttributes::empty();
        attrs.mtime = Some(mtime);
        attrs.atime = Some(mtime);
        let _ = sftp.set_metadata(remote, attrs).await;
    }

    let msg = if resumed {
        format!("续传 {} 字节", written)
    } else {
        format!("上传 {} 字节", written)
    };
    Ok((written, resumed, msg))
}

/// 在一条已建立的 SFTP 会话内下载单个文件。
pub(crate) async fn download_via(
    sftp: &SftpSession,
    remote: &str,
    local: &str,
    opts: &TransferOptions,
) -> Result<(u64, bool, String)> {
    let mut rf = sftp
        .open(remote)
        .await
        .map_err(|e| OwlError::Transfer(format!("打开远端文件失败 {}: {}", remote, e)))?;
    let remote_size = rf
        .seek(SeekFrom::End(0))
        .await
        .map_err(|e| OwlError::Transfer(format!("获取远端文件大小失败: {}", e)))?;
    // 取远端 mtime，下载后写回本地，保证 pull 的幂等性
    let remote_mtime = rf
        .metadata()
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        });

    // 本地父目录
    if let Some(parent) = Path::new(local).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                OwlError::Transfer(format!("创建本地目录 {:?} 失败: {}", parent, e))
            })?;
        }
    }

    let local_exists = tokio::fs::metadata(local).await.is_ok();
    if local_exists && opts.no_overwrite {
        return Err(OwlError::Transfer(format!(
            "本地文件已存在且指定了 no-overwrite: {}",
            local
        )));
    }

    let mut offset = 0u64;
    let mut resumed = false;
    if local_exists && opts.resume {
        offset = tokio::fs::metadata(local)
            .await
            .map_err(|e| OwlError::Transfer(format!("读取本地文件大小失败: {}", e)))?
            .len();
        resumed = offset > 0;
        if offset >= remote_size {
            return Ok((0u64, resumed, "已存在且大小匹配，跳过".to_string()));
        }
    }

    rf.seek(SeekFrom::Start(offset))
        .await
        .map_err(|e| OwlError::Transfer(format!("远端文件 seek 失败: {}", e)))?;

    let mut lf = if resumed && offset > 0 {
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(local)
            .await
            .map_err(|e| OwlError::Transfer(format!("以追加模式打开本地文件失败: {}", e)))?;
        f.seek(SeekFrom::End(0)).await.ok();
        f
    } else {
        tokio::fs::File::create(local)
            .await
            .map_err(|e| OwlError::Transfer(format!("创建本地文件失败: {}", e)))?
    };

    let mut buf = vec![0u8; opts.chunk_size.max(4096)];
    let mut written = 0u64;
    loop {
        let n = rf
            .read(&mut buf)
            .await
            .map_err(|e| OwlError::Transfer(format!("读取远端文件失败: {}", e)))?;
        if n == 0 {
            break;
        }
        lf.write_all(&buf[..n])
            .await
            .map_err(|e| OwlError::Transfer(format!("写入本地文件失败: {}", e)))?;
        written += n as u64;
    }
    lf.flush()
        .await
        .map_err(|e| OwlError::Transfer(format!("刷新本地文件失败: {}", e)))?;
    drop(lf);
    rf.close().await.ok();

    // 写回远端 mtime 到本地文件，保证 pull 后再次同步能正确判定「两端一致」。
    if let Some(mtime) = remote_mtime {
        if let Some(t) = std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(mtime)) {
            let _ = std::fs::File::options()
                .write(true)
                .open(local)
                .and_then(|f| f.set_modified(t));
        }
    }

    let msg = if resumed {
        format!("续传 {} 字节", written)
    } else {
        format!("下载 {} 字节", written)
    };
    Ok((written, resumed, msg))
}

/// 取远端路径的父目录（按 '/' 分割，不依赖本地 OS 语义）。
pub(crate) fn remote_parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    let idx = trimmed.rfind('/')?;
    if idx == 0 {
        Some("/")
    } else {
        Some(&trimmed[..idx])
    }
}

/// 逐级创建远端目录；已存在的目录忽略错误。
pub(crate) async fn ensure_remote_dir(sftp: &SftpSession, path: &str) -> Result<()> {
    let absolute = path.starts_with('/');
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut cur = if absolute {
        String::from("/")
    } else {
        String::new()
    };
    for part in parts {
        if !cur.is_empty() && !cur.ends_with('/') {
            cur.push('/');
        }
        cur.push_str(part);
        match sftp.try_exists(&cur).await {
            Ok(true) => continue,
            Ok(false) => {
                sftp.create_dir(&cur).await.map_err(|e| {
                    OwlError::Transfer(format!("创建远端目录 {} 失败: {}", cur, e))
                })?;
            }
            Err(e) => {
                return Err(OwlError::Transfer(format!(
                    "检查远端目录 {} 失败: {}",
                    cur, e
                )))
            }
        }
    }
    Ok(())
}
