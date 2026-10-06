//! 把本协议导出的 JSON Schema 转成 TypeScript 类型声明。
//!
//! 只覆盖 schemars 为本协议输出的子集：对象、`$ref`、`oneOf`/`anyOf`、`enum`、`const`、
//! 数组、映射与可空类型。认不出的结构一律写成 `unknown`，不猜。
use serde_json::Value;
use std::collections::BTreeMap;

/// 每个 `$defs` 条目及根（以 `title` 命名）各输出一个 `export type`，按名字排序。
pub fn typescript(schema: &Value) -> String {
    let mut defs = BTreeMap::new();
    if let Some(map) = schema.get("$defs").and_then(Value::as_object) {
        for (name, def) in map {
            defs.insert(name.clone(), def.clone());
        }
    }
    if let Some(title) = schema.get("title").and_then(Value::as_str) {
        let mut root = schema.clone();
        if let Some(map) = root.as_object_mut() {
            map.remove("$defs");
            map.remove("$schema");
            map.remove("title");
        }
        defs.insert(title.to_owned(), root);
    }
    defs.iter()
        .map(|(name, def)| {
            let mut out = doc("", def);
            let variants = union(def);
            if let Some(variants) = variants.filter(|v| v.len() > 1) {
                out.push_str(&format!("export type {name} ="));
                for (i, variant) in variants.iter().enumerate() {
                    out.push('\n');
                    out.push_str(&doc("  ", variant));
                    out.push_str(&format!("  | {}", inline(variant)));
                    if i + 1 == variants.len() {
                        out.push(';');
                    }
                }
                out.push('\n');
            } else if is_object(def) {
                out.push_str(&format!("export type {name} = {};\n", block(def)));
            } else {
                out.push_str(&format!("export type {name} = {};\n", inline(def)));
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn doc(indent: &str, schema: &Value) -> String {
    match schema.get("description").and_then(Value::as_str) {
        Some(text) => {
            let text = text.replace("*/", "* /");
            if text.contains('\n') {
                let mut out = format!("{indent}/**\n");
                for line in text.lines() {
                    out.push_str(&format!("{indent} * {line}\n").replace(" * \n", " *\n"));
                }
                out.push_str(&format!("{indent} */\n"));
                out
            } else {
                format!("{indent}/** {text} */\n")
            }
        }
        None => String::new(),
    }
}

fn union(schema: &Value) -> Option<Vec<Value>> {
    schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
        .cloned()
}

fn is_object(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
        && schema.get("properties").is_some()
}

fn properties(schema: &Value) -> Vec<(String, Value, bool)> {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut props: Vec<(String, Value, bool)> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| {
            p.iter()
                .map(|(k, v)| (k.clone(), v.clone(), required.contains(&k.as_str())))
                .collect()
        })
        .unwrap_or_default();
    // 判别字段放最前，其余按名字排序：输出不随 serde_json 是否保留键序而变。
    props.sort_by(|(a, _, _), (b, _, _)| {
        let tag = |k: &str| !matches!(k, "type" | "status" | "code" | "kind");
        (tag(a), a).cmp(&(tag(b), b))
    });
    props
}

fn field(name: &str, schema: &Value, required: bool) -> String {
    let key = if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        name.to_owned()
    } else {
        format!("{name:?}")
    };
    format!(
        "{key}{}: {}",
        if required { "" } else { "?" },
        inline(schema)
    )
}

fn block(schema: &Value) -> String {
    let mut out = String::from("{\n");
    for (name, prop, required) in properties(schema) {
        out.push_str(&doc("  ", &prop));
        out.push_str(&format!("  {};\n", field(&name, &prop, required)));
    }
    out.push('}');
    out
}

fn inline(schema: &Value) -> String {
    match schema {
        Value::Bool(true) => return "unknown".into(),
        Value::Bool(false) => return "never".into(),
        _ => {}
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return reference.rsplit('/').next().unwrap_or("unknown").to_owned();
    }
    if let Some(value) = schema.get("const") {
        return value.to_string();
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(variants) = union(schema) {
        return variants.iter().map(inline).collect::<Vec<_>>().join(" | ");
    }
    if let Some(types) = schema.get("type").and_then(Value::as_array) {
        return types
            .iter()
            .map(|t| {
                let mut single = schema.clone();
                single["type"] = t.clone();
                inline(&single)
            })
            .collect::<Vec<_>>()
            .join(" | ");
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => "string".into(),
        Some("integer") | Some("number") => "number".into(),
        Some("boolean") => "boolean".into(),
        Some("null") => "null".into(),
        Some("array") => {
            let item = schema.get("items").map(inline).unwrap_or("unknown".into());
            if item.contains(' ') {
                format!("({item})[]")
            } else {
                format!("{item}[]")
            }
        }
        Some("object") if schema.get("properties").is_some() => {
            let fields = properties(schema)
                .into_iter()
                .map(|(name, prop, required)| field(&name, &prop, required))
                .collect::<Vec<_>>();
            format!("{{ {} }}", fields.join("; "))
        }
        Some("object") => match schema.get("additionalProperties") {
            Some(value) if value != &Value::Bool(false) => {
                format!("{{ [key: string]: {} }}", inline(value))
            }
            _ => "{ [key: string]: unknown }".into(),
        },
        _ => "unknown".into(),
    }
}
