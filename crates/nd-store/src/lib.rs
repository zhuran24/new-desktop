//! 单写事务、WAL 只读快照池与内容寻址附件。
mod blobs;
pub use blobs::Blobs;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
pub use rusqlite::{OptionalExtension, params};
use std::{
    ops::Deref,
    path::Path,
    sync::{Condvar, Mutex},
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("存储忙")]
    Busy,
    #[error("存储已满")]
    Full,
    #[error("存储损坏")]
    Corrupt,
    #[error("事务取消: {0}")]
    Aborted(String),
    #[error(transparent)]
    Sql(rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode;
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy,
            Some(ErrorCode::DiskFull) => Self::Full,
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => Self::Corrupt,
            _ => Self::Sql(error),
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub struct Store {
    blob_io: Mutex<()>,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    available: Condvar,
}
pub struct Tx<'a> {
    transaction: Transaction<'a>,
    hooks: Vec<Box<dyn FnOnce() + Send>>,
}
impl Deref for Tx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.transaction
    }
}
impl Tx<'_> {
    /// 在提交成功之后、释放写连接之前运行；钩子不得重入 Store::write。
    pub fn on_commit(&mut self, hook: impl FnOnce() + Send + 'static) {
        self.hooks.push(Box::new(hook));
    }
}
pub struct ReadConn<'a> {
    store: &'a Store,
    connection: Option<Connection>,
}
impl Deref for ReadConn<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.connection.as_ref().unwrap()
    }
}
impl Drop for ReadConn<'_> {
    fn drop(&mut self) {
        let connection = self.connection.take().unwrap();
        let _ = connection.execute_batch("ROLLBACK");
        self.store.readers.lock().unwrap().push(connection);
        self.store.available.notify_one();
    }
}
impl Store {
    pub fn open(path: impl AsRef<Path>, read_pool_size: usize) -> Result<Self> {
        if read_pool_size == 0 {
            return Err(Error::Aborted("read pool must not be empty".into()));
        }
        let writer = Connection::open(path.as_ref())?;
        writer.busy_timeout(Duration::from_secs(2))?;
        writer.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let readers = (0..read_pool_size)
            .map(|_| {
                let c = Connection::open_with_flags(
                    path.as_ref(),
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                c.busy_timeout(Duration::from_secs(2))?;
                c.execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;")?;
                Ok(c)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            blob_io: Mutex::new(()),
            writer: Mutex::new(writer),
            readers: Mutex::new(readers),
            available: Condvar::new(),
        })
    }
    /// 闭包同步执行，不得做网络/文件 I/O 或等待别的进程；Err 自动回滚。
    pub fn write<T>(&self, operation: impl FnOnce(&mut Tx<'_>) -> Result<T>) -> Result<T> {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let mut tx = Tx {
            transaction: writer.transaction_with_behavior(TransactionBehavior::Immediate)?,
            hooks: vec![],
        };
        let result = operation(&mut tx)?;
        tx.transaction.commit()?;
        for hook in tx.hooks {
            hook();
        }
        Ok(result)
    }
    /// 有界池；每个句柄是一份读事务，Drop 归还连接。
    pub fn read(&self) -> Result<ReadConn<'_>> {
        let mut readers = self.readers.lock().unwrap();
        while readers.is_empty() {
            readers = self.available.wait(readers).unwrap();
        }
        let connection = readers.pop().unwrap();
        drop(readers);
        let read = ReadConn {
            store: self,
            connection: Some(connection),
        };
        read.execute_batch("BEGIN DEFERRED")?;
        Ok(read)
    }
}
