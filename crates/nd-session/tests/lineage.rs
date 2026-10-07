//! 谱系的纯函数接缝：落地消息和实际回合是两种事实。
use nd_backend::{BackendSessionId, CarrierId, Ticket};
use nd_session::lineage::{Event, Lineage, NativePosition};

fn position(native: &str) -> NativePosition {
    NativePosition {
        carrier: CarrierId::from("c1"),
        backend_session: BackendSessionId::claude("bs1"),
        native: native.into(),
    }
}
fn root() -> Lineage {
    Lineage::default()
        .fold(&Event::Root {
            segment: "s1".into(),
            carrier: "c1".into(),
            backend_session: BackendSessionId::claude("bs1"),
        })
        .unwrap()
}
fn land(lineage: Lineage, message: &str, ticket: &str, native: &str) -> Lineage {
    lineage
        .fold(&Event::Landed {
            message: message.into(),
            ticket: Ticket::from(ticket),
            position: position(native),
        })
        .unwrap()
}
fn observe(lineage: Lineage, key: &str, natives: &[&str], complete: bool) -> Lineage {
    lineage
        .fold(&Event::TurnObserved {
            carrier: "c1".into(),
            backend_session: BackendSessionId::claude("bs1"),
            key: key.into(),
            natives: natives.iter().map(|s| s.to_string()).collect(),
            complete,
            last_assistant: None,
        })
        .unwrap()
}
#[test]
fn clear_rebinds_the_same_carrier_without_mixing_old_and_new_rounds() {
    let before = observe(
        land(root(), "old", "old#1", "old-u"),
        "old-t",
        &["old-u"],
        true,
    );
    let clear = Event::Branch {
        segment: "z-clear".into(),
        from: "s1".into(),
        through: None,
        kind: nd_session::lineage::BranchKind::Clear,
        carrier: "c1".into(),
        backend_session: BackendSessionId::claude("bs2"),
    };
    let cleared = before.fold(&clear).unwrap();
    let landed = cleared
        .fold(&Event::Landed {
            message: "new".into(),
            ticket: "new#1".into(),
            position: NativePosition {
                carrier: "c1".into(),
                backend_session: BackendSessionId::claude("bs2"),
                native: "new-u".into(),
            },
        })
        .unwrap();
    let after = landed
        .fold(&Event::TurnObserved {
            carrier: "c1".into(),
            backend_session: BackendSessionId::claude("bs2"),
            key: "new-t".into(),
            natives: vec!["new-u".into()],
            complete: true,
            last_assistant: None,
        })
        .unwrap();
    assert_eq!(after.turns("s1").unwrap()[0].messages, ["old"]);
    assert_eq!(after.turns("z-clear").unwrap()[0].messages, ["new"]);
    assert!(after.common_prefix("s1", "z-clear").unwrap().is_empty());
    assert_eq!(after.fold(&clear).unwrap(), after);
    let restored: Lineage = serde_json::from_str(&serde_json::to_string(&after).unwrap()).unwrap();
    assert_eq!(restored, after);
}

#[test]
fn landed_prompts_open_one_round_only_when_their_actual_turn_is_known() {
    let landed = land(
        land(root(), "message-a", "ticket-a#1", "uuid-a"),
        "message-b",
        "ticket-b#1",
        "uuid-b",
    );
    assert!(landed.turns("s1").unwrap().is_empty());
    let running = observe(landed, "native-turn", &["uuid-a", "uuid-b"], false);
    let turns = running.turns("s1").unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].messages, ["message-a", "message-b"]);
    assert_eq!(turns[0].positions, [position("uuid-a"), position("uuid-b")]);
    assert!(!turns[0].complete);
    let id = turns[0].id.clone();
    let ended = observe(running, "native-turn", &["uuid-a", "uuid-b"], true);
    assert_eq!(ended.turns("s1").unwrap()[0].id, id);
    assert!(ended.turns("s1").unwrap()[0].complete);
    assert_eq!(
        observe(ended.clone(), "native-turn", &["uuid-a", "uuid-b"], true),
        ended
    );
}

#[test]
fn rewind_keeps_the_old_path_and_nested_branches_share_only_the_explicit_prefix() {
    use nd_session::lineage::BranchKind;
    let one = observe(land(root(), "a", "a#1", "ua"), "ta", &["ua"], true);
    let first = one.turns("s1").unwrap()[0].id.clone();
    let two = observe(land(one, "b", "b#1", "ub"), "tb", &["ub"], true);
    let branched = two
        .fold(&Event::Branch {
            segment: "s2".into(),
            from: "s1".into(),
            through: Some(first.clone()),
            kind: BranchKind::Rewind,
            carrier: "c2".into(),
            backend_session: BackendSessionId::claude("bs2"),
        })
        .unwrap();
    assert_eq!(branched.inactive_messages(), ["b"]);
    assert_eq!(
        land(branched.clone(), "unmapped", "unmapped#1", "unmapped-u").inactive_messages(),
        ["b"]
    );
    assert_eq!(branched.current(), Some("s2"));
    assert_eq!(branched.turns("s1").unwrap().len(), 2);
    assert_eq!(branched.turns("s2").unwrap().len(), 1);
    assert_eq!(
        branched.common_prefix("s1", "s2").unwrap(),
        std::slice::from_ref(&first)
    );
    let empty = branched
        .fold(&Event::Branch {
            segment: "s3".into(),
            from: "s2".into(),
            through: None,
            kind: BranchKind::Rewind,
            carrier: "c3".into(),
            backend_session: BackendSessionId::claude("bs3"),
        })
        .unwrap();
    let mut inactive = empty.inactive_messages();
    inactive.sort();
    assert_eq!(inactive, ["a", "b"]);
    assert!(
        empty
            .fold(&Event::Activate {
                segment: "s1".into()
            })
            .unwrap()
            .inactive_messages()
            .is_empty()
    );
    assert!(empty.common_prefix("s1", "s3").unwrap().is_empty());
    let topology = empty.topology();
    assert_eq!(
        topology
            .iter()
            .map(|n| (
                n.segment.as_str(),
                n.parent.as_deref(),
                n.shared_rounds,
                n.current
            ))
            .collect::<Vec<_>>(),
        [
            ("s1", None, 0, false),
            ("s2", Some("s1"), 1, false),
            ("s3", Some("s2"), 0, true)
        ]
    );
    let restored = empty
        .fold(&Event::Activate {
            segment: "s1".into(),
        })
        .unwrap();
    assert_eq!(restored.current(), Some("s1"));
    assert_eq!(restored.turns("s1").unwrap().len(), 2);
    assert_eq!(restored.turns("s2").unwrap()[0].id, first);
}

#[test]
fn switching_backends_keeps_the_segment_and_tracks_intervals_and_imported_positions() {
    let first = observe(land(root(), "a", "a#1", "ua"), "ta", &["ua"], true);
    let turn = first.turns("s1").unwrap()[0].id.clone();
    let switched = first
        .fold(&Event::SwitchBackend {
            segment: "s1".into(),
            carrier: "codex".into(),
            backend_session: BackendSessionId::codex("thread"),
        })
        .unwrap();
    assert_eq!(switched.current(), Some("s1"));
    assert_eq!(switched.topology().len(), 1);
    let bindings = &switched.segment("s1").unwrap().bindings;
    assert_eq!(
        (
            bindings[0].from,
            bindings[0].to,
            bindings[1].from,
            bindings[1].to
        ),
        (0, Some(1), 1, None)
    );
    let target = NativePosition {
        carrier: "codex".into(),
        backend_session: BackendSessionId::codex("thread"),
        native: "turn-remote".into(),
    };
    let imported = switched
        .fold(&Event::Imported {
            segment: "s1".into(),
            carrier: "codex".into(),
            through: turn.clone(),
            positions: vec![(turn, target.clone())],
        })
        .unwrap();
    assert_eq!(
        imported.turns("s1").unwrap()[0].positions,
        [position("ua"), target]
    );
    assert!(
        imported
            .sync_point(&CarrierId::from("codex"))
            .unwrap()
            .invalid
            .is_none()
    );
    let invalid = imported
        .fold(&Event::InvalidateSync {
            carrier: "codex".into(),
            reason: "压缩跨过同步点".into(),
        })
        .unwrap();
    assert_eq!(
        invalid
            .sync_point(&CarrierId::from("codex"))
            .unwrap()
            .invalid
            .as_deref(),
        Some("压缩跨过同步点")
    );
    assert_eq!(invalid.turns("s1").unwrap().len(), 1);
    assert_eq!(invalid.switches().len(), 1);
}

#[test]
fn a_session_fork_carries_its_explicit_origin_and_shares_turn_identity() {
    use nd_session::lineage::SegmentTarget;
    let source = observe(land(root(), "a", "a#1", "ua"), "ta", &["ua"], true);
    let id = source.turns("s1").unwrap()[0].id.clone();
    let child = source
        .fork(
            "session-original".into(),
            "s1",
            Some(id.clone()),
            SegmentTarget {
                segment: "child".into(),
                carrier: "child-carrier".into(),
                backend_session: BackendSessionId::claude("child-bs"),
            },
        )
        .unwrap();
    assert_eq!(source.current(), Some("s1"));
    assert_eq!(child.current(), Some("child"));
    assert_eq!(
        child.common_prefix_with("child", &source, "s1").unwrap(),
        [id]
    );
    let origin = child.origin().unwrap();
    assert_eq!(origin.session.as_str(), "session-original");
    assert_eq!(origin.segment, "s1");
    assert_eq!(child.topology().len(), 1);
    // 相同文字/相同界面消息名不会使独立根段凭空产生亲缘。
    let unrelated = observe(
        land(root(), "a", "different#1", "different"),
        "other-turn",
        &["different"],
        true,
    );
    assert!(
        child
            .common_prefix_with("child", &unrelated, "s1")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn late_completion_and_repeated_native_positions_do_not_invent_new_human_rounds() {
    let running = observe(
        land(root(), "same-message", "retry#2", "new-uuid"),
        "native-turn",
        &["new-uuid"],
        false,
    );
    let id = running.turns("s1").unwrap()[0].id.clone();
    // 终结事实可以只指向已经认出的回合；重复报告不倒退 complete。
    let ended = observe(running, "native-turn", &[], true);
    assert!(ended.turns("s1").unwrap()[0].complete);
    let repeated = observe(ended, "background-followup", &["new-uuid"], true);
    assert_eq!(repeated.turns("s1").unwrap().len(), 1);
    assert_eq!(repeated.turns("s1").unwrap()[0].id, id);
    assert_eq!(
        observe(repeated.clone(), "unknown-input", &["not-echoed"], true),
        repeated
    );
}

#[test]
fn conflicting_identities_are_rejected_without_changing_the_lineage() {
    let state = land(root(), "a", "a#1", "ua");
    let before = state.clone();
    assert!(
        state
            .fold(&Event::Root {
                segment: "s1".into(),
                carrier: "different".into(),
                backend_session: BackendSessionId::claude("wrong")
            })
            .is_err()
    );
    assert!(
        state
            .fold(&Event::Landed {
                message: "other".into(),
                ticket: "other#1".into(),
                position: position("ua")
            })
            .is_err()
    );
    assert!(
        state
            .fold(&Event::Branch {
                segment: "s2".into(),
                from: "s1".into(),
                through: None,
                kind: nd_session::lineage::BranchKind::Rewind,
                carrier: "c1".into(),
                backend_session: BackendSessionId::claude("bs1")
            })
            .is_err()
    );
    assert_eq!(state, before);
}

#[test]
fn clear_external_continuation_and_invalid_cut_points_preserve_the_source() {
    use nd_session::lineage::BranchKind;
    let running = observe(land(root(), "a", "a#1", "ua"), "ta", &["ua"], false);
    let turn = running.turns("s1").unwrap()[0].id.clone();
    let branch = Event::Branch {
        segment: "outside".into(),
        from: "s1".into(),
        through: Some(turn.clone()),
        kind: BranchKind::ExternalContinuation,
        carrier: "external".into(),
        backend_session: BackendSessionId::claude("outside"),
    };
    assert!(running.fold(&branch).is_err());
    let ended = observe(running, "ta", &["ua"], true);
    let outside = ended.fold(&branch).unwrap();
    assert_eq!(outside.current(), Some("s1"));
    assert_eq!(outside.common_prefix("s1", "outside").unwrap(), [turn]);
    assert_eq!(outside.fold(&branch).unwrap(), outside);
    let clear = Event::Branch {
        segment: "clear".into(),
        from: "s1".into(),
        through: None,
        kind: BranchKind::Clear,
        carrier: "cleared".into(),
        backend_session: BackendSessionId::claude("cleared"),
    };
    let cleared = outside.fold(&clear).unwrap();
    assert_eq!(cleared.current(), Some("clear"));
    assert!(cleared.turns("clear").unwrap().is_empty());
    assert_eq!(cleared.turns("s1").unwrap().len(), 1);
    assert!(
        cleared
            .fold(&Event::Activate {
                segment: "absent".into()
            })
            .is_err()
    );
    assert!(
        cleared
            .fold(&Event::Branch {
                segment: "bad".into(),
                from: "s1".into(),
                through: Some("absent".into()),
                kind: BranchKind::Rewind,
                carrier: "bad".into(),
                backend_session: BackendSessionId::claude("bad")
            })
            .is_err()
    );
}

proptest::proptest! {
    #[test]
    fn every_rewind_cut_has_the_specified_prefix_and_survives_serialization(rounds in 1usize..15, cut in 0usize..15) {
        use nd_session::lineage::BranchKind;
        let mut source = root();
        for n in 0..rounds {
            source = observe(land(source, &format!("m{n}"), &format!("t{n}"), &format!("u{n}")), &format!("turn{n}"), &[&format!("u{n}")], true);
        }
        let count = cut.min(rounds);
        let through = count.checked_sub(1).map(|n| source.turns("s1").unwrap()[n].id.clone());
        let event = Event::Branch { segment: "earlier-lexically".into(), from: "s1".into(), through, kind: BranchKind::Rewind,
            carrier: "branch".into(), backend_session: BackendSessionId::claude("branch") };
        let branch = source.fold(&event).unwrap();
        proptest::prop_assert_eq!(branch.common_prefix("s1", "earlier-lexically").unwrap().len(), count);
        proptest::prop_assert_eq!(branch.turns("s1").unwrap().len(), rounds);
        proptest::prop_assert_eq!(&branch.topology()[0].segment, "s1");
        let restored: Lineage = serde_json::from_str(&serde_json::to_string(&branch).unwrap()).unwrap();
        proptest::prop_assert_eq!(restored.fold(&event).unwrap(), branch);
    }
}

#[test]
fn an_import_records_a_mirrors_sync_point_before_it_becomes_current() {
    let source = observe(land(root(), "a", "a#1", "ua"), "ta", &["ua"], true);
    let turn = source.turns("s1").unwrap()[0].id.clone();
    let prepared = source
        .fold(&Event::CarrierKnown {
            segment: "s1".into(),
            carrier: "mirror".into(),
            backend_session: BackendSessionId::codex("thread"),
        })
        .unwrap();
    let imported = prepared
        .fold(&Event::Imported {
            segment: "s1".into(),
            carrier: "mirror".into(),
            through: turn.clone(),
            positions: vec![(
                turn,
                NativePosition {
                    carrier: "mirror".into(),
                    backend_session: BackendSessionId::codex("thread"),
                    native: "remote-turn".into(),
                },
            )],
        })
        .unwrap();
    assert_eq!(
        imported
            .segment("s1")
            .unwrap()
            .bindings
            .last()
            .unwrap()
            .carrier
            .as_str(),
        "c1"
    );
    assert!(imported.switches().is_empty());
    assert_eq!(imported.sync_point(&"mirror".into()).unwrap().segment, "s1");
    let uncertain = prepared
        .fold(&Event::InvalidateSync {
            carrier: "mirror".into(),
            reason: "导入结果不明".into(),
        })
        .unwrap();
    assert_eq!(
        uncertain.sync_point(&"mirror".into()).unwrap().segment,
        "s1"
    );
    assert_eq!(
        uncertain.turns("s1").unwrap()[0].positions,
        [position("ua")]
    );
    // 只有落定事实才关闭旧承载区间。
    let settled = imported
        .fold(&Event::SwitchBackend {
            segment: "s1".into(),
            carrier: "mirror".into(),
            backend_session: BackendSessionId::codex("thread"),
        })
        .unwrap();
    assert_eq!(settled.segment("s1").unwrap().bindings.len(), 2);
}

#[test]
fn replaying_an_older_turn_prefix_does_not_move_the_completed_fork_anchor() {
    let landed = land(root(), "a", "a#1", "ua");
    let early = Event::TurnObserved {
        carrier: "c1".into(),
        backend_session: BackendSessionId::claude("bs1"),
        key: "turn".into(),
        natives: vec!["ua".into()],
        complete: false,
        last_assistant: Some("first-assistant".into()),
    };
    let running = landed.fold(&early).unwrap();
    let ended = running
        .fold(&Event::TurnObserved {
            carrier: "c1".into(),
            backend_session: BackendSessionId::claude("bs1"),
            key: "turn".into(),
            natives: vec!["ua".into()],
            complete: true,
            last_assistant: Some("final-assistant".into()),
        })
        .unwrap();
    assert_eq!(ended.fold(&early).unwrap(), ended);
    let followup = ended
        .fold(&Event::TurnObserved {
            carrier: "c1".into(),
            backend_session: BackendSessionId::claude("bs1"),
            key: "background-followup".into(),
            natives: vec!["ua".into()],
            complete: true,
            last_assistant: Some("background-assistant".into()),
        })
        .unwrap();
    assert_eq!(
        followup.turns("s1").unwrap()[0]
            .last_assistant
            .as_ref()
            .unwrap()
            .native,
        "final-assistant"
    );
}

#[test]
fn imported_native_positions_cannot_identify_two_different_rounds() {
    let first = observe(land(root(), "a", "a#1", "ua"), "ta", &["ua"], true);
    let second = observe(land(first, "b", "b#1", "ub"), "tb", &["ub"], true);
    let ids: Vec<_> = second
        .turns("s1")
        .unwrap()
        .iter()
        .map(|t| t.id.clone())
        .collect();
    let target = second
        .fold(&Event::CarrierKnown {
            segment: "s1".into(),
            carrier: "mirror".into(),
            backend_session: BackendSessionId::codex("mirror"),
        })
        .unwrap();
    let native = NativePosition {
        carrier: "mirror".into(),
        backend_session: BackendSessionId::codex("mirror"),
        native: "same-native-turn".into(),
    };
    assert!(
        target
            .fold(&Event::Imported {
                segment: "s1".into(),
                carrier: "mirror".into(),
                through: ids[1].clone(),
                positions: vec![(ids[0].clone(), native.clone()), (ids[1].clone(), native)]
            })
            .is_err()
    );
}
