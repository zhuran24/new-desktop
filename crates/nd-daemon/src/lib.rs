//! New Desktop 守护进程及 nd-wire 入口。
pub mod commands;
#[cfg(feature = "scenarios")]
mod faults;
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
use futures::{SinkExt, StreamExt};
use nd_config::{Config, FileSource, Section};
use nd_kernel::{ConfigChange, Kernel};
use nd_wire::{Fallback, Item, PROTOCOL_VERSION, ReceiptLookup, Request, Response as WireResponse};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, broadcast};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

mod namespaces;
pub use namespaces::{
    Execution as NamespaceExecution, NamespaceProvider, Pending as NamespaceFuture,
    StreamError as NamespaceStreamError, Subscription as NamespaceSubscription,
};
use namespaces::{Providers, RunsNamespace, SessionNamespace, SystemNamespace};
mod diagnostics;
use diagnostics::Diagnostics;
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
    fn check(&self) -> nd_config::Result<()> {
        if self.gc_interval_seconds == 0
            || self.gc_interval_seconds > 86400
            || self.blob_grace_seconds > 31536000
        {
            return Err(nd_config::Error::Invalid(
                "附件清理间隔须为 1..86400 秒，宽限期须不超过一年".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Deserialize)]
struct CommandsConfig {
    receipt_keep_ms: u64,
}
impl Section for CommandsConfig {
    const NAME: &'static str = "commands";
    fn check(&self) -> nd_config::Result<()> {
        if self.receipt_keep_ms == 0 || self.receipt_keep_ms > 31536000000 {
            return Err(nd_config::Error::Invalid("收据保留期须为 1ms..1年".into()));
        }
        Ok(())
    }
}
/// Claude 后端：钉住的 CLI、两个 mod、CLI 的配置目录（独占登记扫描它的注册表）。没配就没有 Claude 后端。
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct ClaudeSection {
    cli: PathBuf,
    hook_mod: PathBuf,
    action_mod: PathBuf,
    config_dir: PathBuf,
    #[serde(default)]
    socket: Option<PathBuf>,
    /// 后端进程的基础环境取守护进程继承的环境；测试关掉它，只用 `env`。
    #[serde(default = "yes")]
    inherit_env: bool,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default = "hello_ms")]
    hello_timeout_ms: u64,
    #[serde(default = "poll_ms")]
    poll_timeout_ms: u64,
    #[serde(default = "init_ms")]
    init_timeout_ms: u64,
    #[serde(default)]
    record: bool,
}
fn yes() -> bool {
    true
}
fn hello_ms() -> u64 {
    10_000
}
fn poll_ms() -> u64 {
    25_000
}
fn init_ms() -> u64 {
    30_000
}
#[derive(Clone, Deserialize)]
struct SessionsConfig {
    #[serde(default = "enabled_by_default")]
    auto_title: bool,
    idle_reclaim_ms: u64,
    tick_ms: u64,
}
fn enabled_by_default() -> bool {
    true
}
impl Section for SessionsConfig {
    const NAME: &'static str = "sessions";
    fn check(&self) -> nd_config::Result<()> {
        if self.idle_reclaim_ms == 0 || !(1..=60_000).contains(&self.tick_ms) {
            return Err(nd_config::Error::Invalid(
                "闲置回收时限须大于 0，检查间隔须为 1..60000ms".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Deserialize)]
struct WireConfig {
    send_queue: usize,
    send_timeout_ms: u64,
}
impl Section for WireConfig {
    const NAME: &'static str = "wire";
    fn check(&self) -> nd_config::Result<()> {
        if !(1..=4096).contains(&self.send_queue) || !(1..=60000).contains(&self.send_timeout_ms) {
            return Err(nd_config::Error::Invalid(
                "发送队列须为1..4096，超时须为1..60000ms".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Deserialize)]
#[serde(transparent)]
struct WatchdogsSection(Option<nd_runs::Config>);
impl Section for WatchdogsSection {
    const NAME: &'static str = "watchdogs";
}
#[derive(Clone, Deserialize)]
#[serde(transparent)]
struct OptionalClaude(Option<ClaudeSection>);
impl Section for OptionalClaude {
    const NAME: &'static str = "claude";
}

fn validate_config(value: &Value) -> nd_config::Result<()> {
    WatchdogsSection::parse(value)?;
    OptionalClaude::parse(value)?;
    SessionsConfig::parse(value)?;
    DiagnosticsConfig::parse(value)?;
    StorageConfig::parse(value)?;
    WireConfig::parse(value)?;
    CommandsConfig::parse(value)?;
    Ok(())
}

struct Engine {
    runs_config: Option<nd_runs::Config>,
    blobs: Arc<nd_store::Blobs>,
    config: Arc<Config>,
    scope: nd_kernel::ScopeId,
    kernel: Kernel,
    providers: Providers,
    global: Arc<SystemNamespace>,
    started: tokio::time::Instant,
    config_error: Option<String>,
}
impl Engine {
    async fn open(
        config: Arc<Config>,
        blobs: Arc<nd_store::Blobs>,
        store: Arc<nd_store::Store>,
        sessions: Arc<nd_session::Sessions>,
        runs: Option<Arc<nd_runs::Watchdogs>>,
        _root: &Path,
    ) -> Result<Self> {
        store.write(|tx| {
            commands::migrate(tx)?;
            commands::expire(tx)?;
            diagnostics::migrate(tx)?;
            Ok(())
        })?;
        let mut kernel = Kernel::new();
        let scope = kernel.scope();
        let global = Arc::new(SystemNamespace::new(
            store.clone(),
            config.clone(),
            _root.into(),
        ));
        let mut providers = Providers::default();
        providers.install(&mut kernel, scope, "system", false, global.clone())?;
        providers.install(
            &mut kernel,
            scope,
            "sessions",
            false,
            Arc::new(SessionNamespace(sessions)),
        )?;
        if let Some(runs) = runs {
            providers.install(
                &mut kernel,
                scope,
                "runs",
                false,
                Arc::new(RunsNamespace(runs)),
            )?;
        }
        providers.install(
            &mut kernel,
            scope,
            "diagnostics",
            true,
            Arc::new(Diagnostics {
                store,
                config: config.clone(),
                fault_root: _root.into(),
            }),
        )?;
        let mut this = Self {
            runs_config: WatchdogsSection::parse(&config.snapshot().value)?.0,
            blobs,
            config: config.clone(),
            scope,
            kernel,
            providers,
            global,
            started: tokio::time::Instant::now(),
            config_error: None,
        };
        this.configure(&config).await?;
        this.global.initialize(this.items(&config));
        Ok(this)
    }
    async fn configure(&mut self, config: &Config) -> Result<()> {
        let next: Option<nd_runs::Config> = WatchdogsSection::parse(&config.snapshot().value)?.0;
        if next != self.runs_config {
            return Err("看守配置已变化，须重启守护进程才能生效".into());
        }
        let section = config.section::<DiagnosticsConfig>()?.get()?;
        self.kernel
            .configure(&[ConfigChange::new("diagnostics", section.value.enabled, 0)])?;
        self.kernel.reconcile(self.started.elapsed()).await;
        Ok(())
    }
    fn names(&self) -> BTreeMap<String, u32> {
        self.providers.names()
    }
    fn items(&self, config: &Config) -> Vec<Item> {
        self.global.system(Item {
            id: "system".into(),
            namespace: "system".into(),
            kind: "status".into(),
            data: json!({"ready":true,"namespaces":self.names(),"config":config.snapshot(),"config_error":self.config_error,"components":{"diagnostics":component_status(self.kernel.state("diagnostics"))}}),
            fallback: Fallback {
                title: "New Desktop".into(),
                text: self.config_error.clone().unwrap_or("守护进程已就绪".into()),
            },
        });
        self.providers.snapshot()
    }

    fn publish(&self, config: &Config) {
        self.global.publish(self.items(config));
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
    if request.before.is_some()
        || request.after.is_some()
        || request.around.is_some()
        || request.limit == 0
        || request.limit > 1000
    {
        return Err("invalid_page".into());
    }
    Ok(nd_wire::Page {
        items: items.into_iter().take(request.limit as usize).collect(),
        next: None,
        newer: None,
        anchor: None,
        at: None,
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
/// 组装会话组件，次序照恢复的依赖：独占登记（恢复中）→ 看守托管报身份 → 适配器与名册装载会话、
/// 对账 → 独占登记身份已知、第一次扫描完成 → 放行。会话的命令在放行之前起操作会等着。
struct SessionAssembly {
    sessions: Arc<nd_session::Sessions>,
    runs: Option<Arc<nd_runs::Watchdogs>>,
}
async fn assemble_sessions(
    config: &Config,
    store: &Arc<nd_store::Store>,
    blobs: &Arc<nd_store::Blobs>,
    paths: &Paths,
) -> Result<SessionAssembly> {
    let value = config.snapshot().value;
    let watchdogs_config: Option<nd_runs::Config> = WatchdogsSection::parse(&value)?.0;
    let claude: Option<ClaudeSection> = OptionalClaude::parse(&value)?.0;
    let sessions_config: SessionsConfig = SessionsConfig::parse(&value)?;
    let keep = CommandsConfig::parse(&value)?.receipt_keep_ms;
    let registry_root = claude
        .as_ref()
        .map(|c| c.config_dir.clone())
        .unwrap_or_else(|| paths.data.join("no-claude-registry"));
    let claims = {
        let store = store.clone();
        Arc::new(
            tokio::task::spawn_blocking(move || {
                nd_claims::Exclusivity::open(store, nd_claims::RegistryConfig::new(registry_root))
            })
            .await??,
        )
    };
    let generation = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut backends = nd_backend::Backends::new();
    let runs = watchdogs_config
        .map(nd_runs::Watchdogs::new)
        .transpose()?
        .map(Arc::new);
    if let Some(watchdogs) = &runs {
        // 看守托管先报每个还在的后端进程的身份。
        for found in watchdogs.recover().await? {
            let claims = claims.clone();
            tokio::task::spawn_blocking(move || {
                claims.observe(found.observation(generation, nd_claims::BackendKind::Claude))
            })
            .await??;
        }
        if let Some(claude) = claude {
            let mut cfg = nd_claude::ClaudeConfig::new(
                claude.cli,
                claude.hook_mod,
                claude.action_mod,
                claude
                    .socket
                    .unwrap_or_else(|| paths.runtime.join("mod.sock")),
            );
            cfg.env = if claude.inherit_env {
                std::env::vars().collect()
            } else {
                BTreeMap::new()
            };
            cfg.env.extend(claude.env);
            cfg.hello_timeout = Duration::from_millis(claude.hello_timeout_ms);
            cfg.poll_timeout = Duration::from_millis(claude.poll_timeout_ms);
            cfg.init_timeout = Duration::from_millis(claude.init_timeout_ms);
            cfg.record = claude.record;
            let record = claude.record;
            let adapter = nd_claude::ClaudeBackend::new(
                nd_claude::Claude::new(cfg, watchdogs.clone())?,
                watchdogs.clone(),
                claims.clone(),
                generation,
                blobs.clone(),
                nd_claude::ClaudeBackendConfig {
                    record_dir: record.then(|| paths.data.join("recordings")),
                    ..Default::default()
                },
            );
            backends = backends.with(adapter);
        }
    }
    let sessions = nd_session::Sessions::new(
        store.clone(),
        blobs.clone(),
        claims.clone(),
        backends,
        nd_session::EngineConfig {
            auto_title: sessions_config.auto_title,
            receipt_keep_ms: keep,
            idle_reclaim: Duration::from_millis(sessions_config.idle_reclaim_ms),
            tick: Duration::from_millis(sessions_config.tick_ms),
            check_purity: cfg!(feature = "scenarios"),
            faults: None,
        },
    )?;
    sessions.recover()?;
    // 监听可先提供快照与收据；新操作在来源追平、身份已知、首轮扫描完成之前回 unavailable。
    let recovering_sessions = sessions.clone();
    tokio::spawn(async move {
        while !recovering_sessions.adopted() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = tokio::task::spawn_blocking(move || -> nd_store::Result<()> {
            claims.observe(nd_claims::Observed::Recovered)?;
            // 扫描失败仍停在 Checking，独占登记自己的线程继续重扫。
            if let Err(e) = claims.refresh() {
                eprintln!("CLI 注册表第一次扫描没完成：{e}");
            }
            Ok(())
        })
        .await;
    });
    Ok(SessionAssembly { sessions, runs })
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
        json!({"watchdogs":null,"claude":null,"sessions":{"idle_reclaim_ms":900000,"tick_ms":1000},"diagnostics":{"enabled":true},"commands":{"receipt_keep_ms":604800000},"wire":{"send_queue":128,"send_timeout_ms":5000},"storage":{"blob_grace_seconds":86400,"gc_interval_seconds":3600}}),
        validate_config,
    )?);
    let store = Arc::new(nd_store::Store::open(paths.data.join("state.sqlite"), 4)?);
    let blobs = Arc::new(nd_store::Blobs::open(
        paths.data.join("blobs"),
        store.clone(),
    )?);
    let SessionAssembly { sessions, runs } =
        assemble_sessions(&config, &store, &blobs, &paths).await?;
    let state = Arc::new(Mutex::new(
        Engine::open(
            config.clone(),
            blobs.clone(),
            store.clone(),
            sessions.clone(),
            runs.clone(),
            &paths.data,
        )
        .await?,
    ));
    let mut collection_changes = sessions.listing().changes();
    let collection_sessions = sessions.clone();
    let run_collector = tokio::spawn(async move {
        let Some(runs) = runs else {
            return;
        };
        loop {
            let sessions = collection_sessions.clone();
            let runs = runs.clone();
            let result = tokio::task::spawn_blocking(move || -> Result<()> {
                runs.collect_unused(&sessions.referenced_runs()?)?;
                Ok(())
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                eprintln!("看守目录回收未完成：{result:?}");
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(30)) => {},
                changed = collection_changes.changed() => { if changed.is_err() { break; } },
            }
        }
    });
    let mut listing_changes = sessions.listing().changes();
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
    let receipts_gc = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let store = store.clone();
            let _ = tokio::task::spawn_blocking(move || store.write(commands::expire)).await;
        }
    });
    let listener = local.listen()?;
    let monitor_state = state.clone();
    let monitor = tokio::spawn(async move {
        loop {
            // 先记登记变化计数再 reconcile，避免漏掉挂载期间发生的变化。
            let (changed, deadline) = {
                let mut engine = monitor_state.lock().await;
                let revision = engine.kernel.change_count();
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
                _ = listing_changes.changed() => {},
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
    run_collector.abort();
    receipts_gc.abort();
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
fn enqueue(queue: &tokio::sync::mpsc::Sender<WireResponse>, response: WireResponse) -> bool {
    queue.try_send(response).is_ok()
}
async fn serve(socket: WebSocket, state: Arc<Mutex<Engine>>) {
    let config = state
        .lock()
        .await
        .config
        .section::<WireConfig>()
        .unwrap()
        .get()
        .unwrap()
        .value;
    let (mut sink, mut incoming) = socket.split();
    let (outgoing, mut queued) = tokio::sync::mpsc::channel::<WireResponse>(config.send_queue);
    let (close, mut closing) = tokio::sync::oneshot::channel::<()>();
    let mut writer = tokio::spawn(async move {
        loop {
            let response = tokio::select! {
                biased;
                _ = &mut closing => {
                    // 队列溢出时 Bye 尽力而为，不能等慢接收者释放资源。
                    let _ = tokio::time::timeout(Duration::from_millis(config.send_timeout_ms),
                        sink.send(Message::Text(serde_json::to_string(&WireResponse::Bye { resume: true }).unwrap().into()))).await;
                    break;
                },
                response = queued.recv() => match response { Some(response) => response, None => break },
            };
            let text = serde_json::to_string(&response).expect("wire serialization");
            if !matches!(
                tokio::time::timeout(
                    Duration::from_millis(config.send_timeout_ms),
                    sink.send(Message::Text(text.into()))
                )
                .await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
    });
    let mut greeted = false;
    let (overflow, mut overflowed) = tokio::sync::mpsc::channel::<()>(1);
    let mut forwards: BTreeMap<String, tokio::task::JoinHandle<()>> = BTreeMap::new();
    'connection: loop {
        tokio::select! {
            _ = &mut writer => {
                for forward in forwards.values() { forward.abort(); }
                return;
            },
            _ = overflowed.recv() => break,
            frame = incoming.next() => {
                let Some(Ok(frame)) = frame else { break; };
                let Message::Text(text) = frame else {
                    if matches!(frame, Message::Close(_)) { break; } else { continue; }
                };
                let Ok(request) = serde_json::from_str::<Request>(&text) else { break; };
                let response = match request {
                    Request::Hello { version: PROTOCOL_VERSION, namespaces } => {
                        greeted = true;
                        let engine = state.lock().await;
                        let supported = engine.names();
                        let negotiated = if namespaces.is_empty() { supported } else {
                            supported.into_iter().filter_map(|(name, version)| {
                                namespaces.get(&name).copied().filter(|v| *v > 0).map(|v| (name, version.min(v)))
                            }).collect()
                        };
                        WireResponse::Hello { version: PROTOCOL_VERSION, epoch: engine.global.epoch(), namespaces: negotiated }
                    }
                    Request::Subscribe { stream, since } if greeted => {
                        let subscription = state.lock().await.providers.with(&stream, |p| p.events(stream.clone(), since, config.send_queue))
                            .unwrap_or_else(|| Err(namespaces::StreamError { code: "unsupported", message: "没有这个事件命名空间".into() }));
                        match subscription {
                            Err(error) => WireResponse::Error { code: error.code.into(), message: error.message },
                            Ok(subscription) => {
                                if let Some(old) = forwards.remove(&stream) { old.abort(); }
                                let namespaces::Subscription { first, replay, mut events, guard } = subscription;
                                for event in replay {
                                    if !enqueue(&outgoing, WireResponse::Event { event }) { break 'connection; }
                                }
                                if !enqueue(&outgoing, first) { break; }
                                let (queue, overflow) = (outgoing.clone(), overflow.clone());
                                forwards.insert(stream, tokio::spawn(async move {
                                    let _watching = guard;
                                    loop {
                                        match events.recv().await {
                                            Ok(event) => if !enqueue(&queue, WireResponse::Event { event }) { let _ = overflow.try_send(()); break; },
                                            Err(broadcast::error::RecvError::Lagged(_)) => { let _ = overflow.try_send(()); break; },
                                            Err(broadcast::error::RecvError::Closed) => break,
                                        }
                                    }
                                }));
                                continue;
                            }
                        }
                    }
                    Request::Get { id, res, page } if greeted => {
                        let pending = state.lock().await.providers.with(&res, |p| p.page(res.clone(), page));
                        let result = match pending { Some(pending) => pending.await, None => Err("not_found".into()) };
                        page_reply(id, result)
                    }
                    Request::Models { id, backend, cwd } if greeted => {
                        let pending = state.lock().await.providers.with("models", |p| p.models(backend, cwd));
                        let result = match pending { Some(pending) => pending.await, None => Err("not_found".into()) };
                        page_reply(id, result)
                    }
                    Request::Command { id, name } if greeted => {
                        let result = state.lock().await.providers.with(&name, |p| p.command(&name)).unwrap_or(Err("not_found".into()));
                        page_reply(id, result)
                    }
                    Request::Execute { id, command } if greeted => {
                        let execution = {
                            let engine = state.lock().await;
                            engine.providers.with(&command.name, |p| p.execute(command.clone())).flatten()
                                .unwrap_or_else(|| engine.global.reject(command))
                        };
                        let detached = execution.detached;
                        let (pending_state, queue, overflow) = (state.clone(), outgoing.clone(), overflow.clone());
                        let finish = async move {
                            let result = execution.result.await;
                            let engine = pending_state.lock().await;
                            engine.publish(&engine.config.clone());
                            drop(engine);
                            if !enqueue(&queue, WireResponse::CommandReply { id, result }) { let _ = overflow.try_send(()); }
                        };
                        // 提供者决定收据时点；按到达次序受理的普通命令仍串行，Delivery 不阻挡控制。
                        if detached { tokio::spawn(finish); } else { finish.await; }
                        continue;
                    }
                    Request::Receipt { id, command_id, content_hash } if greeted => {
                        let result = state.lock().await.providers.with("system", |p| p.receipt(&command_id, content_hash.as_deref())).flatten()
                            .unwrap_or(ReceiptLookup::Unavailable { reason: "收据命名空间不可用".into() });
                        WireResponse::ReceiptReply { id, result }
                    }
                    Request::Bye => break,
                    _ => WireResponse::Error { code: "unsupported".into(), message: "请求、流或版本不支持".into() },
                };
                if !enqueue(&outgoing, response) { break; }
            },
        }
    }
    for (_, forward) in forwards {
        forward.abort();
    }
    let _ = close.send(());
    let _ = writer.await;
}

fn page_reply(id: u64, result: std::result::Result<Value, String>) -> WireResponse {
    match result {
        Ok(value) => WireResponse::Reply {
            id,
            value,
            error: None,
        },
        Err(error) => WireResponse::Reply {
            id,
            value: Value::Null,
            error: Some(error),
        },
    }
}

async fn get_blob(
    State(state): State<Arc<Mutex<Engine>>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> std::result::Result<Vec<u8>, axum::http::StatusCode> {
    let id: nd_wire::BlobId = id.parse().map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
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
    let id: nd_wire::BlobId = id
        .parse()
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    if nd_wire::BlobId::of(&bytes) != id {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let blobs = state.lock().await.blobs.clone();
    tokio::task::spawn_blocking(move || blobs.put(&bytes).map(|id| id.to_string()))
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)
}

async fn shutdown() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
}
