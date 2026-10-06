//! 守护进程（适配器）重启：后端进程和两个 mod 都不受影响；mod 自动重连、按要求重报 hello，
//! 重启前的操作可按操作 id 查到状态和结果。
#![cfg(feature = "scenarios")]
mod support;
use nd_claude::{CommandResult, InitOptions};
use nd_mod_proto::{Action, HelloCause, ModName, OpPhase, Outcome, QueryAnswer};
use std::time::Duration;
use support::*;

#[tokio::test]
async fn mods_rebind_after_an_adapter_restart_and_earlier_operations_can_be_queried() {
    let fx = Fixture::start("claude-restart").await;
    let session = session_id();
    let (op_id, before, caps) = {
        let claude = fx.claude(fx.config());
        let run = claude
            .open("restart", fx.fresh(&session), InitOptions::default())
            .await
            .unwrap();
        let op_id = run.send(ModName::Actions, Action::Ping).unwrap();
        let outcome = run.result(&op_id, Duration::from_secs(10)).await;
        assert!(
            matches!(outcome, Some(CommandResult::Outcome(Outcome::Done { .. }))),
            "{outcome:?}"
        );
        (op_id, run.binding(), run.ready().caps.clone())
        // 适配器连同 mod 通道一起丢掉：socket 关闭，mod 的长轮询失败后每秒重试。
    };
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let claude = fx.claude(fx.config());
    let run = claude
        .adopt("restart", &session, caps.clone())
        .await
        .unwrap();
    let after = run.binding();
    assert_eq!(after.backend_session_id, session);
    assert_eq!(after.binding_epoch, 0);
    for module in ModName::ALL {
        let hello = &after.mods[&module];
        assert_eq!(hello.cause, HelloCause::Rehello, "{module:?}");
        assert_eq!(
            hello.mod_gen, before.mods[&module].mod_gen,
            "{module:?} was not reloaded"
        );
    }
    assert_eq!(run.ready().identity.pid, before_pid(&fx));
    assert_eq!(run.ready().caps, caps);
    let answer = run
        .command(
            ModName::Actions,
            Action::Query {
                op_ids: vec![op_id.clone(), "never-sent".into()],
            },
            Duration::from_secs(10),
        )
        .await;
    let Some(CommandResult::Outcome(Outcome::Done { value })) = answer else {
        panic!("{answer:?}")
    };
    let answer: QueryAnswer = serde_json::from_value(value).unwrap();
    assert_eq!(answer.ops[0].op_id, op_id);
    assert_eq!(answer.ops[0].phase, OpPhase::Done);
    assert!(
        matches!(&answer.ops[0].outcome, Some(Outcome::Done { value }) if value["backend_session_id"] == session.as_str())
    );
    assert_eq!(answer.ops[1].phase, OpPhase::Unknown);
    drop(run);
    drop(claude);
    fx.close();
}

fn before_pid(fx: &Fixture) -> u32 {
    fx.scenario
        .watchdogs()
        .unwrap()
        .inspect()
        .unwrap()
        .into_iter()
        .find(|f| f.run == "restart")
        .and_then(|f| f.identity)
        .unwrap()
        .pid
}
