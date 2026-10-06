use crate::{
    Dep, Error, Guard, Kernel, Key, Registration, ScopeId, ServiceKey,
    registry::{self, Registry},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use std::{future::Future, pin::Pin};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation(pub(crate) u64);

#[derive(Clone, Debug)]
pub struct ComponentSpec {
    pub id: String,
    pub scope: ScopeId,
    pub(crate) requires: Vec<ServiceKey>,
    pub(crate) provides: Vec<ServiceKey>,
    pub(crate) timeout: Duration,
    optional: bool,
}

impl ComponentSpec {
    pub fn new(id: impl Into<String>, scope: ScopeId) -> Self {
        Self {
            id: id.into(),
            scope,
            requires: Vec::new(),
            provides: Vec::new(),
            timeout: Duration::from_secs(30),
            optional: false,
        }
    }
    pub fn requires(mut self, key: ServiceKey) -> Self {
        self.requires.push(key);
        self
    }
    pub fn provides(mut self, key: ServiceKey) -> Self {
        self.provides.push(key);
        self
    }
    pub fn wait_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

/// revision 只标记必须重建的配置；可原位更新的字段由提供者自己更新。
#[derive(Clone, Debug)]
pub struct ConfigChange {
    pub component: String,
    pub enabled: bool,
    pub revision: u64,
}

impl ConfigChange {
    pub fn new(component: impl Into<String>, enabled: bool, revision: u64) -> Self {
        Self {
            component: component.into(),
            enabled,
            revision,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComponentState {
    Waiting {
        missing: Vec<ServiceKey>,
        since: Duration,
    },
    Unavailable {
        missing: Vec<ServiceKey>,
        since: Duration,
    },
    Running {
        generation: Generation,
    },
    Failed {
        message: String,
    },
    Disabled,
    Stopping {
        generation: Generation,
        phase: StopPhase,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopWhy {
    HandBack,
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopPhase {
    Quiesce,
    Release,
}

pub type StopFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// 收尾 future 可以挂起。若驱动 future 被取消，再次驱动会重试未完成的阶段；
/// 实现必须在自身保存进度，保证该阶段可重入。Drop 仅撤销登记，不异步收尾。
pub trait Lifecycle: Send {
    fn quiesce(&mut self, _why: StopWhy) -> StopFuture<'_> {
        Box::pin(async {})
    }
    fn release(&mut self, _why: StopWhy) -> StopFuture<'_> {
        Box::pin(async {})
    }
}

/// 初始化期间创建的守卫由内核保管；返回错误时同步全部撤回。
pub struct Mount {
    registry: Arc<Mutex<Registry>>,
    guards: Vec<Arc<Guard>>,
    spec: ComponentSpec,
    bindings: BTreeMap<ServiceKey, u64>,
    generation: Generation,
    revision: u64,
}

impl Mount {
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn register(
        &mut self,
        registration: Registration,
    ) -> Result<crate::RegistrationToken, Error> {
        if registration.scope != self.spec.scope {
            return Err(Error::InvalidConfig(
                "registration outside component scope".into(),
            ));
        }
        let guard = Arc::new(registry::register(&self.registry, registration)?);
        let token = guard.token();
        self.guards.push(guard);
        Ok(token)
    }

    pub fn provide<T: ?Sized + Send + Sync + 'static>(
        &mut self,
        key: Key<T>,
        value: Arc<T>,
    ) -> Result<(), Error> {
        if !self.spec.provides.contains(&key.erased) {
            return Err(Error::UndeclaredService(key.erased));
        }
        self.guards
            .push(Arc::new(registry::provide(&self.registry, key, value)?));
        Ok(())
    }

    pub fn require<T: ?Sized + Send + Sync + 'static>(
        &mut self,
        key: Key<T>,
    ) -> Result<Dep<T>, Error> {
        let binding = *self
            .bindings
            .get(&key.erased)
            .ok_or_else(|| Error::UndeclaredService(key.erased.clone()))?;
        let mut dependency = registry::require(&self.registry, key);
        dependency.binding = Some(binding);
        self.guards.push(dependency.guard.clone());
        Ok(dependency)
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        for guard in self.guards.iter().rev() {
            guard.revoke();
        }
    }
}

type Factory = Box<dyn Fn(&mut Mount) -> Result<Box<dyn Lifecycle>, Error> + Send + Sync>;
pub(crate) struct Component {
    spec: ComponentSpec,
    factory: Factory,
    state: ComponentState,
    instance: Option<Instance>,
    enabled: bool,
    revision: u64,
    waiting_since: Option<Duration>,
    failed_attempt: Option<Attempt>,
}

#[derive(Clone, PartialEq, Eq)]
struct Attempt {
    revision: u64,
    bindings: BTreeMap<ServiceKey, u64>,
}

struct Instance {
    generation: Generation,
    lifecycle: Box<dyn Lifecycle>,
    mount: Mount,
}

pub(crate) struct StopPlan {
    order: Vec<(String, StopWhy)>,
    phase: StopPhase,
    cursor: usize,
    extra: Vec<u64>,
}

impl Kernel {
    pub fn install(
        &mut self,
        spec: ComponentSpec,
        factory: impl Fn(&mut Mount) -> Result<Box<dyn Lifecycle>, Error> + Send + Sync + 'static,
    ) -> Result<(), Error> {
        if self.components.contains_key(&spec.id) {
            return Err(Error::DuplicateComponent(spec.id));
        }
        validate_graph(
            self.components
                .values()
                .map(|c| &c.spec)
                .chain(std::iter::once(&spec)),
        )?;
        self.components.insert(
            spec.id.clone(),
            Component {
                spec,
                factory: Box::new(factory),
                state: ComponentState::Waiting {
                    missing: Vec::new(),
                    since: Duration::ZERO,
                },
                instance: None,
                enabled: true,
                revision: 0,
                waiting_since: None,
                failed_attempt: None,
            },
        );
        registry::changed(&self.registry);
        Ok(())
    }

    pub fn state(&self, id: &str) -> Option<ComponentState> {
        self.components.get(id).map(|c| c.state.clone())
    }

    /// 与 `reconcile(now)` 使用同一单调时钟；由调用方运行时设置一次性唤醒。
    pub fn next_deadline(&self) -> Option<Duration> {
        self.components
            .values()
            .filter_map(|c| {
                if matches!(c.state, ComponentState::Waiting { .. }) {
                    c.waiting_since
                        .map(|since| since.saturating_add(c.spec.timeout))
                } else {
                    None
                }
            })
            .min()
    }

    /// 整批校验成功才更新。这里只接收已经通过配置模块类型校验的修订号。
    pub fn configure(&mut self, changes: &[ConfigChange]) -> Result<(), Error> {
        let mut seen = BTreeSet::new();
        for change in changes {
            let component = self.components.get(&change.component).ok_or_else(|| {
                Error::InvalidConfig(format!("unknown component {}", change.component))
            })?;
            if !seen.insert(&change.component) {
                return Err(Error::InvalidConfig(format!(
                    "duplicate change {}",
                    change.component
                )));
            }
            if !change.enabled && !component.spec.optional {
                return Err(Error::InvalidConfig(format!(
                    "{} is resident",
                    change.component
                )));
            }
        }
        let mut changed = false;
        for change in changes {
            let c = self.components.get_mut(&change.component).unwrap();
            if c.enabled == change.enabled && c.revision == change.revision {
                continue;
            }
            changed = true;
            c.failed_attempt = None;
            c.enabled = change.enabled;
            c.revision = change.revision;
            if c.instance.is_none() {
                c.waiting_since = None;
                c.state = if c.enabled {
                    ComponentState::Waiting {
                        missing: Vec::new(),
                        since: Duration::ZERO,
                    }
                } else {
                    ComponentState::Disabled
                };
            }
        }
        if changed {
            registry::changed(&self.registry);
        }
        Ok(())
    }

    /// 异步结果回到唯一协调者后经此闸门提交；检查与闭包执行之间不让出。
    pub fn with_generation<R>(
        &mut self,
        id: &str,
        generation: Generation,
        apply: impl FnOnce() -> R,
    ) -> Option<R> {
        (self.state(id) == Some(ComponentState::Running { generation })).then(apply)
    }

    /// 由进程的唯一生命周期驱动者调用；时钟由调用方提供。
    pub async fn reconcile(&mut self, now: Duration) {
        self.drive_stop().await;
        // 先求换代的传递闭包，防止消费者暂时绑定仍未撤回的旧提供者。
        let mut affected = BTreeSet::new();
        loop {
            let before = affected.len();
            let invalid_keys: Vec<_> = affected
                .iter()
                .flat_map(|id| &self.components[id].spec.provides)
                .collect();
            for (id, component) in &self.components {
                if let Some(instance) = &component.instance {
                    let state = self.registry.lock().unwrap();
                    if !component.enabled
                        || component.revision != instance.mount.revision
                        || instance.mount.bindings.iter().any(|(key, bound)| {
                            state.provider_id(key) != Some(*bound) || invalid_keys.contains(&key)
                        })
                    {
                        affected.insert(id.clone());
                    }
                }
            }
            if affected.len() == before {
                break;
            }
        }
        self.begin_stop(
            affected
                .into_iter()
                .map(|id| (id, StopWhy::HandBack))
                .collect(),
            Vec::new(),
        );
        self.drive_stop().await;

        let mut attempted = BTreeSet::new();
        loop {
            let mut started = false;
            for (id, component) in &mut self.components {
                if component.instance.is_some() || !component.enabled {
                    continue;
                }
                let bindings: BTreeMap<_, _> = {
                    let registry = self.registry.lock().unwrap();
                    component
                        .spec
                        .requires
                        .iter()
                        .filter_map(|key| registry.provider_id(key).map(|id| (key.clone(), id)))
                        .collect()
                };
                let missing: Vec<_> = component
                    .spec
                    .requires
                    .iter()
                    .filter(|k| !bindings.contains_key(k))
                    .cloned()
                    .collect();
                if !missing.is_empty() {
                    let since = *component.waiting_since.get_or_insert(now);
                    component.state = if now.saturating_sub(since) >= component.spec.timeout {
                        ComponentState::Unavailable { missing, since }
                    } else {
                        ComponentState::Waiting { missing, since }
                    };
                    continue;
                }
                let attempt = Attempt {
                    revision: component.revision,
                    bindings: bindings.clone(),
                };
                if component.failed_attempt.as_ref() == Some(&attempt)
                    || !attempted.insert(id.clone())
                {
                    continue;
                }
                component.waiting_since = None;
                let generation = Generation(registry::next_id());
                let mut mount = Mount {
                    registry: self.registry.clone(),
                    guards: Vec::new(),
                    spec: component.spec.clone(),
                    bindings,
                    generation,
                    revision: component.revision,
                };
                let initialized = (component.factory)(&mut mount).and_then(|lifecycle| {
                    let registry = self.registry.lock().unwrap();
                    if mount
                        .bindings
                        .iter()
                        .any(|(key, id)| registry.provider_id(key) != Some(*id))
                    {
                        return Err(Error::Initialization(
                            "dependency changed during initialization".into(),
                        ));
                    }
                    for key in &component.spec.provides {
                        if !registry
                            .provider_id(key)
                            .is_some_and(|id| mount.guards.iter().any(|guard| guard.id == id))
                        {
                            return Err(Error::Initialization(format!(
                                "{} did not provide {}",
                                component.spec.id, key.type_name
                            )));
                        }
                    }
                    Ok(lifecycle)
                });
                match initialized {
                    Ok(lifecycle) => {
                        component.failed_attempt = None;
                        component.state = ComponentState::Running { generation };
                        component.instance = Some(Instance {
                            generation,
                            lifecycle,
                            mount,
                        });
                        started = true;
                    }
                    Err(error) => {
                        component.failed_attempt = Some(attempt);
                        component.state = ComponentState::Failed {
                            message: error.to_string(),
                        }
                    }
                }
            }
            if !started {
                break;
            }
        }
    }

    /// 停止此作用域并撤销登记。跨作用域消费者交还连接后等待，不被永久禁用。
    pub async fn stop(&mut self, scope: ScopeId, why: StopWhy) {
        self.drive_stop().await;
        let mut affected = BTreeMap::new();
        let mut keys: BTreeSet<_> = self
            .snapshot()
            .providers
            .into_iter()
            .filter(|key| key.scope == scope)
            .collect();
        for (id, c) in &mut self.components {
            if c.spec.scope == scope {
                c.enabled = false;
                if c.instance.is_some() {
                    affected.insert(id.clone(), why);
                } else {
                    c.state = ComponentState::Disabled;
                }
                keys.extend(c.spec.provides.iter().cloned());
            }
        }
        loop {
            let before = affected.len();
            for (id, c) in &self.components {
                if c.instance.is_some() && c.spec.requires.iter().any(|key| keys.contains(key)) {
                    affected.entry(id.clone()).or_insert(StopWhy::HandBack);
                    keys.extend(c.spec.provides.iter().cloned());
                }
            }
            if before == affected.len() {
                break;
            }
        }
        let extra = self
            .registry
            .lock()
            .unwrap()
            .entries
            .iter()
            .filter_map(|(id, entry)| (entry.scope() == scope).then_some(*id))
            .collect();
        self.begin_stop(affected, extra);
        self.drive_stop().await;
    }

    fn begin_stop(&mut self, affected: BTreeMap<String, StopWhy>, extra: Vec<u64>) {
        if affected.is_empty() && extra.is_empty() {
            return;
        }
        let mut order = Vec::new();
        let mut visited = BTreeSet::new();
        fn visit(
            id: &str,
            components: &BTreeMap<String, Component>,
            affected: &BTreeMap<String, StopWhy>,
            visited: &mut BTreeSet<String>,
            order: &mut Vec<(String, StopWhy)>,
        ) {
            if !visited.insert(id.to_owned()) {
                return;
            }
            for (provider, c) in components {
                if affected.contains_key(provider)
                    && components[id]
                        .spec
                        .requires
                        .iter()
                        .any(|key| c.spec.provides.contains(key))
                {
                    visit(provider, components, affected, visited, order);
                }
            }
            order.push((id.to_owned(), affected[id]));
        }
        for id in affected.keys() {
            visit(id, &self.components, &affected, &mut visited, &mut order);
        }
        order.reverse();
        let mut registry = self.registry.lock().unwrap();
        registry.hidden.extend(extra.iter().copied());
        for (id, _) in &order {
            let component = self.components.get_mut(id).unwrap();
            let instance = component.instance.as_ref().unwrap();
            component.state = ComponentState::Stopping {
                generation: instance.generation,
                phase: StopPhase::Quiesce,
            };
            // 依赖守卫仍能在第一段调用所绑定的旧提供者；新消费者看不到它。
            for guard in &instance.mount.guards {
                if !matches!(
                    registry.entries.get(&guard.id),
                    Some(registry::Entry::Dependency(_))
                ) {
                    registry.hidden.insert(guard.id);
                }
            }
        }
        self.pending_stop = Some(StopPlan {
            order,
            phase: StopPhase::Quiesce,
            cursor: 0,
            extra,
        });
        drop(registry);
        registry::changed(&self.registry);
    }

    async fn drive_stop(&mut self) {
        while let Some(plan) = &self.pending_stop {
            if plan.cursor == plan.order.len() {
                if plan.phase == StopPhase::Quiesce {
                    let plan = self.pending_stop.as_mut().unwrap();
                    plan.phase = StopPhase::Release;
                    plan.cursor = 0;
                    continue;
                }
                let plan = self.pending_stop.take().unwrap();
                let removed: Vec<_> = {
                    let mut registry = self.registry.lock().unwrap();
                    plan.extra
                        .into_iter()
                        .filter_map(|id| {
                            registry.hidden.remove(&id);
                            registry.entries.remove(&id)
                        })
                        .collect()
                };
                if !removed.is_empty() {
                    registry::changed(&self.registry);
                }
                drop(removed);
                break;
            }
            let (id, why) = plan.order[plan.cursor].clone();
            let phase = plan.phase;
            let c = self.components.get_mut(&id).unwrap();
            let instance = c.instance.as_mut().unwrap();
            c.state = ComponentState::Stopping {
                generation: instance.generation,
                phase,
            };
            match phase {
                StopPhase::Quiesce => instance.lifecycle.quiesce(why).await,
                StopPhase::Release => {
                    instance.lifecycle.release(why).await;
                    c.instance.take();
                    c.waiting_since = None;
                    c.state = if c.enabled {
                        ComponentState::Waiting {
                            missing: Vec::new(),
                            since: Duration::ZERO,
                        }
                    } else {
                        ComponentState::Disabled
                    };
                }
            }
            self.pending_stop.as_mut().unwrap().cursor += 1;
        }
    }
}

fn validate_graph<'a>(specs: impl Iterator<Item = &'a ComponentSpec>) -> Result<(), Error> {
    let specs: BTreeMap<_, _> = specs.map(|spec| (spec.id.as_str(), spec)).collect();
    let mut providers = BTreeMap::new();
    for spec in specs.values() {
        for key in &spec.provides {
            if key.scope != spec.scope {
                return Err(Error::InvalidConfig(format!(
                    "{} provides outside its scope",
                    spec.id
                )));
            }
            if providers.insert(key, spec.id.as_str()).is_some() {
                return Err(Error::DuplicateProvider(key.clone()));
            }
        }
    }
    fn visit<'a>(
        id: &'a str,
        specs: &BTreeMap<&'a str, &'a ComponentSpec>,
        providers: &BTreeMap<&ServiceKey, &'a str>,
        visiting: &mut BTreeSet<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Result<(), Error> {
        if done.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(Error::InvalidConfig(format!("dependency cycle at {id}")));
        }
        for key in &specs[id].requires {
            if let Some(provider) = providers.get(key) {
                visit(provider, specs, providers, visiting, done)?;
            }
        }
        visiting.remove(id);
        done.insert(id);
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut done = BTreeSet::new();
    for id in specs.keys() {
        visit(id, &specs, &providers, &mut visiting, &mut done)?;
    }
    Ok(())
}
