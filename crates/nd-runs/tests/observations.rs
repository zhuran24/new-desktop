use nd_claims::*;
use std::sync::Arc;
mod support {
    use std::process::{Child, Command, Stdio};
    pub struct ShortProcess(Child);
    impl ShortProcess {
        pub fn start() -> Self {
            Self(
                Command::new("/usr/bin/sleep")
                    .arg("120")
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        }
        pub fn identity(&self) -> nd_claims::Identity {
            nd_claims::Identity::read(self.0.id()).unwrap()
        }
        pub fn stop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    impl Drop for ShortProcess {
        fn drop(&mut self) {
            self.stop();
        }
    }

    pub fn ready(
        store: std::sync::Arc<nd_store::Store>,
        root: &std::path::Path,
    ) -> nd_claims::Exclusivity {
        let claims =
            nd_claims::Exclusivity::open(store, nd_claims::RegistryConfig::new(root.join("cli")))
                .unwrap();
        claims.observe(nd_claims::Observed::Recovered).unwrap();
        claims.refresh().unwrap();
        claims
    }
}
use support::ShortProcess;
#[test]
fn a_verified_exit_before_up_releases_the_reserved_backend_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let mut child = ShortProcess::start();
    let bs = BackendSessionId::claude("startup-exit");
    let open = Act::Open {
        session: "s".into(),
        bs: NewBs::Known(bs.clone()),
        via: "before-up".into(),
    };
    assert!(matches!(
        store.write(|tx| claims.admit(tx, "first", &open)).unwrap(),
        Admit::Go(_)
    ));
    let found = nd_runs::Found {
        run: "before-up".into(),
        identity: Some(child.identity()),
        state: nd_runs::RunState::Gone {
            reason: nd_runs::GoneReason::Exited,
        },
        high: 0,
        exit: Some(1),
        tail: nd_runs::Tail::Available,
        detail: None,
    };
    // A claimed exit while the process is still alive is insufficient evidence.
    claims
        .observe(found.observation(1, BackendKind::Claude))
        .unwrap();
    assert!(claims.lease(&bs).unwrap().is_some());
    child.stop();
    claims
        .observe(found.observation(1, BackendKind::Claude))
        .unwrap();
    assert_eq!(claims.lease(&bs).unwrap(), None);
    drop(claims);
    let claims = support::ready(store.clone(), dir.path());
    claims
        .observe(found.observation(1, BackendKind::Claude))
        .unwrap();
    let next = Act::Open {
        session: "s".into(),
        bs: NewBs::Known(bs),
        via: "next-run".into(),
    };
    assert!(matches!(
        store.write(|tx| claims.admit(tx, "next", &next)).unwrap(),
        Admit::Go(_)
    ));
}

#[test]
fn watchdog_identity_reports_cannot_turn_mismatched_or_live_processes_into_gone() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let mut child = ShortProcess::start();
    let bs = BackendSessionId::claude("watchdog");
    store
        .write(|tx| {
            claims.admit(
                tx,
                "open",
                &Act::Open {
                    session: "s".into(),
                    bs: NewBs::Known(bs.clone()),
                    via: "r".into(),
                },
            )
        })
        .unwrap();
    let mut found = nd_runs::Found {
        run: "r".into(),
        identity: Some(child.identity()),
        state: nd_runs::RunState::Up,
        high: 0,
        exit: None,
        tail: nd_runs::Tail::Available,
        detail: None,
    };
    claims
        .observe(found.observation(1, BackendKind::Claude))
        .unwrap();
    found.state = nd_runs::RunState::Gone {
        reason: nd_runs::GoneReason::ProcGone,
    };
    claims
        .observe(found.observation(1, BackendKind::Claude))
        .unwrap();
    assert!(claims.lease(&bs).unwrap().unwrap().unknown);
    child.stop();
    claims
        .observe(found.observation(1, BackendKind::Claude))
        .unwrap();
    assert_eq!(claims.lease(&bs).unwrap(), None);
}
