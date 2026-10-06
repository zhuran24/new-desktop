use nd_claude_records::RecordIndex;
use serde_json::{Value, json};

fn transcript(rows: &[Value]) -> Vec<u8> {
    rows.iter()
        .flat_map(|row| {
            let mut bytes = serde_json::to_vec(row).unwrap();
            bytes.push(b'\n');
            bytes
        })
        .collect()
}

fn message(id: &str, parent: Option<&str>, role: &str, text: &str) -> Value {
    json!({"type":role,"uuid":id,"parentUuid":parent,
        "timestamp":"2026-10-06T12:00:00.000Z",
        "message":{"id":format!("msg_{id}"),"role":role,"content":text}})
}

#[test]
fn history_follows_the_current_leaf_and_excludes_a_rewound_branch() {
    let data = transcript(&[
        message("alpha", None, "user", "你好"),
        message("beta", Some("alpha"), "assistant", "old reply"),
        message("delta", Some("alpha"), "assistant", "new reply"),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    let history = index.current().unwrap();
    assert_eq!(history.leaf(), Some("delta"));
    assert_eq!(history.ids().collect::<Vec<_>>(), ["alpha", "delta"]);
}

#[test]
fn explicit_rewind_selects_an_old_leaf_until_new_main_conversation_arrives() {
    let mut rows = vec![
        message("alpha", None, "user", "first"),
        message("beta", Some("alpha"), "assistant", "old reply"),
        message("delta", Some("alpha"), "assistant", "new reply"),
        json!({"type":"last-prompt","leafUuid":"beta","explicit":true}),
        json!({"type":"custom-title","uuid":"metadata-id","customTitle":"changed"}),
        json!({"type":"assistant","uuid":"agent","parentUuid":null,"isSidechain":true,"message":{"role":"assistant","content":"side"}}),
        // CLI exit writes the same leaf again without explicit. Keep the pin.
        json!({"type":"last-prompt","leafUuid":"beta"}),
    ];
    let data = transcript(&rows);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(index.current().unwrap().leaf(), Some("beta"));
    rows.push(message("epsilon", Some("beta"), "user", "continue"));
    let data = transcript(&rows);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(index.current().unwrap().leaf(), Some("epsilon"));
}

#[test]
fn two_real_cli_compactions_retain_latest_summary_and_preserved_tail_only() {
    let data = include_bytes!("fixtures/claude-2.1.289.jsonl");
    let index = RecordIndex::parse(data).unwrap();
    let history = index.current().unwrap();
    // Independent observations: real CLI resume -> export + the next Messages request.
    let ids = history.ids().collect::<Vec<_>>();
    assert_eq!(history.leaf(), Some("1ca8fd4c-ec1e-4bad-b87c-b14aadcec784"));
    assert_eq!(
        &ids[..4],
        [
            "1b12fdca-fbca-4b1f-ba24-7999e3d54624", // second boundary
            "dd615cf4-f1bd-4858-b53d-9d138ed0d0f9", // latest summary
            "67981b44-bdfd-49a8-aac5-24ccbd983df7", // preserved assistant
            "b28bd732-194b-490b-b15b-8216eb1e97bf", // preserved attachment
        ]
    );
    assert!(!ids.contains(&"511eb979-1f1f-4796-8bda-4a0afbf4e918"));
    assert!(!ids.contains(&"96b41d87-3c1a-40a5-bedd-43edbe2f3259"));
}

#[test]
fn pages_read_exact_utf8_byte_ranges_without_repeating_or_skipping_records() {
    use std::num::NonZeroUsize;
    let data = transcript(&[
        message("a", None, "user", "你好🌍"),
        message("b", Some("a"), "assistant", "second"),
        message("c", Some("b"), "user", "third"),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    let history = index.current().unwrap();
    let page = history.page(None, NonZeroUsize::new(2).unwrap()).unwrap();
    assert_eq!(
        page.records.iter().map(|r| r.uuid()).collect::<Vec<_>>(),
        ["b", "c"]
    );
    assert_eq!(page.next_before, Some("b"));
    for record in &page.records {
        assert_eq!(record.raw(), &data[record.byte_range()]);
        assert_eq!(record.decode()["uuid"], record.uuid());
    }
    let older = history
        .page(page.next_before, NonZeroUsize::new(2).unwrap())
        .unwrap();
    assert_eq!(older.records.len(), 1);
    assert_eq!(older.records[0].decode()["message"]["content"], "你好🌍");
    assert_eq!(older.next_before, None);
    assert_eq!(
        index.record("c").unwrap().decode()["message"]["content"],
        "third"
    );
    assert!(
        history
            .page(Some("unknown"), NonZeroUsize::new(2).unwrap())
            .is_err()
    );
}

#[test]
fn explicit_empty_survives_metadata_but_new_conversation_clears_it() {
    let mut rows = vec![
        message("old", None, "user", "old"),
        json!({"type":"last-prompt","leafUuid":null,"explicit":true}),
        json!({"type":"last-prompt","lastPrompt":"title only"}),
    ];
    let data = transcript(&rows);
    let index = RecordIndex::parse(&data).unwrap();
    assert!(index.current().unwrap().is_cleared());
    assert_eq!(index.current().unwrap().leaf(), None);
    rows.push(message("new", None, "user", "new"));
    let data = transcript(&rows);
    let index = RecordIndex::parse(&data).unwrap();
    assert!(!index.current().unwrap().is_cleared());
    assert_eq!(index.current().unwrap().leaf(), Some("new"));
}

#[test]
fn without_last_prompt_timestamp_selection_searches_descendants_of_last_main_row() {
    let mut later = message("future-child", Some("root"), "assistant", "late");
    later["timestamp"] = json!("2026-10-06T12:01:00.000Z");
    let unrelated = message("unrelated", None, "assistant", "unrelated");
    // Child is written before its parent, as can happen with concurrent output.
    let mut rows = vec![later, unrelated, message("root", None, "user", "root")];
    let data = transcript(&rows);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(index.current().unwrap().leaf(), Some("future-child"));
    // Once leaf metadata exists, it selects its own branch even if the last row is elsewhere.
    rows.push(json!({"type":"last-prompt","leafUuid":"unrelated"}));
    let data = transcript(&rows);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(index.current().unwrap().leaf(), Some("unrelated"));
}

#[test]
fn legacy_preserved_segment_relinks_the_same_real_cli_tail() {
    // Compatibility derivative: same real recording, older compactMetadata shape.
    let rows = include_str!("fixtures/claude-2.1.289.jsonl")
        .lines()
        .map(|line| {
            let mut row: Value = serde_json::from_str(line).unwrap();
            if let Some(metadata) = row
                .get_mut("compactMetadata")
                .and_then(Value::as_object_mut)
            {
                metadata.remove("preservedMessages");
            }
            row
        })
        .collect::<Vec<_>>();
    let data = transcript(&rows);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(
        index.current().unwrap().ids().take(4).collect::<Vec<_>>(),
        [
            "1b12fdca-fbca-4b1f-ba24-7999e3d54624",
            "dd615cf4-f1bd-4858-b53d-9d138ed0d0f9",
            "67981b44-bdfd-49a8-aac5-24ccbd983df7",
            "b28bd732-194b-490b-b15b-8216eb1e97bf",
        ]
    );
}

#[test]
fn a_missing_selected_parent_requires_a_fuller_snapshot_instead_of_a_false_root() {
    let data = transcript(&[message(
        "tail",
        Some("not-in-this-read"),
        "assistant",
        "reply",
    )]);
    let index = RecordIndex::parse(&data).unwrap();
    assert!(matches!(
        index.current(),
        Err(nd_claude_records::Error::MissingParent { .. })
    ));
}

#[test]
fn a_torn_append_is_reported_at_its_byte_offset() {
    let mut data = transcript(&[message("a", None, "user", "你好")]);
    let offset = data.len();
    data.extend_from_slice(b"{\"type\":\"last-prompt\",\"leafUuid\":");
    assert!(
        matches!(RecordIndex::parse(&data), Err(nd_claude_records::Error::IncompleteRecord { byte_offset }) if byte_offset == offset)
    );
}

#[test]
fn leaf_is_a_message_but_history_keeps_its_trailing_attachments() {
    let data = transcript(&[
        message("a", None, "user", "prompt"),
        message("b", Some("a"), "assistant", "reply"),
        json!({"type":"attachment","uuid":"context","parentUuid":"b","timestamp":"2026-10-06T12:00:01.000Z","attachment":{"type":"future_context","unknown":42}}),
        json!({"type":"last-prompt","leafUuid":"context"}),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    let history = index.current().unwrap();
    assert_eq!(history.leaf(), Some("b"));
    assert_eq!(history.ids().collect::<Vec<_>>(), ["a", "b", "context"]);
    assert_eq!(
        index.record("context").unwrap().decode()["attachment"]["unknown"],
        42
    );
}

#[test]
fn broken_compact_preservation_never_silently_drops_context() {
    let data = transcript(&[
        message("old", None, "assistant", "history"),
        json!({"type":"system","subtype":"compact_boundary","uuid":"boundary","parentUuid":null,
            "compactMetadata":{"preservedMessages":{"anchorUuid":"summary","uuids":["missing"]}}}),
        message("summary", Some("boundary"), "user", "summary"),
        message("next", Some("summary"), "assistant", "reply"),
    ]);
    assert!(matches!(
        RecordIndex::parse(&data),
        Err(nd_claude_records::Error::BrokenCompaction { .. })
    ));
}

#[test]
fn legacy_progress_is_a_parent_bridge_not_a_history_item() {
    let data = transcript(&[
        message("a", None, "user", "prompt"),
        json!({"type":"progress","uuid":"p1","parentUuid":"a"}),
        json!({"type":"progress","uuid":"p2","parentUuid":"p1"}),
        message("b", Some("p2"), "assistant", "reply"),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(
        index.current().unwrap().ids().collect::<Vec<_>>(),
        ["a", "b"]
    );
    // Original evidence remains untouched, even though the selected chain bridges progress.
    assert_eq!(index.record("b").unwrap().decode()["parentUuid"], "p2");
}

#[test]
fn preserved_tail_is_the_leaf_even_when_summary_is_last_in_file() {
    let data = transcript(&[
        message("old", None, "user", "drop"),
        message("kept", Some("old"), "assistant", "keep"),
        json!({"type":"system","subtype":"compact_boundary","uuid":"boundary","parentUuid":null,
            "compactMetadata":{"preservedMessages":{"anchorUuid":"summary","uuids":["kept"]}}}),
        message("summary", Some("boundary"), "user", "summary"),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    let history = index.current().unwrap();
    assert_eq!(history.leaf(), Some("kept"));
    assert_eq!(
        history.ids().collect::<Vec<_>>(),
        ["boundary", "summary", "kept"]
    );
}

#[test]
fn malformed_conversation_envelopes_cannot_become_valid_empty_or_root_histories() {
    for row in [
        json!(17),
        json!({"type":"user","message":{"content":"lost UUID"}}),
        json!({"type":"user","uuid":"a","parentUuid":17,"message":{"content":"bad parent"}}),
        json!({"type":"assistant","uuid":"a","parentUuid":null,"message":"bad message"}),
        json!({"type":"user","uuid":"a","parentUuid":null,"message":{"content":17}}),
    ] {
        assert!(matches!(
            RecordIndex::parse(&transcript(&[row])),
            Err(nd_claude_records::Error::InvalidRecord { .. })
        ));
    }
}

#[test]
fn a_leaf_without_a_cli_parseable_timestamp_is_not_a_resume_baseline() {
    let mut row = message("a", None, "user", "prompt");
    for timestamp in [Value::Null, json!("not a date")] {
        row["timestamp"] = timestamp;
        let data = transcript(&[row.clone()]);
        let index = RecordIndex::parse(&data).unwrap();
        assert!(matches!(
            index.current(),
            Err(nd_claude_records::Error::InvalidTimestamp { .. })
        ));
    }
}

#[test]
fn parallel_assistant_blocks_and_sibling_tool_results_are_not_lost() {
    let mut a1 = message("a1", Some("u"), "assistant", "");
    a1["message"]["id"] = json!("same-reply");
    a1["message"]["content"] =
        json!([{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"one"}}]);
    let mut a2 = message("a2", Some("u"), "assistant", "");
    a2["message"]["id"] = json!("same-reply");
    a2["message"]["content"] =
        json!([{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"two"}}]);
    let mut r1 = message("r1", Some("a1"), "user", "");
    r1["message"]["content"] = json!([{"type":"tool_result","tool_use_id":"t1","content":"one"}]);
    let mut r2 = message("r2", Some("a2"), "user", "");
    r2["message"]["content"] = json!([{"type":"tool_result","tool_use_id":"t2","content":"two"}]);
    let data = transcript(&[
        message("u", None, "user", "read both"),
        a1,
        a2,
        r1,
        r2,
        message("done", Some("r2"), "assistant", "done"),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(
        index.current().unwrap().ids().collect::<Vec<_>>(),
        ["u", "a1", "a2", "r1", "r2", "done"]
    );
}

#[test]
fn compacted_away_sibling_blocks_cannot_reappear_through_batch_recovery() {
    let mut discarded = message("discarded", Some("u"), "assistant", "old block");
    discarded["message"]["id"] = json!("reply");
    let mut kept = message("kept", Some("u"), "assistant", "tail block");
    kept["message"]["id"] = json!("reply");
    let data = transcript(&[
        message("u", None, "user", "old"),
        discarded,
        kept,
        json!({"type":"system","subtype":"compact_boundary","uuid":"boundary","parentUuid":null,
            "compactMetadata":{"preservedMessages":{"anchorUuid":"summary","uuids":["kept"]}}}),
        message("summary", Some("boundary"), "user", "summary"),
        message("next", Some("summary"), "user", "next"),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(
        index.current().unwrap().ids().collect::<Vec<_>>(),
        ["boundary", "summary", "kept", "next"]
    );
    assert!(index.record("discarded").is_some());
}

#[test]
fn rewritten_uuid_uses_latest_bytes_in_lookup_and_history() {
    let data = transcript(&[
        message("u", None, "user", "prompt"),
        message("a", Some("u"), "assistant", "original"),
        message("a", Some("u"), "assistant", "corrected"),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    let history = index.current().unwrap();
    let page = history
        .page(None, std::num::NonZeroUsize::new(10).unwrap())
        .unwrap();
    assert_eq!(page.records.len(), 2);
    assert_eq!(page.records[1].decode()["message"]["content"], "corrected");
    assert_eq!(
        page.records[1].byte_range(),
        index.record("a").unwrap().byte_range()
    );
}

#[test]
fn timestamp_fallback_uses_descendants_even_when_parent_clock_is_ahead() {
    let mut child = message("child", Some("root"), "assistant", "child");
    child["timestamp"] = json!("2026-10-06T11:00:00.000Z");
    let mut other = message("other", None, "assistant", "other tree");
    other["timestamp"] = json!("2026-10-06T13:00:00.000Z");
    let data = transcript(&[child, other, message("root", None, "user", "root")]);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(index.current().unwrap().leaf(), Some("child"));
}

#[test]
fn a_later_parallel_tool_result_advances_an_ordinary_pin_but_not_an_explicit_rewind() {
    let mut assistant = message("a", Some("u"), "assistant", "");
    assistant["message"]["content"] = json!([
        {"type":"tool_use","id":"t1","name":"Read","input":{}},
        {"type":"tool_use","id":"t2","name":"Read","input":{}},
    ]);
    let result = |id: &str, tool: &str| {
        let mut row = message(id, Some("a"), "user", "");
        row["message"]["content"] =
            json!([{"type":"tool_result","tool_use_id":tool,"content":"ok"}]);
        row
    };
    for (explicit, expected) in [(false, "r2"), (true, "r1")] {
        let data = transcript(&[
            message("u", None, "user", "read both"),
            assistant.clone(),
            result("r1", "t1"),
            json!({"type":"last-prompt","leafUuid":"r1","explicit":explicit}),
            result("r2", "t2"),
        ]);
        let index = RecordIndex::parse(&data).unwrap();
        assert_eq!(index.current().unwrap().leaf(), Some(expected));
    }
}

#[test]
fn large_history_pages_cover_every_record_with_offsets_past_five_mib() {
    use std::num::NonZeroUsize;
    let mut data = Vec::new();
    let mut parent = None;
    for i in 0..8192 {
        let id = format!("row-{i}");
        let row = message(&id, parent.as_deref(), "user", &"分页内容".repeat(80));
        data.extend(transcript(&[row]));
        parent = Some(id);
    }
    assert!(data.len() > 5 * 1024 * 1024);
    let index = RecordIndex::parse(&data).unwrap();
    let history = index.current().unwrap();
    assert_eq!(history.leaf(), Some("row-8191"));
    let mut before = None;
    let mut pages = Vec::new();
    loop {
        let page = history
            .page(before, NonZeroUsize::new(97).unwrap())
            .unwrap();
        before = page.next_before;
        pages.push(page.records);
        if before.is_none() {
            break;
        }
    }
    let records = pages.into_iter().rev().flatten().collect::<Vec<_>>();
    assert_eq!(records.len(), 8192);
    for (i, record) in records.iter().enumerate() {
        assert_eq!(record.uuid(), format!("row-{i}"));
        assert_eq!(record.raw(), &data[record.byte_range()]);
    }
    assert!(records.last().unwrap().byte_range().start > 5 * 1024 * 1024);
}

#[test]
fn a_parent_cycle_is_rejected_without_returning_a_partial_history() {
    let data = transcript(&[
        message("a", Some("b"), "user", "a"),
        message("b", Some("a"), "assistant", "b"),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    assert!(matches!(
        index.current(),
        Err(nd_claude_records::Error::ParentCycle { .. })
    ));
}

#[test]
fn empty_snapshot_differs_from_explicitly_cleared_history() {
    let index = RecordIndex::parse(b"").unwrap();
    let history = index.current().unwrap();
    assert!(!history.is_cleared());
    assert_eq!(history.leaf(), None);
    assert!(
        history
            .page(None, std::num::NonZeroUsize::new(1).unwrap())
            .unwrap()
            .records
            .is_empty()
    );
}

#[test]
fn fork_briefing_does_not_invalidate_an_explicit_leaf() {
    let data = transcript(&[
        message("u", None, "user", "prompt"),
        message("a", Some("u"), "assistant", "before rewind"),
        json!({"type":"last-prompt","leafUuid":"u","explicit":true}),
        json!({"type":"attachment","uuid":"briefing","parentUuid":"a",
            "timestamp":"2026-10-06T12:00:01.000Z","attachment":{"type":"fork_briefing"}}),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(index.current().unwrap().leaf(), Some("u"));
}

#[test]
fn parallel_results_with_a_stale_parent_are_recovered_by_unique_call_id() {
    let mut assistant = message("a", Some("u"), "assistant", "");
    assistant["message"]["content"] = json!([
        {"type":"tool_use","id":"t1","name":"Read","input":{}},
        {"type":"tool_use","id":"t2","name":"Read","input":{}},
    ]);
    let mut r1 = message("r1", Some("u"), "user", "");
    r1["sourceToolAssistantUUID"] = json!("a");
    r1["message"]["content"] = json!([{"type":"tool_result","tool_use_id":"t1","content":"first"}]);
    let mut r2 = message("r2", Some("a"), "user", "");
    r2["message"]["content"] =
        json!([{"type":"tool_result","tool_use_id":"t2","content":"second"}]);
    let data = transcript(&[
        message("u", None, "user", "read"),
        assistant,
        r1,
        r2,
        message("done", Some("r2"), "assistant", "done"),
    ]);
    let index = RecordIndex::parse(&data).unwrap();
    assert_eq!(
        index.current().unwrap().ids().collect::<Vec<_>>(),
        ["u", "a", "r1", "r2", "done"]
    );
}

#[test]
fn real_cli_prefixes_select_rewind_and_new_branch_before_compaction() {
    let data = include_bytes!("fixtures/claude-2.1.289.jsonl");
    let mut end = 0;
    let mut checked_rewind = false;
    for line in data.split_inclusive(|b| *b == b'\n') {
        let row: Value = serde_json::from_slice(line).unwrap();
        if row["subtype"] == "compact_boundary" {
            let index = RecordIndex::parse(&data[..end]).unwrap();
            let history = index.current().unwrap();
            assert_eq!(history.leaf(), Some("13cc7a33-7991-4d8a-8452-feb2b2e94031"));
            let ids = history.ids().collect::<Vec<_>>();
            assert!(ids.contains(&"a353e706-bf3b-4ce7-9e0f-fe96e0c29dd4"));
            assert!(!ids.contains(&"511eb979-1f1f-4796-8bda-4a0afbf4e918"));
            assert!(!ids.contains(&"8278c693-389f-43ce-aaaa-ae6bf9fce824"));
            assert!(checked_rewind);
            return;
        }
        end += line.len();
        if row["type"] == "last-prompt" && row["explicit"] == true {
            let index = RecordIndex::parse(&data[..end]).unwrap();
            assert_eq!(
                index.current().unwrap().leaf(),
                Some("658049da-8517-4275-aa1b-cacd275dda35")
            );
            checked_rewind = true;
        }
    }
    panic!("missing real CLI compact boundary");
}
