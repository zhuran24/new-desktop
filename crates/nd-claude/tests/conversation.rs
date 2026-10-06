//! 录制回归：主接缝场景录下的真 CLI 对话流水（流式文字、工具调用与结果、两个回合），
//! 喂给对话状态机，得到和录制时一样的归一化事实；再拿它当同一组输入，比对对话投影的
//! 最简版和增量版。
use nd_backend::ItemKind;
use nd_claude::convo::{Convo, replay};
use nd_session::projection::{Projection, Shown, project};
use nd_watchdog_proto::{Event, Record, read_fixture};
use std::path::{Path, PathBuf};

fn recorded(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/conversation/claude/2.1.289")
        .join(name)
}

fn conversation() -> Vec<Record> {
    let (meta, records) = read_fixture(&recorded("stream-tool-two-turns.jsonl")).unwrap();
    assert_eq!(
        (
            meta.capability.as_str(),
            meta.backend.as_str(),
            meta.version.as_str()
        ),
        ("conversation", "claude", "2.1.289")
    );
    records
}

#[test]
fn recorded_conversation_replays_to_the_committed_facts() {
    let records = conversation();
    let replayed = replay(&records);
    let committed: Vec<Vec<Convo>> = serde_json::from_str(
        &std::fs::read_to_string(recorded("stream-tool-two-turns.facts.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(replayed, committed);
    let facts: Vec<&Convo> = replayed.iter().flatten().collect();
    // 每条写出的 user 行恰好回显一次。
    let written: Vec<&String> = facts
        .iter()
        .filter_map(|f| match f {
            Convo::Written { uuid } => Some(uuid),
            _ => None,
        })
        .collect();
    let echoed: Vec<&String> = facts
        .iter()
        .filter_map(|f| match f {
            Convo::Echo { uuid } => Some(uuid),
            _ => None,
        })
        .collect();
    assert_eq!(written.len(), 2);
    assert_eq!(written, echoed);
    // 每个文字块的增量连起来就是完整块的文字。
    for fact in &facts {
        if let Convo::Block { item } = fact
            && item.kind == ItemKind::Text
        {
            let streamed: String = facts
                .iter()
                .filter_map(|f| match f {
                    Convo::Delta { item: id, text, .. } if *id == item.id => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(streamed, item.text, "{}", item.id);
        }
    }
    assert_eq!(
        facts
            .iter()
            .filter(|f| matches!(f, Convo::TurnEnded { ok: true, .. }))
            .count(),
        2
    );
    // 新进程、没有后台任务：收尾判据是已收尾。
    assert!(facts.iter().any(|f| matches!(
        f,
        Convo::Tasks {
            drain: nd_backend::Drain::Drained
        }
    )));
}

fn echo_line(records: &[Record]) -> (usize, String) {
    records
        .iter()
        .enumerate()
        .find_map(|(i, r)| match &r.event {
            Event::Out { line } if line.contains("\"isReplay\":true") => Some((i, line.clone())),
            _ => None,
        })
        .unwrap()
}

#[test]
fn only_an_echo_with_the_original_uuid_counts_as_landing() {
    let records = conversation();
    let first_written = replay(&records)
        .into_iter()
        .flatten()
        .find_map(|f| match f {
            Convo::Written { uuid } => Some(uuid),
            _ => None,
        })
        .unwrap();
    let (index, line) = echo_line(&records);
    // 回显换成别的 uuid：那条消息没有回显。
    let mut altered = records.clone();
    altered[index].event = Event::Out {
        line: line.replace(&first_written, "00000000-0000-4000-8000-000000000000"),
    };
    let facts: Vec<Convo> = replay(&altered).into_iter().flatten().collect();
    assert!(!facts.contains(&Convo::Echo {
        uuid: first_written.clone()
    }));
    // 回显那一行没了：同一回合照样结束，result 也报了它的 uuid，但这不算落地。
    let mut dropped = records.clone();
    dropped.remove(index);
    let facts: Vec<Convo> = replay(&dropped).into_iter().flatten().collect();
    assert!(!facts.contains(&Convo::Echo {
        uuid: first_written.clone()
    }));
    assert!(
        facts
            .iter()
            .any(|f| matches!(f, Convo::TurnEnded { ok: true, .. }))
    );
    assert!(facts.contains(&Convo::Lifecycle {
        uuid: first_written,
        state: "started".into()
    }));
}

/// 对话投影的差分基准：同一组从录制回放出来的输入，最简版与增量版的结果一样。
#[test]
fn simple_and_incremental_projections_agree_on_the_recorded_conversation() {
    let records = conversation();
    let mut log = vec![];
    let mut turns = 0;
    for fact in replay(&records).into_iter().flatten() {
        match fact {
            Convo::Written { uuid } => log.push(Shown::Prompt {
                id: uuid.clone(),
                text: "…".into(),
                intent: "fold".into(),
                state: "written".into(),
                native: Some(uuid),
                reason: None,
            }),
            Convo::Echo { uuid } => log.push(Shown::Prompt {
                id: uuid.clone(),
                text: "…".into(),
                intent: "fold".into(),
                state: "landed".into(),
                native: Some(uuid),
                reason: None,
            }),
            Convo::Delta { item, kind, text } => log.push(Shown::Delta { item, kind, text }),
            Convo::Block { item } => log.push(Shown::Block { item }),
            Convo::TurnEnded {
                ok, subtype, error, ..
            } => {
                turns += 1;
                log.push(Shown::Turn {
                    carrier: "c".into(),
                    n: turns,
                    ok,
                    subtype,
                    error,
                });
            }
            _ => {}
        }
    }
    assert!(log.iter().any(Shown::is_delta));
    let simple = project(&log);
    let mut incremental = Projection::default();
    for shown in &log {
        incremental.apply(shown);
    }
    assert_eq!(simple, incremental.items());
    // 也在每个前缀上比：流式进行到一半时两边也一样（快照带进行中条目的累积内容）。
    for cut in 1..log.len() {
        let mut partial = Projection::default();
        for shown in &log[..cut] {
            partial.apply(shown);
        }
        assert_eq!(project(&log[..cut]), partial.items(), "prefix {cut}");
    }
}

#[test]
fn recorded_human_rounds_keep_their_user_uuids_and_final_assistant_anchor() {
    let facts: Vec<_> = replay(&conversation()).into_iter().flatten().collect();
    let mapped: Vec<_> = facts
        .iter()
        .filter_map(|f| match f {
            Convo::TurnMapped {
                turn,
                uuids,
                complete,
                last_assistant,
            } => Some((turn, uuids, complete, last_assistant)),
            _ => None,
        })
        .collect();
    let ended: Vec<_> = mapped.iter().filter(|(_, _, done, _)| **done).collect();
    assert_eq!(ended.len(), 2);
    assert_eq!(ended[0].1, &["8bbb9518-5d44-435a-af28-c83760afc30e"]);
    assert_eq!(ended[1].1, &["c2dcd113-067a-49d8-a5f4-706d4940060d"]);
    assert_ne!(ended[0].0, ended[1].0);
    for end in ended {
        assert!(end.3.is_some());
        assert!(
            mapped
                .iter()
                .any(|(id, ids, done, _)| id == &end.0 && !**done && ids == &end.1)
        );
    }
}
