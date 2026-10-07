use crate::*;

pub fn decode(input: &FrozenInput) -> Result<Decoded, ConvertError> {
    if !input.complete {
        return Err(ConvertError::Incomplete);
    }
    let mut positions = BTreeSet::new();
    let mut entries = Vec::new();
    for item in &input.items {
        if item.position.is_empty() || !positions.insert(&item.position) {
            return Err(ConvertError::Invalid(
                "empty or duplicate native position".into(),
            ));
        }
        if input.backend == BackendKind::Codex
            && !matches!(
                item.payload["type"].as_str(),
                Some("message" | "userMessage")
            )
        {
            let (role, parts) = codex::thread_item(item)?;
            entries.push(Entry {
                source_id: input.source_id.clone(),
                position: item.position.clone(),
                role,
                parts,
                native: item.clone(),
                backend: input.backend,
            });
            continue;
        }
        let mut m = item.payload.get("message").unwrap_or(&item.payload).clone();
        if m["type"] == "userMessage" {
            m["role"] = json!("user");
        }
        let role = match m["role"].as_str() {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => return Err(ConvertError::Invalid("invalid role".into())),
        };
        let parts = if let Some(s) = m["content"].as_str() {
            vec![Part::Text(s.into())]
        } else {
            m["content"]
                .as_array()
                .ok_or_else(|| ConvertError::Invalid("invalid content".into()))?
                .iter()
                .map(|b| match b["type"].as_str() {
                    Some("image") if b["source"]["type"] == "base64" => Ok(images::inline(
                        &field(&b["source"], "media_type")?,
                        &field(&b["source"], "data")?,
                    )),
                    Some("image" | "localImage" | "input_image") => {
                        images::decode_reference(input, b)
                    }
                    Some("thinking" | "redacted_thinking") => Ok(Part::Reasoning {
                        backend: input.backend,
                        value: b.clone(),
                    }),
                    Some("tool_use") => Ok(Part::ToolCall {
                        id: field(b, "id")?,
                        name: field(b, "name")?,
                        input: b["input"].clone(),
                    }),
                    Some("tool_result") => Ok(Part::ToolResult {
                        id: field(b, "tool_use_id")?,
                        content: b["content"].clone(),
                        is_error: b["is_error"].as_bool().unwrap_or(false),
                    }),
                    Some("text" | "input_text" | "output_text") => {
                        Ok(Part::Text(field(b, "text")?))
                    }
                    _ => Ok(Part::Notice {
                        reason: "unsupported_block_as_text".into(),
                        value: b.clone(),
                    }),
                })
                .collect::<Result<_, ConvertError>>()?
        };
        entries.push(Entry {
            source_id: input.source_id.clone(),
            position: item.position.clone(),
            role,
            parts,
            native: item.clone(),
            backend: input.backend,
        });
    }
    validate_tools(&entries)?;
    Ok(Decoded {
        entries,
        loss: LossReport::default(),
    })
}

pub fn encode(decoded: &Decoded, to: BackendKind) -> Result<Encoded, ConvertError> {
    validate_tools(&decoded.entries)?;
    let mut items = Vec::new();
    let mut loss = decoded.loss.clone();
    for entry in &decoded.entries {
        let native_message = entry
            .native
            .payload
            .get("message")
            .unwrap_or(&entry.native.payload);
        if entry.backend != to && has_metadata(native_message) {
            loss.entries.push(Loss {
                position: entry.position.clone(),
                reason: "native_metadata_not_transferred".into(),
            });
        }
        if to == BackendKind::Codex
            && let [
                Part::Reasoning {
                    backend: BackendKind::Codex,
                    value,
                },
            ] = entry.parts.as_slice()
        {
            items.push(codex::reasoning(value));
            continue;
        }
        let mut groups: Vec<(Role, Vec<Value>)> = Vec::new();
        for (part_index, part) in entry.parts.iter().enumerate() {
            let (role, mut block) = match part {
                Part::Notice { reason, value } => {
                    loss.entries.push(Loss {
                        position: entry.position.clone(),
                        reason: reason.clone(),
                    });
                    (
                        entry.role.clone(),
                        json!({"type":match to {
                        BackendKind::Claude => "text", BackendKind::Codex if entry.role == Role::User => "input_text", BackendKind::Codex => "output_text"
                    },"text":format!("[external history: {reason}]\n{value}")}),
                    )
                }
                Part::Image { media_type, data } => (
                    Role::User,
                    match to {
                        BackendKind::Claude => {
                            json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}})
                        }
                        BackendKind::Codex => {
                            json!({"type":"input_image","image_url":format!("data:{media_type};base64,{data}")})
                        }
                    },
                ),
                Part::Reasoning { backend, value } => {
                    if *backend != to {
                        loss.entries.push(Loss {
                            position: entry.position.clone(),
                            reason: "native_reasoning_omitted".into(),
                        });
                        continue;
                    }
                    (entry.role.clone(), value.clone())
                }
                Part::Text(text) => (
                    entry.role.clone(),
                    json!({"type": match to {
                    BackendKind::Claude => "text", BackendKind::Codex if entry.role == Role::User => "input_text", BackendKind::Codex => "output_text"
                }, "text":text}),
                ),
                Part::ToolCall { id, name, input } if to == BackendKind::Claude => (
                    Role::Assistant,
                    json!({"type":"tool_use","id":id,"name":name,"input":input}),
                ),
                Part::ToolResult {
                    id,
                    content,
                    is_error,
                } if to == BackendKind::Claude => {
                    let mut b = json!({"type":"tool_result","tool_use_id":id,"content":content});
                    if *is_error {
                        b["is_error"] = json!(true);
                    }
                    (Role::User, b)
                }
                Part::ToolCall { id, name, input } => {
                    loss.entries.push(Loss {
                        position: entry.position.clone(),
                        reason: "tool_call_as_text".into(),
                    });
                    (
                        Role::Assistant,
                        json!({"type":"output_text","text":format!("[external_agent_tool_call: {name}; id={id}]\n{input}")}),
                    )
                }
                Part::ToolResult {
                    id,
                    content,
                    is_error,
                } => {
                    loss.entries.push(Loss {
                        position: entry.position.clone(),
                        reason: "tool_result_as_text".into(),
                    });
                    (
                        Role::Assistant,
                        json!({"type":"output_text","text":format!("[external_agent_tool_result: {id}; is_error={is_error}]\n{content}")}),
                    )
                }
            };
            if entry.backend == BackendKind::Claude
                && to == BackendKind::Claude
                && let Some(original) = native_message["content"]
                    .get(part_index)
                    .filter(|v| v["type"] == block["type"])
                    .and_then(Value::as_object)
            {
                let mut with_metadata = original.clone();
                with_metadata.extend(block.as_object().unwrap().clone());
                if let Part::ToolResult { is_error, .. } = part
                    && original.contains_key("is_error")
                {
                    with_metadata.insert("is_error".into(), json!(is_error));
                }
                block = Value::Object(with_metadata);
            }
            if groups.last().is_none_or(|(r, _)| *r != role) {
                groups.push((role, Vec::new()));
            }
            groups.last_mut().unwrap().1.push(block);
        }
        for (group_index, (role, content)) in groups.into_iter().enumerate() {
            let digest = hash(&(
                &entry.source_id,
                &entry.position,
                &entry.native,
                group_index,
            ));
            let uuid = format!(
                "{}-{}-8{}-8{}-{}",
                &digest[..8],
                &digest[8..12],
                &digest[13..16],
                &digest[17..20],
                &digest[20..32]
            );
            let role = match role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            items.push(match to {
                BackendKind::Claude if role == "user" => json!({"type":"user","uuid":uuid,"session_id":"","parent_tool_use_id":null,"message":{"role":role,"content":content},"shouldQuery":false,"client_composed":true}),
                BackendKind::Claude => {
                    let raw = entry.native.payload.get("message").unwrap_or(&entry.native.payload);
                    let mut message = if entry.backend == BackendKind::Claude { raw.clone() } else { json!({}) };
                    let object = message.as_object_mut().ok_or_else(||ConvertError::Invalid("invalid native message".into()))?;
                    object.entry("id").or_insert_with(||json!(format!("msg_nd_{}",&digest[..32])));
                    object.entry("type").or_insert(json!("message"));
                    object.entry("model").or_insert(json!("nd-import"));
                    object.entry("stop_reason").or_insert(json!("end_turn"));
                    object.entry("usage").or_insert(json!({"input_tokens":0,"output_tokens":0}));
                    message["role"] = json!(role);
                    message["content"] = json!(content);
                    json!({"type":"assistant","uuid":uuid,"session_id":"","parent_tool_use_id":null,"message":message})
                },
                BackendKind::Codex => json!({"type":"message","role":role,"content":content}),
            });
        }
    }
    Ok(Encoded { items, loss })
}

fn has_metadata(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.iter().any(|(k, v)| {
            matches!(
                k.as_str(),
                "phase"
                    | "usage"
                    | "timestamp"
                    | "model"
                    | "citations"
                    | "stop_reason"
                    | "stop_sequence"
            ) || has_metadata(v)
        }),
        Value::Array(a) => a.iter().any(has_metadata),
        _ => false,
    }
}
