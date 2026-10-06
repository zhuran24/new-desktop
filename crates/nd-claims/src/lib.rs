//! Persistent backend-session leases. All decisions participate in the caller's transaction.
use nd_store::{Result, Store, Tx, params};
pub use nd_watchdog_proto::Identity;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
mod cli;
mod registry;
pub use cli::{CliCommands, PinnedCli};
pub use registry::{ExternalEntry, RegistryConfig};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendKind {
    Claude,
    Codex,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendSessionId {
    pub kind: BackendKind,
    pub id: String,
}
impl BackendSessionId {
    pub fn claude(id: impl Into<String>) -> Self {
        Self {
            kind: BackendKind::Claude,
            id: id.into(),
        }
    }
    pub fn codex(id: impl Into<String>) -> Self {
        Self {
            kind: BackendKind::Codex,
            id: id.into(),
        }
    }
    fn key(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NewBs {
    Known(BackendSessionId),
    Fresh(BackendKind),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Act {
    Open {
        session: String,
        bs: NewBs,
        via: String,
    },
    Write {
        session: String,
        bs: BackendSessionId,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Admit {
    Go(Pass),
    Wait(Obstacle),
    No(Refusal),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pass {
    pub route: Route,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Route {
    Fresh,
    Live(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Obstacle {
    ExternalWriter(ExternalEntry),
    ExternalUnverified(ExternalEntry),
    Recovering,
    Checking,
    HolderUnknown(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    RunAlreadyHolds(String),
    RunGone(String),
    HeldByOtherSession(String),
    HeldByOtherRun(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub bs: BackendSessionId,
    pub run: String,
    pub session: String,
    pub unknown: bool,
    pub confirmed: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Grant {
    act: Act,
    pass: Pass,
    active: bool,
    recheck: bool,
    bound: Option<BackendSessionId>,
    reserved: Option<BackendSessionId>,
}
#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    causes: BTreeMap<String, Act>,
    #[serde(default)]
    gone: BTreeSet<String>,
    #[serde(default)]
    scan_failed: bool,
    #[serde(default)]
    recovery: Recovery,
    #[serde(default)]
    external: Vec<ExternalEntry>,
    #[serde(default)]
    leaves: BTreeMap<String, String>,
    #[serde(default)]
    own: BTreeMap<String, OwnProcess>,
    leases: BTreeMap<String, Lease>,
    grants: BTreeMap<String, Grant>,
}

pub struct Exclusivity {
    store: Arc<Store>,
    config: RegistryConfig,
    changes: tokio::sync::watch::Sender<u64>,
    commands: Arc<dyn CliCommands>,
    interest: Arc<std::sync::atomic::AtomicUsize>,
    scan_lock: Arc<std::sync::Mutex<()>>,
    stop: Option<std::sync::mpsc::SyncSender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
}
fn error(e: impl std::fmt::Display) -> nd_store::Error {
    nd_store::Error::Aborted(e.to_string())
}
impl Exclusivity {
    pub fn open(store: Arc<Store>, config: RegistryConfig) -> Result<Self> {
        Self::open_with_commands(store, config, Arc::new(cli::Unconfigured))
    }
    pub fn open_with_commands(
        store: Arc<Store>,
        config: RegistryConfig,
        commands: Arc<dyn CliCommands>,
    ) -> Result<Self> {
        if config.rescan_interval.is_zero() || !config.root.is_absolute() {
            return Err(error(
                "registry requires an absolute root and a nonzero scan interval",
            ));
        }
        std::fs::create_dir_all(&config.root)?;
        store.write(|tx| {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS claims_state (id INTEGER PRIMARY KEY CHECK(id=1), body TEXT NOT NULL)")?;
            tx.execute("INSERT OR IGNORE INTO claims_state VALUES (1, ?1)", [serde_json::to_string(&State::default()).map_err(error)?])?;
            Ok(())
        })?;
        store.write(|tx| {
            let mut state = Self::in_tx(tx)?;
            state.recovery = Recovery::IdentitiesPending;
            state.external.clear();
            for own in state.own.values_mut() {
                own.unknown = true;
            }
            for lease in state.leases.values_mut() {
                lease.unknown = true;
            }
            Self::save(tx, &state)
        })?;
        let (changes, _) = tokio::sync::watch::channel(0);
        let interest = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scan_lock = Arc::new(std::sync::Mutex::new(()));
        let (stop, receiver) = std::sync::mpsc::sync_channel(1);
        let background = Self {
            changes: changes.clone(),
            commands: commands.clone(),
            interest: interest.clone(),
            store: store.clone(),
            config: config.clone(),
            scan_lock: scan_lock.clone(),
            stopped: stopped.clone(),
            stop: None,
            worker: None,
        };
        let notify = stop.clone();
        let worker = std::thread::Builder::new()
            .name("nd-claims-scan".into())
            .spawn(move || {
                use notify::Watcher;
                // Every event (including overflow/error) causes a complete scan. Periodic scans
                // cover missed watches, creation of directories, and /proc exits without file writes.
                let registry_paths = [
                    background.config.root.join("sessions"),
                    background.config.root.join("jobs"),
                ];
                let mut watcher =
                    notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                        if event.is_err()
                            || event.as_ref().is_ok_and(|e| {
                                e.need_rescan()
                                    || (!matches!(e.kind, notify::EventKind::Access(_))
                                        && e.paths.iter().any(|path| {
                                            registry_paths.iter().any(|root| path.starts_with(root))
                                        }))
                            })
                        {
                            let _ = notify.try_send(());
                        }
                    })
                    .ok();
                if let Some(watcher) = watcher.as_mut() {
                    let _ =
                        watcher.watch(&background.config.root, notify::RecursiveMode::NonRecursive);
                }
                let mut watched = BTreeSet::new();
                loop {
                    for name in ["sessions", "jobs"] {
                        let path = background.config.root.join(name);
                        if !path.is_dir() {
                            watched.remove(name);
                            continue;
                        }
                        if !watched.contains(name)
                            && let Some(watcher) = watcher.as_mut()
                            && watcher
                                .watch(&path, notify::RecursiveMode::Recursive)
                                .is_ok()
                        {
                            watched.insert(name);
                        }
                    }
                    if background
                        .stopped
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        break;
                    }
                    match receiver.recv_timeout(background.config.rescan_interval) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            if !background
                                .stopped
                                .load(std::sync::atomic::Ordering::Acquire)
                            {
                                let _ = background.refresh();
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self {
            changes,
            commands,
            interest,
            store,
            config,
            scan_lock,
            stopped,
            stop: Some(stop),
            worker: Some(worker),
        })
    }
    fn state(&self) -> Result<State> {
        let db = self.store.read()?;
        let body: String =
            db.query_row("SELECT body FROM claims_state WHERE id=1", [], |r| r.get(0))?;
        serde_json::from_str(&body).map_err(error)
    }
    fn in_tx(tx: &Tx<'_>) -> Result<State> {
        let body: String =
            tx.query_row("SELECT body FROM claims_state WHERE id=1", [], |r| r.get(0))?;
        serde_json::from_str(&body).map_err(error)
    }
    fn save(tx: &Tx<'_>, state: &State) -> Result<()> {
        tx.execute(
            "UPDATE claims_state SET body=?1 WHERE id=1",
            params![serde_json::to_string(state).map_err(error)?],
        )?;
        Ok(())
    }
    pub fn lease(&self, bs: &BackendSessionId) -> Result<Option<Lease>> {
        Ok(self.state()?.leases.get(&bs.key()).cloned())
    }
    fn decide(state: &State, act: &Act) -> Admit {
        if state.scan_failed {
            return Admit::Wait(Obstacle::Checking);
        }
        if state.recovery != Recovery::Ready {
            return Admit::Wait(Obstacle::Recovering);
        }
        if let Act::Open { via, bs, .. } = act {
            if state.gone.contains(via) {
                return Admit::No(Refusal::RunGone(via.clone()));
            }
            if state.own.get(via).is_some_and(|p| p.unknown) {
                return Admit::Wait(Obstacle::HolderUnknown(via.clone()));
            }
            let kind = match bs {
                NewBs::Known(bs) => &bs.kind,
                NewBs::Fresh(kind) => kind,
            };
            if *kind == BackendKind::Claude
                && state.leases.values().any(|lease| {
                    &lease.run == via && !matches!(bs, NewBs::Known(id) if id == &lease.bs)
                })
            {
                return Admit::No(Refusal::RunAlreadyHolds(via.clone()));
            }
        }
        let (session, bs, via) = match act {
            Act::Open {
                session,
                bs: NewBs::Known(bs),
                via,
            } => (session, Some(bs), Some(via)),
            Act::Open { .. } => {
                return Admit::Go(Pass {
                    route: Route::Fresh,
                });
            }
            Act::Write { session, bs } => (session, Some(bs), None),
        };
        if let Some(entry) = state.external.iter().find(|e| e.bs.as_ref() == bs) {
            return Admit::Wait(if entry.identity.is_some() {
                Obstacle::ExternalWriter(entry.clone())
            } else {
                Obstacle::ExternalUnverified(entry.clone())
            });
        }
        if let Some(lease) = bs.and_then(|bs| state.leases.get(&bs.key())) {
            if &lease.session != session && lease.bs.kind == BackendKind::Claude {
                return Admit::No(Refusal::HeldByOtherSession(lease.session.clone()));
            }
            if via.is_some_and(|via| via != &lease.run) {
                return Admit::No(Refusal::HeldByOtherRun(lease.run.clone()));
            }
            if lease.unknown {
                return Admit::Wait(Obstacle::HolderUnknown(lease.run.clone()));
            }
            if !lease.confirmed {
                return if via.is_some() {
                    Admit::Go(Pass {
                        route: Route::Fresh,
                    })
                } else {
                    Admit::Wait(Obstacle::Checking)
                };
            }
            return Admit::Go(Pass {
                route: Route::Live(lease.run.clone()),
            });
        }
        Admit::Go(Pass {
            route: Route::Fresh,
        })
    }
    pub fn peek(&self, act: &Act) -> Result<Admit> {
        Ok(Self::decide(&self.state()?, act))
    }
    pub fn admit(&self, tx: &mut Tx<'_>, cause: &str, act: &Act) -> Result<Admit> {
        let mut state = Self::in_tx(tx)?;
        if state.causes.get(cause).is_some_and(|prior| prior != act) {
            return Err(error("CauseReused"));
        }
        state.causes.insert(cause.into(), act.clone());
        if let Some(grant) = state.grants.get(cause) {
            if &grant.act != act {
                return Err(error("CauseReused"));
            }
            return Ok(if matches!(act, Act::Write { .. }) || grant.recheck {
                Self::decide(&state, act)
            } else {
                Admit::Go(grant.pass.clone())
            });
        }
        let decision = Self::decide(&state, act);
        if let Admit::Go(pass) = &decision {
            let mut reserved = None;
            if let Act::Open {
                session,
                bs: NewBs::Known(bs),
                via,
            } = act
            {
                if !state.leases.contains_key(&bs.key()) {
                    reserved = Some(bs.clone());
                }
                state.leases.entry(bs.key()).or_insert_with(|| Lease {
                    bs: bs.clone(),
                    run: via.clone(),
                    session: session.clone(),
                    unknown: false,
                    confirmed: false,
                });
            }
            state.grants.insert(
                cause.into(),
                Grant {
                    act: act.clone(),
                    pass: pass.clone(),
                    active: true,
                    recheck: false,
                    bound: None,
                    reserved,
                },
            );
        }
        self.commit(tx, &state)?;
        Ok(decision)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnProcess {
    identity: Identity,
    generation: u64,
    kind: BackendKind,
    unknown: bool,
    #[serde(default)]
    at: Option<u64>,
}
#[derive(Clone, Debug)]
pub enum GoneHow {
    Exited,
    ProcGone,
    NeverLaunched,
}
#[derive(Clone, Debug)]
pub enum Observed {
    NeverOpened {
        cause: String,
    },
    Recovered,
    Holding {
        run: String,
        generation: u64,
        at: u64,
        now: Vec<Held>,
    },
    Up {
        run: String,
        identity: Identity,
        generation: u64,
        kind: BackendKind,
    },
    Gone {
        run: String,
        identity: Option<Identity>,
        how: GoneHow,
    },
    IdentityMismatch {
        run: String,
    },
}
impl Exclusivity {
    pub fn observe(&self, observation: Observed) -> Result<()> {
        self.store.write(|tx| self.observe_in(tx, observation))
    }
    /// Evidence is supplied by the identity-checked watchdog/adapter, not by command receipts.
    pub fn observe_in(&self, tx: &mut Tx<'_>, observation: Observed) -> Result<()> {
        let mut state = Self::in_tx(tx)?;
        match observation {
            Observed::NeverOpened { cause } => {
                let Some(grant) = state.grants.get(&cause) else {
                    return Err(error("UnknownReservation"));
                };
                let Act::Open { via, .. } = &grant.act else {
                    return Err(error("NotAnOpen"));
                };
                if grant.bound.is_some() {
                    return Err(error("AlreadyObservedHolding"));
                }
                if let Some(bs) = &grant.reserved
                    && let Some(lease) = state.leases.get(&bs.key())
                {
                    if lease.confirmed || &lease.run != via {
                        return Err(error("AlreadyObservedHolding"));
                    }
                    state.leases.remove(&bs.key());
                }
                state.grants.get_mut(&cause).unwrap().active = false;
            }
            Observed::Recovered => state.recovery = Recovery::IdentitiesKnown,
            Observed::Holding {
                run,
                generation,
                at,
                now,
            } => {
                let Some(own) = state.own.get(&run) else {
                    return Ok(());
                };
                if own.unknown || own.generation != generation || own.at.is_some_and(|n| at <= n) {
                    return Ok(());
                }
                if now.iter().any(|h| h.bs.kind != own.kind)
                    || (own.kind == BackendKind::Claude && now.len() > 1)
                {
                    return Err(error("InvalidHolding"));
                }
                for held in &now {
                    if let Some(lease) = state.leases.get(&held.bs.key())
                        && lease.run != run
                    {
                        let other = lease.run.clone();
                        Self::mark_unknown(&mut state, &run);
                        Self::mark_unknown(&mut state, &other);
                        return self.commit(tx, &state);
                    }
                }
                if own.kind == BackendKind::Claude
                    && !now.is_empty()
                    && state
                        .leases
                        .values()
                        .any(|l| l.run == run && !l.confirmed && !now.iter().any(|h| h.bs == l.bs))
                {
                    Self::mark_unknown(&mut state, &run);
                    return self.commit(tx, &state);
                }
                state.leases.retain(|_, l| {
                    l.run != run || !l.confirmed || now.iter().any(|h| h.bs == l.bs)
                });
                for held in now {
                    if let Some(leaf) = held.last_leaf {
                        state.leaves.insert(held.bs.key(), leaf);
                    }
                    state.leases.insert(
                        held.bs.key(),
                        Lease {
                            bs: held.bs,
                            run: run.clone(),
                            session: held.session,
                            unknown: false,
                            confirmed: true,
                        },
                    );
                }
                state.own.get_mut(&run).unwrap().at = Some(at);
            }
            Observed::Up {
                run,
                identity,
                generation,
                kind,
            } => {
                if state.gone.contains(&run) {
                    return Ok(());
                }
                if let Some(old) = state.own.get(&run) {
                    if generation < old.generation {
                        return Ok(());
                    }
                    if old.identity != identity {
                        Self::mark_unknown(&mut state, &run);
                        return self.commit(tx, &state);
                    }
                }
                state.own.insert(
                    run.clone(),
                    OwnProcess {
                        identity,
                        generation,
                        kind,
                        unknown: false,
                        at: state.own.get(&run).and_then(|p| p.at),
                    },
                );
                for lease in state.leases.values_mut().filter(|l| l.run == run) {
                    lease.unknown = false;
                }
            }
            Observed::IdentityMismatch { run } => Self::mark_unknown(&mut state, &run),
            Observed::Gone { run, identity, how } => {
                let matches = match state.own.get(&run) {
                    Some(own) => {
                        identity.as_ref() == Some(&own.identity)
                            && !matches!(how, GoneHow::NeverLaunched)
                    }
                    None => identity.is_none() && matches!(how, GoneHow::NeverLaunched),
                };
                if matches {
                    state.leases.retain(|_, l| l.run != run);
                    state.own.remove(&run);
                    state.gone.insert(run.clone());
                    for grant in state.grants.values_mut() {
                        if matches!(&grant.act, Act::Open { via, .. } if via == &run) {
                            grant.active = false;
                        }
                    }
                } else {
                    Self::mark_unknown(&mut state, &run);
                }
            }
        }
        self.commit(tx, &state)
    }
    fn mark_unknown(state: &mut State, run: &str) {
        if let Some(own) = state.own.get_mut(run) {
            own.unknown = true;
        }
        for lease in state.leases.values_mut().filter(|l| l.run == run) {
            lease.unknown = true;
        }
    }
}

#[derive(Clone, Debug)]
pub struct Held {
    pub bs: BackendSessionId,
    pub session: String,
    pub last_leaf: Option<String>,
}
impl Exclusivity {
    pub fn owned_leaf(&self, bs: &BackendSessionId) -> Result<Option<String>> {
        Ok(self.state()?.leaves.get(&bs.key()).cloned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindResult {
    Bound,
    Conflict { held_by: String },
}
impl Exclusivity {
    /// Conflict is a value so the caller can commit the suspension of both holders.
    pub fn bind(&self, tx: &mut Tx<'_>, cause: &str, bs: &BackendSessionId) -> Result<BindResult> {
        let mut state = Self::in_tx(tx)?;
        let Some(grant) = state.grants.get(cause) else {
            return Err(error("BindWithoutReservation"));
        };
        if !grant.active {
            return Err(error("ReservationEnded"));
        }
        let Act::Open {
            session,
            via,
            bs: reserved,
        } = &grant.act
        else {
            return Err(error("BindWithoutReservation"));
        };
        let (session, via) = (session.clone(), via.clone());
        let kind = match reserved {
            NewBs::Known(id) => &id.kind,
            NewBs::Fresh(kind) => kind,
        };
        if kind != &bs.kind {
            return Err(error("BindBackendMismatch"));
        }
        if matches!(reserved, NewBs::Known(expected) if expected != bs) {
            return Err(error("BindIdMismatch"));
        }
        if bs.kind == BackendKind::Claude
            && state.leases.values().any(|l| l.run == via && &l.bs != bs)
        {
            Self::mark_unknown(&mut state, &via);
            self.commit(tx, &state)?;
            return Ok(BindResult::Conflict { held_by: via });
        }
        if let Some(old) = state.leases.get(&bs.key())
            && old.run != via
        {
            let held_by = old.run.clone();
            Self::mark_unknown(&mut state, &held_by);
            Self::mark_unknown(&mut state, &via);
            self.commit(tx, &state)?;
            return Ok(BindResult::Conflict { held_by });
        }
        if grant.bound.as_ref().is_some_and(|old| old != bs) {
            return Err(error("ReservationAlreadyBound"));
        }
        state.grants.get_mut(cause).unwrap().bound = Some(bs.clone());
        let unknown = state.own.get(&via).is_some_and(|p| p.unknown);
        let lease = state.leases.entry(bs.key()).or_insert_with(|| Lease {
            bs: bs.clone(),
            run: via,
            session,
            unknown,
            confirmed: false,
        });
        lease.confirmed = true;
        self.commit(tx, &state)?;
        Ok(BindResult::Bound)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Recovery {
    #[default]
    IdentitiesPending,
    IdentitiesKnown,
    Ready,
}
impl Exclusivity {
    pub fn recovery(&self) -> Result<Recovery> {
        Ok(self.state()?.recovery)
    }
    /// I/O runs outside the caller transaction. A partial registry never completes recovery.
    pub fn refresh(&self) -> Result<()> {
        let _scan = self.scan_lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.recovery()? == Recovery::IdentitiesPending {
            return Ok(());
        }
        let scanned = registry::scan(&self.config).and_then(|mut entries| {
            if self.interest.load(std::sync::atomic::Ordering::Acquire) > 0 {
                let bytes = self.commands.agents().map_err(error)?;
                registry::merge_agents(&mut entries, &bytes)?;
            }
            Ok(entries)
        });
        let entries = match scanned {
            Ok(entries) => entries,
            Err(e) => {
                self.store.write(|tx| {
                    let mut s = Self::in_tx(tx)?;
                    s.scan_failed = true;
                    self.commit(tx, &s)
                })?;
                return Err(e);
            }
        };
        self.store.write(|tx| {
            let mut state = Self::in_tx(tx)?;
            state.scan_failed = false;
            state.external = entries
                .into_iter()
                .filter(|entry| {
                    !entry
                        .identity
                        .as_ref()
                        .is_some_and(|id| state.own.values().any(|own| &own.identity == id))
                })
                .collect();
            state.recovery = Recovery::Ready;
            self.commit(tx, &state)
        })
    }
    pub fn externals(&self) -> Result<Vec<ExternalEntry>> {
        let state = self.state()?;
        Ok(state
            .external
            .into_iter()
            .filter(|e| {
                !e.bs
                    .as_ref()
                    .is_some_and(|bs| state.leases.contains_key(&bs.key()))
            })
            .collect())
    }
}

impl Drop for Exclusivity {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.stopped
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(stop) = self.stop.take() {
                let _ = stop.try_send(());
            }
            let _ = worker.join();
        }
    }
}

pub struct ListInterest {
    count: Arc<std::sync::atomic::AtomicUsize>,
}
impl Drop for ListInterest {
    fn drop(&mut self) {
        self.count.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
impl Exclusivity {
    pub fn want_list(&self) -> ListInterest {
        self.interest
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        ListInterest {
            count: self.interest.clone(),
        }
    }
}

impl Exclusivity {
    /// Only for a ticket the adapter proved Withheld (never written). Ordinary replay uses admit.
    /// Waiting does not release responsibility: NeverOpened/Holding/Gone must supply that evidence.
    pub fn readmit(&self, tx: &mut Tx<'_>, cause: &str, act: &Act) -> Result<Admit> {
        let mut state = Self::in_tx(tx)?;
        if let Some(grant) = state.grants.get(cause) {
            if &grant.act != act {
                return Err(error("CauseReused"));
            }
            let current = Self::decide(&state, act);
            if !matches!(current, Admit::Go(_)) {
                state.grants.get_mut(cause).unwrap().recheck = true;
                self.commit(tx, &state)?;
                return Ok(current);
            }
        }
        self.admit(tx, cause, act)
    }
}

/// A read-only comparison, not a resume grant. #49 integrates it with the resume transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeafCheck {
    pub owned: Option<String>,
    pub current: Option<String>,
}
impl Exclusivity {
    /// Read the full record outside any Store transaction. Never promote a file tail to an own leaf.
    pub fn check_record(&self, bs: &BackendSessionId, path: &std::path::Path) -> Result<LeafCheck> {
        if bs.kind != BackendKind::Claude {
            return Err(error("ClaudeRecordRequired"));
        }
        let bytes = std::fs::read(path)?;
        let index = nd_claude_records::RecordIndex::parse(&bytes).map_err(error)?;
        let history = index.current().map_err(error)?;
        Ok(LeafCheck {
            owned: self.owned_leaf(bs)?,
            current: history.leaf().map(str::to_owned),
        })
    }
}

impl Exclusivity {
    /// Consume #6's already cgroup-checked observation. /proc is read before entering SQLite.
    /// An unknown reason, missing identity, or still-live process never releases a lease.
    pub fn observe_watchdog(
        &self,
        found: &nd_runs::Found,
        generation: u64,
        kind: BackendKind,
    ) -> Result<()> {
        let run = found.run.clone();
        let observation = match (
            found.state.as_str(),
            found.identity.as_ref(),
            found.reason.as_deref(),
        ) {
            ("Up", Some(identity), _) if identity.matching() == Some(true) => Observed::Up {
                run,
                identity: identity.clone(),
                generation,
                kind,
            },
            ("Gone", Some(identity), Some(reason @ ("ProcGone" | "Exited")))
                if identity.matching() == Some(false) =>
            {
                Observed::Gone {
                    run,
                    identity: Some(identity.clone()),
                    how: if reason == "Exited" {
                        GoneHow::Exited
                    } else {
                        GoneHow::ProcGone
                    },
                }
            }
            ("Gone", None, Some("NeverLaunched")) => Observed::Gone {
                run,
                identity: None,
                how: GoneHow::NeverLaunched,
            },
            _ => Observed::IdentityMismatch { run },
        };
        self.observe(observation)
    }
}

impl Exclusivity {
    /// Invalidation stream for the registry/session owner. After a change, re-read peek/lease/externals.
    /// Persisted state is authoritative; coalesced notifications never carry responsibilities.
    pub fn watch(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }
    fn commit(&self, tx: &mut Tx<'_>, state: &State) -> Result<()> {
        let body = serde_json::to_string(state).map_err(error)?;
        let changed = tx.execute(
            "UPDATE claims_state SET body=?1 WHERE id=1 AND body<>?1",
            [&body],
        )?;
        if changed > 0 {
            let changes = self.changes.clone();
            tx.on_commit(move || {
                changes.send_modify(|revision| *revision += 1);
            });
        }
        Ok(())
    }
}
