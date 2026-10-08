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

#[tokio::test]
async fn adapter_adoption_numbers_after_the_large_input_still_blocked_in_the_pipe() {
    let fx = Fixture::start("adapter-blocked-input").await;
    let session = session_id();
    let claude = fx.claude(fx.config());
    let mut run = claude
        .open("blocked", fx.fresh(&session), InitOptions::default())
        .await
        .unwrap();
    let caps = run.ready().caps.clone();
    let pid = rustix::process::Pid::from_raw(run.ready().identity.pid as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::STOP).unwrap();
    let first = tokio::spawn(async move {
        run.write(
            &serde_json::json!({"type":"control_request","request_id":"before-adopt",
            "request":{"subtype":"get_settings"},"padding":"x".repeat(256*1024)}),
        )
        .await
    });
    let runs = fx.scenario.watchdogs().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if runs
                .records("blocked", 0, 1000)
                .unwrap()
                .iter()
                .any(|r| matches!(r.event, nd_watchdog_proto::Event::In { in_seq: 2, .. }))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !first.is_finished(),
        "the input must still be blocked in the real CLI pipe"
    );
    first.abort();
    let _ = first.await;
    drop(claude);
    let mut config = fx.config();
    config.hello_timeout = Duration::from_millis(100);
    let claude = fx.claude(config);
    let mut adopted = claude.adopt("blocked", &session, caps).await.unwrap();
    // Sequence allocation is entirely in Claude::adopt / ClaudeRun::write.
    // Both responses must come from the pinned CLI after the blocked write drains.
    let second = tokio::spawn(async move {
        adopted
            .write(
                &serde_json::json!({"type":"control_request","request_id":"after-adopt",
            "request":{"subtype":"get_settings"}}),
            )
            .await
            .unwrap();
        adopted
    });
    rustix::process::kill_process(pid, rustix::process::Signal::CONT).unwrap();
    let mut adopted = second.await.unwrap();
    let frames = adopted
        .wait_frame(Duration::from_secs(5), |f| {
            f["type"] == "control_response" && f["response"]["request_id"] == "after-adopt"
        })
        .await
        .unwrap();
    for id in ["before-adopt", "after-adopt"] {
        assert!(frames.iter().any(|f| f["response"]["request_id"] == id
            && f["response"]["subtype"] == "success"), "missing real CLI response {id}");
    }
    drop(adopted);
    drop(claude);
    fx.close();
}
