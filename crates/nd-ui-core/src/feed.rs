//! 把 Tokio socket 与界面 executor 隔开；有界队列只传完整副本。
use crate::SyncReplica;
use nd_wire::Snapshot;
use std::{io, path::Path, time::Duration};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug)]
pub enum FeedUpdate {
    Snapshot(Snapshot),
    Unavailable(String),
}

/// 专用线程持有唯一同步副本。丢弃/取消界面接收任务只关连接，不停守护进程。
pub struct ReplicaFeed {
    updates: mpsc::Receiver<FeedUpdate>,
    stop: Option<oneshot::Sender<()>>,
    stopped: Option<oneshot::Receiver<()>>,
}
impl ReplicaFeed {
    pub fn start(path: impl AsRef<Path>, stream: impl Into<String>) -> io::Result<Self> {
        let path = path.as_ref().to_owned();
        let stream = stream.into();
        let (tx, updates) = mpsc::channel(1);
        let (stop, mut stopping) = oneshot::channel();
        let (done, stopped) = oneshot::channel();
        std::thread::Builder::new()
            .name("nd-wire".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(async move {
                        let mut replica = None;
                        let work = async {
                            loop {
                                if replica.is_none() {
                                    match SyncReplica::connect(&path).await {
                                        Ok(mut fresh) => match fresh.subscribe(&stream).await {
                                            Ok(snapshot) => {
                                                replica = Some(fresh);
                                                if tx
                                                    .send(FeedUpdate::Snapshot(snapshot))
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                if tx
                                                    .send(FeedUpdate::Unavailable(e.to_string()))
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                                tokio::time::sleep(Duration::from_millis(250))
                                                    .await;
                                            }
                                        },
                                        Err(e) => {
                                            if tx
                                                .send(FeedUpdate::Unavailable(e.to_string()))
                                                .await
                                                .is_err()
                                            {
                                                break;
                                            }
                                            tokio::time::sleep(Duration::from_millis(250)).await;
                                        }
                                    }
                                } else {
                                    match replica.as_mut().unwrap().next().await {
                                        Ok(snapshot) => {
                                            if tx
                                                .send(FeedUpdate::Snapshot(snapshot))
                                                .await
                                                .is_err()
                                            {
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            replica = None;
                                            if tx
                                                .send(FeedUpdate::Unavailable(e.to_string()))
                                                .await
                                                .is_err()
                                            {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        };
                        tokio::select! { _ = work => {}, _ = &mut stopping => {} }
                        if let Some(mut replica) = replica {
                            let _ =
                                tokio::time::timeout(Duration::from_millis(200), replica.close())
                                    .await;
                        }
                    }),
                    Err(e) => {
                        let _ = tx.blocking_send(FeedUpdate::Unavailable(e.to_string()));
                    }
                }
                let _ = done.send(());
            })?;
        Ok(Self {
            updates,
            stop: Some(stop),
            stopped: Some(stopped),
        })
    }
    /// 可在任意 executor 等待；不在调用线程启动 Tokio runtime。
    pub async fn recv(&mut self) -> Option<FeedUpdate> {
        self.updates.recv().await
    }
    pub async fn close(mut self) {
        self.request_stop();
        if let Some(stopped) = self.stopped.take() {
            let _ = stopped.await;
        }
    }
    fn request_stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
impl Drop for ReplicaFeed {
    fn drop(&mut self) {
        self.request_stop();
    }
}
