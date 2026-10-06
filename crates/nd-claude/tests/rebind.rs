//! `/clear` 换了后端会话 id：两个 mod 重报 hello、重新绑定；带旧 id 或旧代次的命令被拒。
#![cfg(feature = "scenarios")]
mod support;
use nd_claude::{CommandResult, Fact, InitOptions};
use nd_mod_proto::{Action, HelloCause, ModName, Outcome, Rejection};
use std::{path::Path, time::Duration};
use support::*;

fn pong_session(result: Option<CommandResult>) -> String {
    match result {
        Some(CommandResult::Outcome(Outcome::Done { value })) => {
            value["backend_session_id"].as_str().unwrap().to_owned()
        }
        other => panic!("expected pong, got {other:?}"),
    }
}

#[tokio::test]
async fn clear_rebinds_both_mods_to_the_new_session_and_stale_commands_are_refused() {
    let fx = Fixture::start("claude-clear").await;
    let mut config = fx.config();
    // 长轮询挂得比重绑时限长：重绑不能靠轮询自然到期。
    config.poll_timeout = Duration::from_secs(20);
    let claude = fx.claude(config);
    let old = session_id();
    let mut run = claude
        .open("clear", fx.fresh(&old), InitOptions::default())
        .await
        .unwrap();
    for module in ModName::ALL {
        let pong = run
            .command(module, Action::Ping, Duration::from_secs(10))
            .await;
        assert_eq!(pong_session(pong), old, "{module:?}");
    }
    let before = run.binding();
    let started = std::time::Instant::now();
    run.write(&serde_json::json!({"type":"user","message":{"role":"user","content":"/clear"},"parent_tool_use_id":null,"session_id":old}))
        .await
        .unwrap();
    let after = run
        .wait_binding(Duration::from_secs(3), |b| {
            b.binding_epoch == 1 && b.mods.len() == 2
        })
        .await
        .unwrap();
    assert!(
        after.binding_epoch == 1 && after.mods.len() == 2,
        "not rebound in {:?}: {after:?}",
        started.elapsed()
    );
    let new = after.backend_session_id.clone();
    assert_ne!(new, old);
    assert_eq!(after.mods[&ModName::Hook].cause, HelloCause::Clear);
    assert_eq!(after.mods[&ModName::Actions].cause, HelloCause::Rehello);
    for module in ModName::ALL {
        // 模块没有重载，代次不变；只换了后端会话。
        assert_eq!(after.mods[&module].mod_gen, before.mods[&module].mod_gen);
        assert_eq!(after.mods[&module].backend_session_id, new);
    }
    // 带旧后端会话 id 的命令 mod 拒绝执行。
    let stale = run
        .command_as(
            ModName::Actions,
            Action::Ping,
            &old,
            &after.mods[&ModName::Actions].mod_gen,
            Duration::from_secs(10),
        )
        .await;
    assert_eq!(
        stale,
        Some(CommandResult::Outcome(Outcome::Rejected {
            reason: Rejection::StaleSession {
                current: new.clone()
            }
        }))
    );
    let wrong_gen = run
        .command_as(
            ModName::Hook,
            Action::Ping,
            &new,
            "not-this-load",
            Duration::from_secs(10),
        )
        .await;
    assert_eq!(
        wrong_gen,
        Some(CommandResult::Outcome(Outcome::Rejected {
            reason: Rejection::StaleGen {
                current: after.mods[&ModName::Hook].mod_gen.clone()
            }
        }))
    );
    for module in ModName::ALL {
        let pong = run
            .command(module, Action::Ping, Duration::from_secs(10))
            .await;
        assert_eq!(pong_session(pong), new, "{module:?}");
    }
    // 钩子 mod 把 session.end(clear) 作为报告交上来。
    assert!(run.recording().iter().flat_map(|r| &r.facts).any(|f| matches!(
        f,
        Fact::Reported { module: ModName::Hook, body: nd_mod_proto::ReportBody::SessionEnd { reason, ended_session_id }, .. }
            if reason == "clear" && *ended_session_id == old
    )));
    // CLI 在 stdout 上也换了 id：之后的帧都带新 id。
    let frames = run
        .wait_frame(Duration::from_secs(10), |f| {
            f["type"] == "system" && f["subtype"] == "init"
        })
        .await
        .unwrap();
    assert_eq!(frames.last().unwrap()["session_id"], new.as_str());
    // 录下的 mod 往返读回后重放，事实与现场一致；可导出成默认回归用的夹具。
    let path = fx.scenario.root().join("clear-rebind.jsonl");
    let meta = nd_claude::fixture::FixtureMeta {
        format: nd_claude::fixture::FORMAT,
        capability: "mod-channel".into(),
        backend: "claude".into(),
        version: "2.1.289".into(),
        scenario: "clear-rebind".into(),
    };
    nd_claude::fixture::write_fixture(&path, &meta, &old, &run.recording()).unwrap();
    let (_, start, records) = nd_claude::fixture::read_fixture(&path).unwrap();
    assert_eq!(records, run.recording());
    let replayed = nd_claude::fixture::replay(&start, &records);
    assert_eq!(
        replayed,
        records.iter().map(|r| r.facts.clone()).collect::<Vec<_>>()
    );
    if let Some(dest) = std::env::var_os("ND_TEST_EVIDENCE") {
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::copy(&path, Path::new(&dest).join("clear-rebind.jsonl")).unwrap();
    }
    drop(run);
    drop(claude);
    fx.close();
}
