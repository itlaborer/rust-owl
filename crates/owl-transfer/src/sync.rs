//! rsync 式目录增量同步。
//!
//! 语义对齐 rsync 的默认快速检查（quick check）：递归扫描两端文件清单，
//! 按「大小 + 修改时间」判断差异，只传输新增/变化的文件；可选内容校验和，
//! 可选删除目标端多余文件。不是 rsync 的 rolling-checksum 块级增量协议，
//! 但配合断点续传可覆盖绝大多数配置/日志/发布包同步场景。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use owl_core::error::{OwlError, Result};
use owl_node::ResolvedNode;
use russh_sftp::client::SftpSession;
use tokio::io::AsyncReadExt;
use tokio::time::timeout;

use crate::{download_via, ensure_remote_dir, upload_via, SftpTransfer, TransferOptions};

/// 同步方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    /// 本地 → 远端
    Push,
    /// 远端 → 本地
    Pull,
}

impl SyncDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncDirection::Push => "push",
            SyncDirection::Pull => "pull",
        }
    }
}

/// 对单个文件做出的同步决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    /// 目标端不存在，需新建
    Create,
    /// 大小/时间/校验和不同，需覆盖
    Update,
    /// 源端已不存在，需删除目标端文件（仅 --delete）
    Delete,
    /// 两端一致，跳过
    Skip,
}

impl SyncAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncAction::Create => "新增",
            SyncAction::Update => "更新",
            SyncAction::Delete => "删除",
            SyncAction::Skip => "跳过",
        }
    }
}

/// 单条同步计划。
#[derive(Debug, Clone)]
pub struct SyncChange {
    /// 相对同步根目录的 POSIX 风格路径
    pub rel: String,
    pub action: SyncAction,
    /// 人类可读的判定依据
    pub reason: String,
}

/// 同步选项。
#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub direction: SyncDirection,
    /// 删除目标端存在但源端不存在的文件（危险操作，需显式开启）
    pub delete: bool,
    /// 大小与修改时间相同时仍读取内容比较校验和
    pub checksum: bool,
    /// 只生成计划，不实际传输/删除
    pub dry_run: bool,
    /// 忽略修改时间，只按大小判断（配合 checksum 可纯按内容）
    pub ignore_times: bool,
    /// 保留断点续传（默认开启）
    pub resume: bool,
    /// 排除模式，支持 `*`、`?` 通配符；匹配相对路径或文件名
    pub excludes: Vec<String>,
    /// 分块大小
    pub chunk_size: usize,
    /// 单文件超时（秒）
    pub timeout_secs: u64,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            direction: SyncDirection::Push,
            delete: false,
            checksum: false,
            dry_run: false,
            ignore_times: false,
            resume: true,
            excludes: Vec::new(),
            chunk_size: 64 * 1024,
            timeout_secs: 300,
        }
    }
}

/// 单节点同步结果统计。
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub node_name: String,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub deleted: usize,
    /// 实际传输字节数（dry-run 为 0）
    pub bytes: u64,
    /// 完整计划（含 Skip）
    pub changes: Vec<SyncChange>,
    /// 传输/删除失败的错误信息
    pub errors: Vec<String>,
    pub success: bool,
}

/// 文件清单条目。
#[derive(Debug, Clone)]
struct FileEntry {
    size: u64,
    mtime_secs: Option<u64>,
}

impl SftpTransfer {
    /// 对一个节点执行目录增量同步。
    ///
    /// `local_root` 始终是本地目录，`remote_root` 始终是远端目录；
    /// 方向由 `opts.direction` 决定谁是源、谁是目标。
    pub async fn sync_dir(
        &self,
        node: &ResolvedNode,
        local_root: &str,
        remote_root: &str,
        opts: &SyncOptions,
    ) -> Result<SyncReport> {
        let mut report = SyncReport {
            node_name: node.name.clone(),
            ..Default::default()
        };

        let local_path = Path::new(local_root);
        if !local_path.is_dir() {
            return Err(OwlError::Transfer(format!(
                "本地目录不存在或不是目录: {}",
                local_root
            )));
        }

        // 整次同步复用一条 SFTP 会话，避免每个文件重连/重开 subsystem。
        let sftp = self.sftp_session(node).await?;

        let mut local_files = BTreeMap::new();
        scan_local_dir(local_path, "", &opts.excludes, &mut local_files)?;

        let mut remote_files = BTreeMap::new();
        // "/" 经 trim_end_matches('/') 会变成空串，部分 SFTP 实现对空路径会回落到
        // 客户端 cwd 而非根目录，导致文件写到未知位置。这里显式保留 "/"。
        let remote_root_normalized = {
            let t = remote_root.trim_end_matches('/');
            if t.is_empty() {
                "/"
            } else {
                t
            }
        };
        if sftp
            .try_exists(remote_root_normalized)
            .await
            .map_err(|e| OwlError::Transfer(format!("检查远端目录失败: {}", e)))?
        {
            scan_remote_dir(
                &sftp,
                remote_root_normalized,
                "",
                &opts.excludes,
                &mut remote_files,
            )
            .await?;
        } else if opts.dry_run {
            // 只出计划时不创建远端目录
        } else {
            ensure_remote_dir(&sftp, remote_root_normalized).await?;
        }

        let plan = build_plan(
            local_path,
            remote_root_normalized,
            &sftp,
            &local_files,
            &remote_files,
            opts,
        )
        .await?;

        let transfer_opts = TransferOptions {
            overwrite: true,
            no_overwrite: false,
            resume: opts.resume,
            create_dirs: true,
            chunk_size: opts.chunk_size,
            timeout_secs: opts.timeout_secs,
        };

        let mut deleted_dirs = BTreeSet::new();
        let per_file_timeout = Duration::from_secs(opts.timeout_secs.max(1));

        for change in &plan {
            match change.action {
                SyncAction::Skip => {
                    report.skipped += 1;
                }
                SyncAction::Create | SyncAction::Update => {
                    let local_file = local_path.join(&change.rel);
                    let local_str = local_file.to_string_lossy().into_owned();
                    let remote_str = join_remote(remote_root_normalized, &change.rel);

                    if change.action == SyncAction::Create {
                        report.created += 1;
                    } else {
                        report.updated += 1;
                    }
                    if opts.dry_run {
                        continue;
                    }

                    let op = async {
                        match opts.direction {
                            SyncDirection::Push => {
                                upload_via(&sftp, &local_str, &remote_str, &transfer_opts).await
                            }
                            SyncDirection::Pull => {
                                download_via(&sftp, &remote_str, &local_str, &transfer_opts).await
                            }
                        }
                    };

                    match timeout(per_file_timeout, op).await {
                        Ok(Ok((bytes, _resumed, _msg))) => {
                            report.bytes += bytes;
                        }
                        Ok(Err(e)) => report.errors.push(format!("{}: {}", change.rel, e)),
                        Err(_) => report.errors.push(format!(
                            "{}: 传输超时（{}s）",
                            change.rel, opts.timeout_secs
                        )),
                    }
                }
                SyncAction::Delete => {
                    report.deleted += 1;
                    if opts.dry_run {
                        continue;
                    }
                    let ok = match opts.direction {
                        SyncDirection::Push => {
                            let remote_str = join_remote(remote_root_normalized, &change.rel);
                            sftp.remove_file(remote_str.as_str())
                                .await
                                .map_err(|e| e.to_string())
                        }
                        SyncDirection::Pull => {
                            let local_file = local_path.join(&change.rel);
                            tokio::fs::remove_file(&local_file)
                                .await
                                .map_err(|e| e.to_string())
                        }
                    };
                    if let Err(e) = ok {
                        report
                            .errors
                            .push(format!("{}: 删除失败 {}", change.rel, e));
                    } else if let Some(parent) = parent_rel(&change.rel) {
                        if !parent.is_empty() {
                            deleted_dirs.insert(parent.to_string());
                        }
                    }
                }
            }
        }

        if !opts.dry_run && !deleted_dirs.is_empty() {
            match opts.direction {
                SyncDirection::Push => {
                    prune_remote_dirs(&sftp, remote_root_normalized, &deleted_dirs).await
                }
                SyncDirection::Pull => prune_local_dirs(local_path, &deleted_dirs),
            }
        }

        report.changes = plan;
        sftp.close().await.ok();
        report.success = report.errors.is_empty();
        Ok(report)
    }
}

/// 递归扫描本地目录，记录相对路径 → 文件元信息。
fn scan_local_dir(
    dir: &Path,
    prefix: &str,
    excludes: &[String],
    out: &mut BTreeMap<String, FileEntry>,
) -> Result<()> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| OwlError::Transfer(format!("读取本地目录 {:?} 失败: {}", dir, e)))?;
    for item in rd {
        let item = item.map_err(|e| OwlError::Transfer(format!("读取本地目录项失败: {}", e)))?;
        let name = item.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{}/{}", prefix, name)
        };
        if is_excluded(&rel, excludes) {
            continue;
        }
        let ft = item
            .file_type()
            .map_err(|e| OwlError::Transfer(format!("读取文件类型 {} 失败: {}", rel, e)))?;
        if ft.is_dir() {
            scan_local_dir(&item.path(), &rel, excludes, out)?;
        } else if ft.is_file() {
            let md = item
                .metadata()
                .map_err(|e| OwlError::Transfer(format!("读取文件属性 {} 失败: {}", rel, e)))?;
            out.insert(
                rel,
                FileEntry {
                    size: md.len(),
                    mtime_secs: md.modified().ok().and_then(mtime_secs),
                },
            );
        }
        // 符号链接等其它类型跳过，避免同步循环
    }
    Ok(())
}

/// 递归扫描远端目录。SFTP 的 read_dir 返回带元数据的同步迭代器，
/// 目录项里自带 stat，无需逐文件再发 STAT 请求。
async fn scan_remote_dir(
    sftp: &SftpSession,
    root: &str,
    prefix: &str,
    excludes: &[String],
    out: &mut BTreeMap<String, FileEntry>,
) -> Result<()> {
    let dir = join_remote(root, if prefix.is_empty() { "" } else { prefix });
    let entries = sftp
        .read_dir(dir.as_str())
        .await
        .map_err(|e| OwlError::Transfer(format!("读取远端目录 {} 失败: {}", dir, e)))?;
    for entry in entries {
        let name = entry.file_name();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{}/{}", prefix, name)
        };
        if is_excluded(&rel, excludes) {
            continue;
        }
        let ft = entry.file_type();
        if ft.is_dir() {
            Box::pin(scan_remote_dir(sftp, root, &rel, excludes, &mut *out)).await?;
        } else if ft.is_file() {
            let md = entry.metadata();
            out.insert(
                rel,
                FileEntry {
                    size: md.len(),
                    mtime_secs: md.modified().ok().and_then(mtime_secs),
                },
            );
        }
    }
    Ok(())
}

/// 生成同步计划。
async fn build_plan(
    local_root: &Path,
    remote_root: &str,
    sftp: &SftpSession,
    local: &BTreeMap<String, FileEntry>,
    remote: &BTreeMap<String, FileEntry>,
    opts: &SyncOptions,
) -> Result<Vec<SyncChange>> {
    let (src, dst) = match opts.direction {
        SyncDirection::Push => (local, remote),
        SyncDirection::Pull => (remote, local),
    };

    let mut plan = Vec::new();
    for (rel, src_file) in src {
        let reason = match dst.get(rel) {
            None => Some("目标端不存在".to_string()),
            Some(dst_file) => {
                if src_file.size != dst_file.size {
                    Some(format!("大小 {} → {}", dst_file.size, src_file.size))
                } else if !opts.ignore_times
                    && !mtime_close(src_file.mtime_secs, dst_file.mtime_secs)
                {
                    Some("修改时间不同".to_string())
                } else if opts.checksum {
                    let local_hash = hash_local(&local_root.join(rel)).await?;
                    let remote_hash = hash_remote(sftp, &join_remote(remote_root, rel)).await?;
                    if local_hash == remote_hash {
                        None
                    } else {
                        Some("内容校验和不同".to_string())
                    }
                } else {
                    None
                }
            }
        };

        match reason {
            Some(r) => plan.push(SyncChange {
                rel: rel.clone(),
                action: if dst.contains_key(rel) {
                    SyncAction::Update
                } else {
                    SyncAction::Create
                },
                reason: r,
            }),
            None => plan.push(SyncChange {
                rel: rel.clone(),
                action: SyncAction::Skip,
                reason: "两端一致".to_string(),
            }),
        }
    }

    if opts.delete {
        for rel in dst.keys() {
            if !src.contains_key(rel) {
                plan.push(SyncChange {
                    rel: rel.clone(),
                    action: SyncAction::Delete,
                    reason: "源端已不存在".to_string(),
                });
            }
        }
    }

    plan.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(plan)
}

/// FNV-1a 64 位流式校验和。仅用于判断内容是否相同，不用于安全场景。
fn fnv1a(buf: &[u8], seed: u64) -> u64 {
    let mut h = seed;
    for &b in buf {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0001_b3);
    }
    h
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

async fn hash_local(path: &Path) -> Result<u64> {
    let mut f = tokio::fs::File::open(path)
        .await
        .map_err(|e| OwlError::Transfer(format!("校验和读取本地文件失败 {:?}: {}", path, e)))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut h = FNV_OFFSET;
    loop {
        let n = f
            .read(&mut buf)
            .await
            .map_err(|e| OwlError::Transfer(format!("校验和读取失败 {:?}: {}", path, e)))?;
        if n == 0 {
            break;
        }
        h = fnv1a(&buf[..n], h);
    }
    Ok(h)
}

async fn hash_remote(sftp: &SftpSession, path: &str) -> Result<u64> {
    let mut f = sftp
        .open(path)
        .await
        .map_err(|e| OwlError::Transfer(format!("校验和读取远端文件失败 {}: {}", path, e)))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut h = FNV_OFFSET;
    loop {
        let n = f
            .read(&mut buf)
            .await
            .map_err(|e| OwlError::Transfer(format!("校验和读取失败 {}: {}", path, e)))?;
        if n == 0 {
            break;
        }
        h = fnv1a(&buf[..n], h);
    }
    f.close().await.ok();
    Ok(h)
}

/// 拼接远端路径。路径始终按 POSIX 语义用 `/` 分隔。
fn join_remote(root: &str, rel: &str) -> String {
    let base = root.trim_end_matches('/');
    if rel.is_empty() {
        if base.is_empty() {
            "/".to_string()
        } else {
            base.to_string()
        }
    } else if base.is_empty() {
        format!("/{}", rel)
    } else {
        format!("{}/{}", base, rel)
    }
}

fn parent_rel(rel: &str) -> Option<&str> {
    let idx = rel.rfind('/')?;
    if idx == 0 {
        Some("")
    } else {
        Some(&rel[..idx])
    }
}

fn mtime_secs(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// SFTP/部分文件系统的时间戳精度只有秒级，允许 1 秒误差避免误判。
fn mtime_close(a: Option<u64>, b: Option<u64>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => x.abs_diff(y) <= 1,
        (None, None) => true,
        _ => false,
    }
}

/// 简易 glob：`*` 匹配任意字符，`?` 匹配单字符。
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn is_excluded(rel: &str, excludes: &[String]) -> bool {
    if excludes.is_empty() {
        return false;
    }
    let file_name = rel.rsplit('/').next().unwrap_or(rel);
    excludes
        .iter()
        .any(|p| glob_match(p, rel) || glob_match(p, file_name))
}

/// 删除后自底向上清理远端空目录（不动同步根目录）。
async fn prune_remote_dirs(sftp: &SftpSession, root: &str, dirs: &BTreeSet<String>) {
    let mut list: Vec<&String> = dirs.iter().collect();
    list.sort_by(|a, b| b.len().cmp(&a.len()));
    for rel in list {
        if rel.is_empty() {
            continue;
        }
        let full = join_remote(root, rel);
        if let Ok(mut rd) = sftp.read_dir(full.as_str()).await {
            if rd.next().is_none() {
                let _ = sftp.remove_dir(full.as_str()).await;
            }
        }
    }
}

/// 删除后自底向上清理本地空目录（不动同步根目录）。
fn prune_local_dirs(root: &Path, dirs: &BTreeSet<String>) {
    let mut list: Vec<&String> = dirs.iter().collect();
    list.sort_by(|a, b| b.len().cmp(&a.len()));
    for rel in list {
        if rel.is_empty() {
            continue;
        }
        let full: PathBuf = root.join(rel);
        if let Ok(mut rd) = std::fs::read_dir(&full) {
            if rd.next().is_none() {
                let _ = std::fs::remove_dir(&full);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_patterns() {
        assert!(glob_match("*.log", "app.log"));
        assert!(glob_match("*.log", "sub/app.log"));
        assert!(glob_match("data/*", "data/a.json"));
        assert!(!glob_match("*.log", "app.txt"));
        assert!(glob_match("file?.txt", "file1.txt"));
    }

    #[test]
    fn mtime_tolerance() {
        assert!(mtime_close(Some(100), Some(101)));
        assert!(!mtime_close(Some(100), Some(103)));
        assert!(mtime_close(None, None));
        assert!(!mtime_close(Some(1), None));
    }

    #[test]
    fn remote_join() {
        assert_eq!(join_remote("/tmp/foo/", "a/b.txt"), "/tmp/foo/a/b.txt");
        assert_eq!(join_remote("/", "a.txt"), "/a.txt");
        assert_eq!(join_remote("data", ""), "data");
    }

    #[test]
    fn fnv_stable() {
        assert_eq!(fnv1a(b"hello", FNV_OFFSET), fnv1a(b"hello", FNV_OFFSET));
        assert_ne!(fnv1a(b"hello", FNV_OFFSET), fnv1a(b"world", FNV_OFFSET));
    }
}
