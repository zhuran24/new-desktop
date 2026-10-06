//! 录制回归：主接缝场景录下的 mod 往返，喂给协议状态机，得到同样的归一化事实。
use nd_claude::{Fact, ModEvent, ModState, fixture};
use nd_mod_proto::{
    Action, Command, Hello, HelloCause, ModName, NextQuery, Outcome, Rejection, ResultPost,
};
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

/// 构造的输入（不是录制）：命令已交给 mod、还没回结果时，mod 重载换了代次。
#[test]
fn a_resendable_command_lost_with_a_reloaded_mod_is_resent_under_the_same_op_id() {
    let hello = |mod_gen: &str, cause| Hello {
        proto: 1,
        run: "r".into(),
        module: ModName::Actions,
        mod_version: "0.1.0".into(),
        mod_gen: mod_gen.into(),
        backend_session_id: "s".into(),
        cli_version: "2.1.289".into(),
        cause,
    };
    let poll = |mod_gen: &str| ModEvent::Poll {
        query: NextQuery {
            run: "r".into(),
            module: ModName::Actions,
            mod_gen: mod_gen.into(),
            backend_session_id: "s".into(),
        },
    };
    let mut state = ModState::new("s");
    state.apply(&ModEvent::Hello {
        hello: hello("g1", HelloCause::Start),
    });
    state.apply(&ModEvent::Settled);
    state.apply(&ModEvent::Send {
        module: ModName::Actions,
        command: Command {
            op_id: "op".into(),
            expected_backend_session_id: "s".into(),
            expected_mod_gen: "g1".into(),
            action: Action::Ping,
        },
    });
    assert_eq!(
        state.apply(&poll("g1")),
        vec![Fact::Delivered {
            module: ModName::Actions,
            op_id: "op".into()
        }]
    );
    let facts = state.apply(&ModEvent::Hello {
        hello: hello("g2", HelloCause::Start),
    });
    assert!(facts.contains(&Fact::Reloaded {
        module: ModName::Actions,
        from_gen: "g1".into(),
        to_gen: "g2".into()
    }));
    assert!(facts.contains(&Fact::Resent {
        module: ModName::Actions,
        op_id: "op".into()
    }));
    assert_eq!(
        state.deliverable(ModName::Actions)[0].expected_mod_gen,
        "g2"
    );
    assert_eq!(
        state.apply(&poll("g1")),
        vec![Fact::Rehello {
            module: ModName::Actions
        }]
    );
    assert_eq!(
        state.apply(&poll("g2")),
        vec![Fact::Delivered {
            module: ModName::Actions,
            op_id: "op".into()
        }]
    );
    let done = Outcome::Done {
        value: serde_json::json!({"backend_session_id":"s","mod_gen":"g2"}),
    };
    assert_eq!(
        state.apply(&ModEvent::Result {
            op_id: "op".into(),
            post: ResultPost {
                run: "r".into(),
                module: ModName::Actions,
                mod_gen: "g2".into(),
                backend_session_id: "s".into(),
                outcome: done.clone(),
            },
        }),
        vec![Fact::Finished {
            module: ModName::Actions,
            op_id: "op".into(),
            outcome: done
        }]
    );
}
