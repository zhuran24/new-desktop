//! R10-E1：等两个 hello 之后才写 initialize，其中的 agents、perTaskStopAffordance、
//! supportedDialogKinds 三项声明仍生效。
#![cfg(feature = "scenarios")]
mod support;
use nd_claude::{InitOptions, Readiness};
use nd_testkit::{ModelReply, Route};
use serde_json::{Value, json};
use std::time::Duration;
use support::*;

const PROBE_PROMPT: &str = "ND_PROBE_AGENT_SYSTEM_PROMPT_7f3a";

struct Observed {
    frames: Vec<Value>,
    probe_requests: Vec<Value>,
    first_agents: Vec<String>,
    second_agents: Vec<String>,
}

fn agent_names(response: &Value) -> Vec<String> {
    response["agents"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn notification_seen(frames: &[Value]) -> bool {
    frames
        .iter()
        .any(|f| f["type"] == "system" && f["subtype"] == "task_notification")
}

/// 主对话派一个声明在 initialize 里的后台代理，它的模型请求被扣住时按 Esc（interrupt）。
async fn interrupt_while_a_declared_background_agent_runs(
    name: &str,
    per_task_stop: bool,
) -> Observed {
    let fx = Fixture::start(name).await;
    let claude = fx.claude(fx.config());
    let mut init = InitOptions {
        per_task_stop_affordance: per_task_stop,
        dialog_kinds: vec!["refusal_fallback_prompt".into()],
        ..Default::default()
    };
    init.hook_agents.insert(
        "nd-probe".into(),
        json!({"description":"offline probe agent","prompt":PROBE_PROMPT,"model":MODEL}),
    );
    let session = session_id();
    let mut run = claude
        .open("r10e1", fx.fresh(&session), init)
        .await
        .unwrap();
    assert_eq!(run.ready().caps.readiness, Readiness::Full);
    let main = Route::new(None, MODEL);
    let endpoint = fx.scenario.endpoint();
    endpoint.enqueue(
        main.clone(),
        ModelReply::tool(
            "toolu_probe_1",
            "Agent",
            json!({"description":"offline probe","prompt":"report PROBE","subagent_type":"nd-probe","run_in_background":true}),
        ),
    );
    let agent_gate = endpoint.enqueue_any_agent_held(MODEL, ModelReply::text("PROBE_AGENT_DONE"));
    let turn_gate = endpoint.enqueue_held(main.clone(), ModelReply::text("MAIN_TURN_TEXT"));
    for _ in 0..4 {
        endpoint.enqueue(main.clone(), ModelReply::text("AFTER_NOTIFICATION"));
    }
    run.write(&json!({"type":"user","message":{"role":"user","content":"start the probe"},"parent_tool_use_id":null,"session_id":session}))
        .await
        .unwrap();
    // 后台代理的请求和主回合的第二个请求都扣在伪端点上：回合仍在进行。
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let requests = endpoint.requests();
            if requests.iter().filter(|r| r.route == main).count() >= 2
                && requests.iter().any(|r| r.route.agent.is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "requests: {:?}",
            endpoint
                .requests()
                .iter()
                .map(|r| &r.route)
                .collect::<Vec<_>>()
        )
    });
    run.write(
        &json!({"type":"control_request","request_id":"esc-1","request":{"subtype":"interrupt"}}),
    )
    .await
    .unwrap();
    let mut frames = run
        .wait_frame(Duration::from_secs(20), |f| f["type"] == "result")
        .await
        .unwrap();
    drop(turn_gate);
    tokio::time::sleep(Duration::from_millis(500)).await;
    agent_gate.release();
    if notification_seen(&frames) {
        // 没声明时代理在 Esc 那一刻已被停掉，通知已在回合结果之前或同时到达。
        tokio::time::sleep(Duration::from_millis(300)).await;
    } else {
        frames.extend(
            run.wait_frame(Duration::from_secs(20), |f| {
                f["type"] == "system" && f["subtype"] == "task_notification"
            })
            .await
            .unwrap(),
        );
    }
    run.write(&json!({"type":"control_request","request_id":"second-init","request":{"subtype":"initialize","agents":{"nd-probe-late":{"description":"too late","prompt":"late"}}}}))
        .await
        .unwrap();
    let second = run
        .wait_frame(Duration::from_secs(10), |f| {
            f["response"]["request_id"] == "second-init"
        })
        .await
        .unwrap();
    let second_agents = agent_names(&second.last().unwrap()["response"]["response"]);
    let first_agents = agent_names(&run.ready().initialize);
    let probe_requests = endpoint
        .requests()
        .into_iter()
        .filter(|r| r.route.agent.is_some())
        .map(|r| r.body)
        .collect();
    drop(run);
    drop(claude);
    fx.close();
    Observed {
        frames,
        probe_requests,
        first_agents,
        second_agents,
    }
}

fn notification(frames: &[Value]) -> Option<&Value> {
    frames
        .iter()
        .find(|f| f["type"] == "system" && f["subtype"] == "task_notification")
}

#[tokio::test]
async fn declarations_written_after_both_hellos_still_take_effect() {
    let observed = interrupt_while_a_declared_background_agent_runs("r10e1-on", true).await;
    // agents：后台代理按这次 initialize 里的定义跑，系统提示就是声明的那段。
    assert_eq!(observed.probe_requests.len(), 1);
    assert!(
        observed.probe_requests[0]["system"]
            .to_string()
            .contains(PROBE_PROMPT)
    );
    // perTaskStopAffordance：Esc 只结束回合，后台代理照跑，放行后正常完成。
    let finished = notification(&observed.frames).expect("task notification");
    assert_eq!(finished["status"], "completed", "{finished}");
    assert_eq!(finished["summary"], "PROBE_AGENT_DONE");
    let interrupted = observed
        .frames
        .iter()
        .position(|f| f["type"] == "result")
        .unwrap();
    let completed = observed.frames.iter().position(|f| f == finished).unwrap();
    assert!(interrupted < completed);
    // forwardSubagentText：子代理的文字按 parent_tool_use_id 转到 stdout。
    assert!(observed.frames.iter().any(|f| f["type"] == "assistant"
        && f["parent_tool_use_id"] == "toolu_probe_1"
        && f["message"]["content"][0]["text"] == "PROBE_AGENT_DONE"));
    // 这次 initialize 是 CLI 认的第一次：之后再来的 initialize 不再改一次性设置（agents、
    // supportedDialogKinds、perTaskStopAffordance 同属这一组）。
    assert!(observed.first_agents.contains(&"nd-probe".to_owned()));
    assert!(!observed.second_agents.contains(&"nd-probe-late".to_owned()));
}

#[tokio::test]
async fn without_per_task_stop_the_same_interrupt_kills_the_background_agent() {
    let observed = interrupt_while_a_declared_background_agent_runs("r10e1-off", false).await;
    let stopped = notification(&observed.frames).expect("task notification");
    assert_eq!(stopped["status"], "stopped", "{stopped}");
    assert!(
        observed
            .frames
            .iter()
            .any(|f| f["subtype"] == "task_updated" && f["patch"]["status"] == "killed")
    );
}
