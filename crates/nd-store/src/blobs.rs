use crate::{Error, Result, Store, Tx};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

/// 文件先持久化，再登记；业务主人必须在自己的写事务里 hold/release。
pub struct Blobs {
    directory: PathBuf,
    store: Arc<Store>,
}
impl Blobs {
    pub fn open(directory: impl Into<PathBuf>, store: Arc<Store>) -> Result<Self> {
        let directory = directory.into();
        std::fs::create_dir_all(&directory)?;
        store.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS nd_blobs (
                id TEXT PRIMARY KEY, size INTEGER NOT NULL, unused_since INTEGER,
                deleting INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE IF NOT EXISTS nd_blob_refs (
                    blob TEXT NOT NULL REFERENCES nd_blobs(id), owner TEXT NOT NULL,
                    PRIMARY KEY (blob, owner));",
            )?;
            Ok(())
        })?;
        Ok(Self { directory, store })
    }
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let _io = self.store.blob_io.lock().unwrap();
        let id = format!("{:x}", Sha256::digest(bytes));
        let path = self.path(&id)?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.directory)?;
        temp.write_all(bytes)?;
        temp.as_file().sync_all()?;
        match temp.persist_noclobber(&path) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if self.get(&id)? != bytes {
                    return Err(Error::Corrupt);
                }
            }
            Err(e) => return Err(e.error.into()),
        }
        std::fs::File::open(&self.directory)?.sync_all()?;
        self.store.write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO nd_blobs(id,size,unused_since) VALUES (?1,?2,unixepoch())",
                crate::params![id, bytes.len() as u64],
            )?;
            let deleting: bool =
                tx.query_row("SELECT deleting FROM nd_blobs WHERE id=?1", [&id], |r| {
                    r.get(0)
                })?;
            if deleting {
                return Err(Error::Busy);
            }
            tx.execute("UPDATE nd_blobs SET unused_since=unixepoch() WHERE id=?1 AND unused_since IS NOT NULL", [&id])?;
            Ok(())
        })?;
        Ok(id)
    }
    pub fn get(&self, id: &str) -> Result<Vec<u8>> {
        let bytes = std::fs::read(self.path(id)?)?;
        if format!("{:x}", Sha256::digest(&bytes)) != id {
            return Err(Error::Corrupt);
        }
        Ok(bytes)
    }
    pub fn hold(&self, tx: &mut Tx<'_>, id: &str, owner: &str) -> Result<()> {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM nd_blobs WHERE id=?1 AND deleting=0)",
            [id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Aborted("附件缺失或正在清理".into()));
        }
        tx.execute(
            "INSERT OR IGNORE INTO nd_blob_refs(blob,owner) VALUES (?1,?2)",
            [id, owner],
        )?;
        tx.execute("UPDATE nd_blobs SET unused_since=NULL WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn release(&self, tx: &mut Tx<'_>, id: &str, owner: &str) -> Result<()> {
        tx.execute(
            "DELETE FROM nd_blob_refs WHERE blob=?1 AND owner=?2",
            [id, owner],
        )?;
        tx.execute("UPDATE nd_blobs SET unused_since=COALESCE(unused_since,unixepoch()) WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM nd_blob_refs WHERE blob=?1)", [id])?;
        Ok(())
    }
    pub fn references(&self, id: &str) -> Result<u64> {
        Ok(self.store.read()?.query_row(
            "SELECT count(*) FROM nd_blob_refs WHERE blob=?1",
            [id],
            |r| r.get(0),
        )?)
    }
    /// 标记删除和引用检查在同一事务里；文件删除在事务外，可在崩溃后重试。
    pub fn collect(&self, grace: std::time::Duration) -> Result<usize> {
        let _io = self.store.blob_io.lock().unwrap();
        let grace =
            i64::try_from(grace.as_secs()).map_err(|_| Error::Aborted("grace too large".into()))?;
        let ids = self.store.write(|tx| {
            tx.execute("UPDATE nd_blobs SET deleting=1 WHERE unused_since <= unixepoch() - ?1 AND NOT EXISTS(SELECT 1 FROM nd_blob_refs WHERE blob=nd_blobs.id)", [grace])?;
            let mut statement = tx.prepare("SELECT id FROM nd_blobs WHERE deleting=1")?;
            Ok(statement.query_map([], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?)
        })?;
        for id in &ids {
            match std::fs::remove_file(self.path(id)?) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            std::fs::File::open(&self.directory)?.sync_all()?;
            self.store.write(|tx| {
                tx.execute("DELETE FROM nd_blobs WHERE id=?1 AND deleting=1", [id])?;
                Ok(())
            })?;
        }
        Ok(ids.len())
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Aborted("无效的 SHA-256".into()));
        }
        Ok(Path::new(&self.directory).join(id))
    }
}
