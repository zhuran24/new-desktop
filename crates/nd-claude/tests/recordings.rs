//! 录制回归：主接缝场景录下的 mod 往返，喂给协议状态机，得到同样的归一化事实。
use nd_claude::{Fact, fixture};
use nd_mod_proto::{ModName, Outcome, Rejection};

use std::path::Path;

fn recorded(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mod/claude/2.1.289")
        .join(name)
}

#[test]
fn recorded_clear_rebind_replays_to_the_same_facts() {
    let (meta, session, records) = fixture::read_fixture(&recorded("clear-rebind.jsonl")).unwrap();
    assert_eq!(meta.backend, "claude");
    assert_eq!(meta.version, "2.1.289");
    assert_eq!(meta.capability, "mod-channel");
    let replayed = fixture::replay(&session, &records);
    for (record, facts) in records.iter().zip(&replayed) {
        assert_eq!(&record.facts, facts, "record {}", record.seq);
    }
    let facts: Vec<&Fact> = replayed.iter().flatten().collect();
    // 这段录制里有：两个 mod 先报预定 id，/clear 后各自重报新 id，带旧 id 的命令被拒。
    assert!(
        facts
            .iter()
            .any(|f| matches!(f, Fact::Rebound { binding_epoch: 1, from, .. } if *from == session))
    );
    for module in ModName::ALL {
        let bound = facts
            .iter()
            .filter(|f| matches!(f, Fact::Bound { module: m, matches: true, .. } if m == &module))
            .count();
        assert!(bound >= 2, "{module:?}");
    }
    assert!(facts.iter().any(|f| matches!(
        f,
        Fact::Finished {
            outcome: Outcome::Rejected {
                reason: Rejection::StaleSession { .. }
            },
            ..
        }
    )));
}

#[test]
fn a_fixture_with_a_missing_or_reordered_line_is_rejected() {
    let text = std::fs::read_to_string(recorded("clear-rebind.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let dir = tempfile::tempdir().unwrap();
    let dropped = dir.path().join("dropped.jsonl");
    let mut kept = lines.clone();
    kept.remove(lines.len() / 2);
    std::fs::write(&dropped, kept.join("\n") + "\n").unwrap();
    assert!(fixture::read_fixture(&dropped).is_err());
    let swapped = dir.path().join("swapped.jsonl");
    let mut order = lines.clone();
    order.swap(2, 3);
    std::fs::write(&swapped, order.join("\n") + "\n").unwrap();
    assert!(fixture::read_fixture(&swapped).is_err());
    let truncated = dir.path().join("truncated.jsonl");
    std::fs::write(&truncated, lines[..lines.len() - 1].join("\n") + "\n").unwrap();
    assert!(fixture::read_fixture(&truncated).is_err());
}

/// 真 CLI 和真实 mod 的在途重载录制；只按可观察事实断言，不检查内部队列。
#[test]
fn recorded_reloads_preserve_unsent_work_and_the_original_session_target() {
    for name in ["reload-pending.jsonl", "reload-rebind.jsonl"] {
        let (_, session, records) = fixture::read_fixture(&recorded(name)).unwrap();
        let replayed = fixture::replay(&session, &records);
        assert_eq!(
            replayed,
            records.iter().map(|r| r.facts.clone()).collect::<Vec<_>>()
        );
        let facts: Vec<_> = replayed.iter().flatten().collect();
        assert!(facts.iter().any(|f| matches!(f, Fact::Resent { .. })));
        if name == "reload-rebind.jsonl" {
            assert_eq!(
                facts
                    .iter()
                    .filter(|f| matches!(
                        f,
                        Fact::Finished {
                            outcome: Outcome::Rejected {
                                reason: Rejection::StaleSession { .. }
                            },
                            ..
                        }
                    ))
                    .count(),
                2
            );
        } else {
            assert!(!facts.iter().any(|f| matches!(f, Fact::Unknown { .. })));
            assert!(facts.iter().enumerate().any(|(at, f)| match f {
                Fact::Resent { op_id, .. } => facts[..at].iter().any(|earlier| matches!(earlier, Fact::Delivered { op_id: sent, .. } if sent == op_id)),
                _ => false,
            }), "recording must include a delivered Ping whose result was in flight at reload");
        }
    }
}

/// 真 CLI 录下的 `!`、总结（定位不到）、fork 型子代理的往返：重放事实一致；三种命令都不可重发；
/// mod 的结论按适配器的规则解释出各自的结果。
#[test]
fn recorded_invocations_replay_and_none_of_them_is_resendable() {
    use nd_backend::{
        Anchor, CompactScope, Done, Invocation, Invoked, Outcome as PortOutcome, Refusal,
    };
    let (meta, session, records) = fixture::read_fixture(&recorded("invocations.jsonl")).unwrap();
    assert_eq!(meta.scenario, "invocations");
    let replayed = fixture::replay(&session, &records);
    for (record, facts) in records.iter().zip(&replayed) {
        assert_eq!(&record.facts, facts, "record {}", record.seq);
    }
    let facts: Vec<&Fact> = replayed.iter().flatten().collect();
    for op in ["op-shell", "op-compact", "op-fork"] {
        assert!(
            facts.iter().any(|f| matches!(
                f,
                Fact::Sent { op_id, resend: nd_mod_proto::Resend::NotResendable, .. } if op_id == op
            )),
            "{op} must be sent as not resendable"
        );
    }
    let finished = |op: &str| {
        facts
            .iter()
            .find_map(|f| match f {
                Fact::Finished { op_id, outcome, .. } if op_id == op => {
                    Some(nd_claude::CommandResult::Outcome(outcome.clone()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{op} finished"))
    };
    let shell = Invocation::Shell {
        command: "echo RECORDED_BANG; export X=1".into(),
    };
    let PortOutcome::Ok {
        done:
            Done::Invoked {
                result:
                    Invoked::Shell {
                        exit,
                        stdout,
                        appended,
                        ..
                    },
            },
    } = nd_claude::invoke::outcome(&shell, Some(&finished("op-shell")))
    else {
        panic!("shell outcome");
    };
    assert_eq!((exit, appended), (Some(0), true));
    assert!(stdout.contains("RECORDED_BANG"));
    let compact = Invocation::Compact {
        scope: CompactScope::From,
        anchor: Anchor {
            candidates: vec![],
            selected: 0,
            text: "NOT_IN_THE_CONVERSATION".into(),
            attachments: vec![],
            nth: 1,
            of: 1,
        },
    };
    assert!(matches!(
        nd_claude::invoke::outcome(&compact, Some(&finished("op-compact"))),
        PortOutcome::Refused {
            refusal: Refusal::AnchorGone { .. }
        }
    ));
    let fork = Invocation::ForkAgent {
        prompt: "RECORDED_FORK".into(),
    };
    assert!(matches!(
        nd_claude::invoke::outcome(&fork, Some(&finished("op-fork"))),
        PortOutcome::Ok {
            done: Done::Invoked {
                result: Invoked::Forked { .. }
            }
        }
    ));
}
