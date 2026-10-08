//! 模块重载换了代次：旧代次留下的结果查不到，带旧代次的命令被拒。
#![cfg(feature = "scenarios")]
mod support;
use nd_claude::{CommandResult, InitOptions};
use nd_mod_proto::{Action, ModName, OpPhase, Outcome, QueryAnswer, Rejection};
use std::time::Duration;
use support::*;

#[tokio::test]
async fn a_reloaded_mod_reports_a_new_generation_and_refuses_commands_for_the_old_one() {
    let fx = Fixture::start("claude-reload").await;
    let claude = fx.claude(fx.config());
    let session = session_id();
    let mut run = claude
        .open("reload", fx.fresh(&session), InitOptions::default())
        .await
        .unwrap();
    let old_gen = run.binding().mods[&ModName::Actions].mod_gen.clone();
    let op_id = run.send(ModName::Actions, Action::Ping).unwrap();
    assert!(matches!(
        run.result(&op_id, Duration::from_secs(10)).await,
        Some(CommandResult::Outcome(Outcome::Done { .. }))
    ));
    // 改动模块文件后让 CLI 重载 mod（reload_plugins）：模块变量清零，session.start 再跑一遍。
    let file = fx
        .scenario
        .root()
        .join("mods/new-desktop-actions/hooks/channel.ts");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("\n// reloaded by the scenario\n");
    std::fs::write(&file, text).unwrap();
    run.write(&serde_json::json!({"type":"control_request","request_id":"reload-1","request":{"subtype":"reload_plugins"}}))
        .await
        .unwrap();
    let after = run
        .wait_binding(Duration::from_secs(15), |b| {
            b.mods
                .get(&ModName::Actions)
                .is_some_and(|h| h.mod_gen != old_gen)
        })
        .await
        .unwrap();
    let new_gen = after.mods[&ModName::Actions].mod_gen.clone();
    assert_ne!(new_gen, old_gen, "{after:?}");
    assert_eq!(after.binding_epoch, 0, "same backend session");
    assert!(run.recording().iter().flat_map(|r| &r.facts).any(|f| matches!(
        f,
        nd_claude::Fact::Reloaded { module: ModName::Actions, from_gen, to_gen } if *from_gen == old_gen && *to_gen == new_gen
    )));
    let answer = run
        .command(
            ModName::Actions,
            Action::Query {
                op_ids: vec![op_id],
            },
            Duration::from_secs(10),
        )
        .await;
    let Some(CommandResult::Outcome(Outcome::Done { value })) = answer else {
        panic!("{answer:?}")
    };
    let answer: QueryAnswer = serde_json::from_value(value).unwrap();
    assert_eq!(answer.ops[0].phase, OpPhase::Unknown);
    let stale = run
        .command_as(
            ModName::Actions,
            Action::Ping,
            &session,
            &old_gen,
            Duration::from_secs(10),
        )
        .await;
    assert_eq!(
        stale,
        Some(CommandResult::Outcome(Outcome::Rejected {
            reason: Rejection::StaleGen { current: new_gen }
        }))
    );
    drop(run);
    drop(claude);
    fx.close();
}

async fn reload(fx: &Fixture, run: &mut nd_claude::ClaudeRun) {
    let old = run
        .recording()
        .iter()
        .rev()
        .find_map(|r| match &r.event {
            nd_claude::ModEvent::Hello { hello } if hello.module == ModName::Actions => {
                Some(hello.mod_gen.clone())
            }
            _ => None,
        })
        .unwrap();
    let path = fx
        .scenario
        .root()
        .join("mods/new-desktop-actions/hooks/channel.ts");
    let mut code = std::fs::read_to_string(&path).unwrap();
    code.push_str("\n// reload timing scenario\n");
    std::fs::write(path, code).unwrap();
    run.write(&serde_json::json!({"type":"control_request","request_id":uuid::Uuid::new_v4().to_string(),"request":{"subtype":"reload_plugins"}})).await.unwrap();
    let after = run
        .wait_binding(Duration::from_secs(10), |b| {
            b.mods
                .get(&ModName::Actions)
                .is_some_and(|h| h.mod_gen != old)
        })
        .await
        .unwrap();
    assert_ne!(after.mods[&ModName::Actions].mod_gen, old);
}

#[tokio::test]
async fn real_reload_preserves_unsent_commands_and_rejects_the_old_session() {
    for rebind in [false, true] {
        let fx = Fixture::start(if rebind {
            "reload-rebind"
        } else {
            "reload-pending"
        })
        .await;
        let config = fx.config();
        let socket = config.socket.clone();
        let upstream = socket.with_file_name("upstream.sock");
        let claude = fx.claude(config);
        std::fs::rename(&socket, &upstream).unwrap();
        let gates = fx.scenario.root().join("gates");
        std::fs::create_dir(&gates).unwrap();
        std::fs::write(gates.join("hold-next"), "").unwrap();
        let mut proxy = tokio::process::Command::new("/usr/bin/python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/mod_proxy.py"
            ))
            .arg(&socket)
            .arg(upstream)
            .arg(&gates)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let session = session_id();
        let mut run = claude
            .open("reload", fx.fresh(&session), InitOptions::default())
            .await
            .unwrap();
        let ping = run.send(ModName::Actions, Action::Ping).unwrap();
        let compact = run
            .send(
                ModName::Actions,
                Action::Compact {
                    spec: nd_mod_proto::SummarizeSpec {
                        scope: nd_mod_proto::CompactScope::From,
                        sha256: "0".repeat(64),
                        nth: 1,
                        of: 1,
                    },
                },
            )
            .unwrap();
        if rebind {
            run.write(&serde_json::json!({"type":"user","message":{"role":"user","content":"/clear"},"parent_tool_use_id":null,"session_id":session})).await.unwrap();
            let binding = run
                .wait_binding(Duration::from_secs(3), |b| b.backend_session_id != session)
                .await
                .unwrap();
            assert_ne!(binding.backend_session_id, session);
        }
        reload(&fx, &mut run).await;
        assert_eq!(
            run.result(&compact, Duration::from_millis(10)).await,
            None,
            "never delivered is still safe to dispatch after reload"
        );
        std::fs::remove_file(gates.join("hold-next")).unwrap();
        let ping_result = run.result(&ping, Duration::from_secs(10)).await;
        let compact_result = run.result(&compact, Duration::from_secs(10)).await;
        if rebind {
            for result in [ping_result, compact_result] {
                assert!(
                    matches!(
                        result,
                        Some(CommandResult::Outcome(Outcome::Rejected {
                            reason: Rejection::StaleSession { .. }
                        }))
                    ),
                    "{result:?}"
                );
            }
        } else {
            assert!(
                matches!(
                    ping_result,
                    Some(CommandResult::Outcome(Outcome::Done { .. }))
                ),
                "{ping_result:?}"
            );
            assert!(
                matches!(
                    compact_result,
                    Some(CommandResult::Outcome(Outcome::Done { .. }))
                ),
                "{compact_result:?}"
            );
            // 已交付的 Ping 的真实 result 请求被扣在传输层；重载后按同一 op_id 重发。
            std::fs::write(gates.join("hold-result"), "").unwrap();
            let _ = std::fs::remove_file(gates.join("result-seen"));
            let inflight = uuid::Uuid::new_v4().to_string();
            std::fs::write(
                gates.join("drop-result"),
                serde_json::json!({
                    "op_id":inflight, "mod_gen":run.binding().mods[&ModName::Actions].mod_gen
                })
                .to_string(),
            )
            .unwrap();
            assert!(run.send_as(ModName::Actions, &inflight, Action::Ping));
            tokio::time::timeout(Duration::from_secs(3), async {
                while !gates.join("target-result-seen").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            reload(&fx, &mut run).await;
            let generation = run.binding().mods[&ModName::Actions].mod_gen.clone();
            std::fs::remove_file(gates.join("hold-result")).unwrap();
            assert!(matches!(
                run.result(&inflight, Duration::from_secs(10)).await,
                Some(CommandResult::Outcome(Outcome::Done { .. }))
            ));
            assert!(
                gates.join("result-dropped").exists(),
                "the old result must be lost at the transport boundary"
            );
            let recording = run.recording();
            assert_eq!(recording.iter().flat_map(|r| &r.facts).filter(|f|
                matches!(f, nd_claude::Fact::Delivered { op_id, .. } if op_id == &inflight)).count(), 2,
                "the same op must actually reach the new mod generation");
            assert!(recording.iter().any(|r| matches!(&r.event,
                nd_claude::ModEvent::Result { op_id, post } if op_id == &inflight && post.mod_gen == generation)
                && r.facts.iter().any(|f| matches!(f, nd_claude::Fact::Finished { op_id, .. } if op_id == &inflight))));
        }
        let name = if rebind {
            "reload-rebind"
        } else {
            "reload-pending"
        };
        let path = fx.scenario.root().join(format!("{name}.jsonl"));
        nd_claude::fixture::write_fixture(
            &path,
            &nd_claude::fixture::FixtureMeta {
                format: 1,
                capability: "mod-channel".into(),
                backend: "claude".into(),
                version: "2.1.289".into(),
                scenario: name.into(),
            },
            &session,
            &run.recording(),
        )
        .unwrap();
        if let Some(dest) = std::env::var_os("ND_MOD_FIXTURE_DIR") {
            std::fs::copy(
                path,
                std::path::Path::new(&dest).join(format!("{name}.jsonl")),
            )
            .unwrap();
        }
        proxy.kill().await.unwrap();
        drop(run);
        drop(claude);
        fx.close();
    }
}
