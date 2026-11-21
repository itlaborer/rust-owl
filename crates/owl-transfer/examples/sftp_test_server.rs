//! 本地 SFTP 测试服务器（仅用于端到端验证 owl file sync）。
//!
//! 把 SFTP 路径直接映射到本机真实文件系统（以启动参数指定的 root 为根），
//! 支持 owl-transfer 增量同步所需的全部操作：
//! stat/lstat/open/read/write/close/fstat/opendir/readdir/mkdir/remove/rmdir/realpath。
//!
//! 用法：
//!   cargo run -p owl-transfer --example sftp_test_server -- <root_dir> [port]
//! 默认监听 127.0.0.1:2222，接受任意用户名/密码。

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, server};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};

// ---------------- SSH 层 ----------------

#[derive(Clone)]
struct SshServer {
    root: Arc<PathBuf>,
}

impl server::Server for SshServer {
    type Handler = SshSession;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
        SshSession {
            root: self.root.clone(),
            channels: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

struct SshSession {
    root: Arc<PathBuf>,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
}

impl server::Handler for SshSession {
    type Error = anyhow::Error;

    fn auth_password(&mut self, _user: &str, _password: &str) -> impl Future<Output = Result<Auth, Self::Error>> + Send {
        async { Ok(Auth::Accept) }
    }

    fn auth_publickey(
        &mut self,
        _user: &str,
        _key: VerifiedPublicKey<'_>,
    ) -> impl Future<Output = Result<Auth, Self::Error>> + Send {
        async { Ok(Auth::Accept) }
    }

    fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let channels = self.channels.clone();
        async move {
            channels.lock().unwrap().insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }
    }

    fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async move {
            let _ = session.close(channel);
            Ok(())
        }
    }

    fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let root = self.root.clone();
        let channels = self.channels.clone();
        let is_sftp = name == "sftp";
        async move {
            if is_sftp {
                let channel = channels.lock().unwrap().remove(&channel_id);
                session.channel_success(channel_id)?;
                if let Some(channel) = channel {
                    let handler = LocalSftp::new(root);
                    russh_sftp::server::run(channel.into_stream(), handler).await;
                }
            } else {
                session.channel_failure(channel_id)?;
            }
            Ok(())
        }
    }
}

// ---------------- SFTP 层 ----------------

/// 打开的文件句柄状态。
struct OpenFile {
    path: PathBuf,
    /// 当前读写偏移（用 seek 也可，这里显式维护更直观）
    file: std::fs::File,
}

/// 目录迭代状态。
struct OpenDir {
    entries: Vec<(String, FileAttributes)>,
    pos: usize,
}

enum HandleKind {
    File(OpenFile),
    Dir(OpenDir),
}

struct LocalSftp {
    root: Arc<PathBuf>,
    handles: HashMap<String, HandleKind>,
    next_handle: u64,
}

impl LocalSftp {
    fn new(root: Arc<PathBuf>) -> Self {
        Self {
            root,
            handles: HashMap::new(),
            next_handle: 1,
        }
    }

    /// 把 SFTP 的 POSIX 路径映射到本地路径。
    fn local(&self, path: &str) -> PathBuf {
        let trimmed = path.trim_start_matches('/');
        self.root.join(trimmed)
    }

    fn new_handle(&mut self, kind: HandleKind) -> String {
        let key = format!("h{}", self.next_handle);
        self.next_handle += 1;
        self.handles.insert(key.clone(), kind);
        key
    }

    fn ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        }
    }

    fn err(id: u32, code: StatusCode) -> Status {
        Status {
            id,
            status_code: code,
            error_message: format!("{:?}", code),
            language_tag: "en-US".into(),
        }
    }

    fn io_status(id: u32, e: &std::io::Error) -> Status {
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
            std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
            _ => StatusCode::Failure,
        };
        Self::err(id, code)
    }

    fn attrs_from_meta(id: u32, meta: &std::fs::Metadata) -> Attrs {
        Attrs {
            id,
            attrs: FileAttributes::from(meta),
        }
    }
}

impl russh_sftp::server::Handler for LocalSftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> impl Future<Output = Result<Version, Self::Error>> + Send {
        async { Ok(Version::new()) }
    }

    fn realpath(
        &mut self,
        id: u32,
        path: String,
    ) -> impl Future<Output = Result<Name, Self::Error>> + Send {
        let p = if path.is_empty() || path == "." { "/".to_string() } else { path };
        async move {
            Ok(Name {
                id,
                files: vec![File::dummy(p)],
            })
        }
    }

    fn stat(
        &mut self,
        id: u32,
        path: String,
    ) -> impl Future<Output = Result<Attrs, Self::Error>> + Send {
        let local = self.local(&path);
        async move {
            match std::fs::metadata(&local) {
                Ok(meta) => Ok(Self::attrs_from_meta(id, &meta)),
                Err(e) => Err(match e.kind() {
                    std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
                    _ => StatusCode::Failure,
                }),
            }
        }
    }

    fn lstat(
        &mut self,
        id: u32,
        path: String,
    ) -> impl Future<Output = Result<Attrs, Self::Error>> + Send {
        let local = self.local(&path);
        async move {
            match std::fs::symlink_metadata(&local) {
                Ok(meta) => Ok(Self::attrs_from_meta(id, &meta)),
                Err(e) => Err(match e.kind() {
                    std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
                    _ => StatusCode::Failure,
                }),
            }
        }
    }

    fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> impl Future<Output = Result<Handle, Self::Error>> + Send {
        let local = self.local(&filename);
        let want_write = pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND);
        let want_read = pflags.contains(OpenFlags::READ);
        let mut opts = std::fs::OpenOptions::new();
        opts.read(want_read || want_write).write(want_write);
        if pflags.contains(OpenFlags::CREATE) {
            if pflags.contains(OpenFlags::EXCLUDE) {
                opts.create_new(true);
            } else {
                opts.create(true);
            }
        }
        if pflags.contains(OpenFlags::TRUNCATE) {
            opts.truncate(true);
        }
        if pflags.contains(OpenFlags::APPEND) {
            opts.append(true);
        }
        async move {
            match opts.open(&local) {
                Ok(file) => {
                    let key = self.new_handle(HandleKind::File(OpenFile { path: local, file }));
                    Ok(Handle { id, handle: key })
                }
                Err(e) => Err(match e.kind() {
                    std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
                    std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
                    std::io::ErrorKind::AlreadyExists => StatusCode::Failure,
                    _ => StatusCode::Failure,
                }),
            }
        }
    }

    fn close(
        &mut self,
        id: u32,
        handle: String,
    ) -> impl Future<Output = Result<Status, Self::Error>> + Send {
        self.handles.remove(&handle);
        async move { Ok(Self::ok(id)) }
    }

    fn fstat(
        &mut self,
        id: u32,
        handle: String,
    ) -> impl Future<Output = Result<Attrs, Self::Error>> + Send {
        let meta = self.handles.get(&handle).and_then(|h| match h {
            HandleKind::File(f) => f.file.metadata().ok(),
            HandleKind::Dir(_) => std::fs::metadata(&f_path_placeholder()).ok(),
        });
        async move {
            match meta {
                Some(m) => Ok(Self::attrs_from_meta(id, &m)),
                None => Err(StatusCode::NoSuchFile),
            }
        }
    }

    fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> impl Future<Output = Result<Status, Self::Error>> + Send {
        let local = self.local(&path);
        async move {
            // 保留客户端传来的 mtime（SFTP v3 为秒级 Unix 时间戳）
            if let Some(mtime) = attrs.mtime {
                use std::time::{Duration, UNIX_EPOCH};
                let t = UNIX_EPOCH + Duration::from_secs(mtime as u64);
                let r = std::fs::File::options()
                    .write(true)
                    .open(&local)
                    .and_then(|f| f.set_modified(t));
                if r.is_err() {
                    return Ok(Self::err(id, StatusCode::Failure));
                }
            }
            Ok(Self::ok(id))
        }
    }

    fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> impl Future<Output = Result<Status, Self::Error>> + Send {
        let path = self.handles.get(&handle).and_then(|h| match h {
            HandleKind::File(f) => Some(f.path.clone()),
            _ => None,
        });
        async move {
            match path {
                Some(p) => {
                    if let Some(mtime) = attrs.mtime {
                        use std::time::{Duration, UNIX_EPOCH};
                        let t = UNIX_EPOCH + Duration::from_secs(mtime as u64);
                        let _ = std::fs::File::open(&p).and_then(|f| f.set_modified(t));
                    }
                    Ok(Self::ok(id))
                }
                None => Ok(Self::err(id, StatusCode::NoSuchFile)),
            }
        }
    }

    fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> impl Future<Output = Result<Data, Self::Error>> + Send {
        use std::io::{Read, Seek, SeekFrom};
        let result = (|| -> std::io::Result<Vec<u8>> {
            match self.handles.get_mut(&handle) {
                Some(HandleKind::File(f)) => {
                    f.file.seek(SeekFrom::Start(offset))?;
                    let mut buf = vec![0u8; len as usize];
                    let n = f.file.read(&mut buf)?;
                    buf.truncate(n);
                    Ok(buf)
                }
                _ => Err(std::io::Error::new(std::io::ErrorKind::Other, "bad handle")),
            }
        })();
        async move {
            match result {
                Ok(data) => Ok(Data { id, data }),
                Err(_) => Err(StatusCode::Failure),
            }
        }
    }

    fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> impl Future<Output = Result<Status, Self::Error>> + Send {
        use std::io::{Seek, SeekFrom, Write};
        let result = (|| -> std::io::Result<()> {
            match self.handles.get_mut(&handle) {
                Some(HandleKind::File(f)) => {
                    f.file.seek(SeekFrom::Start(offset))?;
                    f.file.write_all(&data)?;
                    f.file.flush()?;
                    Ok(())
                }
                _ => Err(std::io::Error::new(std::io::ErrorKind::Other, "bad handle")),
            }
        })();
        async move {
            match result {
                Ok(()) => Ok(Self::ok(id)),
                Err(e) => Ok(Self::io_status(id, &e)),
            }
        }
    }

    fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> impl Future<Output = Result<Status, Self::Error>> + Send {
        let local = self.local(&path);
        async move {
            match std::fs::create_dir(&local) {
                Ok(()) => Ok(Self::ok(id)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(Self::ok(id)),
                Err(e) => Ok(Self::io_status(id, &e)),
            }
        }
    }

    fn remove(
        &mut self,
        id: u32,
        filename: String,
    ) -> impl Future<Output = Result<Status, Self::Error>> + Send {
        let local = self.local(&filename);
        async move {
            match std::fs::remove_file(&local) {
                Ok(()) => Ok(Self::ok(id)),
                Err(e) => Ok(Self::io_status(id, &e)),
            }
        }
    }

    fn rmdir(
        &mut self,
        id: u32,
        path: String,
    ) -> impl Future<Output = Result<Status, Self::Error>> + Send {
        let local = self.local(&path);
        async move {
            match std::fs::remove_dir(&local) {
                Ok(()) => Ok(Self::ok(id)),
                Err(e) => Ok(Self::io_status(id, &e)),
            }
        }
    }

    fn opendir(
        &mut self,
        id: u32,
        path: String,
    ) -> impl Future<Output = Result<Handle, Self::Error>> + Send {
        let local = self.local(&path);
        let mut entries = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&local) {
            for e in rd.flatten() {
                if let Ok(meta) = e.metadata() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name == "." || name == ".." {
                        continue;
                    }
                    entries.push((name, FileAttributes::from(&meta)));
                }
            }
        }
        let key = self.new_handle(HandleKind::Dir(OpenDir { entries, pos: 0 }));
        async move { Ok(Handle { id, handle: key }) }
    }

    fn readdir(
        &mut self,
        id: u32,
        handle: String,
    ) -> impl Future<Output = Result<Name, Self::Error>> + Send {
        let mut batch = Vec::new();
        let mut eof = true;
        if let Some(HandleKind::Dir(d)) = self.handles.get_mut(&handle) {
            // 每批最多返回 64 个，返回完后再给 Eof
            while d.pos < d.entries.len() && batch.len() < 64 {
                let (name, attrs) = &d.entries[d.pos];
                batch.push(File::new(name.clone(), attrs.clone()));
                d.pos += 1;
            }
            eof = d.pos >= d.entries.len();
        }
        async move {
            if batch.is_empty() && eof {
                Err(StatusCode::Eof)
            } else {
                Ok(Name { id, files: batch })
            }
        }
    }
}

/// 仅为 fstat 目录句柄时的占位（实际 rust 目录句柄很少走 fstat，返回根属性即可）。
fn f_path_placeholder() -> PathBuf {
    PathBuf::from(".")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let root = args
        .get(1)
        .map(|s| PathBuf::from(s))
        .unwrap_or_else(|| PathBuf::from("sftp_root"));
    let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2222);
    std::fs::create_dir_all(&root)?;
    let root = Arc::new(root);

    eprintln!("SFTP test server root={:?} port={}", root, port);

    // 用固定 seed 的 Ed25519 key，避免随机生成依赖 rand 版本差异
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let config = server::Config {
        auth_rejection_time: Duration::from_secs(0),
        auth_rejection_time_initial: Some(Duration::from_secs(0)),
        keys: vec![key],
        ..Default::default()
    };

    let mut server = SshServer { root };
    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;
    server.run_on_address(Arc::new(config), addr).await?;
    Ok(())
}
