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
    let op_id = run.send(ModName::Actions, Action::Ping);
    assert!(matches!(
        run.result(&op_id, Duration::from_secs(10)).await,
        Some(CommandResult::Outcome(Outcome::Done { .. }))
    ));
    // 改动模块文件后让 CLI 重载插件：模块变量清零，session.start 再跑一遍。
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
