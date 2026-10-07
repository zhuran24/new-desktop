#[test]
fn published_schema_matches_rust_and_future_items_keep_fallback() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../protocol");
    for (name, schema) in [
        (
            "request.schema.json",
            schemars::schema_for!(nd_wire::Request),
        ),
        (
            "response.schema.json",
            schemars::schema_for!(nd_wire::Response),
        ),
    ] {
        let published: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join(name)).expect("published schema"))
                .unwrap();
        assert_eq!(published, serde_json::to_value(schema).unwrap());
    }
    let response: nd_wire::Response = serde_json::from_str(r#"{"type":"snapshot","future":true,"snapshot":{"stream":"global","epoch":"e","cursor":7,"items":[{"id":"future-1","namespace":"new-feature","kind":"unknown-kind","data":{"new_enum":"future-value"},"fallback":{"title":"未来条目","text":"仍能读到这条内容"},"added":42}]}}"#).unwrap();
    let nd_wire::Response::Snapshot { snapshot } = response else {
        panic!("snapshot")
    };
    assert_eq!(snapshot.items[0].kind, "unknown-kind");
    assert_eq!(snapshot.items[0].data["new_enum"], "future-value");
    assert_eq!(snapshot.items[0].fallback.text, "仍能读到这条内容");
}

#[test]
fn future_receipt_and_reply_states_do_not_turn_valid_frames_into_disconnects() {
    for text in [
        r#"{"type":"command_reply","id":1,"result":{"status":"receipt","receipt":{"status":"absorbed","future":true}}}"#,
        r#"{"type":"command_reply","id":2,"result":{"status":"refused","future":true}}"#,
        r#"{"type":"receipt_reply","id":3,"result":{"status":"pending","future":true}}"#,
    ] {
        assert!(
            serde_json::from_str::<nd_wire::Response>(text).is_ok(),
            "future state invalidated the frame: {text}"
        );
    }
}
