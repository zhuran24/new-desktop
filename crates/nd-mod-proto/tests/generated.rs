//! 生成物与 Rust 类型一致；TS 生成器按 JSON Schema 的语义输出类型。

#[test]
fn typescript_follows_schema_semantics_for_objects_unions_and_optionals() {
    let schema = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Root",
        "type": "object",
        "properties": {"hello": {"$ref": "#/$defs/Hello"}},
        "required": ["hello"],
        "$defs": {
            "Hello": {
                "description": "First message.",
                "type": "object",
                "properties": {
                    "proto": {"type": "integer", "format": "uint32", "minimum": 0},
                    "mod": {"$ref": "#/$defs/ModName"},
                    "note": {"type": ["string", "null"]},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "extra": {"type": "object", "additionalProperties": {"type": "boolean"}},
                    "payload": true
                },
                "required": ["proto", "mod", "tags", "extra", "payload"]
            },
            "ModName": {"type": "string", "enum": ["new-desktop", "new-desktop-actions"]},
            "Action": {"oneOf": [
                {"type": "object", "properties": {"type": {"type": "string", "const": "ping"}}, "required": ["type"]},
                {"description": "Ask about earlier operations.", "type": "object", "properties": {
                    "type": {"type": "string", "const": "query"},
                    "op_ids": {"type": "array", "items": {"type": "string"}}
                }, "required": ["type", "op_ids"]}
            ]}
        }
    });
    assert_eq!(
        nd_mod_proto::ts::typescript(&schema),
        r#"export type Action =
  | { type: "ping" }
  /** Ask about earlier operations. */
  | { type: "query"; op_ids: string[] };

/** First message. */
export type Hello = {
  extra: { [key: string]: boolean };
  mod: ModName;
  note?: string | null;
  payload: unknown;
  proto: number;
  tags: string[];
};

export type ModName = "new-desktop" | "new-desktop-actions";

export type Root = {
  hello: Hello;
};
"#
    );
}

#[test]
fn committed_schema_and_mod_typescript_match_the_rust_types() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let published: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("protocol/mod.schema.json")).expect("published mod schema"),
    )
    .unwrap();
    assert_eq!(published, nd_mod_proto::schema());
    for module in ["new-desktop", "new-desktop-actions"] {
        let committed =
            std::fs::read_to_string(root.join("mods").join(module).join("hooks/proto.ts"))
                .expect("generated proto.ts");
        assert_eq!(committed, nd_mod_proto::typescript_module(), "{module}");
    }
}

#[test]
fn every_action_declares_whether_it_may_be_resent_after_reconnect() {
    use nd_mod_proto::{Action, Resend};
    assert_eq!(Action::Ping.resend(), Resend::Resendable);
    assert_eq!(
        Action::Query { op_ids: vec![] }.resend(),
        Resend::Resendable
    );
}
