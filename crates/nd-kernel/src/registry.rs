use std::{
    any::{Any, TypeId, type_name},
    collections::{BTreeMap, BTreeSet},
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};

use crate::Error;

pub(crate) fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut id = NEXT.load(Ordering::Relaxed);
    loop {
        let next = id.checked_add(1).expect("kernel identity space exhausted");
        match NEXT.compare_exchange_weak(id, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return id,
            Err(actual) => id = actual,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegistrationToken(pub(crate) u64);

/// 提供者身份严格按类型、作用域、分区匹配，不向父作用域回退。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceKey {
    pub scope: ScopeId,
    pub partition: String,
    pub type_name: &'static str,
    type_id: TypeId,
}

pub struct Key<T: ?Sized> {
    pub(crate) erased: ServiceKey,
    marker: PhantomData<fn() -> Arc<T>>,
}

impl<T: ?Sized + 'static> Key<T> {
    pub fn new(scope: ScopeId, partition: impl Into<String>) -> Self {
        Self {
            erased: ServiceKey {
                scope,
                partition: partition.into(),
                type_name: type_name::<T>(),
                type_id: TypeId::of::<T>(),
            },
            marker: PhantomData,
        }
    }

    pub fn erased(&self) -> ServiceKey {
        self.erased.clone()
    }
}

impl<T: ?Sized> Clone for Key<T> {
    fn clone(&self) -> Self {
        Self {
            erased: self.erased.clone(),
            marker: PhantomData,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegistrationKind {
    Command,
    Subscription,
    Slot,
    Timer,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Registration {
    pub scope: ScopeId,
    pub kind: RegistrationKind,
    pub name: String,
}

impl Registration {
    pub fn new(scope: ScopeId, kind: RegistrationKind, name: impl Into<String>) -> Self {
        Self {
            scope,
            kind,
            name: name.into(),
        }
    }
}

/// 当前可见登记；顺序固定，可以直接用于协议诊断和 UI 帧内槽位对账。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub providers: Vec<ServiceKey>,
    pub dependencies: Vec<ServiceKey>,
    pub registrations: Vec<Registration>,
}

pub(crate) enum Entry {
    Provider(ServiceKey, Arc<dyn Any + Send + Sync>),
    Dependency(ServiceKey),
    Registration(Registration),
}

impl Entry {
    pub fn scope(&self) -> ScopeId {
        match self {
            Self::Provider(key, _) | Self::Dependency(key) => key.scope,
            Self::Registration(r) => r.scope,
        }
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    pub entries: BTreeMap<u64, Entry>,
    pub hidden: BTreeSet<u64>,
    pub revision: u64,
    waiters: BTreeMap<u64, Waker>,
}

pub(crate) fn changed(registry: &Arc<Mutex<Registry>>) {
    let waiters = {
        let mut state = registry.lock().unwrap();
        state.revision = state
            .revision
            .checked_add(1)
            .expect("registry revision exhausted");
        std::mem::take(&mut state.waiters)
    };
    for waker in waiters.into_values() {
        waker.wake();
    }
}

/// 可取消的变化通知，不绑定任何 async runtime，不借用 Kernel。
pub struct Changed {
    registry: Arc<Mutex<Registry>>,
    after: u64,
    id: u64,
}

impl Changed {
    pub(crate) fn new(registry: Arc<Mutex<Registry>>, after: u64) -> Self {
        Self {
            registry,
            after,
            id: next_id(),
        }
    }
}

impl Future for Changed {
    type Output = u64;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u64> {
        let mut state = self.registry.lock().unwrap();
        if state.revision != self.after {
            state.waiters.remove(&self.id);
            Poll::Ready(state.revision)
        } else {
            state.waiters.insert(self.id, cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Drop for Changed {
    fn drop(&mut self) {
        self.registry.lock().unwrap().waiters.remove(&self.id);
    }
}

impl Registry {
    pub fn provider_id(&self, key: &ServiceKey) -> Option<u64> {
        self.entries.iter().find_map(|(id, entry)| {
            (matches!(entry, Entry::Provider(k, _) if k == key) && !self.hidden.contains(id))
                .then_some(*id)
        })
    }
    pub fn snapshot(&self) -> Snapshot {
        let mut result = Snapshot::default();
        for (id, entry) in &self.entries {
            if self.hidden.contains(id) {
                continue;
            }
            match entry {
                Entry::Provider(key, _) => result.providers.push(key.clone()),
                Entry::Dependency(key) => result.dependencies.push(key.clone()),
                Entry::Registration(r) => result.registrations.push(r.clone()),
            }
        }
        result.providers.sort();
        result.dependencies.sort();
        result.registrations.sort();
        result
    }
}

/// Drop 同步撤销登记；不操作外部进程、租约或已经发出的工作。
#[must_use = "丢弃守卫会立即撤销登记"]
pub struct Guard {
    pub(crate) id: u64,
    registry: Weak<Mutex<Registry>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.revoke();
    }
}

impl Guard {
    pub fn token(&self) -> RegistrationToken {
        RegistrationToken(self.id)
    }

    pub(crate) fn revoke(&self) {
        if let Some(registry) = self.registry.upgrade() {
            let removed = {
                let mut state = registry.lock().unwrap();
                state.hidden.remove(&self.id);
                state.entries.remove(&self.id)
            };
            if removed.is_some() {
                changed(&registry);
            }
            // 提供者自己的 Drop 也不能在登记表锁内运行。
            drop(removed);
        }
    }
}

pub(crate) fn insert(registry: &Arc<Mutex<Registry>>, entry: Entry) -> Guard {
    let id = next_id();
    registry.lock().unwrap().entries.insert(id, entry);
    changed(registry);
    Guard {
        id,
        registry: Arc::downgrade(registry),
    }
}

pub(crate) fn provide<T: ?Sized + Send + Sync + 'static>(
    registry: &Arc<Mutex<Registry>>,
    key: Key<T>,
    value: Arc<T>,
) -> Result<Guard, Error> {
    let mut state = registry.lock().unwrap();
    if state
        .entries
        .values()
        .any(|entry| matches!(entry, Entry::Provider(k, _) if *k == key.erased))
    {
        return Err(Error::DuplicateProvider(key.erased));
    }
    let id = next_id();
    state
        .entries
        .insert(id, Entry::Provider(key.erased, Arc::new(value)));
    drop(state);
    changed(registry);
    Ok(Guard {
        id,
        registry: Arc::downgrade(registry),
    })
}

pub(crate) fn register(
    registry: &Arc<Mutex<Registry>>,
    registration: Registration,
) -> Result<Guard, Error> {
    let mut state = registry.lock().unwrap();
    if state
        .entries
        .values()
        .any(|entry| matches!(entry, Entry::Registration(r) if *r == registration))
    {
        return Err(Error::DuplicateRegistration(registration));
    }
    let id = next_id();
    state.entries.insert(id, Entry::Registration(registration));
    drop(state);
    changed(registry);
    Ok(Guard {
        id,
        registry: Arc::downgrade(registry),
    })
}

/// 依赖的守卫。缺失时 `with` 返回 None；已经准入的调用不因撤销而中断。
#[must_use = "依赖守卫应随组件实例保存"]
pub struct Dep<T: ?Sized> {
    key: Key<T>,
    pub(crate) guard: Arc<Guard>,
    pub(crate) binding: Option<u64>,
}

impl<T: ?Sized + Send + Sync + 'static> Dep<T> {
    pub fn with<R>(&self, call: impl FnOnce(&T) -> R) -> Option<R> {
        let registry = self.guard.registry.upgrade()?;
        let value = {
            let state = registry.lock().unwrap();
            if !state.entries.contains_key(&self.guard.id) {
                return None;
            }
            state.entries.iter().find_map(|(id, entry)| match entry {
                Entry::Provider(key, value)
                    if *key == self.key.erased
                        && self
                            .binding
                            .map_or(!state.hidden.contains(id), |expected| *id == expected) =>
                {
                    Some(value.downcast_ref::<Arc<T>>().unwrap().clone())
                }
                _ => None,
            })?
        };
        Some(call(&value))
    }
}

pub(crate) fn require<T: ?Sized + Send + Sync + 'static>(
    registry: &Arc<Mutex<Registry>>,
    key: Key<T>,
) -> Dep<T> {
    let guard = insert(registry, Entry::Dependency(key.erased.clone()));
    Dep {
        key,
        guard: Arc::new(guard),
        binding: None,
    }
}
