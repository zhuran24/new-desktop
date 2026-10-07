//! 录 `!`、总结、fork 型子代理的 mod 往返（真 CLI 2.1.289、两个 mod、伪模型端点）：
//! 现场读回重放一致，并导出成默认套件回归用的夹具（`ND_TEST_EVIDENCE` 目录）。
#![cfg(feature = "scenarios")]
mod support;
use nd_backend::{Anchor, CompactScope, Invocation, Invoked, Outcome as PortOutcome, Refusal};
use nd_claude::{InitOptions, invoke};
use nd_mod_proto::ModName;
use nd_testkit::{ModelReply, Route};
use serde_json::json;
use std::{path::Path, time::Duration};
use support::*;

#[tokio::test]
async fn bang_compact_and_fork_round_trips_are_recorded_and_replay_to_the_same_facts() {
    let fx = Fixture::start("claude-invoke").await;
    let claude = fx.claude(fx.config());
    let session = session_id();
    let mut run = claude
        .open("invoke", fx.fresh(&session), InitOptions::default())
        .await
        .unwrap();
    let main = Route::new(None, MODEL);
    fx.scenario
        .endpoint()
        .enqueue(main.clone(), ModelReply::text("在"));
    run.write(&json!({"type":"user","uuid":uuid::Uuid::new_v4().to_string(),"session_id":session,"parent_tool_use_id":null,
        "message":{"role":"user","content":[{"type":"text","text":"PARENT_MARK"}]},"priority":"next","origin":{"kind":"human"}}))
        .await
        .unwrap();
    run.wait_frame(Duration::from_secs(30), |f| f["type"] == "result")
        .await
        .unwrap();

    // `!`：CLI 发来插件发起的 Bash 审批，规则放行这一条，宿主写回答。
    let shell = Invocation::Shell {
        command: "echo RECORDED_BANG; export X=1".into(),
    };
    assert!(run.send_as(
        ModName::Actions,
        "op-shell",
        invoke::action(&shell, None).unwrap()
    ));
    let asked = run
        .wait_frame(Duration::from_secs(30), |f| {
            f["type"] == "control_request" && f["request"]["subtype"] == "can_use_tool"
        })
        .await
        .unwrap();
    let request = asked.last().unwrap().clone();
    let Invocation::Shell { command } = &shell else {
        unreachable!()
    };
    assert!(invoke::auto_approves(command, &request["request"]));
    run.write(&invoke::allow(
        request["request_id"].as_str().unwrap(),
        &request["request"],
    ))
    .await
    .unwrap();
    let result = run.result("op-shell", Duration::from_secs(30)).await;
    let Some(PortOutcome::Ok {
        done:
            nd_backend::Done::Invoked {
                result:
                    Invoked::Shell {
                        exit,
                        stdout,
                        appended,
                        ..
                    },
            },
    }) = result.as_ref().map(|r| invoke::outcome(&shell, Some(r)))
    else {
        panic!("shell: {result:?}");
    };
    assert_eq!(exit, Some(0));
    assert!(stdout.contains("RECORDED_BANG") && appended);

    // 总结一条对话里没有的提示：钩子 mod 定位不到，不压缩、不请求模型。
    let before = fx.scenario.endpoint().requests().len();
    let compact = Invocation::Compact {
        scope: CompactScope::From,
        anchor: Anchor {
            text: "NOT_IN_THE_CONVERSATION".into(),
            attachments: vec![],
            nth: 1,
            of: 1,
        },
    };
    assert!(run.send_as(
        ModName::Actions,
        "op-compact",
        invoke::action(&compact, Some("NOT_IN_THE_CONVERSATION")).unwrap()
    ));
    let result = run.result("op-compact", Duration::from_secs(30)).await;
    assert!(
        matches!(
            result.as_ref().map(|r| invoke::outcome(&compact, Some(r))),
            Some(PortOutcome::Refused {
                refusal: Refusal::AnchorGone { .. }
            })
        ),
        "{result:?}"
    );
    assert_eq!(fx.scenario.endpoint().requests().len(), before);

    // fork 型子代理：带父对话，后台跑完。
    fx.scenario
        .endpoint()
        .enqueue_any_agent(MODEL, ModelReply::text("子任务完成"));
    let fork = Invocation::ForkAgent {
        prompt: "RECORDED_FORK".into(),
    };
    assert!(run.send_as(
        ModName::Actions,
        "op-fork",
        invoke::action(&fork, None).unwrap()
    ));
    let result = run.result("op-fork", Duration::from_secs(30)).await;
    assert!(
        matches!(
            result.as_ref().map(|r| invoke::outcome(&fork, Some(r))),
            Some(PortOutcome::Ok {
                done: nd_backend::Done::Invoked {
                    result: Invoked::Forked { .. }
                }
            })
        ),
        "{result:?}"
    );

    let path = fx.scenario.root().join("invocations.jsonl");
    let meta = nd_claude::fixture::FixtureMeta {
        format: nd_claude::fixture::FORMAT,
        capability: "mod-channel".into(),
        backend: "claude".into(),
        version: "2.1.289".into(),
        scenario: "invocations".into(),
    };
    nd_claude::fixture::write_fixture(&path, &meta, &session, &run.recording()).unwrap();
    let (_, start, records) = nd_claude::fixture::read_fixture(&path).unwrap();
    assert_eq!(records, run.recording());
    assert_eq!(
        nd_claude::fixture::replay(&start, &records),
        records.iter().map(|r| r.facts.clone()).collect::<Vec<_>>()
    );
    if let Some(dest) = std::env::var_os("ND_TEST_EVIDENCE") {
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::copy(&path, Path::new(&dest).join("invocations.jsonl")).unwrap();
    }
    drop(run);
    drop(claude);
    fx.close();
}
