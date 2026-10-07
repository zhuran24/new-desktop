use std::sync::Arc;

use futures::executor::block_on;
use futures::{FutureExt, channel::oneshot};
use nd_kernel::ConfigChange;
use nd_kernel::{ComponentSpec, ComponentState, Lifecycle};
use nd_kernel::{Dep, StopFuture, StopWhy};
use nd_kernel::{Kernel, Key, Registration, RegistrationKind};
use std::sync::Mutex;
use std::time::Duration;

struct Running;
impl Lifecycle for Running {}

#[test]
fn missing_dependency_waits_times_out_and_recovers_when_provided() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let key = Key::<str>::new(scope, "primary");
    let output = Registration::new(scope, RegistrationKind::Command, "hello");
    let command = output.clone();
    kernel
        .install(
            ComponentSpec::new("consumer", scope)
                .requires(key.erased())
                .wait_timeout(Duration::from_secs(5)),
            move |mount| {
                mount.register(command.clone())?;
                Ok(Box::new(Running))
            },
        )
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    assert!(matches!(
        kernel.state("consumer"),
        Some(ComponentState::Waiting { .. })
    ));
    assert!(kernel.snapshot().registrations.is_empty());
    block_on(kernel.reconcile(Duration::from_secs(5)));
    assert!(matches!(
        kernel.state("consumer"),
        Some(ComponentState::Unavailable { .. })
    ));
    let _provider = kernel.provide(key, Arc::<str>::from("hello")).unwrap();
    block_on(kernel.reconcile(Duration::from_secs(6)));
    assert!(matches!(
        kernel.state("consumer"),
        Some(ComponentState::Running { .. })
    ));
    assert_eq!(kernel.snapshot().registrations, vec![output]);
}

#[test]
fn replacing_a_provider_rebuilds_only_its_consumers_with_the_new_binding() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let key = Key::<str>::new(scope, "primary");
    let output = Key::<str>::new(scope, "output");
    let dep_key = key.clone();
    let out_key = output.clone();
    kernel
        .install(
            ComponentSpec::new("consumer", scope)
                .requires(key.erased())
                .provides(output.erased()),
            move |mount| {
                let input = mount.require(dep_key.clone())?;
                let text = input.with(str::to_owned).unwrap();
                mount.provide(out_key.clone(), Arc::<str>::from(text))?;
                Ok(Box::new(Running))
            },
        )
        .unwrap();
    kernel
        .install(ComponentSpec::new("unrelated", scope), |_| {
            Ok(Box::new(Running))
        })
        .unwrap();
    let provider = kernel
        .provide(key.clone(), Arc::<str>::from("first"))
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    let old = kernel.state("consumer");
    let unrelated = kernel.state("unrelated");
    let output_dep = kernel.require(output);
    assert_eq!(output_dep.with(str::to_owned), Some("first".into()));
    drop(provider);
    let _replacement = kernel.provide(key, Arc::<str>::from("second")).unwrap();
    block_on(kernel.reconcile(Duration::from_secs(1)));
    assert_ne!(kernel.state("consumer"), old);
    assert_eq!(kernel.state("unrelated"), unrelated);
    assert_eq!(output_dep.with(str::to_owned), Some("second".into()));
}

#[test]
fn incomplete_initialization_revokes_even_an_escaped_dependency() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let source = Key::<str>::new(scope, "source");
    let promised = Key::<str>::new(scope, "promised");
    let _provider = kernel
        .provide(source.clone(), Arc::<str>::from("ok"))
        .unwrap();
    let baseline = kernel.snapshot();
    let escaped = Arc::new(Mutex::new(None));
    let saved = escaped.clone();
    kernel
        .install(
            ComponentSpec::new("incomplete", scope)
                .requires(source.erased())
                .provides(promised.erased()),
            move |mount| {
                *saved.lock().unwrap() = Some(mount.require(source.clone())?);
                for kind in [
                    RegistrationKind::Command,
                    RegistrationKind::Subscription,
                    RegistrationKind::Slot,
                    RegistrationKind::Timer,
                ] {
                    mount.register(Registration::new(scope, kind, "partial"))?;
                }
                // 工厂忘记发布声明的提供者，也必须按初始化失败处理。
                Ok(Box::new(Running))
            },
        )
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    assert!(matches!(
        kernel.state("incomplete"),
        Some(ComponentState::Failed { .. })
    ));
    assert_eq!(kernel.snapshot(), baseline);
    assert_eq!(
        escaped
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .with(str::to_owned),
        None
    );
}

struct Draining {
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
    gate: Option<oneshot::Receiver<()>>,
    dependency: Option<Dep<str>>,
}

impl Lifecycle for Draining {
    fn quiesce(&mut self, why: StopWhy) -> StopFuture<'_> {
        Box::pin(async move {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:quiesce:{why:?}", self.name));
            if let Some(gate) = &mut self.gate {
                gate.await.unwrap();
            }
            if let Some(dep) = &self.dependency {
                assert_eq!(dep.with(str::to_owned), Some("alive".into()));
            }
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:drained", self.name));
        })
    }
    fn release(&mut self, _: StopWhy) -> StopFuture<'_> {
        Box::pin(async move {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:release", self.name));
        })
    }
}

#[test]
fn stopping_waits_for_every_quiesce_before_any_release_in_reverse_dependency_order() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let service = Key::<str>::new(scope, "service");
    let log = Arc::new(Mutex::new(Vec::new()));
    let (send, recv) = oneshot::channel();
    let recv = Mutex::new(Some(recv));
    let provider_log = log.clone();
    let key = service.clone();
    kernel
        .install(
            ComponentSpec::new("provider", scope).provides(service.erased()),
            move |mount| {
                mount.provide(key.clone(), Arc::<str>::from("alive"))?;
                Ok(Box::new(Draining {
                    name: "provider",
                    log: provider_log.clone(),
                    gate: recv.lock().unwrap().take(),
                    dependency: None,
                }))
            },
        )
        .unwrap();
    let consumer_log = log.clone();
    let key = service.clone();
    kernel
        .install(
            ComponentSpec::new("consumer", scope).requires(service.erased()),
            move |mount| {
                Ok(Box::new(Draining {
                    name: "consumer",
                    log: consumer_log.clone(),
                    gate: None,
                    dependency: Some(mount.require(key.clone())?),
                }))
            },
        )
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    let newcomer = kernel.require(service);
    let mut stop = Box::pin(kernel.stop(scope, StopWhy::Finish));
    assert!(stop.as_mut().now_or_never().is_none());
    assert_eq!(
        *log.lock().unwrap(),
        [
            "consumer:quiesce:Finish",
            "consumer:drained",
            "provider:quiesce:Finish"
        ]
    );
    assert_eq!(newcomer.with(str::to_owned), None);
    send.send(()).unwrap();
    block_on(stop);
    assert_eq!(
        *log.lock().unwrap(),
        [
            "consumer:quiesce:Finish",
            "consumer:drained",
            "provider:quiesce:Finish",
            "provider:drained",
            "consumer:release",
            "provider:release"
        ]
    );
    drop(newcomer);
    assert_eq!(kernel.snapshot(), Default::default());
    block_on(kernel.reconcile(Duration::from_secs(1)));
    assert!(kernel.snapshot().providers.is_empty());
}

#[test]
fn config_rebuild_rejects_late_results_and_optional_disable_restores_registries() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let baseline = kernel.snapshot();
    kernel
        .install(ComponentSpec::new("view", scope).optional(), move |mount| {
            mount.register(Registration::new(
                scope,
                RegistrationKind::Slot,
                "conversation",
            ))?;
            Ok(Box::new(Running))
        })
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    let Some(ComponentState::Running { generation: old }) = kernel.state("view") else {
        panic!()
    };
    kernel
        .configure(&[ConfigChange::new("view", true, 1)])
        .unwrap();
    block_on(kernel.reconcile(Duration::from_secs(1)));
    let Some(ComponentState::Running { generation: new }) = kernel.state("view") else {
        panic!()
    };
    let mut text = "initial";
    assert_eq!(kernel.with_generation("view", old, || text = "stale"), None);
    assert_eq!(text, "initial");
    assert_eq!(
        kernel.with_generation("view", new, || text = "current"),
        Some(())
    );
    assert_eq!(text, "current");
    kernel
        .configure(&[ConfigChange::new("view", false, 1)])
        .unwrap();
    block_on(kernel.reconcile(Duration::from_secs(2)));
    assert_eq!(kernel.state("view"), Some(ComponentState::Disabled));
    assert_eq!(kernel.snapshot(), baseline);
}

#[test]
fn invalid_dependency_graph_is_rejected_before_installing_any_new_component() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let a = Key::<str>::new(scope, "a");
    let b = Key::<str>::new(scope, "b");
    kernel
        .install(
            ComponentSpec::new("a", scope)
                .provides(a.erased())
                .requires(b.erased()),
            |_| Ok(Box::new(Running)),
        )
        .unwrap();
    assert!(
        kernel
            .install(
                ComponentSpec::new("b", scope)
                    .provides(b.erased())
                    .requires(a.erased()),
                |_| Ok(Box::new(Running))
            )
            .is_err()
    );
    assert_eq!(kernel.state("b"), None);
    assert!(
        kernel
            .install(
                ComponentSpec::new("duplicate", scope).provides(a.erased()),
                |_| Ok(Box::new(Running))
            )
            .is_err()
    );
    assert_eq!(kernel.state("duplicate"), None);
    assert!(
        kernel
            .install(
                ComponentSpec::new("self", scope)
                    .provides(b.erased())
                    .requires(b.erased()),
                |_| Ok(Box::new(Running))
            )
            .is_err()
    );
}

#[test]
fn waiting_deadline_starts_at_first_reconcile_and_restarts_after_dependency_loss() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let key = Key::<str>::new(scope, "late");
    kernel
        .install(
            ComponentSpec::new("late", scope)
                .requires(key.erased())
                .wait_timeout(Duration::from_secs(5)),
            |_| Ok(Box::new(Running)),
        )
        .unwrap();
    block_on(kernel.reconcile(Duration::from_secs(100)));
    assert!(
        matches!(kernel.state("late"), Some(ComponentState::Waiting { since, .. }) if since == Duration::from_secs(100))
    );
    let guard = kernel.provide(key, Arc::<str>::from("ok")).unwrap();
    block_on(kernel.reconcile(Duration::from_secs(101)));
    drop(guard);
    block_on(kernel.reconcile(Duration::from_secs(200)));
    assert!(
        matches!(kernel.state("late"), Some(ComponentState::Waiting { since, .. }) if since == Duration::from_secs(200))
    );
    block_on(kernel.reconcile(Duration::from_secs(205)));
    assert!(matches!(
        kernel.state("late"),
        Some(ComponentState::Unavailable { .. })
    ));
}

#[test]
fn registry_changes_wake_the_driver_without_polling_or_a_lost_wakeup() {
    use futures::task::{ArcWake, waker};
    use std::{
        future::Future,
        sync::atomic::{AtomicUsize, Ordering},
        task::Context,
    };
    struct Wakes(AtomicUsize);
    impl ArcWake for Wakes {
        fn wake_by_ref(arc: &Arc<Self>) {
            arc.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let kernel = Kernel::new();
    let scope = kernel.scope();
    let before = kernel.revision();
    let mut changes = Box::pin(kernel.changed_since(before));
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    let waker = waker(wakes.clone());
    assert!(
        changes
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    let guard = kernel
        .provide(Key::<u32>::new(scope, "wake"), Arc::new(7))
        .unwrap();
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
    let after = block_on(changes);
    assert!(after > before);
    // 变化先于第一次 poll 发生，也不能丢。
    let changes = kernel.changed_since(after);
    drop(guard);
    assert!(block_on(changes) > after);
}

#[test]
fn revoked_registration_cannot_call_a_replacement_with_the_same_name() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let registration = Registration::new(scope, RegistrationKind::Timer, "tick");
    let guard = kernel.register(registration.clone()).unwrap();
    let old = guard.token();
    assert_eq!(kernel.with_registration(old, || "first"), Some("first"));
    drop(guard);
    let replacement = kernel.register(registration).unwrap();
    assert_eq!(kernel.with_registration(old, || "stale"), None);
    assert_eq!(
        kernel.with_registration(replacement.token(), || "second"),
        Some("second")
    );
}

#[test]
fn dependency_revoked_during_initialization_cannot_publish_a_running_instance() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let source = Key::<str>::new(scope, "source");
    let guard = Mutex::new(Some(
        kernel
            .provide(source.clone(), Arc::<str>::from("source"))
            .unwrap(),
    ));
    kernel
        .install(
            ComponentSpec::new("racy", scope).requires(source.erased()),
            move |mount| {
                drop(guard.lock().unwrap().take());
                mount.register(Registration::new(scope, RegistrationKind::Command, "racy"))?;
                Ok(Box::new(Running))
            },
        )
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    assert!(!matches!(
        kernel.state("racy"),
        Some(ComponentState::Running { .. })
    ));
    assert_eq!(kernel.snapshot(), Default::default());
}

#[test]
fn invalid_config_batch_does_not_disable_earlier_optional_components() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    kernel
        .install(ComponentSpec::new("optional", scope).optional(), |_| {
            Ok(Box::new(Running))
        })
        .unwrap();
    kernel
        .install(ComponentSpec::new("resident", scope), |_| {
            Ok(Box::new(Running))
        })
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    let original = kernel.state("optional");
    assert!(
        kernel
            .configure(&[
                ConfigChange::new("optional", false, 1),
                ConfigChange::new("resident", false, 1)
            ])
            .is_err()
    );
    block_on(kernel.reconcile(Duration::from_secs(1)));
    assert_eq!(kernel.state("optional"), original);
    assert!(
        kernel
            .configure(&[
                ConfigChange::new("optional", false, 1),
                ConfigChange::new("unknown", true, 0)
            ])
            .is_err()
    );
    assert!(
        kernel
            .configure(&[
                ConfigChange::new("optional", false, 1),
                ConfigChange::new("optional", true, 2)
            ])
            .is_err()
    );
    block_on(kernel.reconcile(Duration::from_secs(2)));
    assert_eq!(kernel.state("optional"), original);
}

trait Greeting: Send + Sync {
    fn greet(&self) -> &'static str;
}
struct Hello;
impl Greeting for Hello {
    fn greet(&self) -> &'static str {
        "hello"
    }
}

#[test]
fn providers_are_unique_by_type_scope_and_partition_including_trait_objects() {
    let kernel = Kernel::new();
    let scope = kernel.scope();
    let other_scope = kernel.scope();
    let key = Key::<dyn Greeting>::new(scope, "same");
    let provider = kernel.provide(key.clone(), Arc::new(Hello)).unwrap();
    assert!(kernel.provide(key.clone(), Arc::new(Hello)).is_err());
    let _different_type = kernel
        .provide(Key::<str>::new(scope, "same"), Arc::<str>::from("text"))
        .unwrap();
    let _different_partition = kernel
        .provide(Key::<dyn Greeting>::new(scope, "other"), Arc::new(Hello))
        .unwrap();
    let _different_scope = kernel
        .provide(
            Key::<dyn Greeting>::new(other_scope, "same"),
            Arc::new(Hello),
        )
        .unwrap();
    let dep = kernel.require(key);
    assert_eq!(dep.with(|g| g.greet()), Some("hello"));
    drop(provider);
    assert_eq!(dep.with(|g| g.greet()), None);
}

#[test]
fn cancelled_stop_resumes_pending_phase_without_repeating_completed_consumers() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let service = Key::<str>::new(scope, "service");
    let log = Arc::new(Mutex::new(Vec::new()));
    let (send, recv) = oneshot::channel();
    let recv = Mutex::new(Some(recv));
    let saved_log = log.clone();
    let key = service.clone();
    kernel
        .install(
            ComponentSpec::new("provider", scope).provides(service.erased()),
            move |mount| {
                mount.provide(key.clone(), Arc::<str>::from("alive"))?;
                Ok(Box::new(Draining {
                    name: "provider",
                    log: saved_log.clone(),
                    gate: recv.lock().unwrap().take(),
                    dependency: None,
                }))
            },
        )
        .unwrap();
    let saved_log = log.clone();
    kernel
        .install(
            ComponentSpec::new("consumer", scope).requires(service.erased()),
            move |_| {
                Ok(Box::new(Draining {
                    name: "consumer",
                    log: saved_log.clone(),
                    gate: None,
                    dependency: None,
                }))
            },
        )
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    let mut stop = Box::pin(kernel.stop(scope, StopWhy::HandBack));
    assert!(stop.as_mut().now_or_never().is_none());
    drop(stop);
    send.send(()).unwrap();
    block_on(kernel.reconcile(Duration::from_secs(1)));
    assert_eq!(
        *log.lock().unwrap(),
        [
            "consumer:quiesce:HandBack",
            "consumer:drained",
            "provider:quiesce:HandBack",
            "provider:quiesce:HandBack",
            "provider:drained",
            "consumer:release",
            "provider:release"
        ]
    );
    assert_eq!(kernel.snapshot(), Default::default());
}

#[test]
fn stopping_one_scope_leaves_unrelated_registrations_and_restarts_cross_scope_consumers_later() {
    let mut kernel = Kernel::new();
    let provider_scope = kernel.scope();
    let consumer_scope = kernel.scope();
    let service = Key::<str>::new(provider_scope, "service");
    let log = Arc::new(Mutex::new(Vec::new()));
    let saved_log = log.clone();
    kernel
        .install(
            ComponentSpec::new("consumer", consumer_scope).requires(service.erased()),
            move |_| {
                Ok(Box::new(Draining {
                    name: "consumer",
                    log: saved_log.clone(),
                    gate: None,
                    dependency: None,
                }))
            },
        )
        .unwrap();
    let _unrelated = kernel
        .register(Registration::new(
            consumer_scope,
            RegistrationKind::Slot,
            "unrelated",
        ))
        .unwrap();
    let external = kernel.require(service.clone());
    let baseline = kernel.snapshot();
    let _provider = kernel
        .provide(service.clone(), Arc::<str>::from("old"))
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    block_on(kernel.stop(provider_scope, StopWhy::Finish));
    assert_eq!(kernel.snapshot(), baseline);
    assert_eq!(
        *log.lock().unwrap(),
        [
            "consumer:quiesce:HandBack",
            "consumer:drained",
            "consumer:release"
        ]
    );
    assert!(external.with(str::to_owned).is_none());
    let _new_provider = kernel.provide(service, Arc::<str>::from("new")).unwrap();
    assert_eq!(external.with(str::to_owned).as_deref(), Some("new"));
    block_on(kernel.reconcile(Duration::from_secs(1)));
    assert!(matches!(
        kernel.state("consumer"),
        Some(ComponentState::Running { .. })
    ));
}

#[test]
fn provider_destructors_can_revoke_other_guards_without_holding_the_registry_lock() {
    struct RevokeOnDrop(Mutex<Option<nd_kernel::Guard>>);
    impl Drop for RevokeOnDrop {
        fn drop(&mut self) {
            drop(self.0.lock().unwrap().take());
        }
    }
    let kernel = Kernel::new();
    let scope = kernel.scope();
    let command = kernel
        .register(Registration::new(
            scope,
            RegistrationKind::Command,
            "reentrant",
        ))
        .unwrap();
    let provider = kernel
        .provide(
            Key::<RevokeOnDrop>::new(scope, "drop"),
            Arc::new(RevokeOnDrop(Mutex::new(Some(command)))),
        )
        .unwrap();
    drop(provider);
    assert_eq!(kernel.snapshot(), Default::default());
}

#[test]
fn driver_deadline_survives_unchanged_config_and_disappears_after_timeout() {
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let key = Key::<str>::new(scope, "missing");
    kernel
        .install(
            ComponentSpec::new("waiting", scope)
                .requires(key.erased())
                .wait_timeout(Duration::from_secs(5)),
            |_| Ok(Box::new(Running)),
        )
        .unwrap();
    block_on(kernel.reconcile(Duration::from_secs(10)));
    assert_eq!(kernel.next_deadline(), Some(Duration::from_secs(15)));
    kernel
        .configure(&[ConfigChange::new("waiting", true, 0)])
        .unwrap();
    block_on(kernel.reconcile(Duration::from_secs(12)));
    assert_eq!(kernel.next_deadline(), Some(Duration::from_secs(15)));
    block_on(kernel.reconcile(Duration::from_secs(15)));
    assert_eq!(kernel.next_deadline(), None);
    assert!(matches!(
        kernel.state("waiting"),
        Some(ComponentState::Unavailable { .. })
    ));
}

#[test]
fn failed_initialization_does_not_spin_on_its_own_registration_notifications() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut kernel = Kernel::new();
    let scope = kernel.scope();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    kernel
        .install(ComponentSpec::new("fails", scope), move |mount| {
            counter.fetch_add(1, Ordering::SeqCst);
            mount.register(Registration::new(
                scope,
                RegistrationKind::Timer,
                "temporary",
            ))?;
            Err(nd_kernel::Error::Initialization(
                "unavailable resource".into(),
            ))
        })
        .unwrap();
    block_on(kernel.reconcile(Duration::ZERO));
    let revision = kernel.revision();
    block_on(kernel.reconcile(Duration::from_secs(1)));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(kernel.revision(), revision);
    kernel
        .configure(&[ConfigChange::new("fails", true, 1)])
        .unwrap();
    block_on(kernel.reconcile(Duration::from_secs(2)));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn dropping_guards_restores_every_registry_and_revokes_call_rights() {
    let kernel = Kernel::new();
    let scope = kernel.scope();
    let key = Key::<str>::new(scope, "primary");
    let baseline = kernel.snapshot();
    let provider = kernel
        .provide(key.clone(), Arc::<str>::from("ready"))
        .unwrap();
    let dependency = kernel.require(key);
    assert_eq!(dependency.with(str::to_owned), Some("ready".into()));
    let guards: Vec<_> = [
        RegistrationKind::Command,
        RegistrationKind::Subscription,
        RegistrationKind::Slot,
        RegistrationKind::Timer,
    ]
    .into_iter()
    .map(|kind| {
        kernel
            .register(Registration::new(scope, kind, "example"))
            .unwrap()
    })
    .collect();
    assert_eq!(kernel.snapshot().registrations.len(), 4);
    drop(guards);
    drop(provider);
    assert_eq!(dependency.with(str::to_owned), None);
    drop(dependency);
    assert_eq!(kernel.snapshot(), baseline);
}
