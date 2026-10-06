use nd_claims::*;
use std::sync::Arc;

#[test]
fn opening_a_backend_session_reserves_it_for_one_run_and_rolls_back_with_the_caller() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let bs = BackendSessionId::claude("one");
    let open = Act::Open {
        session: "session-a".into(),
        bs: NewBs::Known(bs.clone()),
        via: "run-a".into(),
    };
    let result: nd_store::Result<()> = store.write(|tx| {
        assert!(matches!(claims.admit(tx, "create", &open)?, Admit::Go(_)));
        Err(nd_store::Error::Aborted("caller failed".into()))
    });
    assert!(result.is_err());
    assert_eq!(claims.lease(&bs).unwrap(), None);
    let first = store.write(|tx| claims.admit(tx, "create", &open)).unwrap();
    assert_eq!(
        store.write(|tx| claims.admit(tx, "create", &open)).unwrap(),
        first
    );
    let other = Act::Open {
        session: "session-b".into(),
        bs: NewBs::Known(bs.clone()),
        via: "run-b".into(),
    };
    assert_eq!(
        store.write(|tx| claims.admit(tx, "other", &other)).unwrap(),
        Admit::No(Refusal::HeldByOtherSession("session-a".into()))
    );
    assert_eq!(claims.lease(&bs).unwrap().unwrap().run, "run-a");
}

mod support;
use support::ShortProcess;
#[test]
fn identity_mismatch_and_daemon_restart_preserve_responsibility_until_verified_exit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let mut child = ShortProcess::start();
    let who = child.identity();
    let bs = BackendSessionId::claude("owned");
    let open = Act::Open {
        session: "s".into(),
        bs: NewBs::Known(bs.clone()),
        via: "r".into(),
    };
    store.write(|tx| claims.admit(tx, "create", &open)).unwrap();
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: who.clone(),
            generation: 1,
            kind: BackendKind::Claude,
        })
        .unwrap();
    let mut wrong = who.clone();
    wrong.start_ticks += 1;
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: wrong.clone(),
            generation: 2,
            kind: BackendKind::Claude,
        })
        .unwrap();
    let write = Act::Write {
        session: "s".into(),
        bs: bs.clone(),
    };
    assert_eq!(
        claims.peek(&write).unwrap(),
        Admit::Wait(Obstacle::HolderUnknown("r".into()))
    );
    claims
        .observe(Observed::Gone {
            run: "r".into(),
            identity: Some(wrong),
            how: GoneHow::ProcGone,
        })
        .unwrap();
    assert!(claims.lease(&bs).unwrap().is_some());
    drop(claims);
    let claims = support::ready(store.clone(), dir.path());
    assert!(claims.lease(&bs).unwrap().is_some());
    child.stop();
    claims
        .observe(Observed::Gone {
            run: "r".into(),
            identity: Some(who),
            how: GoneHow::ProcGone,
        })
        .unwrap();
    assert_eq!(claims.lease(&bs).unwrap(), None);
}

#[test]
fn holding_observations_clear_only_confirmed_old_bindings_and_keep_the_owned_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let child = ShortProcess::start();
    let old = BackendSessionId::claude("before-clear");
    let new = BackendSessionId::claude("after-clear");
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: child.identity(),
            generation: 2,
            kind: BackendKind::Claude,
        })
        .unwrap();
    let held = |bs: BackendSessionId, leaf: Option<&str>| Held {
        bs,
        session: "s".into(),
        last_leaf: leaf.map(str::to_string),
    };
    claims
        .observe(Observed::Holding {
            run: "r".into(),
            generation: 2,
            at: 10,
            now: vec![held(old.clone(), Some("own-leaf"))],
        })
        .unwrap();
    claims
        .observe(Observed::Holding {
            run: "r".into(),
            generation: 1,
            at: 100,
            now: vec![],
        })
        .unwrap();
    claims
        .observe(Observed::Holding {
            run: "r".into(),
            generation: 2,
            at: 9,
            now: vec![],
        })
        .unwrap();
    assert!(claims.lease(&old).unwrap().is_some());
    let rollback: nd_store::Result<()> = store.write(|tx| {
        claims.observe_in(
            tx,
            Observed::Holding {
                run: "r".into(),
                generation: 2,
                at: 11,
                now: vec![held(new.clone(), None)],
            },
        )?;
        Err(nd_store::Error::Aborted("batch failed".into()))
    });
    assert!(rollback.is_err());
    assert!(claims.lease(&old).unwrap().is_some());
    claims
        .observe(Observed::Holding {
            run: "r".into(),
            generation: 2,
            at: 11,
            now: vec![held(new.clone(), None)],
        })
        .unwrap();
    assert_eq!(claims.lease(&old).unwrap(), None);
    assert_eq!(claims.lease(&new).unwrap().unwrap().run, "r");
    assert_eq!(claims.owned_leaf(&old).unwrap(), Some("own-leaf".into()));
}

#[test]
fn fresh_reservations_bind_independently_and_conflicts_pause_both_runs_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    for (cause, session, via) in [("a", "sa", "ra"), ("b", "sb", "rb")] {
        let act = Act::Open {
            session: session.into(),
            bs: NewBs::Fresh(BackendKind::Codex),
            via: via.into(),
        };
        store.write(|tx| claims.admit(tx, cause, &act)).unwrap();
    }
    let a = BackendSessionId::codex("a");
    let b = BackendSessionId::codex("b");
    store.write(|tx| claims.bind(tx, "a", &a)).unwrap();
    store.write(|tx| claims.bind(tx, "b", &b)).unwrap();
    assert_eq!(claims.lease(&a).unwrap().unwrap().run, "ra");
    assert_eq!(claims.lease(&b).unwrap().unwrap().run, "rb");
    let conflict = store.write(|tx| claims.bind(tx, "b", &a)).unwrap();
    assert_eq!(
        conflict,
        BindResult::Conflict {
            held_by: "ra".into()
        }
    );
    assert_eq!(claims.lease(&a).unwrap().unwrap().run, "ra");
    assert!(claims.lease(&a).unwrap().unwrap().unknown);
    assert!(claims.lease(&b).unwrap().unwrap().unknown);
}

#[test]
fn recovery_requires_identities_then_a_complete_scan_and_excludes_own_pid_before_holding() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let root = dir.path().join("cli");
    let child = ShortProcess::start();
    let who = child.identity();
    support::registry(&root, &who, "not-held-yet");
    let claims = Exclusivity::open(store, RegistryConfig::new(&root)).unwrap();
    let act = Act::Open {
        session: "s".into(),
        bs: NewBs::Known(BackendSessionId::claude("not-held-yet")),
        via: "r".into(),
    };
    claims.refresh().unwrap();
    assert_eq!(
        claims.peek(&act).unwrap(),
        Admit::Wait(Obstacle::Recovering)
    );
    assert!(claims.externals().unwrap().is_empty());
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: who,
            generation: 1,
            kind: BackendKind::Claude,
        })
        .unwrap();
    claims.observe(Observed::Recovered).unwrap();
    assert_eq!(claims.recovery().unwrap(), Recovery::IdentitiesKnown);
    assert_eq!(
        claims.peek(&act).unwrap(),
        Admit::Wait(Obstacle::Recovering)
    );
    claims.refresh().unwrap();
    assert_eq!(claims.recovery().unwrap(), Recovery::Ready);
    assert!(claims.externals().unwrap().is_empty());
    assert!(matches!(claims.peek(&act).unwrap(), Admit::Go(_)));
}

#[test]
fn a_second_process_with_the_same_session_id_blocks_even_an_already_admitted_write() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let own = ShortProcess::start();
    let external = ShortProcess::start();
    let bs = BackendSessionId::claude("shared");
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: own.identity(),
            generation: 1,
            kind: BackendKind::Claude,
        })
        .unwrap();
    claims
        .observe(Observed::Holding {
            run: "r".into(),
            generation: 1,
            at: 1,
            now: vec![Held {
                bs: bs.clone(),
                session: "s".into(),
                last_leaf: None,
            }],
        })
        .unwrap();
    let act = Act::Write {
        session: "s".into(),
        bs: bs.clone(),
    };
    assert!(matches!(
        store.write(|tx| claims.admit(tx, "send", &act)).unwrap(),
        Admit::Go(_)
    ));
    let path = support::registry(&dir.path().join("cli"), &external.identity(), "shared");
    claims.refresh().unwrap();
    assert!(
        claims.externals().unwrap().is_empty(),
        "display filter must not become the conflict filter"
    );
    assert!(matches!(
        store.write(|tx| claims.admit(tx, "send", &act)).unwrap(),
        Admit::Wait(Obstacle::ExternalWriter(_))
    ));
    assert!(claims.lease(&bs).unwrap().is_some());
    std::fs::remove_file(path).unwrap();
    claims.refresh().unwrap();
    assert_eq!(
        store.write(|tx| claims.admit(tx, "send", &act)).unwrap(),
        Admit::Go(Pass {
            route: Route::Live("r".into())
        })
    );
}

#[test]
fn unverified_entries_and_incomplete_registry_reads_never_mean_no_external_writer() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let child = ShortProcess::start();
    let path = support::registry(&dir.path().join("cli"), &child.identity(), "external");
    let mut row: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    row.as_object_mut().unwrap().remove("pid");
    std::fs::write(&path, serde_json::to_vec(&row).unwrap()).unwrap();
    claims.refresh().unwrap();
    let act = Act::Write {
        session: "s".into(),
        bs: BackendSessionId::claude("external"),
    };
    assert!(matches!(
        claims.peek(&act).unwrap(),
        Admit::Wait(Obstacle::ExternalUnverified(_))
    ));
    std::fs::write(&path, b"{\"pid\":").unwrap();
    assert!(claims.refresh().is_err());
    let unrelated = Act::Write {
        session: "s".into(),
        bs: BackendSessionId::claude("other"),
    };
    assert_eq!(
        claims.peek(&unrelated).unwrap(),
        Admit::Wait(Obstacle::Checking)
    );
    std::fs::write(&path, b"{}").unwrap();
    assert!(
        claims.refresh().is_err(),
        "missing session identity is not an empty registry"
    );
    std::fs::remove_file(path).unwrap();
    claims.refresh().unwrap();
    assert!(matches!(claims.peek(&act).unwrap(), Admit::Go(_)));
}

#[test]
fn detection_keeps_running_without_a_list_subscriber_and_stops_without_releasing_leases() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let root = dir.path().join("cli");
    std::fs::create_dir_all(&root).unwrap();
    let mut config = RegistryConfig::new(&root);
    config.rescan_interval = std::time::Duration::from_millis(20);
    let claims = Exclusivity::open(store.clone(), config).unwrap();
    claims.observe(Observed::Recovered).unwrap();
    claims.refresh().unwrap();
    let bs = BackendSessionId::claude("automatic");
    let open = Act::Open {
        session: "s".into(),
        bs: NewBs::Known(bs.clone()),
        via: "r".into(),
    };
    store.write(|tx| claims.admit(tx, "open", &open)).unwrap();
    let own = ShortProcess::start();
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: own.identity(),
            generation: 1,
            kind: BackendKind::Claude,
        })
        .unwrap();
    claims
        .observe(Observed::Holding {
            run: "r".into(),
            generation: 1,
            at: 1,
            now: vec![Held {
                bs: bs.clone(),
                session: "s".into(),
                last_leaf: None,
            }],
        })
        .unwrap();
    let external = ShortProcess::start();
    let path = support::registry(&root, &external.identity(), "automatic");
    let act = Act::Write {
        session: "s".into(),
        bs: bs.clone(),
    };
    support::eventually(|| {
        matches!(
            claims.peek(&act).unwrap(),
            Admit::Wait(Obstacle::ExternalWriter(_))
        )
    });
    std::fs::remove_file(path).unwrap();
    support::eventually(|| matches!(claims.peek(&act).unwrap(), Admit::Go(_)));
    drop(claims);
    let claims = support::ready(store, dir.path());
    assert_eq!(claims.lease(&bs).unwrap().unwrap().run, "r");
}

#[test]
fn scripted_cli_lists_are_unverified_without_registry_identity_and_interest_drop_keeps_detection() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/agents.json")).unwrap();
    raw[0].as_object_mut().unwrap().remove("pid");
    let id = raw[0]["sessionId"].as_str().unwrap().to_owned();
    let commands = Arc::new(support::ScriptedCli::new(serde_json::to_vec(&raw).unwrap()));
    let claims = Exclusivity::open_with_commands(
        store,
        RegistryConfig::new(dir.path().join("cli")),
        commands,
    )
    .unwrap();
    claims.observe(Observed::Recovered).unwrap();
    claims.refresh().unwrap();
    assert!(claims.externals().unwrap().is_empty());
    let interest = claims.want_list();
    claims.refresh().unwrap();
    let act = Act::Write {
        session: "s".into(),
        bs: BackendSessionId::claude(id),
    };
    assert!(matches!(
        claims.peek(&act).unwrap(),
        Admit::Wait(Obstacle::ExternalUnverified(_))
    ));
    drop(interest);
    let external = ShortProcess::start();
    support::registry(
        &dir.path().join("cli"),
        &external.identity(),
        "still-detected",
    );
    claims.refresh().unwrap();
    assert!(matches!(
        claims
            .peek(&Act::Write {
                session: "s".into(),
                bs: BackendSessionId::claude("still-detected")
            })
            .unwrap(),
        Admit::Wait(Obstacle::ExternalWriter(_))
    ));
}

#[test]
fn real_background_job_without_a_pid_blocks_even_when_no_one_is_listing_external_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store, dir.path());
    let job: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/job-state.json")).unwrap();
    let path = dir
        .path()
        .join("cli/jobs")
        .join(job["daemonShort"].as_str().unwrap());
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("state.json"),
        include_bytes!("fixtures/job-state.json"),
    )
    .unwrap();
    claims.refresh().unwrap();
    let act = Act::Write {
        session: "s".into(),
        bs: BackendSessionId::claude(job["sessionId"].as_str().unwrap()),
    };
    assert!(matches!(
        claims.peek(&act).unwrap(),
        Admit::Wait(Obstacle::ExternalUnverified(_))
    ));
}

#[test]
fn only_proven_unsent_grants_are_rechecked_after_restart_and_never_opened_is_observation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let bs = BackendSessionId::claude("reserved");
    let act = Act::Open {
        session: "s".into(),
        bs: NewBs::Known(bs.clone()),
        via: "r".into(),
    };
    let original = store.write(|tx| claims.admit(tx, "create", &act)).unwrap();
    drop(claims);
    let external = ShortProcess::start();
    let path = support::registry(&dir.path().join("cli"), &external.identity(), "reserved");
    let claims = support::ready(store.clone(), dir.path());
    assert_eq!(
        store.write(|tx| claims.admit(tx, "create", &act)).unwrap(),
        original,
        "an accepted/unknown ticket keeps its grant, not permission to reexecute"
    );
    assert!(matches!(
        store
            .write(|tx| claims.readmit(tx, "create", &act))
            .unwrap(),
        Admit::Wait(Obstacle::ExternalWriter(_))
    ));
    assert!(claims.lease(&bs).unwrap().is_some());
    claims
        .observe(Observed::NeverOpened {
            cause: "create".into(),
        })
        .unwrap();
    assert_eq!(claims.lease(&bs).unwrap(), None);
    std::fs::remove_file(path).unwrap();
    claims.refresh().unwrap();
    assert!(
        store.write(|tx| claims.bind(tx, "create", &bs)).is_err(),
        "ended reservation cannot be rebound"
    );
}

#[test]
fn record_checks_use_the_cli_selected_leaf_without_replacing_the_own_journal_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store, dir.path());
    let child = ShortProcess::start();
    let bs = BackendSessionId::claude("record");
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: child.identity(),
            generation: 1,
            kind: BackendKind::Claude,
        })
        .unwrap();
    claims
        .observe(Observed::Holding {
            run: "r".into(),
            generation: 1,
            at: 1,
            now: vec![Held {
                bs: bs.clone(),
                session: "s".into(),
                last_leaf: Some("ed9573a4-c062-4dfb-ac69-643f36e87ab2".into()),
            }],
        })
        .unwrap();
    let path = dir.path().join("record.jsonl");
    std::fs::write(&path, include_bytes!("fixtures/history.jsonl")).unwrap();
    let check = claims.check_record(&bs, &path).unwrap();
    assert_eq!(
        check.current.as_deref(),
        Some("74bd0770-3e0c-44b5-bf5f-aa56cd617bac")
    );
    assert_eq!(
        check.owned.as_deref(),
        Some("ed9573a4-c062-4dfb-ac69-643f36e87ab2")
    );
    assert_eq!(claims.owned_leaf(&bs).unwrap(), check.owned);
    std::fs::write(&path, b"{\"type\":").unwrap();
    assert!(claims.check_record(&bs, &path).is_err());
    assert_eq!(claims.owned_leaf(&bs).unwrap(), check.owned);
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
        state: "Up".into(),
        high: 0,
        exit: None,
        tail: "Available".into(),
        reason: None,
        detail: None,
    };
    claims
        .observe_watchdog(&found, 1, BackendKind::Claude)
        .unwrap();
    found.state = "Gone".into();
    found.reason = Some("ProcGone".into());
    claims
        .observe_watchdog(&found, 1, BackendKind::Claude)
        .unwrap();
    assert!(claims.lease(&bs).unwrap().unwrap().unknown);
    child.stop();
    claims
        .observe_watchdog(&found, 1, BackendKind::Claude)
        .unwrap();
    assert_eq!(claims.lease(&bs).unwrap(), None);
}

#[test]
fn a_claude_run_cannot_reserve_two_ids_or_write_before_binding_or_reappear_after_exit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let mut child = ShortProcess::start();
    let identity = child.identity();
    let bs = BackendSessionId::claude("one");
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
    assert!(
        store
            .write(|tx| claims.bind(tx, "open", &BackendSessionId::claude("wrong-id")))
            .is_err()
    );
    assert_eq!(
        claims
            .peek(&Act::Write {
                session: "s".into(),
                bs: bs.clone()
            })
            .unwrap(),
        Admit::Wait(Obstacle::Checking)
    );
    assert!(matches!(
        store
            .write(|tx| claims.admit(
                tx,
                "other",
                &Act::Open {
                    session: "s".into(),
                    bs: NewBs::Known(BackendSessionId::claude("two")),
                    via: "r".into()
                }
            ))
            .unwrap(),
        Admit::No(_)
    ));
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: identity.clone(),
            generation: 1,
            kind: BackendKind::Claude,
        })
        .unwrap();
    child.stop();
    claims
        .observe(Observed::Gone {
            run: "r".into(),
            identity: Some(identity.clone()),
            how: GoneHow::Exited,
        })
        .unwrap();
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity,
            generation: 1,
            kind: BackendKind::Claude,
        })
        .unwrap();
    claims
        .observe(Observed::Holding {
            run: "r".into(),
            generation: 1,
            at: 1,
            now: vec![Held {
                bs: bs.clone(),
                session: "s".into(),
                last_leaf: None,
            }],
        })
        .unwrap();
    assert_eq!(claims.lease(&bs).unwrap(), None);
    assert!(store.write(|tx| claims.bind(tx, "open", &bs)).is_err());
}

#[test]
fn stale_local_pids_are_ignored_but_foreign_pid_namespaces_remain_unverified() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store, dir.path());
    let child = ShortProcess::start();
    let who = child.identity();
    let path = support::registry(&dir.path().join("cli"), &who, "foreign");
    let mut row: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    row["procStart"] = (who.start_ticks + 1).to_string().into();
    std::fs::write(&path, serde_json::to_vec(&row).unwrap()).unwrap();
    claims.refresh().unwrap();
    assert!(claims.externals().unwrap().is_empty());
    row["procStart"] = who.start_ticks.to_string().into();
    row["startedAt"] = 1.into();
    std::fs::write(&path, serde_json::to_vec(&row).unwrap()).unwrap();
    claims.refresh().unwrap();
    assert!(
        matches!(
            claims
                .peek(&Act::Write {
                    session: "s".into(),
                    bs: BackendSessionId::claude("foreign")
                })
                .unwrap(),
            Admit::Wait(Obstacle::ExternalUnverified(_))
        ),
        "a registry from an earlier boot must not acquire the current process identity"
    );
    row["pidDomain"] = "linux:another-host:pid:[1]".into();
    row["pid"] = 4294967295_u32.into();
    std::fs::write(&path, serde_json::to_vec(&row).unwrap()).unwrap();
    claims.refresh().unwrap();
    assert!(matches!(
        claims
            .peek(&Act::Write {
                session: "s".into(),
                bs: BackendSessionId::claude("foreign")
            })
            .unwrap(),
        Admit::Wait(Obstacle::ExternalUnverified(_))
    ));
}

#[test]
fn subscribers_wake_only_for_committed_claim_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let mut updates = claims.watch();
    let child = ShortProcess::start();
    let up = Observed::Up {
        run: "r".into(),
        identity: child.identity(),
        generation: 1,
        kind: BackendKind::Claude,
    };
    let result: nd_store::Result<()> = store.write(|tx| {
        claims.observe_in(tx, up.clone())?;
        Err(nd_store::Error::Aborted("rollback".into()))
    });
    assert!(result.is_err());
    assert!(!updates.has_changed().unwrap());
    claims.observe(up).unwrap();
    assert!(updates.has_changed().unwrap());
    updates.borrow_and_update();
    claims.refresh().unwrap();
    assert!(
        !updates.has_changed().unwrap(),
        "unchanged scans must not cause notification loops"
    );
}

#[test]
fn another_open_or_never_opened_reservation_cannot_erase_an_existing_thread_lease() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let bs = BackendSessionId::codex("thread");
    let act = Act::Open {
        session: "s".into(),
        bs: NewBs::Fresh(BackendKind::Codex),
        via: "runtime".into(),
    };
    store.write(|tx| claims.admit(tx, "first", &act)).unwrap();
    store.write(|tx| claims.bind(tx, "first", &bs)).unwrap();
    store.write(|tx| claims.admit(tx, "second", &act)).unwrap();
    claims
        .observe(Observed::NeverOpened {
            cause: "second".into(),
        })
        .unwrap();
    assert_eq!(claims.lease(&bs).unwrap().unwrap().run, "runtime");
    let reopen = Act::Open {
        session: "s".into(),
        bs: NewBs::Known(bs.clone()),
        via: "runtime".into(),
    };
    store
        .write(|tx| claims.admit(tx, "third", &reopen))
        .unwrap();
    assert!(claims.lease(&bs).unwrap().unwrap().confirmed);
}

#[test]
fn cause_identity_is_stable_while_waiting_and_readmit_revokes_an_unsent_grant() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims =
        Exclusivity::open(store.clone(), RegistryConfig::new(dir.path().join("cli"))).unwrap();
    let act = Act::Open {
        session: "s".into(),
        bs: NewBs::Known(BackendSessionId::claude("reserved")),
        via: "r".into(),
    };
    assert_eq!(
        store.write(|tx| claims.admit(tx, "create", &act)).unwrap(),
        Admit::Wait(Obstacle::Recovering)
    );
    let other = Act::Open {
        session: "other".into(),
        bs: NewBs::Fresh(BackendKind::Codex),
        via: "r2".into(),
    };
    assert!(
        store
            .write(|tx| claims.admit(tx, "create", &other))
            .is_err()
    );
    claims.observe(Observed::Recovered).unwrap();
    claims.refresh().unwrap();
    assert!(matches!(
        store.write(|tx| claims.admit(tx, "create", &act)).unwrap(),
        Admit::Go(_)
    ));
    let external = ShortProcess::start();
    support::registry(&dir.path().join("cli"), &external.identity(), "reserved");
    claims.refresh().unwrap();
    assert!(matches!(
        store
            .write(|tx| claims.readmit(tx, "create", &act))
            .unwrap(),
        Admit::Wait(_)
    ));
    assert!(
        matches!(
            store.write(|tx| claims.admit(tx, "create", &act)).unwrap(),
            Admit::Wait(_)
        ),
        "readmit's invalidation must survive later retries"
    );
}

#[test]
fn an_empty_holding_snapshot_does_not_prove_an_unwritten_reservation_was_never_created() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let child = ShortProcess::start();
    let bs = BackendSessionId::codex("pending-thread");
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: child.identity(),
            generation: 1,
            kind: BackendKind::Codex,
        })
        .unwrap();
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
    claims
        .observe(Observed::Holding {
            run: "r".into(),
            generation: 1,
            at: 1,
            now: vec![],
        })
        .unwrap();
    assert!(claims.lease(&bs).unwrap().is_some());
    claims
        .observe(Observed::NeverOpened {
            cause: "open".into(),
        })
        .unwrap();
    assert_eq!(claims.lease(&bs).unwrap(), None);
}

#[test]
fn late_bind_does_not_clear_an_identity_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let child = ShortProcess::start();
    let bs = BackendSessionId::claude("late");
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
    claims
        .observe(Observed::Up {
            run: "r".into(),
            identity: child.identity(),
            generation: 1,
            kind: BackendKind::Claude,
        })
        .unwrap();
    claims
        .observe(Observed::IdentityMismatch { run: "r".into() })
        .unwrap();
    store.write(|tx| claims.bind(tx, "open", &bs)).unwrap();
    assert!(claims.lease(&bs).unwrap().unwrap().unknown);
    assert_eq!(
        claims
            .peek(&Act::Write {
                session: "s".into(),
                bs
            })
            .unwrap(),
        Admit::Wait(Obstacle::HolderUnknown("r".into()))
    );
}

#[test]
fn never_opened_releases_only_the_reservation_created_by_that_cause() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(nd_store::Store::open(dir.path().join("state.db"), 2).unwrap());
    let claims = support::ready(store.clone(), dir.path());
    let bs = BackendSessionId::claude("pending");
    let act = Act::Open {
        session: "s".into(),
        bs: NewBs::Known(bs.clone()),
        via: "r".into(),
    };
    store.write(|tx| claims.admit(tx, "first", &act)).unwrap();
    store.write(|tx| claims.admit(tx, "second", &act)).unwrap();
    claims
        .observe(Observed::NeverOpened {
            cause: "second".into(),
        })
        .unwrap();
    assert!(claims.lease(&bs).unwrap().is_some());
    claims
        .observe(Observed::NeverOpened {
            cause: "first".into(),
        })
        .unwrap();
    assert_eq!(claims.lease(&bs).unwrap(), None);
}
