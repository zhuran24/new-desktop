//! 会话组件：会话名册与持久操作引擎（规格「三个加深模块的契约／持久操作引擎」「会话名册」）。
//!
//! - 名册 [`Sessions`]：`session.create` 在同一事务里写会话行、收据和种子操作；按需装载会话执行器；
//!   给侧栏的会话列表。
//! - 每个装载中的会话一个执行器，串行处理它的全部输入（界面命令、后端批次、独占登记的变化），
//!   一个输入一个事务；操作写成纯函数 `run(&View, &mut Journal)`，每个输入之后从头重跑。
//! - 谱系、发送台、对话投影是执行器内部的 fold；界面经 nd-wire 的命令、快照与事件。
//!   谱系另有可直接测试的纯函数接口，不另立组件。
//!
//! 后端经 [`nd_backend::Backends`]；跨会话的独占经 [`nd_claims::Exclusivity`]，在执行器的事务里放行。
mod executor;
mod feed;
pub mod journal;
pub mod lineage;
pub mod ops;
pub mod projection;
pub mod scripted;
mod state;

pub use journal::{Change, Halt, Journal, View};
pub use state::Status;

use executor::{Deps, Executor, Flow, Input, Shared};
use nd_backend::{Backends, SessionId};
use nd_claims::Exclusivity;
use nd_store::Store;
use nd_wire::{Command, CommandReply, Cursor, Event, Fallback, Item, Receipt, Response};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

/// 测试构建用的提交点故障：执行器在这里「崩溃」（停下、丢掉内存状态），库里只留已提交的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// 事务里一切都写了、提交之前。
    BeforeCommit,
    /// 提交之后、把新票交给端口之前。
    AfterCommit,
    /// 交给端口之后、结果入账之前。
    AfterHandoff,
}
pub trait Faults: Send + Sync {
    fn crash(&self, session: &SessionId, point: Fault) -> bool;
}

#[derive(Clone)]
pub struct EngineConfig {
    pub receipt_keep_ms: u64,
    /// 当前进程闲置多久回收；规格默认 15 分钟，测试用配置缩短。
    pub idle_reclaim: Duration,
    /// 闲置检查的间隔。
    pub tick: Duration,
    /// 测试构建：每次重跑都跑两遍比对，抓 `run` 里的非确定性。
    pub check_purity: bool,
    pub faults: Option<Arc<dyn Faults>>,
}
impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            receipt_keep_ms: 7 * 24 * 3600 * 1000,
            idle_reclaim: Duration::from_secs(15 * 60),
            tick: Duration::from_secs(1),
            check_purity: false,
            faults: None,
        }
    }
}

pub fn migrate(tx: &mut nd_store::Tx<'_>) -> nd_store::Result<()> {
    nd_ledger::migrate(tx)?;
    state::migrate(tx)
}

/// 侧栏的会话列表（`global` 流里 `sessions` 命名空间的条目）与一次性的提示。
pub struct Listing {
    items: Mutex<BTreeMap<String, Item>>,
    notices: Mutex<VecDeque<Item>>,
    changed: watch::Sender<u64>,
}
const KEEP_NOTICES: usize = 32;
impl Listing {
    fn new(cores: Vec<state::Core>) -> Self {
        let items = cores
            .iter()
            .map(|core| {
                let item = list_item(core);
                (item.id.clone(), item)
            })
            .collect();
        Self {
            items: Mutex::new(items),
            notices: Mutex::new(VecDeque::new()),
            changed: watch::channel(0).0,
        }
    }
    pub(crate) fn update(&self, core: &state::Core) {
        let Some(meta) = &core.meta else {
            return;
        };
        let id = format!("session/{}", meta.id);
        let mut items = self.items.lock().unwrap();
        let mut changed = false;
        if meta.status == Status::Withdrawn {
            if items.remove(&id).is_some() {
                // 撤掉的会话只提示一次。
                let mut notices = self.notices.lock().unwrap();
                notices.push_back(Item {
                    id: format!("notice/{}", meta.id),
                    namespace: "sessions".into(),
                    kind: "notice".into(),
                    data: json!({"session": meta.id, "status": "withdrawn", "created_by": meta.created_by, "reason": meta.note}),
                    fallback: Fallback {
                        title: "会话没建成".into(),
                        text: format!(
                            "已撤掉：{}",
                            meta.note.clone().unwrap_or_default()
                        ),
                    },
                });
                while notices.len() > KEEP_NOTICES {
                    notices.pop_front();
                }
                changed = true;
            }
        } else {
            let item = list_item(core);
            if items.get(&id) != Some(&item) {
                items.insert(id, item);
                changed = true;
            }
        }
        drop(items);
        if changed {
            self.changed.send_modify(|n| *n += 1);
        }
    }
    pub fn items(&self) -> Vec<Item> {
        let mut out: Vec<Item> = self.items.lock().unwrap().values().cloned().collect();
        out.extend(self.notices.lock().unwrap().iter().cloned());
        out
    }
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
}

fn list_item(core: &state::Core) -> Item {
    let meta = core.meta();
    let alive = core.current_carrier().is_some_and(|c| c.alive);
    let label = match meta.status {
        Status::Preparing => "准备中",
        Status::Active => "",
        Status::Partial => "部分完成",
        Status::Withdrawn => "已撤掉",
    };
    Item {
        id: format!("session/{}", meta.id),
        namespace: "sessions".into(),
        kind: "session".into(),
        data: json!({
            "session": meta.id,
            "status": meta.status.as_str(),
            "created_by": meta.created_by,
            "cwd": meta.cwd,
            "backend": format!("{:?}", meta.kind).to_lowercase(),
            "model": meta.model,
            "note": meta.note,
            "process_alive": alive,
        }),
        fallback: Fallback {
            title: meta.cwd.display().to_string(),
            text: if label.is_empty() {
                meta.status.as_str().into()
            } else {
                format!("{label}：{}", meta.note.clone().unwrap_or_default())
            },
        },
    }
}

struct Handle {
    inputs: mpsc::Sender<Input>,
    shared: Arc<Shared>,
}

/// 订阅一个会话流：开头（快照或续上的事件），之后的事件；丢掉它就不再算「有人在看」。
pub struct Subscription {
    pub first: Response,
    pub replay: Vec<Event>,
    pub events: broadcast::Receiver<Event>,
    guard: WatchGuard,
}
impl Subscription {
    pub fn into_parts(self) -> (Response, Vec<Event>, broadcast::Receiver<Event>, WatchGuard) {
        (self.first, self.replay, self.events, self.guard)
    }
}
pub struct WatchGuard(Arc<Shared>);
impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.0.watchers.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 会话名册：新建、按需装载、列表、稳定收件地址。
pub struct Sessions {
    deps: Deps,
    executors: Mutex<HashMap<SessionId, Arc<Handle>>>,
}

/// 由建会话的命令 id 派生会话 id：同一条命令重试落在同一个会话上。
pub fn session_id_for(command_id: &str) -> SessionId {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("nd-session:{command_id}").as_bytes());
    SessionId(format!(
        "s-{}",
        digest[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}

impl Sessions {
    /// 建表、读回列表。独占登记的变化会唤醒全部装载中的会话（放行可能变了）。
    pub fn new(
        store: Arc<Store>,
        claims: Arc<Exclusivity>,
        backends: Backends,
        config: EngineConfig,
    ) -> nd_store::Result<Arc<Self>> {
        store.write(migrate)?;
        let listing = Arc::new(Listing::new(state::listed(&store)?));
        let this = Arc::new(Self {
            deps: Deps {
                store,
                claims: claims.clone(),
                backends,
                config,
                listing,
            },
            executors: Mutex::new(HashMap::new()),
        });
        let weak = Arc::downgrade(&this);
        let mut changes = claims.watch();
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let Some(this) = weak.upgrade() else { break };
                this.kick_all();
            }
        });
        Ok(this)
    }

    pub fn listing(&self) -> &Arc<Listing> {
        &self.deps.listing
    }
    pub async fn models(
        &self,
        backend: &str,
        cwd: std::path::PathBuf,
    ) -> Result<Vec<nd_wire::Model>, String> {
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err("工作目录必须是已存在的绝对路径".into());
        }
        self.deps.backends.models(backend, cwd).await
    }

    /// 守护进程启动时：装载有活进程、有进行中操作或有未结票的会话，交端口对账。
    pub fn recover(&self) -> nd_store::Result<Vec<SessionId>> {
        let ids = state::needing_recovery(&self.deps.store)?;
        for id in &ids {
            self.handle(id, false)?;
        }
        Ok(ids)
    }

    pub fn kick_all(&self) {
        for handle in self.executors.lock().unwrap().values() {
            let _ = handle.inputs.try_send(Input::Kick);
        }
    }

    fn exists(&self, id: &SessionId) -> nd_store::Result<bool> {
        use nd_store::OptionalExtension;
        Ok(self
            .deps
            .store
            .read()?
            .query_row("SELECT 1 FROM sessions WHERE id=?1", [id.as_str()], |_| {
                Ok(())
            })
            .optional()?
            .is_some())
    }

    fn handle(&self, id: &SessionId, allow_unborn: bool) -> nd_store::Result<Option<Arc<Handle>>> {
        let mut executors = self.executors.lock().unwrap();
        if let Some(handle) = executors.get(id)
            && !handle.inputs.is_closed()
        {
            return Ok(Some(handle.clone()));
        }
        if !allow_unborn && !self.exists(id)? {
            return Ok(None);
        }
        let (inputs, rx) = mpsc::channel(256);
        let (batches, batch_rx) = mpsc::channel(64);
        let shared = Arc::new(Shared {
            feed: Mutex::new(feed::Feed::new(
                format!("session/{id}"),
                String::new(),
                vec![],
            )),
            watchers: Default::default(),
        });
        let (ready, born) = std::sync::mpsc::sync_channel(1);
        let deps = self.deps.clone();
        let session = id.clone();
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name(format!("nd-session-{}", &id.0[..id.0.len().min(10)]))
            .spawn(move || run_executor(session, deps, thread_shared, batches, rx, batch_rx, ready))
            .map_err(nd_store::Error::Io)?;
        let born = born
            .recv()
            .map_err(|_| nd_store::Error::Aborted("session executor did not start".into()))??;
        if !born && !allow_unborn {
            drop(inputs);
            return Ok(None);
        }
        let handle = Arc::new(Handle { inputs, shared });
        executors.insert(id.clone(), handle.clone());
        Ok(Some(handle))
    }

    /// 命令：`session.create`、`session.send`。不是会话命令时返回 None，由别的提供者处理。
    pub async fn execute(&self, command: &Command) -> Option<CommandReply> {
        let target = match command.name.as_str() {
            "session.create" => session_id_for(&command.id),
            "session.send" | "session.interrupt" | "session.withdraw" | "session.draft.save" => {
                match command.args["session"].as_str() {
                    Some(id) => SessionId(id.to_owned()),
                    None => return Some(self.reject_without_session(command, "invalid")),
                }
            }
            _ => return None,
        };
        let allow_unborn = command.name == "session.create";
        let handle = match self.handle(&target, allow_unborn) {
            Ok(Some(handle)) => handle,
            Ok(None) => return Some(self.reject_without_session(command, "not_found")),
            Err(e) => {
                return Some(CommandReply::Unavailable {
                    reason: e.to_string(),
                });
            }
        };
        let (reply, answer) = oneshot::channel();
        if handle
            .inputs
            .send(Input::Command {
                command: command.clone(),
                reply,
            })
            .await
            .is_err()
        {
            return Some(CommandReply::Unavailable {
                reason: "会话执行器已停".into(),
            });
        }
        let result = match answer.await {
            Ok(result) => result,
            // 执行器停了（崩溃）：它的事务要么已提交、要么永远不会提交，按收据定。
            Err(_) => match nd_ledger::lookup(
                &self.deps.store,
                &command.id,
                Some(&command.content_hash()),
            ) {
                Ok(nd_wire::ReceiptLookup::Found { receipt }) => CommandReply::Receipt { receipt },
                Ok(nd_wire::ReceiptLookup::Conflict) => CommandReply::Conflict,
                Ok(nd_wire::ReceiptLookup::Expired) => CommandReply::Expired,
                _ => CommandReply::Unavailable {
                    reason: "会话执行器已停，命令没有受理".into(),
                },
            },
        };
        if allow_unborn
            && !matches!(
                result,
                CommandReply::Receipt {
                    receipt: Receipt::Accepted { .. }
                }
            )
        {
            // 没建成的执行器不留着（会话行不存在）；建成过的照常留着。
            if let Ok(false) = self.exists(&target) {
                self.executors.lock().unwrap().remove(&target);
            }
        }
        Some(result)
    }

    fn reject_without_session(&self, command: &Command, code: &str) -> CommandReply {
        let keep = self.deps.config.receipt_keep_ms;
        self.deps
            .store
            .write(|tx| {
                nd_ledger::execute(tx, command, keep, |_| {
                    Ok(Receipt::Rejected {
                        code: code.into(),
                        now: Value::Null,
                    })
                })
            })
            .unwrap_or_else(|e| CommandReply::Unavailable {
                reason: e.to_string(),
            })
    }

    /// 订阅 `session/<id>`：装载会话（不存在则 None），先快照或续上事件。
    pub fn subscribe(
        &self,
        id: &SessionId,
        since: Option<Cursor>,
    ) -> nd_store::Result<Option<Subscription>> {
        let Some(handle) = self.handle(id, false)? else {
            return Ok(None);
        };
        handle.shared.watchers.fetch_add(1, Ordering::AcqRel);
        let guard = WatchGuard(handle.shared.clone());
        let start = handle.shared.feed.lock().unwrap().subscribe(since);
        Ok(Some(Subscription {
            first: start.first,
            replay: start.replay,
            events: start.events,
            guard,
        }))
    }

    /// 测试与停机用：停掉一个会话的执行器（不结束后端进程、不释放租约）。
    pub fn unload(&self, id: &SessionId) {
        self.executors.lock().unwrap().remove(id);
        self.deps.backends.release(id);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_executor(
    id: SessionId,
    deps: Deps,
    shared: Arc<Shared>,
    batches: mpsc::Sender<nd_backend::Batch>,
    mut inputs: mpsc::Receiver<Input>,
    mut batch_rx: mpsc::Receiver<nd_backend::Batch>,
    ready: std::sync::mpsc::SyncSender<nd_store::Result<bool>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let _ = ready.send(Err(nd_store::Error::Io(e)));
            return;
        }
    };
    runtime.block_on(async move {
        let tick = deps.config.tick;
        let mut executor = match Executor::open(id.clone(), deps, shared.clone(), batches) {
            Ok(executor) => executor,
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        };
        executor.install_feed();
        let _ = ready.send(Ok(executor.born()));
        loop {
            while let Some(local) = executor.next_local() {
                if let Flow::Died = executor.handle(local) {
                    return;
                }
            }
            let input = tokio::select! {
                input = inputs.recv() => match input {
                    Some(input) => input,
                    None => return,
                },
                Some(batch) = batch_rx.recv() => Input::Batch(batch),
                _ = tokio::time::sleep(tick) => Input::Tick,
            };
            if let Flow::Died = executor.handle(input) {
                return;
            }
        }
    });
}
