//! GPUI 等非 Tokio executor 的命令入口。网络仅在专用线程上运行。
use crate::SyncReplica;
use nd_wire::{Command, CommandReply, Model, ReceiptLookup};
use std::{io, path::Path, time::Duration};
use tokio::sync::{mpsc, oneshot};

enum Work {
    Command(Command, oneshot::Sender<Result<CommandReply, String>>),
    Models(String, String, oneshot::Sender<Result<Vec<Model>, String>>),
    Get(
        String,
        nd_wire::PageReq,
        oneshot::Sender<Result<nd_wire::Page, String>>,
    ),
    Receipt(String, oneshot::Sender<Result<ReceiptLookup, String>>),
}
#[derive(Clone)]
pub struct CommandClient {
    tx: mpsc::Sender<Work>,
}
impl CommandClient {
    pub fn start(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_owned();
        let (tx, mut rx) = mpsc::channel(8);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        std::thread::Builder::new()
            .name("nd-commands".into())
            .spawn(move || {
                runtime.block_on(async move {
                    while let Some(work) = rx.recv().await {
                        let connected = tokio::time::timeout(
                            Duration::from_secs(5),
                            SyncReplica::connect(&path),
                        )
                        .await;
                        let mut ui = match connected {
                            Ok(Ok(ui)) => ui,
                            other => {
                                let why = match other {
                                    Ok(Err(e)) => e.to_string(),
                                    _ => "连接守护进程超时".into(),
                                };
                                match work {
                                    Work::Command(_, done) => {
                                        let _ = done.send(Err(why));
                                    }
                                    Work::Models(_, _, done) => {
                                        let _ = done.send(Err(why));
                                    }
                                    Work::Get(_, _, done) => {
                                        let _ = done.send(Err(why));
                                    }
                                    Work::Receipt(_, done) => {
                                        let _ = done.send(Err(why));
                                    }
                                }
                                continue;
                            }
                        };
                        match work {
                            Work::Get(res, page, done) => {
                                let result = tokio::time::timeout(
                                    Duration::from_secs(15),
                                    ui.get(&res, page),
                                )
                                .await
                                .map_err(|_| "历史查询超时".to_owned())
                                .and_then(|r| r.map_err(|e| e.to_string()));
                                let _ = done.send(result);
                            }
                            Work::Receipt(command_id, done) => {
                                let _ = done
                                    .send(ui.receipt(&command_id).await.map_err(|e| e.to_string()));
                            }
                            Work::Command(command, done) => {
                                let _ = done
                                    .send(ui.command(&command).await.map_err(|e| e.to_string()));
                            }
                            Work::Models(backend, cwd, done) => {
                                let result = tokio::time::timeout(
                                    Duration::from_secs(50),
                                    ui.models(&backend, &cwd),
                                )
                                .await
                                .map_err(|_| "模型列表查询超时".to_owned())
                                .and_then(|r| r.map_err(|e| e.to_string()));
                                let _ = done.send(result);
                            }
                        }
                        let _ = tokio::time::timeout(Duration::from_millis(200), ui.close()).await;
                    }
                })
            })?;
        Ok(Self { tx })
    }
    pub async fn get(&self, res: String, page: nd_wire::PageReq) -> Result<nd_wire::Page, String> {
        let (done, result) = oneshot::channel();
        self.tx
            .try_send(Work::Get(res, page, done))
            .map_err(|_| "查询队列已满或连接已关闭".to_owned())?;
        result.await.map_err(|_| "历史查询已取消".to_owned())?
    }
    pub async fn command(&self, command: Command) -> Result<CommandReply, String> {
        let (done, result) = oneshot::channel();
        self.tx
            .try_send(Work::Command(command, done))
            .map_err(|_| "命令队列已满或连接已关闭；未发送".to_owned())?;
        result
            .await
            .map_err(|_| "命令连接已关闭；交付不明".to_owned())?
    }
    /// 恢复待确认命令时只读收据，不把 Missing 当作允许重发。
    pub async fn receipt(&self, command_id: String) -> Result<ReceiptLookup, String> {
        let (done, result) = oneshot::channel();
        self.tx
            .try_send(Work::Receipt(command_id, done))
            .map_err(|_| "查询队列已满或连接已关闭".to_owned())?;
        result.await.map_err(|_| "收据查询已取消".to_owned())?
    }
    pub async fn models(&self, backend: String, cwd: String) -> Result<Vec<Model>, String> {
        let (done, result) = oneshot::channel();
        self.tx
            .try_send(Work::Models(backend, cwd, done))
            .map_err(|_| "查询队列已满或连接已关闭".to_owned())?;
        result.await.map_err(|_| "查询已取消".to_owned())?
    }
}
