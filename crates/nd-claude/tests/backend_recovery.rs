//! Real Claude backend port, watchdog and pinned CLI; only the model endpoint is offline.
#![cfg(feature = "scenarios")]
mod support;
use nd_backend::*;
use nd_claude::{ClaudeBackend, ClaudeBackendConfig};
use std::{sync::Arc, time::Duration};
use support::*;

fn backend(fx: &Fixture) -> Arc<ClaudeBackend> {
    let root = fx.scenario.root();
    let store = Arc::new(nd_store::Store::open(root.join("port.sqlite"), 2).unwrap());
    let claims = Arc::new(
        nd_claims::Exclusivity::open(
            store.clone(),
            nd_claims::RegistryConfig::new(root.join("claude")),
        )
        .unwrap(),
    );
    claims.observe(nd_claims::Observed::Recovered).unwrap();
    claims.refresh().unwrap();
    let blobs = Arc::new(nd_store::Blobs::open(root.join("port-blobs"), store).unwrap());
    ClaudeBackend::new(
        fx.claude(fx.config()),
        Arc::new(fx.scenario.watchdogs().unwrap()),
        claims,
        1,
        blobs,
        ClaudeBackendConfig::default(),
    )
}

#[tokio::test]
async fn initial_setting_rejection_unregisters_the_opened_mod_channel() {
    let fx = Fixture::start("initial-setting-failure").await;
    let backend = backend(&fx);
    let (inbox, mut batches) = tokio::sync::mpsc::channel(100);
    backend.adopt(AdoptPart {
        session: "s".into(),
        inbox,
        carriers: vec![],
        pending: vec![],
    });
    let bs = session_id();
    assert!(matches!(
        backend.act(
            Issued {
                ticket: "open".into(),
                session: "s".into(),
                write_gen: 1
            },
            Act::Open {
                carrier: "c".into(),
                run: "initial-failure".into(),
                spec: OpenSpec {
                    origin: Origin::Fresh {
                        id: BackendSessionId::claude(&bs)
                    },
                    profile: Profile {
                        kind: BackendKind::Claude,
                        model: Some(MODEL.into()),
                        permission_mode: None,
                        effort: None,
                        cwd: "/sandbox/project".into()
                    },
                    live_settings: vec![nd_wire::LiveSetting::PermissionMode(
                        "bypassPermissions".into()
                    )],
                }
            }
        ),
        Admit::Accepted { .. }
    ));
    let outcome = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let batch = batches.recv().await.unwrap();
            for fact in batch.facts {
                if let FactBody::Done { ticket, outcome } = fact.body {
                    assert_eq!(ticket.as_str(), "open");
                    return outcome;
                }
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
    let records = fx
        .scenario
        .watchdogs()
        .unwrap()
        .records("initial-failure", 0, 1000)
        .unwrap();
    assert!(records.iter().any(|r| matches!(&r.event, nd_watchdog_proto::Event::In { line, .. }
        if serde_json::from_str::<serde_json::Value>(line).is_ok_and(|v| v["request"]["subtype"] == "set_permission_mode"))));
    assert!(
        backend
            .claude()
            .channel()
            .binding("initial-failure")
            .is_none(),
        "an initial control rejection must revoke the mod channel registration"
    );
    drop(backend);
    fx.close();
}
