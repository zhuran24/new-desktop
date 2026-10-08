//! New Desktop 的运行时无关组件内核。
//!
//! ```
//! use std::{sync::Arc, time::Duration};
//! use nd_kernel::{ComponentSpec, ConfigChange, Kernel, Key, Lifecycle, StopWhy};
//!
//! struct Active;
//! impl Lifecycle for Active {}
//!
//! # futures::executor::block_on(async {
//! let mut kernel = Kernel::new();
//! let scope = kernel.scope();
//! let key = Key::<str>::new(scope, "greeting");
//! let provided = key.clone();
//! kernel.install(
//!     ComponentSpec::new("greeting", scope).optional().provides(key.erased()),
//!     move |mount| {
//!         mount.provide(provided.clone(), Arc::<str>::from("你好"))?;
//!         Ok(Box::new(Active))
//!     },
//! ).unwrap();
//! kernel.reconcile(Duration::ZERO).await;
//! let greeting = kernel.require(key);
//! assert_eq!(greeting.with(str::to_owned), Some("你好".into()));
//! kernel.configure(&[ConfigChange::new("greeting", false, 0)]).unwrap();
//! kernel.reconcile(Duration::from_secs(1)).await;
//! assert_eq!(greeting.with(str::to_owned), None);
//! kernel.stop(scope, StopWhy::HandBack).await;
//! # });
//! ```

mod component;
mod registry;
pub use component::{ConfigChange, ConfigRevision};

/// 登记表变化计数；只用于等待登记变化，与配置修订号互不兼容。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegistryEpoch(u64);
pub use component::{
    ComponentSpec, ComponentState, Generation, Lifecycle, Mount, StopFuture, StopPhase, StopWhy,
};
pub use registry::Changed;
pub use registry::RegistrationToken;
pub use registry::{
    Dep, Guard, Key, Registration, RegistrationKind, ScopeId, ServiceKey, Snapshot,
};

use std::sync::{Arc, Mutex};

/// 提供者、登记与组件生命周期的唯一协调入口。
#[derive(Default)]
pub struct Kernel {
    registry: Arc<Mutex<registry::Registry>>,
    components: std::collections::BTreeMap<String, component::Component>,
    pending_stop: Option<component::StopPlan>,
}

impl Kernel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scope(&self) -> ScopeId {
        ScopeId(registry::next_id())
    }

    pub fn provide<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: Key<T>,
        value: Arc<T>,
    ) -> Result<Guard, Error> {
        registry::provide(&self.registry, key, value)
    }

    pub fn require<T: ?Sized + Send + Sync + 'static>(&self, key: Key<T>) -> Dep<T> {
        registry::require(&self.registry, key)
    }

    pub fn register(&self, registration: Registration) -> Result<Guard, Error> {
        registry::register(&self.registry, registration)
    }

    /// 准入后已开始的调用可以完成；撤销之后到达的旧 token 被拒绝。
    pub fn with_registration<R>(
        &mut self,
        token: RegistrationToken,
        call: impl FnOnce() -> R,
    ) -> Option<R> {
        let admitted = {
            let registry = self.registry.lock().unwrap();
            matches!(
                registry.entries.get(&token.0),
                Some(registry::Entry::Registration(_))
            ) && !registry.hidden.contains(&token.0)
        };
        admitted.then(call)
    }

    pub fn snapshot(&self) -> Snapshot {
        self.registry.lock().unwrap().snapshot()
    }

    pub fn change_count(&self) -> RegistryEpoch {
        self.registry.lock().unwrap().epoch
    }

    pub fn changed_since(&self, epoch: RegistryEpoch) -> Changed {
        Changed::new(self.registry.clone(), epoch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    DuplicateProvider(ServiceKey),
    DuplicateRegistration(Registration),
    DuplicateComponent(String),
    Initialization(String),
    UndeclaredService(ServiceKey),
    InvalidConfig(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}
