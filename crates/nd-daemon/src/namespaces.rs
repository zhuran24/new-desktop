//! 命名空间端口与组件登记。传输只转发请求，资源名由所属提供者解释。
use crate::{CommandsConfig, Config, Result, commands, page_items};
use nd_config::Section;
use nd_kernel::{
    ComponentSpec, Dep, Kernel, Key, Lifecycle, Registration, RegistrationKind, ScopeId,
};
use nd_wire::{
    Command, CommandReply, Cursor, Event, Fallback, Item, PageReq, ReceiptLookup, Response,
    Snapshot,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

pub type Pending<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub struct Execution {
    pub result: Pending<CommandReply>,
    pub detached: bool,
}
pub struct Subscription {
    pub first: Response,
    pub replay: Vec<Event>,
    pub events: broadcast::Receiver<Event>,
    pub guard: Box<dyn Send>,
}
pub struct StreamError {
    pub code: &'static str,
    pub message: String,
}

/// 每次调用先经组件内核查当前提供者；异步工作只捕获本次已受理调用所需的资源。
pub trait NamespaceProvider: Send + Sync {
    fn names(&self) -> BTreeMap<String, u32>;
    fn registrations(&self) -> Vec<(RegistrationKind, &'static str)> {
        vec![]
    }
    fn accepts(&self, namespace: &str) -> bool {
        self.names().contains_key(namespace)
    }
    /// 当前命名空间对 global 快照的贡献。
    fn snapshot(&self) -> Vec<Item> {
        vec![]
    }
    /// 首帧、补齐与事件来源同时取得，防止快照与订阅之间漏事件。
    fn events(
        &self,
        _stream: String,
        _since: Option<Cursor>,
        _max_replay: usize,
    ) -> std::result::Result<Subscription, StreamError> {
        Err(StreamError {
            code: "unsupported",
            message: "此命名空间没有独立事件流".into(),
        })
    }
    fn command(&self, _name: &str) -> std::result::Result<Value, String> {
        Err("not_found".into())
    }
    fn execute(&self, _command: Command) -> Option<Execution> {
        None
    }
    fn receipt(&self, _id: &str, _hash: Option<&str>) -> Option<ReceiptLookup> {
        None
    }
    fn page(&self, _res: String, page: PageReq) -> Pending<std::result::Result<Value, String>> {
        let result = page_items(self.snapshot(), &page).map(|page| json!(page));
        Box::pin(async move { result })
    }
    /// 保留已有 Models 请求作为兼容入口，调用仍经 models 命名空间路由。
    fn models(
        &self,
        _backend: String,
        _cwd: String,
    ) -> Pending<std::result::Result<Value, String>> {
        Box::pin(async { Err("not_found".into()) })
    }
}

#[derive(Default)]
pub(super) struct Providers {
    entries: Vec<Dep<dyn NamespaceProvider>>,
}
struct MountedProvider;
impl Lifecycle for MountedProvider {}
impl Providers {
    pub fn install(
        &mut self,
        kernel: &mut Kernel,
        scope: ScopeId,
        name: &str,
        optional: bool,
        provider: Arc<dyn NamespaceProvider>,
    ) -> Result<()> {
        let key = Key::<dyn NamespaceProvider>::new(scope, name);
        let mut spec = ComponentSpec::new(name, scope).provides(key.erased());
        if optional {
            spec = spec.optional();
        }
        let provided = key.clone();
        kernel.install(spec, move |mount| {
            mount.provide(provided.clone(), provider.clone())?;
            for (kind, name) in provider.registrations() {
                mount.register(Registration::new(scope, kind, name))?;
            }
            Ok(Box::new(MountedProvider))
        })?;
        self.entries.push(kernel.require(key));
        Ok(())
    }
    pub fn names(&self) -> BTreeMap<String, u32> {
        self.entries
            .iter()
            .filter_map(|entry| entry.with(|p| p.names()))
            .flatten()
            .collect()
    }
    pub fn snapshot(&self) -> Vec<Item> {
        self.entries
            .iter()
            .filter_map(|entry| entry.with(|p| p.snapshot()))
            .flatten()
            .collect()
    }
    pub fn with<R>(
        &self,
        resource: &str,
        call: impl FnOnce(&(dyn NamespaceProvider + 'static)) -> R,
    ) -> Option<R> {
        let namespace = resource.split(['/', '.']).next().unwrap_or(resource);
        self.entries
            .iter()
            .find(|entry| entry.with(|p| p.accepts(namespace)) == Some(true))
            .and_then(|entry| entry.with(call))
    }
}

pub(super) struct SessionNamespace(pub Arc<nd_session::Sessions>);
impl NamespaceProvider for SessionNamespace {
    fn names(&self) -> BTreeMap<String, u32> {
        ["session", "sessions", "models"]
            .map(|s| (s.into(), 1))
            .into()
    }
    fn snapshot(&self) -> Vec<Item> {
        self.0.listing().items()
    }
    fn events(
        &self,
        stream: String,
        since: Option<Cursor>,
        _max_replay: usize,
    ) -> std::result::Result<Subscription, StreamError> {
        let session = stream.strip_prefix("session/").ok_or_else(|| StreamError {
            code: "unsupported",
            message: "须指定 session/<id>".into(),
        })?;
        let subscribed = self
            .0
            .subscribe(&nd_backend::SessionId(session.into()), since)
            .map_err(|e| StreamError {
                code: "unavailable",
                message: e.to_string(),
            })?
            .ok_or_else(|| StreamError {
                code: "not_found",
                message: format!("没有会话 {session}"),
            })?;
        let (first, replay, events, guard) = subscribed.into_parts();
        Ok(Subscription {
            first,
            replay,
            events,
            guard: Box::new(guard),
        })
    }
    fn execute(&self, command: Command) -> Option<Execution> {
        if !command.name.starts_with("session.") {
            return None;
        }
        let sessions = self.0.clone();
        Some(Execution {
            detached: nd_session::delivery(&command.name),
            result: Box::pin(async move {
                sessions
                    .execute(&command)
                    .await
                    .unwrap_or(CommandReply::Unavailable {
                        code: None,
                        reason: "没有这个会话命令".into(),
                    })
            }),
        })
    }
    fn page(&self, res: String, page: PageReq) -> Pending<std::result::Result<Value, String>> {
        let sessions = self.0.clone();
        Box::pin(async move {
            if res == "sessions" {
                return page_items(sessions.listing().items(), &page).map(|page| json!(page));
            }
            let id = res.strip_prefix("session/").ok_or("not_found")?;
            let id = id.strip_suffix("/items").unwrap_or(id);
            sessions
                .page(&nd_backend::SessionId(id.into()), &page)
                .map(|page| json!(page))
        })
    }
    fn models(&self, backend: String, cwd: String) -> Pending<std::result::Result<Value, String>> {
        let sessions = self.0.clone();
        Box::pin(async move {
            sessions
                .models(&backend, cwd.into())
                .await
                .map(|models| json!(models))
        })
    }
}

pub(super) struct RunsNamespace(pub Arc<nd_runs::Watchdogs>);
impl NamespaceProvider for RunsNamespace {
    fn names(&self) -> BTreeMap<String, u32> {
        BTreeMap::from([("runs".into(), 1)])
    }
    fn page(&self, res: String, page: PageReq) -> Pending<std::result::Result<Value, String>> {
        let runs = self.0.clone();
        Box::pin(async move {
            if res != "runs" {
                return Err("not_found".into());
            }
            tokio::task::spawn_blocking(move || {
                let items = runs
                    .inspect()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|found| Item {
                        id: format!("run/{}", found.run),
                        namespace: "runs".into(),
                        kind: "run".into(),
                        fallback: Fallback {
                            title: found.run.clone(),
                            text: found.state.to_string(),
                        },
                        data: json!(found),
                    })
                    .collect();
                page_items(items, &page).map(|page| json!(page))
            })
            .await
            .map_err(|e| e.to_string())?
        })
    }
}

struct GlobalFeed {
    system: Option<Item>,
    snapshot: Snapshot,
    history: VecDeque<Event>,
    events: broadcast::Sender<Event>,
}
pub(super) struct SystemNamespace {
    feed: Mutex<GlobalFeed>,
    pub store: Arc<nd_store::Store>,
    pub config: Arc<Config>,
    pub fault_root: std::path::PathBuf,
}
impl SystemNamespace {
    pub fn new(
        store: Arc<nd_store::Store>,
        config: Arc<Config>,
        fault_root: std::path::PathBuf,
    ) -> Self {
        Self {
            store,
            config,
            fault_root,
            feed: Mutex::new(GlobalFeed {
                system: None,
                snapshot: Snapshot {
                    stream: "global".into(),
                    epoch: uuid::Uuid::new_v4().to_string(),
                    cursor: 0,
                    items: vec![],
                },
                history: VecDeque::new(),
                events: broadcast::channel(128).0,
            }),
        }
    }
    pub fn epoch(&self) -> String {
        self.feed.lock().unwrap().snapshot.epoch.clone()
    }
    pub fn system(&self, item: Item) {
        self.feed.lock().unwrap().system = Some(item);
    }
    pub fn initialize(&self, items: Vec<Item>) {
        self.feed.lock().unwrap().snapshot.items = items;
    }
    pub fn publish(&self, items: Vec<Item>) {
        let mut feed = self.feed.lock().unwrap();
        if items == feed.snapshot.items {
            return;
        }
        let event = Event {
            stream: "global".into(),
            epoch: feed.snapshot.epoch.clone(),
            cursor: feed.snapshot.cursor + 1,
            upsert: items
                .iter()
                .filter(|i| !feed.snapshot.items.contains(i))
                .cloned()
                .collect(),
            remove: feed
                .snapshot
                .items
                .iter()
                .filter(|old| !items.iter().any(|i| i.id == old.id))
                .map(|i| i.id.clone())
                .collect(),
        };
        feed.snapshot.items = items;
        feed.snapshot.cursor = event.cursor;
        feed.history.push_back(event.clone());
        if feed.history.len() > 128 {
            feed.history.pop_front();
        }
        let _ = feed.events.send(event);
    }
    pub fn reject(&self, command: Command) -> Execution {
        let (store, config, root) = (
            self.store.clone(),
            self.config.clone(),
            self.fault_root.clone(),
        );
        Execution {
            detached: false,
            result: Box::pin(async move {
                durable_command(&store, &config, &root, &command, |_| {
                    Ok(nd_wire::Receipt::Rejected {
                        code: "not_found".into(),
                        now: Value::Null,
                    })
                })
            }),
        }
    }
}
impl NamespaceProvider for SystemNamespace {
    fn names(&self) -> BTreeMap<String, u32> {
        BTreeMap::from([("system".into(), 1)])
    }
    fn accepts(&self, namespace: &str) -> bool {
        matches!(namespace, "system" | "global")
    }
    fn snapshot(&self) -> Vec<Item> {
        self.feed.lock().unwrap().system.iter().cloned().collect()
    }
    fn events(
        &self,
        stream: String,
        since: Option<Cursor>,
        max_replay: usize,
    ) -> std::result::Result<Subscription, StreamError> {
        if stream != "global" {
            return Err(StreamError {
                code: "unsupported",
                message: "系统事件流是 global".into(),
            });
        }
        let feed = self.feed.lock().unwrap();
        let events = feed.events.subscribe();
        let snapshot = &feed.snapshot;
        let earliest = feed
            .history
            .front()
            .map_or(snapshot.cursor, |event| event.cursor - 1);
        let (first, replay) = if let Some(since) = since.filter(|c| {
            c.epoch == snapshot.epoch
                && c.seq >= earliest
                && c.seq <= snapshot.cursor
                && snapshot.cursor - c.seq < max_replay as u64
        }) {
            (
                Response::Resumed {
                    stream,
                    epoch: snapshot.epoch.clone(),
                    cursor: snapshot.cursor,
                },
                feed.history
                    .iter()
                    .filter(|e| e.cursor > since.seq)
                    .cloned()
                    .collect(),
            )
        } else {
            (
                Response::Snapshot {
                    snapshot: snapshot.clone(),
                },
                vec![],
            )
        };
        Ok(Subscription {
            first,
            replay,
            events,
            guard: Box::new(()),
        })
    }
    fn receipt(&self, id: &str, hash: Option<&str>) -> Option<ReceiptLookup> {
        Some(commands::lookup(&self.store, id, hash).unwrap_or_else(|e| {
            ReceiptLookup::Unavailable {
                reason: e.to_string(),
            }
        }))
    }
    fn page(&self, res: String, page: PageReq) -> Pending<std::result::Result<Value, String>> {
        if !matches!(res.as_str(), "system" | "global") {
            return Box::pin(async { Err("not_found".into()) });
        }
        let items = self
            .feed
            .lock()
            .unwrap()
            .snapshot
            .items
            .iter()
            .filter(|i| res == "global" || i.namespace == "system")
            .cloned()
            .collect();
        let result = page_items(items, &page).map(|page| json!(page));
        Box::pin(async move { result })
    }
}

/// 收据与业务效果的同事务外壳，供有持久命令的命名空间及未知命令拒绝共用。
pub(super) fn durable_command(
    store: &nd_store::Store,
    config: &Config,
    _root: &Path,
    command: &Command,
    effect: impl FnOnce(&mut nd_store::Tx<'_>) -> nd_store::Result<nd_wire::Receipt>,
) -> CommandReply {
    #[cfg(feature = "scenarios")]
    let fault = crate::faults::Fault::take(_root, &command.id);
    let result = store.write(|tx| {
        let reply = commands::execute(
            tx,
            command,
            CommandsConfig::parse(&config.snapshot().value)
                .expect("validated published section")
                .receipt_keep_ms,
            |tx| {
                let receipt = effect(tx)?;
                #[cfg(feature = "scenarios")]
                fault.crash(crate::faults::Point::AfterEffect);
                Ok(receipt)
            },
        )?;
        #[cfg(feature = "scenarios")]
        fault.crash(crate::faults::Point::BeforeCommit);
        Ok(reply)
    });
    #[cfg(feature = "scenarios")]
    if result.is_ok() {
        fault.crash(crate::faults::Point::AfterCommit);
    }
    result.unwrap_or_else(|e| CommandReply::Unavailable {
        code: None,
        reason: e.to_string(),
    })
}
