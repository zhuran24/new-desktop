//! UDS 目录、单实例锁和内核提供的对端身份检查。
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};
use tokio::net::{UnixListener, UnixStream, unix::SocketAddr};

pub struct LocalSocket {
    directory: std::path::PathBuf,
    lock: File,
}
pub struct LocalListener {
    listener: UnixListener,
    _lock: File,
}
impl LocalSocket {
    pub fn prepare(directory: &Path) -> io::Result<Self> {
        match std::fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = std::fs::symlink_metadata(directory)?;
        if !meta.is_dir() || meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket directory must be owned by daemon uid and not a symlink",
            ));
        }
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(directory.join("daemon.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        Ok(Self {
            directory: directory.into(),
            lock,
        })
    }
    pub fn listen(self) -> io::Result<LocalListener> {
        let path = self.directory.join("nd.sock");
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(&path)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "refuse to replace non-socket",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(LocalListener {
            listener: UnixListener::bind(path)?,
            _lock: self.lock,
        })
    }
}
impl axum::serve::Listener for LocalListener {
    type Io = UnixStream;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (UnixStream, SocketAddr) {
        loop {
            match self.listener.accept().await {
                Ok((stream, address))
                    if stream
                        .peer_cred()
                        .is_ok_and(|cred| cred.uid() == rustix::process::geteuid().as_raw()) =>
                {
                    return (stream, address);
                }
                Ok(_) => {} // 未核实或异 uid 一律拒绝，HTTP/WS 共用此入口。
                Err(e) => {
                    eprintln!("UDS accept: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}
