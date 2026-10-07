//! `Act::Invoke` 的纯计算（直接测）：`!` 的自动批准规则、总结定位用的行文字、mod 结论到票结果。
//! 请求与结果的形状取自钉住的 CLI 2.1.289 的实测（`$.tool.call` Bash、`$.command.run` compact）。
use nd_backend::{Anchor, CompactScope, Invocation, Invoked, Outcome, Refusal};
use nd_claude::{CommandResult, invoke};
use nd_mod_proto::Outcome as ModOutcome;
use serde_json::{Value, json};

/// 2.1.289 上动作 mod 调 `$.tool.call({tool:"Bash"})` 时 CLI 发来的审批请求（实测形状）。
fn plugin_request(command: &str) -> Value {
    json!({
        "subtype": "can_use_tool",
        "tool_name": "Bash",
        "display_name": "Bash",
        "input": {"command": command, "description": "New Desktop ! 模式"},
        "description": "New Desktop ! 模式",
        "permission_suggestions": [],
        "decision_reason": "A variable in this command can't be checked before it runs",
        "tool_use_id": "toolu_plugin_aca3c7ac3c1342fe9af2bac90d826241"
    })
}

#[test]
fn only_the_dispatched_bash_command_is_auto_approved() {
    let command = "pwd; echo FOO=$FOO";
    assert!(invoke::auto_approves(command, &plugin_request(command)));
    // 原文不一致（哪怕只差空格）、模型自己的调用、子代理里的调用、别的工具：都不放行。
    assert!(!invoke::auto_approves(
        command,
        &plugin_request("pwd;  echo FOO=$FOO")
    ));
    let mut model = plugin_request(command);
    model["tool_use_id"] = json!("toolu_01ABCdef");
    assert!(!invoke::auto_approves(command, &model));
    let mut agent = plugin_request(command);
    agent["agent_id"] = json!("a18028247257be7fa");
    assert!(!invoke::auto_approves(command, &agent));
    let mut other = plugin_request(command);
    other["tool_name"] = json!("Write");
    assert!(!invoke::auto_approves(command, &other));
    let mut question = plugin_request(command);
    question["subtype"] = json!("elicitation");
    assert!(!invoke::auto_approves(command, &question));
}

#[test]
fn the_auto_approval_allows_this_call_once_without_adding_rules() {
    let request = plugin_request("ls");
    let answer = invoke::allow("req-1", &request);
    assert_eq!(answer["response"]["request_id"], "req-1");
    let body = &answer["response"]["response"];
    assert_eq!(body["behavior"], "allow");
    assert_eq!(body["updatedInput"], request["input"]);
    assert_eq!(body["toolUseID"], request["tool_use_id"]);
    assert!(body.get("updatedPermissions").is_none());
}

#[test]
fn the_compaction_hook_sees_text_blocks_joined_without_a_separator() {
    // 2.1.289 实测：两块文字 TWO_A、TWO_B 在压缩钩子里是一行 "TWO_ATWO_B"；图片不进文字。
    let content = vec![
        json!({"type":"text","text":"TWO_A"}),
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}}),
        json!({"type":"text","text":"TWO_B"}),
    ];
    assert_eq!(invoke::row_text(&content), "TWO_ATWO_B");
    let anchor = Anchor {
        text: "TWO_A".into(),
        attachments: vec![],
        nth: 2,
        of: 3,
    };
    let what = Invocation::Compact {
        scope: CompactScope::UpTo,
        anchor,
    };
    let nd_mod_proto::Action::Compact { spec } = invoke::action(&what, Some("TWO_ATWO_B")) else {
        panic!("compact action");
    };
    // 参数里只有散列和次序，原文不进 `/compact` 的命令行。
    // 散列是行文字的 SHA-256（mod 用 crypto.subtle.digest 算同一个值）。
    assert_eq!(spec.sha256, invoke::sha256_hex("TWO_ATWO_B"));
    assert_eq!(
        invoke::sha256_hex("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!((spec.nth, spec.of), (2, 3));
    assert_eq!(spec.scope, nd_mod_proto::CompactScope::UpTo);
}

fn done(value: Value) -> Option<CommandResult> {
    Some(CommandResult::Outcome(ModOutcome::Done { value }))
}
fn compact() -> Invocation {
    Invocation::Compact {
        scope: CompactScope::From,
        anchor: Anchor {
            text: "x".into(),
            attachments: vec![],
            nth: 1,
            of: 1,
        },
    }
}

#[test]
fn compaction_results_distinguish_done_unlocatable_and_failed() {
    assert_eq!(
        invoke::outcome(&compact(), done(json!({"compacted": true})).as_ref()),
        Outcome::Ok {
            done: nd_backend::Done::Invoked {
                result: Invoked::Compacted
            }
        }
    );
    // CLI 把 skip 原因包成 "Not compacted · <原因>"（实测）。
    let gone = invoke::outcome(
        &compact(),
        done(json!({"compacted": false, "skipped": "Not compacted · nd-anchor-gone: 对话里找不到这条提示（可能已被总结）"})).as_ref(),
    );
    assert_eq!(
        gone,
        Outcome::Refused {
            refusal: Refusal::AnchorGone {
                why: "对话里找不到这条提示（可能已被总结）".into()
            }
        }
    );
    let failed = invoke::outcome(
        &compact(),
        done(
            json!({"compacted": false, "skipped": "Not compacted · nd-summarize-failed: 400 nope"}),
        )
        .as_ref(),
    );
    assert_eq!(failed, Outcome::failed("400 nope"));
    assert!(!failed.possibly_applied());
}

#[test]
fn shell_results_keep_the_exit_code_and_a_lost_result_is_unknown() {
    let shell = Invocation::Shell {
        command: "exit 3".into(),
    };
    assert_eq!(
        invoke::outcome(
            &shell,
            done(json!({"exit": 3, "stdout": "", "stderr": "x", "appended": true})).as_ref()
        ),
        Outcome::Ok {
            done: nd_backend::Done::Invoked {
                result: Invoked::Shell {
                    exit: Some(3),
                    stdout: String::new(),
                    stderr: "x".into(),
                    appended: true
                }
            }
        }
    );
    assert!(matches!(
        invoke::outcome(
            &shell,
            done(json!({"stdout":"","stderr":"","appended":false,"denied":"policy"})).as_ref()
        ),
        Outcome::Refused {
            refusal: Refusal::Other { .. }
        }
    ));
    // 不可重发：mod 重载丢了结果、查不到结论、跑完之后出错，都是交付不明，不当成没做。
    for lost in [
        Some(CommandResult::Unknown),
        None,
        Some(CommandResult::Outcome(ModOutcome::Failed {
            error: "append threw".into(),
        })),
    ] {
        assert!(matches!(
            invoke::outcome(&shell, lost.as_ref()),
            Outcome::Unknown { .. }
        ));
    }
}

#[test]
fn a_fork_needs_an_agent_id_to_count_as_dispatched() {
    let fork = Invocation::ForkAgent {
        prompt: "查资料".into(),
    };
    assert_eq!(
        invoke::outcome(
            &fork,
            done(json!({"agent_id": "a18028247257be7fa", "model": "claude-haiku-4-5"})).as_ref()
        ),
        Outcome::Ok {
            done: nd_backend::Done::Invoked {
                result: Invoked::Forked {
                    agent: "a18028247257be7fa".into()
                }
            }
        }
    );
    assert!(matches!(
        invoke::outcome(&fork, done(json!({})).as_ref()),
        Outcome::Failed { .. }
    ));
    // `Agent type 'fork' not found`（没开门控）时 `$.agent.spawn` 抛错：明确没派出。
    assert!(
        !invoke::outcome(
            &fork,
            Some(CommandResult::Outcome(ModOutcome::Failed {
                error: "Agent type 'fork' not found".into()
            }))
            .as_ref()
        )
        .possibly_applied()
    );
}
