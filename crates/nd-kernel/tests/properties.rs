use futures::executor::block_on;
use nd_kernel::{
    ComponentSpec, ComponentState, ConfigChange, Error, Kernel, Key, Lifecycle, Registration,
    RegistrationKind, StopFuture, StopWhy,
};
use proptest::prelude::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Running;
impl Lifecycle for Running {}

const KINDS: [RegistrationKind; 4] = [
    RegistrationKind::Command,
    RegistrationKind::Subscription,
    RegistrationKind::Slot,
    RegistrationKind::Timer,
];

proptest! {
    // 装卸序列包括重复开关、换代与每个初始化提交位置的错误。
    #[test]
    fn arbitrary_mounts_restore_all_registries(
        attempts in prop::collection::vec((0usize..7, 0u8..16, any::<u64>()), 1..30),
    ) {
        let mut kernel = Kernel::new();
        let scope = kernel.scope();
        let source = Key::<str>::new(scope, "source");
        let _provider = kernel.provide(source.clone(), Arc::<str>::from("stable")).unwrap();
        let _command = kernel.register(Registration::new(scope, RegistrationKind::Command, "existing")).unwrap();
        let baseline = kernel.snapshot();
        for (index, (fail_at, mask, revision)) in attempts.into_iter().enumerate() {
            let id = format!("component-{index}");
            let output = Key::<usize>::new(scope, id.clone());
            let source = source.clone();
            let name = id.clone();
            kernel.install(ComponentSpec::new(&id, scope).optional().requires(source.erased()).provides(output.erased()), move |mount| {
                let mut step = 0;
                let mut checkpoint = || {
                    let fail = step == fail_at;
                    step += 1;
                    if fail { Err(Error::Initialization("injected at public registration boundary".into())) } else { Ok(()) }
                };
                checkpoint()?;
                let dep = mount.require(source.clone())?;
                assert_eq!(dep.with(str::to_owned), Some("stable".into()));
                checkpoint()?;
                mount.provide(output.clone(), Arc::new(index))?;
                for (bit, kind) in KINDS.into_iter().enumerate() {
                    checkpoint()?;
                    if mask & (1 << bit) != 0 { mount.register(Registration::new(scope, kind, &name))?; }
                }
                Ok(Box::new(Running))
            }).unwrap();
            kernel.configure(&[ConfigChange::new(&id, true, revision)]).unwrap();
            block_on(kernel.reconcile(Duration::from_secs(index as u64)));
            if fail_at < 6 {
                prop_assert!(matches!(kernel.state(&id), Some(ComponentState::Failed { .. })), "initialization must fail");
                prop_assert_eq!(kernel.snapshot(), baseline.clone());
            } else {
                prop_assert!(matches!(kernel.state(&id), Some(ComponentState::Running { .. })), "initialization must succeed");
            }
            kernel.configure(&[ConfigChange::new(&id, false, revision)]).unwrap();
            block_on(kernel.reconcile(Duration::from_secs(index as u64)));
            prop_assert_eq!(kernel.snapshot(), baseline.clone());
        }
    }

    #[test]
    fn only_descendants_rebuild_and_old_generations_never_commit(
        count in 2usize..12,
        changed_seed in any::<usize>(),
        reverse_install in any::<bool>(),
    ) {
        // 二叉依赖树；更改一个节点，其子树重建，兄弟子树必须保留。
        let mut kernel = Kernel::new();
        let scope = kernel.scope();
        let changed = changed_seed % count;
        let mut order: Vec<_> = (0..count).collect();
        if reverse_install { order.reverse(); }
        for i in order {
            let key = Key::<usize>::new(scope, i.to_string());
            let mut spec = ComponentSpec::new(i.to_string(), scope).provides(key.erased());
            if i > 0 { spec = spec.requires(Key::<usize>::new(scope, ((i - 1) / 2).to_string()).erased()); }
            kernel.install(spec, move |mount| {
                mount.provide(key.clone(), Arc::new(i))?;
                Ok(Box::new(Running))
            }).unwrap();
        }
        block_on(kernel.reconcile(Duration::ZERO));
        let before: Vec<_> = (0..count).map(|i| kernel.state(&i.to_string()).unwrap()).collect();
        kernel.configure(&[ConfigChange::new(changed.to_string(), true, 1)]).unwrap();
        block_on(kernel.reconcile(Duration::from_secs(1)));
        for (i, old) in before.into_iter().enumerate() {
            let mut ancestor = i;
            while ancestor > changed { ancestor = (ancestor - 1) / 2; }
            let affected = ancestor == changed;
            let now = kernel.state(&i.to_string()).unwrap();
            prop_assert_eq!(now != old, affected);
            let ComponentState::Running { generation } = old else { panic!("dependency ordering failed") };
            let mut committed = false;
            kernel.with_generation(&i.to_string(), generation, || committed = true);
            prop_assert_eq!(committed, !affected);
        }
        block_on(kernel.stop(scope, StopWhy::HandBack));
        prop_assert_eq!(kernel.snapshot(), Default::default());
    }

    #[test]
    fn in_place_values_do_not_change_provider_or_consumer_generation(values in prop::collection::vec(any::<usize>(), 1..40)) {
        let mut kernel = Kernel::new();
        let scope = kernel.scope();
        let key = Key::<AtomicUsize>::new(scope, "config");
        let value = Arc::new(AtomicUsize::new(0));
        let _provider = kernel.provide(key.clone(), value.clone()).unwrap();
        kernel.install(ComponentSpec::new("consumer", scope).requires(key.erased()), |_| Ok(Box::new(Running))).unwrap();
        block_on(kernel.reconcile(Duration::ZERO));
        let original = kernel.state("consumer");
        let dep = kernel.require(key);
        for updated in values {
            value.store(updated, Ordering::SeqCst);
            block_on(kernel.reconcile(Duration::ZERO));
            prop_assert_eq!(kernel.state("consumer"), original.clone());
            prop_assert_eq!(dep.with(|v| v.load(Ordering::SeqCst)), Some(updated));
        }
    }

    #[test]
    fn both_stop_phases_follow_reverse_dependencies(count in 1usize..16, finish in any::<bool>()) {
        let mut kernel = Kernel::new();
        let scope = kernel.scope();
        let log = Arc::new(Mutex::new(Vec::new()));
        for i in (0..count).rev() {
            let key = Key::<usize>::new(scope, i.to_string());
            let mut spec = ComponentSpec::new(i.to_string(), scope).provides(key.erased());
            if i > 0 { spec = spec.requires(Key::<usize>::new(scope, (i - 1).to_string()).erased()); }
            let log = log.clone();
            kernel.install(spec, move |mount| {
                mount.provide(key.clone(), Arc::new(i))?;
                Ok(Box::new(Ordered { id: i, log: log.clone() }))
            }).unwrap();
        }
        block_on(kernel.reconcile(Duration::ZERO));
        let why = if finish { StopWhy::Finish } else { StopWhy::HandBack };
        block_on(kernel.stop(scope, why));
        let events = log.lock().unwrap();
        prop_assert_eq!(events.len(), count * 2);
        for i in 0..count {
            prop_assert_eq!(events[i], ("quiesce", count - 1 - i, why));
            prop_assert_eq!(events[count + i], ("release", count - 1 - i, why));
        }
    }
}

type StopEvent = (&'static str, usize, StopWhy);

struct Ordered {
    id: usize,
    log: Arc<Mutex<Vec<StopEvent>>>,
}
impl Lifecycle for Ordered {
    fn quiesce(&mut self, why: StopWhy) -> StopFuture<'_> {
        Box::pin(async move {
            self.log.lock().unwrap().push(("quiesce", self.id, why));
        })
    }
    fn release(&mut self, why: StopWhy) -> StopFuture<'_> {
        Box::pin(async move {
            self.log.lock().unwrap().push(("release", self.id, why));
        })
    }
}
