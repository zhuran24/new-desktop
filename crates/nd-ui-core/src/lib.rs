//! 无 GPUI 依赖的 nd-wire 连接与同步副本。
mod client;
pub use client::CommandClient;
use futures::{SinkExt, StreamExt};
use nd_wire::{PROTOCOL_VERSION, Request, Response, Snapshot};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio::net::UnixStream;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct SyncReplica {
    socket: WebSocketStream<UnixStream>,
    path: PathBuf,
    replicas: BTreeMap<String, Snapshot>,
    next_request: u64,
}
impl SyncReplica {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        let stream = UnixStream::connect(&path).await?;
        let (socket, _) = tokio_tungstenite::client_async("ws://localhost/wire", stream).await?;
        let mut this = Self {
            socket,
            path,
            replicas: BTreeMap::new(),
            next_request: 0,
        };
        this.send(Request::Hello {
            version: PROTOCOL_VERSION,
            namespaces: BTreeMap::new(),
        })
        .await?;
        match this.receive().await? {
            Response::Hello {
                version: PROTOCOL_VERSION,
                ..
            } => Ok(this),
            other => Err(format!("invalid hello: {other:?}").into()),
        }
    }
    async fn send(&mut self, req: Request) -> Result<()> {
        self.socket
            .send(Message::Text(serde_json::to_string(&req)?.into()))
            .await?;
        Ok(())
    }
    async fn receive(&mut self) -> Result<Response> {
        while let Some(frame) = self.socket.next().await {
            match frame? {
                Message::Text(text) => return Ok(serde_json::from_str(&text)?),
                Message::Close(_) => break,
                _ => {}
            }
        }
        Err("nd-wire disconnected".into())
    }
    fn apply(&mut self, response: Response) -> Option<Snapshot> {
        match response {
            Response::Snapshot { snapshot } => {
                self.replicas
                    .insert(snapshot.stream.clone(), snapshot.clone());
                Some(snapshot)
            }
            Response::Event { event } => {
                let snapshot = self.replicas.get_mut(&event.stream)?;
                if snapshot.epoch != event.epoch
                    || snapshot.cursor.checked_add(1) != Some(event.cursor)
                {
                    return None;
                }
                snapshot.items.retain(|i| !event.remove.contains(&i.id));
                for item in event.upsert {
                    if let Some(old) = snapshot.items.iter_mut().find(|i| i.id == item.id) {
                        *old = item;
                    } else {
                        snapshot.items.push(item);
                    }
                }
                snapshot.cursor = event.cursor;
                Some(snapshot.clone())
            }
            _ => None,
        }
    }
    pub fn current(&self, stream: &str) -> Option<&Snapshot> {
        self.replicas.get(stream)
    }
    fn request_id(&mut self) -> Result<u64> {
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or("request id exhausted")?;
        Ok(self.next_request)
    }
    pub async fn get(&mut self, res: &str, page: nd_wire::PageReq) -> Result<nd_wire::Page> {
        let id = self.request_id()?;
        self.send(Request::Get {
            id,
            res: res.into(),
            page,
        })
        .await?;
        Ok(serde_json::from_value(self.reply(id).await?)?)
    }
    pub async fn models(&mut self, backend: &str, cwd: &str) -> Result<Vec<nd_wire::Model>> {
        let id = self.request_id()?;
        self.send(Request::Models {
            id,
            backend: backend.into(),
            cwd: cwd.into(),
        })
        .await?;
        Ok(serde_json::from_value(self.reply(id).await?)?)
    }
    async fn reply(&mut self, expected: u64) -> Result<serde_json::Value> {
        loop {
            let response = self.receive().await?;
            if let Response::Reply { id, value, error } = response {
                if id != expected {
                    continue;
                }
                return match error {
                    None => Ok(value),
                    Some(e) => Err(e.into()),
                };
            }
            self.apply(response);
        }
    }
    /// 仅有无副作用的诊断命令；此编号是 RPC 关联号，不是持久命令 id。
    pub async fn query(&mut self, name: &str) -> Result<serde_json::Value> {
        let id = self.request_id()?;
        self.send(Request::Command {
            id,
            name: name.into(),
        })
        .await?;
        self.reply(id).await
    }
    /// 正文只送一次。传输中断只查收据；查不到返回交付不明。
    pub async fn command(&mut self, command: &nd_wire::Command) -> Result<nd_wire::CommandReply> {
        self.command_waiting(command, std::time::Duration::from_secs(5))
            .await
    }
    /// 同 [`Self::command`]，但回应最多等 `wait`：收据等动作有结果的命令（`!`、总结、
    /// 派 fork 型子代理）要等后端做完才回。到时限仍没回应就只查收据，不重发正文。
    pub async fn command_waiting(
        &mut self,
        command: &nd_wire::Command,
        wait: std::time::Duration,
    ) -> Result<nd_wire::CommandReply> {
        let mut attempt = 0u32;
        loop {
            match self.command_once(command, wait).await {
                Ok(result @ nd_wire::CommandReply::Unavailable { .. }) => {
                    let recovering = matches!(
                        &result,
                        nd_wire::CommandReply::Unavailable {
                            code: Some(nd_wire::UnavailableCode::Recovering),
                            ..
                        }
                    );
                    if !recovering && attempt >= 4 {
                        return Ok(result);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50 << attempt.min(5)))
                        .await;
                    attempt = attempt.saturating_add(1);
                }
                Ok(result) => return Ok(result),
                Err(_) => {
                    return Ok(
                        match self
                            .receipt_matching(&command.id, Some(command.content_hash()))
                            .await
                        {
                            Ok(nd_wire::ReceiptLookup::Found { receipt }) => {
                                nd_wire::CommandReply::Receipt { receipt }
                            }
                            Ok(nd_wire::ReceiptLookup::Conflict) => nd_wire::CommandReply::Conflict,
                            Ok(nd_wire::ReceiptLookup::Expired) => nd_wire::CommandReply::Expired,
                            _ => nd_wire::CommandReply::DeliveryUnknown,
                        },
                    );
                }
            }
        }
    }

    async fn command_once(
        &mut self,
        command: &nd_wire::Command,
        wait: std::time::Duration,
    ) -> Result<nd_wire::CommandReply> {
        let id = self.request_id()?;
        self.send(Request::Execute {
            id,
            command: command.clone(),
        })
        .await?;
        tokio::time::timeout(wait, async {
            loop {
                match self.receive().await? {
                    Response::CommandReply { id: got, result } if got == id => return Ok(result),
                    Response::Bye { .. } => return Err("nd-wire disconnected".into()),
                    response => {
                        self.apply(response);
                    }
                }
            }
        })
        .await?
    }
    /// 查询可以安全重试，但 Missing 不能变成重发正文。
    pub async fn receipt(&mut self, command_id: &str) -> Result<nd_wire::ReceiptLookup> {
        self.receipt_matching(command_id, None).await
    }
    async fn receipt_matching(
        &mut self,
        command_id: &str,
        content_hash: Option<String>,
    ) -> Result<nd_wire::ReceiptLookup> {
        match self.receipt_once(command_id, content_hash.clone()).await {
            Ok(result) => Ok(result),
            Err(_) => {
                self.reconnect().await?;
                self.receipt_once(command_id, content_hash).await
            }
        }
    }
    async fn receipt_once(
        &mut self,
        command_id: &str,
        content_hash: Option<String>,
    ) -> Result<nd_wire::ReceiptLookup> {
        let id = self.request_id()?;
        self.send(Request::Receipt {
            id,
            command_id: command_id.into(),
            content_hash,
        })
        .await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match self.receive().await? {
                    Response::ReceiptReply { id: got, result } if got == id => return Ok(result),
                    Response::Bye { .. } => return Err("nd-wire disconnected".into()),
                    response => {
                        self.apply(response);
                    }
                }
            }
        })
        .await?
    }
    async fn reconnect(&mut self) -> Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(mut fresh) = Self::connect(&self.path).await {
                    for (stream, snapshot) in &self.replicas {
                        fresh
                            .send(Request::Subscribe {
                                stream: stream.clone(),
                                since: Some(nd_wire::Cursor {
                                    epoch: snapshot.epoch.clone(),
                                    seq: snapshot.cursor,
                                }),
                            })
                            .await?;
                    }
                    self.socket = fresh.socket;
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await?
    }
    /// 驱动事件和自动重连。取消等待安全；副本只由快照和事件修改。
    pub async fn next(&mut self) -> Result<Snapshot> {
        loop {
            match self.receive().await {
                Ok(Response::Bye { .. }) | Err(_) => loop {
                    if let Ok(mut fresh) = Self::connect(&self.path).await {
                        for stream in self.replicas.keys() {
                            fresh
                                .send(Request::Subscribe {
                                    stream: stream.clone(),
                                    since: self.replicas.get(stream).map(|s| nd_wire::Cursor {
                                        epoch: s.epoch.clone(),
                                        seq: s.cursor,
                                    }),
                                })
                                .await?;
                        }
                        self.socket = fresh.socket;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                },
                Ok(response) => {
                    let gap = match &response {
                        Response::Event { event } => {
                            self.replicas.get(&event.stream).is_none_or(|s| {
                                s.epoch != event.epoch
                                    || s.cursor.checked_add(1) != Some(event.cursor)
                            })
                        }
                        Response::Resumed {
                            stream,
                            epoch,
                            cursor,
                        } => self
                            .replicas
                            .get(stream)
                            .is_none_or(|s| &s.epoch != epoch || s.cursor != *cursor),
                        _ => false,
                    };
                    if gap {
                        let stream = match response {
                            Response::Event { event } => event.stream,
                            Response::Resumed { stream, .. } => stream,
                            _ => unreachable!(),
                        };
                        self.send(Request::Subscribe {
                            stream,
                            since: None,
                        })
                        .await?;
                    } else if let Some(snapshot) = self.apply(response) {
                        return Ok(snapshot);
                    }
                }
            }
        }
    }
    pub async fn put_blob(&self, bytes: &[u8]) -> Result<String> {
        use sha2::{Digest, Sha256};
        let id = format!("{:x}", Sha256::digest(bytes));
        self.http("PUT", &id, bytes.to_vec()).await?;
        Ok(id)
    }
    pub async fn get_blob(&self, id: &str) -> Result<Vec<u8>> {
        self.http("GET", id, vec![]).await
    }
    async fn http(&self, method: &str, id: &str, bytes: Vec<u8>) -> Result<Vec<u8>> {
        use http_body_util::{BodyExt, Full};
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid SHA-256".into());
        }
        let io = hyper_util::rt::TokioIo::new(UnixStream::connect(&self.path).await?);
        let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await?;
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        let response = sender
            .send_request(
                hyper::Request::builder()
                    .method(method)
                    .uri(format!("/blobs/{id}"))
                    .header("Host", "localhost")
                    .body(Full::new(hyper::body::Bytes::from(bytes)))?,
            )
            .await?;
        let status = response.status();
        let body = response.into_body().collect().await?.to_bytes().to_vec();
        task.abort();
        if !status.is_success() {
            return Err(format!("blob HTTP {status}").into());
        }
        Ok(body)
    }
    pub async fn subscribe(&mut self, stream: &str) -> Result<Snapshot> {
        self.send(Request::Subscribe {
            stream: stream.into(),
            since: None,
        })
        .await?;
        loop {
            match self.receive().await? {
                Response::Snapshot { snapshot } if snapshot.stream == stream => {
                    self.replicas.insert(stream.into(), snapshot.clone());
                    return Ok(snapshot);
                }
                Response::Error { code, message } => {
                    return Err(format!("{code}: {message}").into());
                }
                Response::Bye { .. } => return Err("nd-wire disconnected".into()),
                response => {
                    self.apply(response);
                }
            }
        }
    }
}

mod feed;
pub use feed::{FeedUpdate, ReplicaFeed};

impl SyncReplica {
    /// 交还本界面的连接；不结束会话或后端进程。
    pub async fn close(&mut self) -> Result<()> {
        self.send(Request::Bye).await?;
        self.socket.close(None).await?;
        Ok(())
    }
}

mod attachments;
pub use attachments::AttachmentSource;
