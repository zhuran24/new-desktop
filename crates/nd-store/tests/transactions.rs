use nd_store::{Error, Store};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn failed_transaction_rolls_back_rows_and_does_not_publish_commit_hook() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state.sqlite"), 2).unwrap();
    store
        .write(|tx| {
            tx.execute_batch("CREATE TABLE facts(id INTEGER PRIMARY KEY, value TEXT NOT NULL)")?;
            Ok(())
        })
        .unwrap();
    let commits = Arc::new(AtomicUsize::new(0));
    let count = commits.clone();
    let failed: Result<(), Error> = store.write(|tx| {
        tx.execute("INSERT INTO facts VALUES (1, 'must roll back')", [])?;
        tx.on_commit(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        Err(Error::Aborted("deliberate abort".into()))
    });
    assert!(failed.is_err());
    let read = store.read().unwrap();
    assert_eq!(
        read.query_row("SELECT count(*) FROM facts", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(commits.load(Ordering::SeqCst), 0);
    assert!(
        read.execute("INSERT INTO facts VALUES (2, 'read-only must reject')", [])
            .is_err()
    );
    drop(read);
    let count = commits.clone();
    store
        .write(|tx| {
            tx.execute("INSERT INTO facts VALUES (3, 'committed')", [])?;
            tx.on_commit(move || {
                count.fetch_add(1, Ordering::SeqCst);
            });
            Ok(())
        })
        .unwrap();
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    drop(store);
    let store = Store::open(dir.path().join("state.sqlite"), 2).unwrap();
    assert_eq!(
        store
            .read()
            .unwrap()
            .query_row("SELECT value FROM facts", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "committed"
    );
}

#[test]
fn attachment_reference_and_owning_row_commit_or_rollback_together() {
    use nd_store::Blobs;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("state.sqlite"), 2).unwrap());
    let blobs = Blobs::open(dir.path().join("blobs"), store.clone()).unwrap();
    let id = blobs.put(b"abc").unwrap();
    assert_eq!(
        id.as_str(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(blobs.put(b"abc").unwrap(), id);
    store
        .write(|tx| {
            tx.execute_batch("CREATE TABLE messages(id TEXT PRIMARY KEY, blob TEXT)")?;
            Ok(())
        })
        .unwrap();
    let failure: Result<(), Error> = store.write(|tx| {
        tx.execute("INSERT INTO messages VALUES ('m1', ?1)", [id.as_str()])?;
        blobs.hold(tx, &id, "message/m1")?;
        Err(Error::Aborted("simulated rejection".into()))
    });
    assert!(failure.is_err());
    assert_eq!(blobs.collect(std::time::Duration::ZERO).unwrap(), 1);
    assert!(blobs.get(&id).is_err());
    assert_eq!(blobs.put(b"abc").unwrap(), id);
    store
        .write(|tx| {
            tx.execute("INSERT INTO messages VALUES ('m1', ?1)", [id.as_str()])?;
            blobs.hold(tx, &id, "message/m1")?;
            blobs.hold(tx, &id, "message/m1")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(blobs.collect(std::time::Duration::ZERO).unwrap(), 0);
    assert_eq!(blobs.get(&id).unwrap(), b"abc");
    let failure: Result<(), Error> = store.write(|tx| {
        tx.execute("DELETE FROM messages", [])?;
        blobs.release(tx, &id, "message/m1")?;
        Err(Error::Aborted("rollback removal".into()))
    });
    assert!(failure.is_err());
    assert_eq!(blobs.collect(std::time::Duration::ZERO).unwrap(), 0);
    assert_eq!(blobs.get(&id).unwrap(), b"abc");
    store
        .write(|tx| {
            tx.execute("DELETE FROM messages", [])?;
            blobs.release(tx, &id, "message/m1")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(blobs.get(&id).unwrap(), b"abc"); // 零引用不立即删文件。
    assert_eq!(blobs.collect(std::time::Duration::ZERO).unwrap(), 1);
    assert!(blobs.get(&id).is_err());
    assert!(
        store
            .write(|tx| blobs.hold(tx, &"0".repeat(64).parse().unwrap(), "missing"))
            .is_err()
    );
    assert!("../state.sqlite".parse::<nd_id::BlobId>().is_err());
}

#[test]
fn garbage_collection_waits_for_grace_and_preserves_held_content() {
    use nd_store::Blobs;
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("state.sqlite"), 2).unwrap());
    let blobs = Blobs::open(dir.path().join("blobs"), store.clone()).unwrap();
    let held = blobs.put(b"keep").unwrap();
    let unused = blobs.put(b"unused").unwrap();
    store
        .write(|tx| blobs.hold(tx, &held, "message/keep"))
        .unwrap();
    assert_eq!(blobs.collect(Duration::from_secs(3600)).unwrap(), 0);
    assert_eq!(blobs.collect(Duration::ZERO).unwrap(), 1);
    assert_eq!(blobs.get(&held).unwrap(), b"keep");
    assert!(blobs.get(&unused).is_err());
    assert_eq!(blobs.put(b"unused").unwrap(), unused);
    store
        .write(|tx| blobs.hold(tx, &unused, "message/new"))
        .unwrap();
    assert_eq!(blobs.collect(Duration::ZERO).unwrap(), 0);
}

#[test]
fn active_reader_keeps_its_wal_snapshot_while_writer_commits() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state.sqlite"), 2).unwrap();
    store
        .write(|tx| {
            tx.execute_batch(
                "CREATE TABLE counter(value INTEGER); INSERT INTO counter VALUES (1)",
            )?;
            Ok(())
        })
        .unwrap();
    let old = store.read().unwrap();
    assert_eq!(
        old.query_row("SELECT value FROM counter", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        1
    );
    store
        .write(|tx| {
            tx.execute("UPDATE counter SET value=2", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        old.query_row("SELECT value FROM counter", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        1
    );
    let fresh = store.read().unwrap();
    assert_eq!(
        fresh
            .query_row("SELECT value FROM counter", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        2
    );
    drop(old);
    drop(fresh);
    assert_eq!(
        store
            .read()
            .unwrap()
            .query_row("SELECT value FROM counter", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        2
    );
}
