//! 窄接缝一的夹具：真的会话组件、独占登记、名册和 SQLite；只有后端换成脚本化适配器。
#![allow(dead_code)]
use nd_backend::{Backends, SessionId};
use nd_claims::{Exclusivity, Observed, RegistryConfig};
use nd_session::{EngineConfig, Sessions, scripted::ScriptedAdapter};
use nd_store::Store;
use nd_wire::{Command, CommandReply, Item, Receipt, Response, Snapshot};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

pub struct Harness {
    pub dir: Arc<tempfile::TempDir>,
    pub store: Arc<Store>,
    pub claims: Arc<Exclusivity>,
    pub adapter: Arc<ScriptedAdapter>,
    pub sessions: Arc<Sessions>,
    pub config: EngineConfig,
}

pub fn config() -> EngineConfig {
    EngineConfig {
        tick: Duration::from_millis(20),
        check_purity: true,
        ..EngineConfig::default()
    }
}

fn open_claims(dir: &tempfile::TempDir, store: &Arc<Store>) -> Arc<Exclusivity> {
    let mut registry = RegistryConfig::new(dir.path().join("claude"));
    registry.rescan_interval = Duration::from_millis(200);
    Arc::new(Exclusivity::open(store.clone(), registry).unwrap())
}

/// 恢复的两层：身份已知、第一次扫描完成。之后放行不再回 Recovering。
fn ready(claims: &Exclusivity) {
    claims.observe(Observed::Recovered).unwrap();
    claims.refresh().unwrap();
}

impl Harness {
    pub async fn new(config: EngineConfig) -> Self {
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let store = Arc::new(Store::open(dir.path().join("state.sqlite"), 4).unwrap());
        let claims = open_claims(&dir, &store);
        ready(&claims);
        let adapter = ScriptedAdapter::new(claims.clone());
        let blobs =
            Arc::new(nd_store::Blobs::open(dir.path().join("blobs"), store.clone()).unwrap());
        let sessions = Sessions::new(
            store.clone(),
            blobs,
            claims.clone(),
            Backends::new().with(adapter.clone()),
            config.clone(),
        )
        .unwrap();
        Self {
            dir,
            store,
            claims,
            adapter,
            sessions,
            config,
        }
    }

    pub async fn create(&self, id: &str, text: &str) -> CommandReply {
        self.sessions
            .execute(&command(
                id,
                "session.create",
                json!({"cwd":"/proj","text":text,"model":"claude-haiku-4-5"}),
            ))
            .await
            .unwrap()
    }

    pub async fn send(&self, id: &str, session: &SessionId, text: &str) -> CommandReply {
        self.sessions
            .execute(&command(
                id,
                "session.send",
                json!({"session": session, "text": text}),
            ))
            .await
            .unwrap()
    }

    /// 只看一眼：取一次快照就放手，不算「有人在看」。
    pub fn snapshot(&self, session: &SessionId) -> Snapshot {
        let subscription = self.sessions.subscribe(session, None).unwrap().unwrap();
        match subscription.first {
            Response::Snapshot { snapshot } => snapshot,
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    pub async fn wait(
        &self,
        session: &SessionId,
        what: &str,
        until: impl Fn(&Snapshot) -> bool,
    ) -> Snapshot {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = self.snapshot(session);
            if until(&snapshot) {
                return snapshot;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("timed out waiting for {what}: {:#?}", snapshot.items);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

pub fn command(id: &str, name: &str, args: Value) -> Command {
    Command {
        id: id.into(),
        device: "test".into(),
        name: name.into(),
        args,
        expect: json!({}),
    }
}

/// 起操作的命令回 Accepted，带能看到进度的流 `session/<id>`。
pub fn accepted_session(reply: &CommandReply) -> SessionId {
    match reply {
        CommandReply::Receipt {
            receipt:
                Receipt::Accepted {
                    stream: Some(stream),
                    ..
                },
        } => SessionId(stream.strip_prefix("session/").unwrap().to_owned()),
        other => panic!("expected Accepted with a stream, got {other:?}"),
    }
}

pub fn header(snapshot: &Snapshot) -> &Value {
    &item(snapshot, "header").data
}
pub fn item<'a>(snapshot: &'a Snapshot, id: &str) -> &'a Item {
    snapshot
        .items
        .iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("no item {id}: {:#?}", snapshot.items))
}
pub fn prompts(snapshot: &Snapshot) -> Vec<&Item> {
    snapshot
        .items
        .iter()
        .filter(|i| i.kind == "prompt")
        .collect()
}
pub fn prompt<'a>(snapshot: &'a Snapshot, text: &str) -> Option<&'a Item> {
    prompts(snapshot)
        .into_iter()
        .find(|i| i.data["text"] == text)
}

impl Harness {
    /// 模拟守护进程重启：会话组件与独占登记换新实例，后端（脚本化适配器的状态）和 SQLite 不变。
    /// 先报活着的自有进程身份，再完成两层恢复，然后名册装载需要恢复的会话交端口对账。
    pub async fn restart(self, config: EngineConfig) -> Self {
        let Harness {
            dir,
            store,
            claims,
            adapter,
            sessions,
            ..
        } = self;
        drop(sessions);
        let live = adapter.live();
        let identity = adapter.identity();
        let survivor = adapter.reattach_detached();
        drop(adapter);
        drop(claims);
        // 等旧实例真正放手（扫描线程、会话执行器线程退出）。
        tokio::time::sleep(Duration::from_millis(50)).await;
        let claims = open_claims(&dir, &store);
        for run in live.values() {
            claims
                .observe(Observed::Up {
                    run: run.0.clone(),
                    identity: identity.clone(),
                    generation: 2,
                    kind: nd_claims::BackendKind::Claude,
                })
                .unwrap();
        }
        ready(&claims);
        let adapter = survivor.attach(claims.clone());
        let blobs =
            Arc::new(nd_store::Blobs::open(dir.path().join("blobs"), store.clone()).unwrap());
        let sessions = Sessions::new(
            store.clone(),
            blobs,
            claims.clone(),
            Backends::new().with(adapter.clone()),
            config.clone(),
        )
        .unwrap();
        sessions.recover().unwrap();
        Self {
            dir,
            store,
            claims,
            adapter,
            sessions,
            config,
        }
    }
}
