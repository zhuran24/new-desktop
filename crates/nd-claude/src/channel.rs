//! mod 通道：守护进程一侧的 unix socket HTTP 服务。mod 发起长轮询与回报，自动重连。
//!
//! 所有进出消息都经 [`ModState`] 归一化成事实并录下来（mod 往返录制）。只接受同 uid 的连接；
//! 对端 pid 是否等于 CLI pid 属于第 4 步 E19，这里不核。
use crate::protocol::{Fact, ModEvent, ModState};
use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use nd_mod_proto::{
    Command, ErrorReply, Hello, HelloReply, ModName, Next, NextQuery, Outcome, Report, ReportAck,
    ResultPost,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::UnixListener, sync::Notify};

/// 录下的一件事：毫秒时间戳（Unix 纪元）、事件与它产生的事实。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    pub seq: u64,
    pub ms: u64,
    pub event: ModEvent,
    pub facts: Vec<Fact>,
}

/// 一条命令的最终结论。`Unknown`：不可重发的命令随旧代次丢了结果，不能再发。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandResult {
    Outcome(Outcome),
    Unknown,
}

/// 某个后端进程此刻的绑定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub backend_session_id: String,
    pub binding_epoch: u64,
    /// 绑定在当前后端会话上的 mod 的最近一次 hello。
    pub mods: BTreeMap<ModName, Hello>,
}

/// 没人取走的结论最多留这么多条，最早的先丢。
const KEEP_RESULTS: usize = 1024;

struct Run {
    state: ModState,
    /// 只在开了录制时保留；生产默认不录。
    record: Option<Vec<Recorded>>,
    results: HashMap<String, CommandResult>,
    result_order: VecDeque<String>,
}
impl Run {
    fn apply(&mut self, event: ModEvent) -> Vec<Fact> {
        let facts = self.state.apply(&event);
        for fact in &facts {
            match fact {
                Fact::Finished { op_id, outcome, .. } => {
                    self.conclude(op_id, CommandResult::Outcome(outcome.clone()))
                }
                Fact::Unknown { op_id, .. } => self.conclude(op_id, CommandResult::Unknown),
                _ => {}
            }
        }
        if let Some(record) = &mut self.record {
            record.push(Recorded {
                seq: record.len() as u64 + 1,
                ms: now_ms(),
                event,
                facts: facts.clone(),
            });
        }
        facts
    }
    fn conclude(&mut self, op_id: &str, result: CommandResult) {
        if self.results.insert(op_id.to_owned(), result).is_none() {
            self.result_order.push_back(op_id.to_owned());
        }
        while self.result_order.len() > KEEP_RESULTS {
            if let Some(oldest) = self.result_order.pop_front() {
                self.results.remove(&oldest);
            }
        }
    }
    fn take(&mut self, op_id: &str) -> Option<CommandResult> {
        let found = self.results.remove(op_id)?;
        self.result_order.retain(|o| o != op_id);
        Some(found)
    }
    fn binding(&self) -> Binding {
        Binding {
            backend_session_id: self.state.session.clone(),
            binding_epoch: self.state.binding_epoch,
            mods: ModName::ALL
                .iter()
                .filter_map(|m| self.state.current(*m).map(|h| (*m, h.clone())))
                .collect(),
        }
    }
}

struct Inner {
    runs: Mutex<HashMap<String, Run>>,
    changed: Arc<Notify>,
    poll_timeout: Duration,
    record: bool,
    socket: PathBuf,
    /// 绑定时 socket 文件的（设备，inode）：只删自己建的那个，不删后来者替换上的。
    inode: (u64, u64),
    stop: tokio::sync::watch::Sender<bool>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if std::fs::symlink_metadata(&self.socket).is_ok_and(|m| (m.dev(), m.ino()) == self.inode) {
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

/// mod 通道服务。克隆共享同一个服务；最后一个克隆丢弃时停止监听并删掉 socket。
#[derive(Clone)]
pub struct ModChannel {
    inner: Arc<Inner>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

impl ModChannel {
    /// 在 `socket` 上监听。已有的同名 socket 文件（上一个守护进程留下的）会被替换；
    /// 不是 socket 的文件不动、直接报错。`record` 打开 mod 往返录制（场景测试用）。
    pub fn bind(socket: &Path, poll_timeout: Duration, record: bool) -> std::io::Result<Self> {
        match std::fs::symlink_metadata(socket) {
            Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(socket)?,
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "refuse to replace a non-socket file",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = SameUid(UnixListener::bind(socket)?);
        let meta = std::fs::symlink_metadata(socket)?;
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let inner = Arc::new(Inner {
            runs: Mutex::new(HashMap::new()),
            changed: Arc::new(Notify::new()),
            poll_timeout,
            record,
            socket: socket.to_owned(),
            inode: (meta.dev(), meta.ino()),
            stop,
        });
        let router = Router::new()
            .route("/hello", post(hello))
            .route("/next", get(next))
            .route("/result/{op_id}", post(result))
            .route("/report", post(report))
            .with_state(Arc::downgrade(&inner));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = stopped.wait_for(|s| *s).await;
                })
                .await;
        });
        Ok(Self { inner })
    }

    pub fn socket(&self) -> &Path {
        &self.inner.socket
    }

    /// 开始接受这个后端进程的 mod：`expected_session` 是预定或续接的后端会话 id。
    pub fn register(&self, run: &str, expected_session: &str) {
        self.inner.runs.lock().unwrap().insert(
            run.to_owned(),
            Run {
                state: ModState::new(expected_session),
                record: self.inner.record.then(Vec::new),
                results: HashMap::new(),
                result_order: VecDeque::new(),
            },
        );
        self.inner.changed.notify_waiters();
    }

    pub fn unregister(&self, run: &str) {
        self.inner.runs.lock().unwrap().remove(run);
        self.inner.changed.notify_waiters();
    }

    fn with<T>(&self, run: &str, f: impl FnOnce(&mut Run) -> T) -> Option<T> {
        self.inner.runs.lock().unwrap().get_mut(run).map(f)
    }

    /// 握手结束：此后报来新的后端会话 id 算重绑。
    pub fn settle(&self, run: &str) {
        self.with(run, |r| r.apply(ModEvent::Settled));
        self.inner.changed.notify_waiters();
    }

    pub fn binding(&self, run: &str) -> Option<Binding> {
        self.with(run, |r| r.binding())
    }

    /// 录下的 mod 往返；没开录制时为空。
    pub fn recording(&self, run: &str) -> Vec<Recorded> {
        self.with(run, |r| r.record.clone().unwrap_or_default())
            .unwrap_or_default()
    }

    /// 等到 `until` 对当前绑定成立，或到时限后返回最后一次看到的绑定。
    pub async fn wait_binding(
        &self,
        run: &str,
        timeout: Duration,
        until: impl Fn(&Binding) -> bool,
    ) -> Option<Binding> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let binding = self.binding(run)?;
            if until(&binding) {
                return Some(binding);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.binding(run);
            }
        }
    }

    /// 在同一把锁下按当前绑定组装命令并入队；未绑定的 mod 不发送。
    pub fn send_current(
        &self,
        run: &str,
        module: ModName,
        op_id: &str,
        action: nd_mod_proto::Action,
    ) -> bool {
        let sent = self
            .with(run, |r| {
                let hello = r.state.current(module)?;
                let command = Command {
                    op_id: op_id.into(),
                    expected_backend_session_id: hello.backend_session_id.clone(),
                    expected_mod_gen: hello.mod_gen.clone(),
                    action,
                };
                r.apply(ModEvent::Send { module, command });
                Some(())
            })
            .flatten()
            .is_some();
        self.inner.changed.notify_waiters();
        sent
    }

    /// 把命令放进 mod 的队列，唤醒挂着的长轮询。
    pub fn send(&self, run: &str, module: ModName, command: Command) -> bool {
        let sent = self
            .with(run, |r| r.apply(ModEvent::Send { module, command }))
            .is_some();
        self.inner.changed.notify_waiters();
        sent
    }

    /// 等这条命令的结论并取走；到时限还没有就回 None（交付不明，由调用方按可重发类别处理）。
    pub async fn result(&self, run: &str, op_id: &str, timeout: Duration) -> Option<CommandResult> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(found) = self.with(run, |r| r.take(op_id))? {
                return Some(found);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.with(run, |r| r.take(op_id)).flatten();
            }
        }
    }
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorReply {
            code: code.into(),
            message: message.into(),
        }),
    )
        .into_response()
}

type Shared = std::sync::Weak<Inner>;

fn unknown_run() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "unknown_run",
        "这个后端进程不归本守护进程管",
    )
}

async fn hello(State(inner): State<Shared>, Json(hello): Json<Hello>) -> Response {
    let Some(inner) = inner.upgrade() else {
        return unknown_run();
    };
    let reply = {
        let mut runs = inner.runs.lock().unwrap();
        let Some(run) = runs.get_mut(&hello.run) else {
            return unknown_run();
        };
        run.apply(ModEvent::Hello { hello });
        HelloReply {
            binding_epoch: run.state.binding_epoch,
        }
    };
    inner.changed.notify_waiters();
    Json(reply).into_response()
}

/// 长轮询：有命令、要重报或到时限才回。等待期间不持有通道状态，
/// 适配器被丢弃（守护进程停止）时立刻结束，mod 随后重连新的守护进程。
async fn next(State(weak): State<Shared>, Query(query): Query<NextQuery>) -> Response {
    let Some((changed, mut stop, deadline)) = weak.upgrade().map(|inner| {
        (
            inner.changed.clone(),
            inner.stop.subscribe(),
            tokio::time::Instant::now() + inner.poll_timeout,
        )
    }) else {
        return unknown_run();
    };
    loop {
        let notified = changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let answer = {
            let Some(inner) = weak.upgrade() else {
                return unknown_run();
            };
            let mut runs = inner.runs.lock().unwrap();
            let Some(run) = runs.get_mut(&query.run) else {
                return unknown_run();
            };
            let superseded = run
                .state
                .current(query.module)
                .is_some_and(|h| h.mod_gen == query.mod_gen);
            if !run.state.polls_current(&query) && superseded {
                // 这次轮询在途时 mod 已用新 id 重报过：让它带新 id 再来，不必重报。
                Some(Next {
                    commands: vec![],
                    rehello: false,
                })
            } else if !run.state.polls_current(&query) {
                run.apply(ModEvent::Poll {
                    query: query.clone(),
                });
                Some(Next {
                    commands: vec![],
                    rehello: true,
                })
            } else {
                let commands = run.state.deliverable(query.module);
                if commands.is_empty() {
                    None
                } else {
                    run.apply(ModEvent::Poll {
                        query: query.clone(),
                    });
                    Some(Next {
                        commands,
                        rehello: false,
                    })
                }
            }
        };
        if let Some(answer) = answer {
            return Json(answer).into_response();
        }
        tokio::select! {
            _ = &mut notified => {}
            _ = tokio::time::sleep_until(deadline) => {
                return Json(Next { commands: vec![], rehello: false }).into_response();
            }
            _ = stop.wait_for(|s| *s) => return unknown_run(),
        }
    }
}

async fn result(
    State(inner): State<Shared>,
    UrlPath(op_id): UrlPath<String>,
    Json(post): Json<ResultPost>,
) -> Response {
    let Some(inner) = inner.upgrade() else {
        return unknown_run();
    };
    {
        let mut runs = inner.runs.lock().unwrap();
        let Some(run) = runs.get_mut(&post.run) else {
            return unknown_run();
        };
        run.apply(ModEvent::Result { op_id, post });
    }
    inner.changed.notify_waiters();
    Json(serde_json::json!({})).into_response()
}

async fn report(State(inner): State<Shared>, Json(report): Json<Report>) -> Response {
    let Some(inner) = inner.upgrade() else {
        return unknown_run();
    };
    {
        let mut runs = inner.runs.lock().unwrap();
        let Some(run) = runs.get_mut(&report.run) else {
            return unknown_run();
        };
        run.apply(ModEvent::Report { report });
    }
    inner.changed.notify_waiters();
    // 本单还没有事实存储：报告只在内存里，如实回 durable:false。
    Json(ReportAck { durable: false }).into_response()
}

struct SameUid(UnixListener);
impl axum::serve::Listener for SameUid {
    type Io = tokio::net::UnixStream;
    type Addr = tokio::net::unix::SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((stream, address))
                    if stream
                        .peer_cred()
                        .is_ok_and(|c| c.uid() == rustix::process::geteuid().as_raw()) =>
                {
                    return (stream, address);
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.0.local_addr()
    }
}
