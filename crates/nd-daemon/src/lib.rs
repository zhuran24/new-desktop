//! New Desktop 守护进程及 nd-wire 入口。
mod local;
use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
    routing::get,
};
use nd_config::{Config, FileSource, Section};
use nd_kernel::{
    ComponentSpec, ConfigChange, Kernel, Key, Lifecycle, Registration, RegistrationKind,
};
use nd_wire::{
    Event, Fallback, Item, PROTOCOL_VERSION, Request, Response as WireResponse, Snapshot,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, broadcast};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// 提供者由组件内核持有；撤销后不再通过旧 Arc 受理调用。
pub trait NamespaceProvider: Send + Sync {
    fn names(&self) -> BTreeMap<String, u32>;
    fn snapshot(&self) -> Vec<Item>;
    fn command(&self, name: &str) -> std::result::Result<Value, String>;
    fn page(&self, request: &nd_wire::PageReq) -> std::result::Result<nd_wire::Page, String> {
        page_items(self.snapshot(), request)
    }
}
struct Diagnostics;
impl Lifecycle for Diagnostics {}
impl NamespaceProvider for Diagnostics {
    fn names(&self) -> BTreeMap<String, u32> {
        BTreeMap::from([("diagnostics".into(), 1)])
    }
    fn snapshot(&self) -> Vec<Item> {
        vec![Item {
            id: "diagnostics".into(),
            namespace: "diagnostics".into(),
            kind: "status".into(),
            data: json!({"commands":["diagnostics.inspect"]}),
            fallback: Fallback {
                title: "诊断".into(),
                text: "可选诊断组件已启用".into(),
            },
        }]
    }
    fn command(&self, name: &str) -> std::result::Result<Value, String> {
        if name == "diagnostics.inspect" {
            Ok(json!({"status":"ready"}))
        } else {
            Err("not_found".into())
        }
    }
}
#[derive(Clone, Deserialize)]
struct DiagnosticsConfig {
    enabled: bool,
}
impl Section for DiagnosticsConfig {
    const NAME: &'static str = "diagnostics";
}
#[derive(Clone, Deserialize)]
struct StorageConfig {
    blob_grace_seconds: u64,
    gc_interval_seconds: u64,
}
impl Section for StorageConfig {
    const NAME: &'static str = "storage";
}
fn validate_config(value: &Value) -> nd_config::Result<()> {
    serde_json::from_value::<DiagnosticsConfig>(value["diagnostics"].clone())
        .map_err(|e| nd_config::Error::Invalid(e.to_string()))?;
    let storage: StorageConfig = serde_json::from_value(value["storage"].clone())
        .map_err(|e| nd_config::Error::Invalid(e.to_string()))?;
    if storage.gc_interval_seconds == 0
        || storage.gc_interval_seconds > 86400
        || storage.blob_grace_seconds > 31536000
    {
        return Err(nd_config::Error::Invalid(
            "附件清理间隔须为 1..86400 秒，宽限期须不超过一年".into(),
        ));
    }
    Ok(())
}
struct Engine {
    blobs: Arc<nd_store::Blobs>,
    scope: nd_kernel::ScopeId,
    kernel: Kernel,
    diagnostics: nd_kernel::Dep<dyn NamespaceProvider>,
    started: tokio::time::Instant,
    history: VecDeque<Event>,
    snapshot: Snapshot,
    events: broadcast::Sender<Event>,
    config_error: Option<String>,
}
impl Engine {
    async fn open(config: &Config, blobs: Arc<nd_store::Blobs>) -> Result<Self> {
        let mut kernel = Kernel::new();
        let scope = kernel.scope();
        let diagnostics = Key::<dyn NamespaceProvider>::new(scope, "diagnostics");
        let key = diagnostics.clone();
        kernel.install(
            ComponentSpec::new("diagnostics", scope)
                .optional()
                .provides(key.erased()),
            move |mount| {
                mount.provide(
                    key.clone(),
                    Arc::new(Diagnostics) as Arc<dyn NamespaceProvider>,
                )?;
                mount.register(Registration::new(
                    scope,
                    RegistrationKind::Command,
                    "diagnostics.inspect",
                ))?;
                mount.register(Registration::new(
                    scope,
                    RegistrationKind::Subscription,
                    "diagnostics",
                ))?;
                Ok(Box::new(Diagnostics))
            },
        )?;
        let diagnostics = kernel.require(diagnostics);
        let (events, _) = broadcast::channel(128);
        let mut this = Self {
            blobs,
            scope,
            kernel,
            diagnostics,
            started: tokio::time::Instant::now(),
            events,
            history: VecDeque::new(),
            config_error: None,
            snapshot: Snapshot {
                stream: "global".into(),
                epoch: uuid::Uuid::new_v4().to_string(),
                cursor: 0,
                items: vec![],
            },
        };
        this.configure(config).await?;
        this.snapshot.items = this.items(config);
        Ok(this)
    }
    async fn configure(&mut self, config: &Config) -> Result<()> {
        let section = config.section::<DiagnosticsConfig>()?.get()?;
        self.kernel
            .configure(&[ConfigChange::new("diagnostics", section.value.enabled, 0)])?;
        self.kernel.reconcile(self.started.elapsed()).await;
        Ok(())
    }
    fn names(&self) -> BTreeMap<String, u32> {
        let mut names = BTreeMap::from([("system".into(), 1)]);
        if let Some(optional) = self.diagnostics.with(|p| p.names()) {
            names.extend(optional);
        }
        names
    }
    fn items(&self, config: &Config) -> Vec<Item> {
        let mut items = vec![Item {
            id: "system".into(),
            namespace: "system".into(),
            kind: "status".into(),
            data: json!({"ready":true,"namespaces":self.names(),"config":config.snapshot(),"config_error":self.config_error,"components":{"diagnostics":component_status(self.kernel.state("diagnostics"))}}),
            fallback: Fallback {
                title: "New Desktop".into(),
                text: self.config_error.clone().unwrap_or("守护进程已就绪".into()),
            },
        }];
        if let Some(optional) = self.diagnostics.with(|p| p.snapshot()) {
            items.extend(optional);
        }
        items
    }
    fn publish(&mut self, config: &Config) {
        let items = self.items(config);
        if items == self.snapshot.items {
            return;
        }
        let event = Event {
            stream: "global".into(),
            epoch: self.snapshot.epoch.clone(),
            cursor: self.snapshot.cursor + 1,
            upsert: items
                .iter()
                .filter(|item| !self.snapshot.items.contains(item))
                .cloned()
                .collect(),
            remove: self
                .snapshot
                .items
                .iter()
                .filter(|old| !items.iter().any(|i| i.id == old.id))
                .map(|i| i.id.clone())
                .collect(),
        };
        self.snapshot.items = items;
        self.snapshot.cursor = event.cursor;
        self.history.push_back(event.clone());
        if self.history.len() > 128 {
            self.history.pop_front();
        }
        let _ = self.events.send(event);
    }
    fn command(&self, name: &str) -> std::result::Result<Value, String> {
        self.diagnostics
            .with(|p| p.command(name))
            .unwrap_or(Err("not_found".into()))
    }
}

fn component_status(state: Option<nd_kernel::ComponentState>) -> Value {
    use nd_kernel::ComponentState;
    match state {
        Some(ComponentState::Running { .. }) => json!({"state":"running"}),
        Some(ComponentState::Disabled) => json!({"state":"disabled"}),
        Some(ComponentState::Waiting { missing, .. }) => {
            json!({"state":"waiting","missing":format!("{missing:?}")})
        }
        Some(ComponentState::Unavailable { missing, .. }) => {
            json!({"state":"unavailable","missing":format!("{missing:?}")})
        }
        Some(ComponentState::Failed { message }) => json!({"state":"failed","message":message}),
        Some(ComponentState::Stopping { .. }) => json!({"state":"stopping"}),
        None => json!({"state":"absent"}),
    }
}

fn page_items(
    items: Vec<Item>,
    request: &nd_wire::PageReq,
) -> std::result::Result<nd_wire::Page, String> {
    if request.before.is_some() || request.limit == 0 || request.limit > 1000 {
        return Err("invalid_page".into());
    }
    Ok(nd_wire::Page {
        items: items.into_iter().take(request.limit as usize).collect(),
        next: None,
    })
}

/// 独立实例的所有持久及运行时路径；不从后端环境继承凭据目录。
#[derive(Clone, Debug)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub runtime: PathBuf,
}
impl Paths {
    pub fn isolated(root: &Path) -> Self {
        Self {
            config: root.join("config.toml"),
            data: root.to_owned(),
            runtime: root.join("runtime"),
        }
    }
    pub fn from_env() -> Result<Self> {
        let home = PathBuf::from(std::env::var("HOME")?);
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or(home.join(".config"));
        let data = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or(home.join(".local/state"));
        Ok(Self {
            config: config.join("new-desktop/config.toml"),
            data: data.join("new-desktop"),
            runtime: PathBuf::from(std::env::var("XDG_RUNTIME_DIR")?).join("new-desktop"),
        })
    }
}
pub async fn run(root: &Path) -> Result<()> {
    run_at(Paths::isolated(root)).await
}
pub async fn run_at(paths: Paths) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&paths.data)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(paths.config.parent().ok_or("config path has no parent")?)?;
    let local = local::LocalSocket::prepare(&paths.runtime)?;
    let source = Arc::new(FileSource::new(paths.config.clone()));
    let (_watcher, mut changes) = source.watch()?;
    let config = Arc::new(Config::open(
        source,
        json!({"diagnostics":{"enabled":true},"storage":{"blob_grace_seconds":86400,"gc_interval_seconds":3600}}),
        validate_config,
    )?);
    let store = Arc::new(nd_store::Store::open(paths.data.join("state.sqlite"), 4)?);
    let blobs = Arc::new(nd_store::Blobs::open(paths.data.join("blobs"), store)?);
    let state = Arc::new(Mutex::new(Engine::open(&config, blobs.clone()).await?));
    let mut storage_config = config.section::<StorageConfig>()?;
    let collector = tokio::spawn(async move {
        while let Ok(current) = storage_config.get() {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(current.value.gc_interval_seconds)) => {
                    let blobs = blobs.clone();
                    let outcome = tokio::task::spawn_blocking(move || blobs.collect(Duration::from_secs(current.value.blob_grace_seconds))).await;
                    if !matches!(outcome, Ok(Ok(_))) { eprintln!("附件清理未完成: {outcome:?}"); }
                },
                changed = storage_config.changed() => { if changed.is_err() { break; } },
            }
        }
    });
    let listener = local.listen()?;
    let monitor_state = state.clone();
    let monitor = tokio::spawn(async move {
        loop {
            // 先记 revision 再 reconcile，避免漏掉挂载期间发生的变化。
            let (changed, deadline) = {
                let mut engine = monitor_state.lock().await;
                let revision = engine.kernel.revision();
                let now = engine.started.elapsed();
                engine.kernel.reconcile(now).await;
                engine.publish(&config);
                (
                    engine.kernel.changed_since(revision),
                    engine.kernel.next_deadline().map(|d| engine.started + d),
                )
            };
            let timeout = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                _ = changed => {},
                _ = timeout => {},
                change = changes.recv() => {
                    if change.is_none() { break; }
                    let result = config.refresh();
                    let mut engine = monitor_state.lock().await;
                    match result {
                        Ok(_) => {
                            engine.config_error = None;
                            if let Err(e) = engine.configure(&config).await { engine.config_error = Some(e.to_string()); }
                        },
                        Err(e) => engine.config_error = Some(e.to_string()),
                    }
                }
            }
        }
    });
    axum::serve(
        listener,
        Router::new()
            .route("/wire", get(upgrade))
            .route("/blobs/{id}", get(get_blob).put(put_blob))
            .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024))
            .with_state(state.clone()),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    monitor.abort();
    collector.abort();
    let mut engine = state.lock().await;
    let scope = engine.scope;
    engine
        .kernel
        .stop(scope, nd_kernel::StopWhy::HandBack)
        .await;
    Ok(())
}
async fn upgrade(ws: WebSocketUpgrade, State(state): State<Arc<Mutex<Engine>>>) -> Response {
    ws.max_message_size(1024 * 1024)
        .on_upgrade(move |socket| serve(socket, state))
}
async fn send(socket: &mut WebSocket, response: WireResponse) -> bool {
    let text = serde_json::to_string(&response).expect("wire serialization");
    matches!(
        tokio::time::timeout(
            Duration::from_secs(5),
            socket.send(Message::Text(text.into()))
        )
        .await,
        Ok(Ok(()))
    )
}
async fn serve(mut socket: WebSocket, state: Arc<Mutex<Engine>>) {
    let mut greeted = false;
    let mut subscribed = false;
    let mut events = state.lock().await.events.subscribe();
    loop {
        tokio::select! {
            frame = socket.recv() => {
                let Some(Ok(frame)) = frame else { break; };
                let Message::Text(text) = frame else { if matches!(frame, Message::Close(_)) { break; } else { continue; } };
                let Ok(req) = serde_json::from_str::<Request>(&text) else { break; };
                let mut replay = vec![];
                let response = {
                    let engine = state.lock().await;
                    match req {
                        Request::Hello { version: PROTOCOL_VERSION, namespaces } => {
                            greeted = true;
                            let supported = engine.names();
                            let negotiated = if namespaces.is_empty() { supported } else {
                                supported.into_iter().filter_map(|(name, version)| {
                                    namespaces.get(&name).copied().filter(|v| *v > 0).map(|v| (name, version.min(v)))
                                }).collect()
                            };
                            WireResponse::Hello { version: PROTOCOL_VERSION, epoch: engine.snapshot.epoch.clone(), namespaces: negotiated }
                        },
                        Request::Subscribe { stream, since } if greeted && stream == "global" => {
                            events = engine.events.subscribe();
                            subscribed = true;
                            let earliest = engine.history.front().map_or(engine.snapshot.cursor, |event| event.cursor - 1);
                            if let Some(since) = since.filter(|c| c.epoch == engine.snapshot.epoch && c.seq >= earliest && c.seq <= engine.snapshot.cursor) {
                                replay = engine.history.iter().filter(|event| event.cursor > since.seq).cloned().collect();
                                WireResponse::Resumed { stream, epoch: engine.snapshot.epoch.clone(), cursor: engine.snapshot.cursor }
                            } else { WireResponse::Snapshot { snapshot: engine.snapshot.clone() } }
                        },
                        Request::Get { id, res, page } if greeted => {
                            let result = if res == "system" {
                                page_items(engine.snapshot.items.iter().filter(|i| i.namespace == "system").cloned().collect(), &page)
                            } else {
                                engine.diagnostics.with(|p| {
                                    if p.names().contains_key(&res) { p.page(&page) } else { Err("not_found".into()) }
                                }).unwrap_or(Err("not_found".into()))
                            };
                            match result {
                                Ok(page) => WireResponse::Reply { id, value: serde_json::to_value(page).unwrap(), error: None },
                                Err(error) => WireResponse::Reply { id, value: Value::Null, error: Some(error) },
                            }
                        },
                        Request::Command { id, name } if greeted => match engine.command(&name) {
                            Ok(value) => WireResponse::Reply { id, value, error: None },
                            Err(error) => WireResponse::Reply { id, value: Value::Null, error: Some(error) },
                        },
                        Request::Bye => break,
                        _ => WireResponse::Error { code: "unsupported".into(), message: "请求、流或版本不支持".into() },
                    }
                };
                for event in replay {
                    if !send(&mut socket, WireResponse::Event { event }).await { return; }
                }
                if !send(&mut socket, response).await { break; }
            },
            event = events.recv(), if subscribed => match event {
                Ok(event) => if !send(&mut socket, WireResponse::Event { event }).await { break; },
                Err(_) => { let _ = send(&mut socket, WireResponse::Bye { resume: true }).await; break; }
            }
        }
    }
}

async fn get_blob(
    State(state): State<Arc<Mutex<Engine>>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> std::result::Result<Vec<u8>, axum::http::StatusCode> {
    let blobs = state.lock().await.blobs.clone();
    tokio::task::spawn_blocking(move || blobs.get(&id))
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)
}
async fn put_blob(
    State(state): State<Arc<Mutex<Engine>>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    bytes: axum::body::Bytes,
) -> std::result::Result<String, axum::http::StatusCode> {
    use sha2::{Digest, Sha256};
    if format!("{:x}", Sha256::digest(&bytes)) != id {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let blobs = state.lock().await.blobs.clone();
    tokio::task::spawn_blocking(move || blobs.put(&bytes))
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)
}

async fn shutdown() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
}
