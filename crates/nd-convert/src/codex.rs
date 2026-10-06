use crate::*;

pub(crate) fn thread_item(item: &NativeItem) -> Result<(Role, Vec<Part>), ConvertError> {
    let v = &item.payload;
    let parts = match v["type"].as_str() {
        Some("agentMessage" | "plan") => {
            let mut parts = vec![Part::Text(field(v, "text")?)];
            if let Some(questions) = v.get("questions").filter(|q| !q.is_null()) {
                parts.push(Part::Notice {
                    reason: "interaction_as_text".into(),
                    value: questions.clone(),
                });
            }
            parts
        }
        Some("reasoning") => vec![Part::Reasoning {
            backend: BackendKind::Codex,
            value: reasoning(v),
        }],
        Some("commandExecution") => {
            let command = field(v, "command")?;
            let cwd = field(v, "cwd")?;
            let command = format!("cd -- '{}' && {command}", cwd.replace('\'', "'\\''"));
            let code = v["exitCode"].as_i64();
            let failed = v["status"] != "completed" || code != Some(0);
            let output = format!(
                "{}\nExit code {}; status={}",
                v["aggregatedOutput"].as_str().unwrap_or(""),
                code.map_or_else(|| "unknown".into(), |n| n.to_string()),
                v["status"].as_str().unwrap_or("unknown")
            );
            pair(item, 0, "Bash", json!({"command":command}), output, failed)
        }
        Some("fileChange") => {
            let mut parts = Vec::new();
            for change in v["changes"]
                .as_array()
                .ok_or_else(|| ConvertError::Invalid("invalid changes".into()))?
            {
                let path = field(change, "path")?;
                let diff = field(change, "diff")?;
                match change["kind"]["type"].as_str() {
                    Some("add") if v["status"] == "completed" => parts.extend(pair(
                        item,
                        parts.len(),
                        "Write",
                        json!({"file_path":path,"content":diff}),
                        format!("File created successfully at: {path}"),
                        false,
                    )),
                    Some("update")
                        if v["status"] == "completed" && change["kind"]["move_path"].is_null() =>
                    {
                        match hunks(&diff) {
                            Ok(hunks) => {
                                for (old, new) in hunks {
                                    parts.extend(pair(
                                        item,
                                        parts.len(),
                                        "Edit",
                                        json!({"file_path":path,"old_string":old,"new_string":new}),
                                        format!("The file {path} has been updated successfully."),
                                        false,
                                    ));
                                }
                            }
                            Err(_) => parts.push(Part::Notice {
                                reason: "file_change_as_text".into(),
                                value: json!({"change":change,"status":v["status"]}),
                            }),
                        }
                    }
                    _ => parts.push(Part::Notice {
                        reason: "file_change_as_text".into(),
                        value: json!({"change":change,"status":v["status"]}),
                    }),
                }
            }
            parts
        }
        _ => vec![Part::Notice {
            reason: "unsupported_item_as_text".into(),
            value: v.clone(),
        }],
    };
    Ok((Role::Assistant, parts))
}

fn pair(
    item: &NativeItem,
    index: usize,
    name: &str,
    input: Value,
    output: String,
    failed: bool,
) -> Vec<Part> {
    let id = format!("nd_{}", &hash(&(item, index))[..32]);
    vec![
        Part::ToolCall {
            id: id.clone(),
            name: name.into(),
            input,
        },
        Part::ToolResult {
            id,
            content: json!(output),
            is_error: failed,
        },
    ]
}

fn hunks(diff: &str) -> Result<Vec<(String, String)>, ConvertError> {
    let invalid = || ConvertError::Invalid("diff cannot be represented as Edit".into());
    fn count(range: &str, sign: char) -> Option<usize> {
        let range = range.strip_prefix(sign)?;
        let (start, count) = range.split_once(',').unwrap_or((range, "1"));
        start.parse::<usize>().ok()?;
        count.parse().ok()
    }
    let mut out = Vec::new();
    let mut remaining = (0usize, 0usize);
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@ ") {
            if remaining != (0, 0) {
                return Err(invalid());
            }
            let header: Vec<_> = line.split_whitespace().collect();
            if header.len() < 4 || header[3] != "@@" {
                return Err(invalid());
            }
            remaining = (
                count(header[1], '-').ok_or_else(invalid)?,
                count(header[2], '+').ok_or_else(invalid)?,
            );
            out.push((String::new(), String::new()));
            continue;
        }
        let Some((old, new)) = out.last_mut() else {
            return Err(invalid());
        };
        match line.as_bytes().first() {
            Some(b' ') => {
                remaining.0 = remaining.0.checked_sub(1).ok_or_else(invalid)?;
                remaining.1 = remaining.1.checked_sub(1).ok_or_else(invalid)?;
                old.push_str(&line[1..]);
                new.push_str(&line[1..]);
            }
            Some(b'-') => {
                remaining.0 = remaining.0.checked_sub(1).ok_or_else(invalid)?;
                old.push_str(&line[1..]);
            }
            Some(b'+') => {
                remaining.1 = remaining.1.checked_sub(1).ok_or_else(invalid)?;
                new.push_str(&line[1..]);
            }
            _ => return Err(invalid()),
        }
    }
    if out.is_empty() || remaining != (0, 0) || out.iter().any(|(old, _)| old.is_empty()) {
        return Err(invalid());
    }
    Ok(out)
}

pub(crate) fn reasoning(value: &Value) -> Value {
    let mut value = value.clone();
    for (key, kind) in [("summary", "summary_text"), ("content", "reasoning_text")] {
        if let Some(parts) = value.get_mut(key).and_then(Value::as_array_mut) {
            for part in parts {
                if let Some(text) = part.as_str() {
                    *part = json!({"type":kind,"text":text});
                }
            }
        }
    }
    value
}
