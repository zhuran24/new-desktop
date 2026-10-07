//! GPUI 等非 Tokio executor 的命令入口。网络仅在专用线程上运行。
use crate::{AttachmentSource, SyncReplica};
use nd_wire::{Command, CommandReply, Model, ReceiptLookup};
use std::{io, path::Path, time::Duration};
use tokio::sync::{mpsc, oneshot};

/// 收据等动作有结果的命令最多等多久（Bash 最长 10 分钟，压缩要等会话空闲）。
pub const DELIVERY_WAIT: Duration = Duration::from_secs(15 * 60);

enum Work {
    Upload(
        AttachmentSource,
        oneshot::Sender<Result<nd_wire::Attachment, String>>,
    ),
    Blob(nd_wire::BlobId, oneshot::Sender<Result<Vec<u8>, String>>),
    Command(Command, oneshot::Sender<Result<CommandReply, String>>),
    Models(String, String, oneshot::Sender<Result<Vec<Model>, String>>),
    Get(
        String,
        nd_wire::PageReq,
        oneshot::Sender<Result<nd_wire::Page, String>>,
    ),
    Receipt(String, oneshot::Sender<Result<ReceiptLookup, String>>),
    /// 收据等动作有结果的命令：另开连接、另起任务等，不挡住草稿保存等别的命令。
    Deliver(Command, oneshot::Sender<Result<CommandReply, String>>),
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
                        let wait = if matches!(&work, Work::Deliver(..)) {
                            DELIVERY_WAIT
                        } else {
                            Duration::from_secs(5)
                        };
                        let work = match work {
                            Work::Deliver(command, mut done) | Work::Command(command, mut done) => {
                                let path = path.clone();
                                tokio::spawn(async move {
                                    let result = async {
                                        let mut ui = tokio::time::timeout(
                                            Duration::from_secs(5),
                                            SyncReplica::connect(&path),
                                        )
                                        .await
                                        .map_err(|_| "连接守护进程超时".to_owned())?
                                        .map_err(|e| e.to_string())?;
                                        let result = ui
                                            .command_waiting(&command, wait)
                                            .await
                                            .map_err(|e| e.to_string());
                                        let _ = tokio::time::timeout(
                                            Duration::from_millis(200),
                                            ui.close(),
                                        )
                                        .await;
                                        result
                                    };
                                    tokio::select! {
                                        biased;
                                        _ = done.closed() => {}
                                        result = result => {
                                            let _ = done.send(result);
                                        }
                                    }
                                });
                                continue;
                            }
                            work => work,
                        };
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
                                    Work::Upload(_, done) => {
                                        let _ = done.send(Err(why));
                                    }
                                    Work::Blob(_, done) => {
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
                                    Work::Command(..) | Work::Deliver(..) => {
                                        unreachable!("另起任务处理")
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
                            Work::Upload(source, done) => {
                                let result = async {
                                    let (attachment, bytes) = source.read().await?;
                                    ui.put_blob(&bytes).await.map_err(|e| e.to_string())?;
                                    Ok(attachment)
                                }
                                .await;
                                let _ = done.send(result);
                            }
                            Work::Blob(blob, done) => {
                                let _ =
                                    done.send(ui.get_blob(&blob).await.map_err(|e| e.to_string()));
                            }
                            Work::Receipt(command_id, done) => {
                                let _ = done
                                    .send(ui.receipt(&command_id).await.map_err(|e| e.to_string()));
                            }
                            Work::Command(..) | Work::Deliver(..) => unreachable!("另起任务处理"),
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
    pub async fn upload(&self, source: AttachmentSource) -> Result<nd_wire::Attachment, String> {
        let (done, result) = oneshot::channel();
        self.tx
            .try_send(Work::Upload(source, done))
            .map_err(|_| "上传队列已满或连接已关闭".to_owned())?;
        result.await.map_err(|_| "上传已取消".to_owned())?
    }
    pub async fn blob(&self, blob: nd_wire::BlobId) -> Result<Vec<u8>, String> {
        let (done, result) = oneshot::channel();
        self.tx
            .try_send(Work::Blob(blob, done))
            .map_err(|_| "下载队列已满或连接已关闭".to_owned())?;
        result.await.map_err(|_| "下载已取消".to_owned())?
    }
    pub async fn command(&self, command: Command) -> Result<CommandReply, String> {
        let (done, result) = oneshot::channel();
        self.tx
            .try_send(Work::Command(command, done))
            .map_err(|_| "命令队列已满或连接已关闭；未发送".to_owned())?;
        // After enqueueing, loss of the worker is uncertain. Err is reserved
        // for failures proven to happen before any command was sent.
        result.await.unwrap_or(Ok(CommandReply::DeliveryUnknown))
    }
    /// 收据等动作有结果的命令（`!`、总结、`/subtask`）：最多等 [`DELIVERY_WAIT`]，
    /// 等的时候别的命令照常走。到时限没回应只查收据，不重发正文。
    pub async fn deliver(&self, command: Command) -> Result<CommandReply, String> {
        let (done, result) = oneshot::channel();
        self.tx
            .try_send(Work::Deliver(command, done))
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
