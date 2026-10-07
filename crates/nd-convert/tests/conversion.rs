use nd_convert::{BackendKind, FrozenInput, NativeItem, convert};
use serde_json::{Value, json};

fn frozen(backend: BackendKind, values: Vec<Value>) -> FrozenInput {
    FrozenInput {
        backend,
        source_id: "source-session".into(),
        epoch: "effective-history-1".into(),
        complete: true,
        images: Default::default(),
        items: values
            .into_iter()
            .enumerate()
            .map(|(i, payload)| NativeItem {
                position: format!("position-{i}"),
                payload,
            })
            .collect(),
    }
}

#[test]
fn unavailable_file_images_preserve_the_conversation_and_report_loss() {
    for (backend, target, block) in [
        (
            BackendKind::Codex,
            BackendKind::Claude,
            json!({"type":"image","fileId":"file_abc"}),
        ),
        (
            BackendKind::Claude,
            BackendKind::Codex,
            json!({"type":"image","source":{"type":"file","file_id":"file_xyz"}}),
        ),
        (
            BackendKind::Codex,
            BackendKind::Claude,
            json!({"type":"input_image","file_id":"future_file"}),
        ),
    ] {
        let input = frozen(
            backend,
            vec![json!({"type":"message","role":"user","content":[
                {"type":"text","text":"keep this text"}, block
            ]})],
        );
        let result = convert(&input, target, None)
            .expect("unavailable image is a reported loss, not a broken conversation");
        assert!(
            serde_json::to_string(&result.items)
                .unwrap()
                .contains("keep this text")
        );
        assert_eq!(result.loss.entries.len(), 1);
        assert_eq!(result.loss.entries[0].reason, "image_unavailable");
        assert!(
            convert(&input, target, Some(&result.sync))
                .unwrap()
                .items
                .is_empty()
        );
    }
}

#[test]
fn parallel_tools_keep_all_arguments_results_and_report_text_downgrade() {
    let input = frozen(
        BackendKind::Claude,
        vec![
            json!({"role":"assistant","content":[
                {"type":"tool_use","id":"a","name":"Edit","input":{"old_string":"旧","new_string":"新"}},
                {"type":"tool_use","id":"b","name":"Write","input":{"content":"完整文件"}}
            ]}),
            json!({"role":"user","content":[
                {"type":"tool_result","tool_use_id":"b","content":"B_RESULT"},
                {"type":"tool_result","tool_use_id":"a","content":"A_FAILED","is_error":true}
            ]}),
        ],
    );
    let result = convert(&input, BackendKind::Codex, None).unwrap();
    let text = serde_json::to_string(&result.items).unwrap();
    for marker in ["旧", "新", "完整文件", "B_RESULT", "A_FAILED"] {
        assert!(text.contains(marker));
    }
    assert!(result.items.iter().all(|m| m["role"] == "assistant"));
    assert_eq!(result.loss.entries.len(), 4);
    let same = convert(&input, BackendKind::Claude, None).unwrap();
    assert_eq!(
        same.items[0]["message"]["content"],
        input.items[0].payload["content"]
    );
    assert_eq!(
        same.items[1]["message"]["content"],
        input.items[1].payload["content"]
    );
}

#[test]
fn user_and_assistant_text_convert_in_both_directions() {
    let input = frozen(
        BackendKind::Claude,
        vec![
            json!({"role":"user","content":"问题：你好"}),
            json!({"role":"assistant","content":[{"type":"text","text":"回答"}]}),
        ],
    );
    let converted = convert(&input, BackendKind::Codex, None).unwrap();
    assert_eq!(
        converted.items,
        vec![
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"问题：你好"}]}),
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"回答"}]}),
        ]
    );
    let back = convert(
        &frozen(BackendKind::Codex, converted.items),
        BackendKind::Claude,
        None,
    )
    .unwrap();
    assert_eq!(
        back.items[0]["message"]["content"],
        json!([{"type":"text","text":"问题：你好"}])
    );
    assert_eq!(
        back.items[1]["message"]["content"],
        json!([{"type":"text","text":"回答"}])
    );
    assert_eq!(back.items[0]["shouldQuery"], false);
    assert_eq!(back.items[0]["client_composed"], true);
}

#[test]
fn incremental_conversion_requires_the_exact_original_prefix() {
    let mut input = frozen(
        BackendKind::Claude,
        vec![json!({"role":"user","content":"已同步"})],
    );
    let first = convert(&input, BackendKind::Codex, None).unwrap();
    assert!(
        convert(&input, BackendKind::Codex, Some(&first.sync))
            .unwrap()
            .items
            .is_empty()
    );
    input.items.push(NativeItem {
        position: "new".into(),
        payload: json!({"role":"assistant","content":"新增"}),
    });
    let next = convert(&input, BackendKind::Codex, Some(&first.sync)).unwrap();
    assert_eq!(next.items.len(), 1);
    assert_eq!(next.items[0]["content"][0]["text"], "新增");
    let serialized = serde_json::to_string(&first.sync).unwrap();
    let saved = serde_json::from_str(&serialized).unwrap();
    assert_eq!(
        convert(&input, BackendKind::Codex, Some(&saved)).unwrap(),
        next
    );
    input.items[0].payload["content"] = json!("压缩替换");
    assert!(convert(&input, BackendKind::Codex, Some(&saved)).is_err());
}

#[test]
fn signed_thinking_is_kept_for_its_own_backend_and_never_sent_to_the_other() {
    let input = frozen(
        BackendKind::Claude,
        vec![json!({"role":"assistant","model":"claude-test","content":[
        {"type":"thinking","thinking":"PRIVATE_REASONING","signature":"PRIVATE_SIGNATURE"},
        {"type":"redacted_thinking","data":"PRIVATE_REDACTION"},
        {"type":"text","text":"公开回答","citations":[{"future":true}]}
    ],"future_metadata":{"x":1}})],
    );
    let decoded = nd_convert::decode(&input).unwrap();
    assert_eq!(decoded.entries[0].native, input.items[0]);
    let saved: nd_convert::Decoded =
        serde_json::from_value(serde_json::to_value(&decoded).unwrap()).unwrap();
    let same = nd_convert::encode(&saved, BackendKind::Claude).unwrap();
    assert_eq!(
        same.items[0]["message"]["content"][0],
        input.items[0].payload["content"][0]
    );
    assert_eq!(same.items[0]["message"]["model"], "claude-test");
    let cross = nd_convert::encode(&saved, BackendKind::Codex).unwrap();
    let text = serde_json::to_string(&cross.items).unwrap();
    assert!(!text.contains("PRIVATE_"));
    assert!(text.contains("公开回答"));
    assert_eq!(
        cross
            .loss
            .entries
            .iter()
            .filter(|l| l.reason == "native_reasoning_omitted")
            .count(),
        2
    );
}

#[test]
fn inline_images_round_trip_without_reading_any_file_or_url() {
    let input = frozen(
        BackendKind::Claude,
        vec![json!({"role":"user","content":[
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGVsbG8="}}
        ]})],
    );
    let result = convert(&input, BackendKind::Codex, None).unwrap();
    assert_eq!(
        result.items[0]["content"][0],
        json!({"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="})
    );
    let back = convert(
        &frozen(BackendKind::Codex, result.items),
        BackendKind::Claude,
        None,
    )
    .unwrap();
    assert_eq!(
        back.items[0]["message"]["content"],
        input.items[0].payload["content"]
    );
}

#[test]
fn recorded_codex_turn_replays_commands_and_file_changes_as_paired_history() {
    let input = frozen(
        BackendKind::Codex,
        serde_json::from_str(include_str!("fixtures/codex-turn.json")).unwrap(),
    );
    let result = convert(&input, BackendKind::Claude, None).unwrap();
    let blocks: Vec<_> = result
        .items
        .iter()
        .flat_map(|m| m["message"]["content"].as_array().unwrap())
        .collect();
    let calls: Vec<_> = blocks.iter().filter(|b| b["type"] == "tool_use").collect();
    let results: Vec<_> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .collect();
    assert_eq!(calls.len(), 4);
    assert_eq!(results.len(), 4);
    for call in &calls {
        assert_eq!(
            results
                .iter()
                .filter(|r| r["tool_use_id"] == call["id"])
                .count(),
            1
        );
    }
    let write = calls.iter().find(|c| c["name"] == "Write").unwrap();
    assert_eq!(
        write["input"],
        json!({"file_path":"/tmp/audit/proj/docs/plan.md","content":"# Plan\nstep one\n"})
    );
    let edit = calls.iter().find(|c| c["name"] == "Edit").unwrap();
    assert_eq!(
        edit["input"],
        json!({"file_path":"/tmp/audit/proj/notes.txt","old_string":"alpha\nbeta\n","new_string":"alpha\ngamma\n"})
    );
    assert!(
        results.iter().any(
            |r| r["is_error"] == true && r["content"].as_str().unwrap().contains("Exit code 2")
        )
    );
    assert!(
        !serde_json::to_string(&result.items)
            .unwrap()
            .contains("SUMMARY-R1")
    );
}

#[test]
fn unsupported_tools_changes_and_future_blocks_keep_full_content_with_losses() {
    let payloads = vec![
        json!({"type":"collabAgentToolCall","id":"sub","receiverThreadIds":["child"],"result":{"text":"子代理完整结果"}}),
        json!({"type":"fileChange","status":"completed","changes":[
            {"path":"new","kind":{"type":"add"},"diff":"@@ literally a new file\n"},
            {"path":"old","kind":{"type":"update","move_path":"renamed"},"diff":"@@\n-delete\n+add\n"}
        ]}),
        json!({"type":"futureItem","payload":"未知但有用"}),
    ];
    let converted = convert(
        &frozen(BackendKind::Codex, payloads),
        BackendKind::Claude,
        None,
    )
    .unwrap();
    let text = serde_json::to_string(&converted.items).unwrap();
    for marker in [
        "子代理完整结果",
        "@@ literally a new file",
        "renamed",
        "-delete",
        "未知但有用",
    ] {
        assert!(text.contains(marker), "{marker}");
    }
    assert_eq!(converted.loss.entries.len(), 3);
    let unknown = frozen(
        BackendKind::Claude,
        vec![json!({"role":"user","content":[{"type":"future_block","data":"不可静默丢弃"}]})],
    );
    let result = convert(&unknown, BackendKind::Codex, None).unwrap();
    assert!(
        serde_json::to_string(&result.items)
            .unwrap()
            .contains("不可静默丢弃")
    );
    assert_eq!(result.loss.entries.len(), 1);
}

#[test]
fn incomplete_exports_and_broken_tool_pairs_cannot_advance_sync() {
    let mut input = frozen(BackendKind::Claude, vec![]);
    input.complete = false;
    assert_eq!(
        convert(&input, BackendKind::Codex, None),
        Err(nd_convert::ConvertError::Incomplete)
    );
    for values in [
        vec![
            json!({"role":"assistant","content":[{"type":"tool_use","id":"x","name":"Bash","input":{}}]}),
        ],
        vec![
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"orphan"}]}),
        ],
        vec![
            json!({"role":"assistant","content":[{"type":"tool_use","id":"x","name":"Bash","input":{}},{"type":"tool_use","id":"x","name":"Bash","input":{}}]}),
        ],
    ] {
        assert!(
            convert(
                &frozen(BackendKind::Claude, values),
                BackendKind::Codex,
                None
            )
            .is_err()
        );
    }
    let mut duplicate = frozen(
        BackendKind::Claude,
        vec![json!({"role":"user","content":"one"})],
    );
    duplicate.items.push(duplicate.items[0].clone());
    assert!(convert(&duplicate, BackendKind::Codex, None).is_err());
}

#[test]
fn local_images_use_only_frozen_bytes_and_sync_rejects_changed_attachments() {
    let mut input = frozen(
        BackendKind::Codex,
        vec![json!({"type":"userMessage","content":[
            {"type":"localImage","path":"/does/not/exist.png"},
            {"type":"image","url":"https://unreachable.invalid/picture.png"}
        ]})],
    );
    input.images.insert(
        "/does/not/exist.png".into(),
        nd_convert::FrozenImage::from_bytes("image/png", b"frozen"),
    );
    let result = convert(&input, BackendKind::Claude, None).unwrap();
    assert_eq!(
        result.items[0]["message"]["content"][0]["source"]["data"],
        "ZnJvemVu"
    );
    assert_eq!(result.loss.entries.len(), 1);
    input.images.insert(
        "/does/not/exist.png".into(),
        nd_convert::FrozenImage::from_bytes("image/png", b"changed"),
    );
    assert_eq!(
        convert(&input, BackendKind::Claude, Some(&result.sync)),
        Err(nd_convert::ConvertError::SyncInvalid)
    );
    input.images.get_mut("/does/not/exist.png").unwrap().sha256 = "wrong".into();
    assert!(convert(&input, BackendKind::Claude, None).is_err());
}

#[test]
fn replay_envelopes_are_stable_complete_and_set_no_query_flags() {
    let input = frozen(
        BackendKind::Codex,
        vec![
            json!({"type":"userMessage","content":[{"type":"text","text":"/dangerous-slash"}]}),
            json!({"type":"agentMessage","text":"历史回答","phase":"final_answer"}),
        ],
    );
    let a = convert(&input, BackendKind::Claude, None).unwrap();
    let b = convert(&input, BackendKind::Claude, None).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.items[0]["shouldQuery"], false);
    assert_eq!(a.items[0]["client_composed"], true);
    assert_eq!(a.items[0]["session_id"], "");
    assert_eq!(a.items[1]["session_id"], "");
    assert!(a.items[1].get("parent_tool_use_id").unwrap().is_null());
    assert_eq!(a.items[1]["message"]["type"], "message");
    assert_eq!(
        a.items[1]["message"]["usage"],
        json!({"input_tokens":0,"output_tokens":0})
    );
    assert!(a.items[1]["message"]["model"].is_string());
    assert!(
        a.items[1]["message"]["id"]
            .as_str()
            .unwrap()
            .starts_with("msg_nd_")
    );
    let mut other = input;
    other.source_id = "another-session".into();
    assert_ne!(
        convert(&other, BackendKind::Claude, None).unwrap().items[1]["uuid"],
        a.items[1]["uuid"]
    );
}

#[test]
fn codex_reasoning_is_a_native_item_and_never_claude_thinking() {
    let raw = json!({"type":"reasoning","id":"rs-1","summary":[{"type":"summary_text","text":"native summary"}],"encrypted_content":"CIPHER"});
    let input = frozen(BackendKind::Codex, vec![raw.clone()]);
    let decoded = nd_convert::decode(&input).unwrap();
    assert_eq!(decoded.entries[0].native.payload, raw);
    assert_eq!(
        nd_convert::encode(&decoded, BackendKind::Codex)
            .unwrap()
            .items,
        vec![raw]
    );
    let foreign = nd_convert::encode(&decoded, BackendKind::Claude).unwrap();
    assert!(foreign.items.is_empty());
    assert_eq!(foreign.loss.entries[0].reason, "native_reasoning_omitted");
}

#[test]
fn native_claude_content_metadata_survives_the_neutral_round_trip() {
    let input = frozen(
        BackendKind::Claude,
        vec![json!({"role":"assistant","model":"source-model","content":[
        {"type":"text","text":"cited text","citations":[{"type":"web","url":"https://example.invalid"}],"future":17}
    ],"usage":{"input_tokens":123,"output_tokens":7}})],
    );
    let decoded = nd_convert::decode(&input).unwrap();
    let original = nd_convert::encode(&decoded, BackendKind::Claude).unwrap();
    assert_eq!(
        original.items[0]["message"]["content"],
        input.items[0].payload["content"]
    );
    let foreign = nd_convert::encode(&decoded, BackendKind::Codex).unwrap();
    assert!(
        foreign
            .loss
            .entries
            .iter()
            .any(|l| l.reason == "native_metadata_not_transferred")
    );
}

#[test]
fn recorded_effective_exports_keep_preserved_compaction_and_parallel_results() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/claude-exports.json")).unwrap();
    for case in cases {
        let input = frozen(
            BackendKind::Claude,
            case["messages"].as_array().unwrap().clone(),
        );
        let result = convert(&input, BackendKind::Codex, None).unwrap();
        let text = serde_json::to_string(&result.items).unwrap();
        match case["line"].as_u64().unwrap() {
            14 => {
                for marker in [
                    "REVIEW_COMPACT_SUMMARY",
                    "REVIEW_PRESERVED_USER",
                    "REVIEW_PRESERVED_REPLY",
                    "REVIEW_AFTER_REPLY",
                ] {
                    assert!(text.contains(marker));
                }
            }
            33 => {
                for marker in [
                    "REVIEW_FIRST_RESULT",
                    "REVIEW_SECOND_RESULT",
                    "toolu_review_1",
                    "toolu_review_2",
                ] {
                    assert!(text.contains(marker));
                }
            }
            52 => assert!(text.contains("VERIFY_THINK")),
            65 => {
                assert!(text.contains("data:image/png;base64,"));
                assert!(
                    result
                        .loss
                        .entries
                        .iter()
                        .any(|l| l.reason == "native_reasoning_omitted")
                );
                assert!(!result.items.iter().any(|m| m["type"] == "reasoning"));
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn unparseable_edit_diffs_fall_back_without_inventing_file_content() {
    for diff in [
        "@@ -1 +1 @@\n-a\n+b\nUNKNOWN_TRAILER",
        "@@ -1,99 +1,99 @@\n-a\n+b\n",
        "",
    ] {
        let input = frozen(
            BackendKind::Codex,
            vec![json!({"type":"fileChange","status":"completed","changes":[
                {"path":"file.txt","kind":{"type":"update"},"diff":diff}
            ]})],
        );
        let result = convert(&input, BackendKind::Claude, None).unwrap();
        assert!(
            result
                .items
                .iter()
                .flat_map(|m| m["message"]["content"].as_array().unwrap())
                .all(|b| b["type"] != "tool_use")
        );
        assert_eq!(result.loss.entries[0].reason, "file_change_as_text");
        assert!(
            result.items[0]["message"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("file.txt")
        );
    }
}

#[test]
fn sync_rejects_other_sessions_epochs_targets_and_rewound_history() {
    let input = frozen(
        BackendKind::Claude,
        vec![json!({"role":"user","content":"prefix"})],
    );
    let sync = convert(&input, BackendKind::Codex, None).unwrap().sync;
    for changed in [
        FrozenInput {
            source_id: "different".into(),
            ..input.clone()
        },
        FrozenInput {
            epoch: "compacted".into(),
            ..input.clone()
        },
        FrozenInput {
            items: vec![],
            ..input.clone()
        },
        FrozenInput {
            backend: BackendKind::Codex,
            ..input.clone()
        },
    ] {
        assert_eq!(
            convert(&changed, BackendKind::Codex, Some(&sync)),
            Err(nd_convert::ConvertError::SyncInvalid)
        );
    }
    assert_eq!(
        convert(&input, BackendKind::Claude, Some(&sync)),
        Err(nd_convert::ConvertError::SyncInvalid)
    );
}

#[test]
fn a_complete_export_larger_than_the_mod_limit_is_not_truncated() {
    let input = frozen(
        BackendKind::Claude,
        (0..4100)
            .map(|i| json!({"role":"user","content":format!("message-{i}")}))
            .collect(),
    );
    let converted = convert(&input, BackendKind::Codex, None).unwrap();
    assert_eq!(converted.items.len(), 4100);
    assert_eq!(
        converted.items.last().unwrap()["content"][0]["text"],
        "message-4099"
    );
}

#[test]
fn unsafe_or_oversized_image_payloads_get_an_explicit_placeholder() {
    for (media, data) in [
        ("image/svg+xml", "YQ==".to_owned()),
        ("image/png", "invalid?".to_owned()),
        (
            "image/png",
            "A".repeat((nd_convert::MAX_IMAGE_BYTES + 1).div_ceil(3) * 4),
        ),
    ] {
        let result = convert(&frozen(BackendKind::Claude,vec![json!({"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":media,"data":data}}]})]),BackendKind::Codex,None).unwrap();
        assert_eq!(result.items[0]["content"][0]["type"], "input_text");
        assert_eq!(result.loss.entries[0].reason, "image_unavailable");
    }
}

#[test]
fn codex_async_questions_become_conversation_text() {
    let input = frozen(
        BackendKind::Codex,
        vec![
            json!({"type":"agentMessage","text":"请选择","questions":[{"id":"q1","question":"保留哪个文件？","options":["甲","乙"]}]}),
        ],
    );
    let result = convert(&input, BackendKind::Claude, None).unwrap();
    let text = serde_json::to_string(&result.items).unwrap();
    assert!(text.contains("保留哪个文件？"));
    assert!(text.contains("甲"));
    assert!(
        result
            .loss
            .entries
            .iter()
            .any(|l| l.reason == "interaction_as_text")
    );
}

#[test]
fn a_sync_point_from_another_codec_revision_requires_rebuilding() {
    let input = frozen(
        BackendKind::Claude,
        vec![json!({"role":"user","content":"prefix"})],
    );
    let mut sync = convert(&input, BackendKind::Codex, None).unwrap().sync;
    sync.codec_revision += 1;
    assert_eq!(
        convert(&input, BackendKind::Codex, Some(&sync)),
        Err(nd_convert::ConvertError::SyncInvalid)
    );
}

#[test]
fn duplicate_native_positions_across_a_sync_boundary_are_rejected() {
    let mut input = frozen(
        BackendKind::Claude,
        vec![json!({"role":"user","content":"old"})],
    );
    let sync = convert(&input, BackendKind::Codex, None).unwrap().sync;
    input.items.push(NativeItem {
        position: "position-0".into(),
        payload: json!({"role":"assistant","content":"new but not a new position"}),
    });
    assert!(convert(&input, BackendKind::Codex, Some(&sync)).is_err());
}

#[test]
fn codex_thread_reasoning_round_trips_through_neutral_and_responses_items() {
    let input = frozen(
        BackendKind::Codex,
        vec![json!({"type":"reasoning","id":"r","summary":["摘要"],"content":["本方推理"]})],
    );
    let decoded = nd_convert::decode(&input).unwrap();
    let encoded = nd_convert::encode(&decoded, BackendKind::Codex).unwrap();
    let back = nd_convert::decode(&frozen(BackendKind::Codex, encoded.items)).unwrap();
    assert_eq!(decoded.entries[0].parts, back.entries[0].parts);
    assert_eq!(decoded.entries[0].native, input.items[0]);
}
