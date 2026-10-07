//! `Act::Invoke` 的 Claude 编码与结果解释（总结、`!` 模式、fork 型子代理）。纯计算，不做 I/O。
//!
//! - 总结经动作 mod 发 `/compact ND_SUM <参数>`，钩子 mod 的压缩钩子按提示文字的散列和次序定位。
//!   CLI 交给压缩钩子的每条用户行只有「各文字块直接相连」的文字，所以定位用的文字按同样规则从
//!   发出时的内容块算。
//! - `!` 的自动批准只放行守护进程刚派发的那一条：Bash、命令原文一致、没有 agent_id、
//!   工具调用 id 是插件发起的（`toolu_plugin_` 前缀）。
use nd_backend::{CompactScope, Invocation, Invoked, Outcome, Refusal};
use nd_mod_proto::{
    ANCHOR_GONE, Action, CompactDone, ForkDone, Outcome as ModOutcome, Rejection, SUMMARIZE_FAILED,
    ShellDone, SummarizeSpec,
};
use serde_json::Value;

/// CLI 交给压缩钩子的用户行文字：内容块里的文字块直接相连（2.1.289 实测，没有分隔符）。
pub fn row_text(content: &[Value]) -> String {
    content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect()
}

pub fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 给动作 mod 的命令。`row` 是所选提示按发出时的内容块算出的用户行文字（只对总结有用）。
pub fn action(what: &Invocation, row: Option<&str>) -> Action {
    match what {
        Invocation::Compact { scope, anchor } => Action::Compact {
            spec: SummarizeSpec {
                scope: match scope {
                    CompactScope::From => nd_mod_proto::CompactScope::From,
                    CompactScope::UpTo => nd_mod_proto::CompactScope::UpTo,
                },
                sha256: sha256_hex(row.unwrap_or(&anchor.text)),
                nth: anchor.nth,
                of: anchor.of,
            },
        },
        Invocation::Shell { command } => Action::Shell {
            command: command.clone(),
            description: "New Desktop ! 模式".into(),
        },
        Invocation::ForkAgent { prompt } => Action::Fork {
            prompt: prompt.clone(),
            description: description(prompt),
        },
    }
}

fn description(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(40).collect();
    if out.chars().count() < line.chars().count() {
        out.push('…');
    }
    if out.is_empty() {
        "子任务".into()
    } else {
        out
    }
}

/// `!` 的自动批准：只放行守护进程刚派发、原文一致、不带 agentId 的那一条 Bash（规格「审批」）。
pub fn auto_approves(command: &str, request: &Value) -> bool {
    request["subtype"] == "can_use_tool"
        && request["tool_name"] == "Bash"
        && request["input"]["command"].as_str() == Some(command)
        && request["agent_id"].is_null()
        && request["tool_use_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("toolu_plugin_"))
}

/// 自动批准的回应：照原样放行这一次，不加持久规则。
pub fn allow(request_id: &str, request: &Value) -> Value {
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "allow",
                "updatedInput": request["input"],
                "toolUseID": request["tool_use_id"],
            },
        },
    })
}

/// mod 回的结论 → 票的终结结果。`None` 是查不到结论（进程没了、mod 重载丢了结果）。
pub fn outcome(what: &Invocation, result: Option<&crate::CommandResult>) -> Outcome {
    let unknown = |evidence: String| Outcome::Unknown { evidence };
    let refused = |why: String| Outcome::Refused {
        refusal: Refusal::Other { why },
    };
    let Some(result) = result else {
        return unknown("没有等到动作 mod 的结论".into());
    };
    let value = match result {
        crate::CommandResult::Unknown => {
            return unknown("动作 mod 重载，丢了这条不可重发命令的结果".into());
        }
        crate::CommandResult::Outcome(ModOutcome::Rejected { reason }) => {
            return refused(match reason {
                Rejection::StaleSession { .. } => "后端会话已换（/clear），命令没有执行".into(),
                Rejection::StaleGen { .. } => "动作 mod 已重载，命令没有执行".into(),
                Rejection::Unsupported => "动作 mod 不支持这个动作".into(),
            });
        }
        crate::CommandResult::Outcome(ModOutcome::Failed { error }) => {
            return match what {
                // `$.tool.call` 之后的步骤出错时命令可能已经跑过。
                Invocation::Shell { .. } => unknown(format!("动作 mod 执行出错：{error}")),
                _ => Outcome::failed(format!("动作 mod 执行出错：{error}")),
            };
        }
        crate::CommandResult::Outcome(ModOutcome::Done { value }) => value,
    };
    let malformed = || unknown(format!("动作 mod 的结果看不懂：{value}"));
    match what {
        Invocation::Compact { .. } => {
            let Ok(done) = serde_json::from_value::<CompactDone>(value.clone()) else {
                return malformed();
            };
            if done.compacted {
                return Outcome::Ok {
                    done: nd_backend::Done::Invoked {
                        result: Invoked::Compacted,
                    },
                };
            }
            let skipped = done.skipped.unwrap_or_default();
            if let Some(at) = skipped.find(ANCHOR_GONE) {
                Outcome::Refused {
                    refusal: Refusal::AnchorGone {
                        why: skipped[at + ANCHOR_GONE.len()..].trim().into(),
                    },
                }
            } else if let Some(at) = skipped.find(SUMMARIZE_FAILED) {
                Outcome::failed(skipped[at + SUMMARIZE_FAILED.len()..].trim())
            } else {
                Outcome::failed(skipped)
            }
        }
        Invocation::Shell { .. } => {
            let Ok(done) = serde_json::from_value::<ShellDone>(value.clone()) else {
                return malformed();
            };
            match done.denied {
                Some(why) => refused(format!("命令被拒绝：{why}")),
                None => Outcome::Ok {
                    done: nd_backend::Done::Invoked {
                        result: Invoked::Shell {
                            exit: done.exit,
                            stdout: done.stdout,
                            stderr: done.stderr,
                            appended: done.appended,
                        },
                    },
                },
            }
        }
        Invocation::ForkAgent { .. } => {
            let Ok(done) = serde_json::from_value::<ForkDone>(value.clone()) else {
                return malformed();
            };
            match (done.denied, done.agent_id) {
                (Some(why), _) => refused(format!("没有派出：{why}")),
                (None, Some(agent)) => Outcome::Ok {
                    done: nd_backend::Done::Invoked {
                        result: Invoked::Forked { agent },
                    },
                },
                (None, None) => Outcome::failed("CLI 没有给出子代理 id"),
            }
        }
    }
}
