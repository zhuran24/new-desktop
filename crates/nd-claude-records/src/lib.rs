//! Claude Code 记录的只读偏移索引、选链和分页。
//!
//! ```
//! use nd_claude_records::RecordIndex;
//! use std::num::NonZeroUsize;
//!
//! let bytes = br#"{"type":"user","uuid":"u","parentUuid":null,"timestamp":"2026-10-06T12:00:00Z","message":{"role":"user","content":"hello"}}"#;
//! let index = RecordIndex::parse(bytes)?;
//! let history = index.current()?;
//! assert_eq!(history.leaf(), Some("u"));
//! let page = history.page(None, NonZeroUsize::new(20).unwrap())?;
//! assert_eq!(page.records[0].decode()["message"]["content"], "hello");
//! assert_eq!(page.next_before, None);
//! # Ok::<(), nd_claude_records::Error>(())
//! ```

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::{num::NonZeroUsize, ops::Range};

#[derive(Debug)]
struct Node {
    id: String,
    parent: Option<String>,
    kind: String,
    sidechain: bool,
    sidechain_field: Option<Value>,
    range: Range<usize>,
    timestamp: Option<String>,
    message_id: Option<String>,
    agent_id: Option<String>,
    agent_field: Option<Value>,
    source_assistant: Option<String>,
    tool_use: bool,
    tool_result: bool,
    transparent: bool,
    attachment_tool: Option<String>,
    tool_uses: Vec<String>,
    tool_results: Vec<String>,
    active: bool,
}

// Record fields use scalar equality, not JSON structural equality. Each object
// or array parsed from JSON belongs to that occurrence, even when its contents
// equal another field. Missing and null remain distinct unless the caller
// explicitly supplies a default. Numbers share the JSON consumer's f64 domain.
fn same_field(left: Option<&Value>, right: Option<&Value>) -> bool {
    match (left, right) {
        (Some(Value::Array(_) | Value::Object(_)), _)
        | (_, Some(Value::Array(_) | Value::Object(_))) => {
            left.zip(right).is_some_and(|(a, b)| std::ptr::eq(a, b))
        }
        (Some(Value::Number(a)), Some(Value::Number(b))) => a.as_f64() == b.as_f64(),
        _ => left == right,
    }
}

fn sidechain_scope(node: &Node) -> Option<&Value> {
    Some(
        node.sidechain_field
            .as_ref()
            .filter(|v| !v.is_null())
            .unwrap_or(&Value::Bool(false)),
    )
}

/// An immutable view of a caller-owned JSONL snapshot. No file I/O is performed.
#[derive(Debug)]
pub struct RecordIndex<'a> {
    data: &'a [u8],
    nodes: Vec<Node>,
    by_id: HashMap<String, usize>,
    last: Option<String>,
    pin: Option<String>,
    explicit: bool,
    pin_was_explicit: bool,
    cleared: bool,
    has_leaf_metadata: bool,
    preserved_tail: Option<String>,
}

impl<'a> RecordIndex<'a> {
    /// Index a complete immutable snapshot. Errors never yield a partial index.
    pub fn parse(data: &'a [u8]) -> Result<Self, Error> {
        let mut nodes = Vec::new();
        let mut by_id = HashMap::new();
        let mut progress: HashMap<String, Option<String>> = HashMap::new();
        let mut last = None;
        let mut pin = None;
        let mut explicit = false;
        let mut pin_was_explicit = false;
        let mut cleared = false;
        let mut has_leaf_metadata = false;
        let mut preserved = None;
        let mut segment = None;
        let mut boundary = String::new();
        let mut saw_preservation = false;
        let mut offset = 0;
        for line in data.split_inclusive(|b| *b == b'\n') {
            let start = offset;
            offset += line.len();
            let terminated = line.ends_with(b"\n");
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let mut row: Value =
                serde_json::from_slice(line).map_err(|error: serde_json::Error| {
                    if !terminated && error.is_eof() {
                        Error::IncompleteRecord { byte_offset: start }
                    } else {
                        Error::InvalidJson {
                            byte_offset: start,
                            message: error.to_string(),
                        }
                    }
                })?;
            if !row.is_object() {
                return Err(Error::InvalidRecord { byte_offset: start });
            }
            if let Some(parent) = row["parentUuid"].as_str().and_then(|id| progress.get(id)) {
                row["parentUuid"] = serde_json::json!(parent);
            }
            if row["type"] == "progress" {
                if let Some(id) = row["uuid"].as_str() {
                    progress.insert(id.to_owned(), row["parentUuid"].as_str().map(str::to_owned));
                }
                continue;
            }
            if row["type"] == "last-prompt" {
                has_leaf_metadata |= row.get("leafUuid").is_some();
                if let Some(id) = row["leafUuid"].as_str() {
                    pin_was_explicit =
                        row["explicit"] == true || (pin_was_explicit && pin.as_deref() == Some(id));
                    explicit = row["explicit"] == true || (explicit && pin.as_deref() == Some(id));
                    pin = Some(id.to_owned());
                    cleared = false;
                } else if row.get("leafUuid") == Some(&Value::Null) && row["explicit"] == true {
                    cleared = true;
                    pin = None;
                    explicit = false;
                }
            }
            if !matches!(
                row["type"].as_str(),
                Some("user" | "assistant" | "system" | "attachment")
            ) {
                continue;
            }
            if row["uuid"].as_str().is_none_or(str::is_empty)
                || row
                    .get("parentUuid")
                    .is_some_and(|p| !p.is_null() && !p.is_string())
                || (matches!(row["type"].as_str(), Some("user" | "assistant"))
                    && !(row["message"].is_object()
                        && (row["message"]["content"].is_string()
                            || row["message"]["content"].is_array())))
            {
                return Err(Error::InvalidRecord { byte_offset: start });
            }
            if row["subtype"] == "compact_boundary" {
                boundary = row["uuid"].as_str().unwrap_or_default().to_owned();
                preserved = row["compactMetadata"].get("preservedMessages").cloned();
                segment = row["compactMetadata"].get("preservedSegment").cloned();
                saw_preservation |= preserved.is_some() || segment.is_some();
                pin = None;
                explicit = false;
            }
            if let Some(id) = row["uuid"].as_str() {
                if row["isSidechain"] != true
                    && !(row["type"] == "attachment"
                        && row["attachment"]["type"] == "fork_briefing")
                {
                    last = Some(id.to_owned());
                    cleared = false;
                    explicit = false;
                }
                let node = Node {
                    active: true,
                    id: id.to_owned(),
                    parent: row["parentUuid"].as_str().map(str::to_owned),
                    kind: row["type"].as_str().unwrap().into(),
                    sidechain: row["isSidechain"] == true,
                    sidechain_field: row.get("isSidechain").cloned(),
                    range: start..start + line.len(),
                    timestamp: row["timestamp"].as_str().map(str::to_owned),
                    message_id: row["message"]["id"].as_str().map(str::to_owned),
                    agent_id: row["agentId"].as_str().map(str::to_owned),
                    agent_field: row.get("agentId").cloned(),
                    source_assistant: row["sourceToolAssistantUUID"].as_str().map(str::to_owned),
                    tool_use: row["type"] == "assistant"
                        && row["message"]["content"]
                            .as_array()
                            .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_use")),
                    transparent: row["type"] == "attachment"
                        || (row["type"] == "system" && row["subtype"] != "compact_boundary")
                        || (row["type"] == "user"
                            && row["isMeta"] == true
                            && !row["message"]["content"].as_array().is_some_and(|blocks| {
                                blocks.iter().any(|b| b["type"] == "tool_result")
                            })),
                    attachment_tool: row["attachment"]["toolUseID"].as_str().map(str::to_owned),
                    tool_uses: if row["type"] == "assistant" {
                        block_ids(&row, "tool_use", "id")
                    } else {
                        Vec::new()
                    },
                    tool_results: if row["type"] == "user" {
                        block_ids(&row, "tool_result", "tool_use_id")
                    } else {
                        Vec::new()
                    },
                    tool_result: row["type"] == "user"
                        && row["message"]["content"].as_array().is_some_and(|blocks| {
                            blocks.iter().any(|b| b["type"] == "tool_result")
                        }),
                };
                if let Some(i) = by_id.get(id).copied() {
                    nodes[i] = node;
                } else {
                    by_id.insert(id.to_owned(), nodes.len());
                    nodes.push(node);
                }
            }
        }
        if preserved.is_none()
            && let Some(segment) = segment
        {
            let mut ids = Vec::new();
            let mut seen = HashSet::new();
            let mut next = segment["tailUuid"].as_str();
            while let Some(id) = next {
                if !seen.insert(id) {
                    break;
                }
                let Some(i) = by_id.get(id) else {
                    break;
                };
                ids.push(id);
                if Some(id) == segment["headUuid"].as_str() {
                    ids.reverse();
                    preserved =
                        Some(serde_json::json!({"anchorUuid":segment["anchorUuid"],"uuids":ids}));
                    break;
                }
                next = nodes[*i].parent.as_deref();
            }
            if preserved.is_none() {
                return Err(Error::BrokenCompaction { uuid: boundary });
            }
        }
        let mut preserved_tail = None;
        let mut retained = HashSet::new();
        if let Some(preserved) = preserved {
            if let (Some(anchor), Some(ids)) = (
                preserved["anchorUuid"].as_str(),
                preserved["uuids"].as_array(),
            ) {
                let mut unique = HashSet::new();
                if !by_id.contains_key(anchor)
                    || ids.iter().any(|id| {
                        id.as_str().is_none_or(|id| {
                            id == anchor || !by_id.contains_key(id) || !unique.insert(id)
                        })
                    })
                {
                    return Err(Error::BrokenCompaction { uuid: boundary });
                }
                let mut parent = anchor.to_owned();
                let first = ids.first().and_then(Value::as_str);
                for id in ids.iter().filter_map(Value::as_str) {
                    if let Some(i) = by_id.get(id) {
                        retained.insert(id.to_owned());
                        nodes[*i].parent = Some(parent);
                        parent = id.to_owned();
                    }
                }
                if !ids.is_empty() {
                    preserved_tail = Some(parent.clone());
                }
                for node in &mut nodes {
                    if node.parent.as_deref() == Some(anchor) && Some(node.id.as_str()) != first {
                        node.parent = Some(parent.clone());
                    }
                }
            } else {
                return Err(Error::BrokenCompaction { uuid: boundary });
            }
        }
        if saw_preservation {
            let boundary_index = by_id[&boundary];
            for (i, node) in nodes.iter_mut().enumerate() {
                node.active = i >= boundary_index || retained.contains(&node.id);
            }
            let removed: HashSet<_> = nodes
                .iter()
                .filter(|n| !n.active)
                .map(|n| n.id.clone())
                .collect();
            if let Some(tail) = &preserved_tail {
                for node in &mut nodes {
                    if node.active
                        && matches!(node.kind.as_str(), "user" | "assistant")
                        && node.parent.as_ref().is_some_and(|id| removed.contains(id))
                    {
                        node.parent = Some(tail.clone());
                    }
                }
            }
        }
        Ok(Self {
            data,
            nodes,
            by_id,
            last,
            pin,
            explicit,
            pin_was_explicit,
            cleared,
            has_leaf_metadata,
            preserved_tail,
        })
    }

    /// Look up an original record, including records outside the current history.
    pub fn record(&self, uuid: &str) -> Option<Record<'_>> {
        self.by_id
            .get(uuid)
            .map(|i| Record::new(&self.nodes[*i], self.data))
    }

    fn parent(&self, node: &Node) -> Result<Option<&Node>, Error> {
        node.parent
            .as_ref()
            .map(|id| {
                self.by_id
                    .get(id)
                    .map(|i| &self.nodes[*i])
                    .filter(|n| n.active)
                    .ok_or_else(|| Error::MissingParent {
                        uuid: node.id.clone(),
                        parent: id.clone(),
                    })
            })
            .transpose()
    }

    fn descends(&self, mut from: usize, ancestor: usize) -> bool {
        let mut seen = HashSet::new();
        while seen.insert(from) {
            if from == ancestor {
                return true;
            }
            let Some(parent) = self.nodes[from]
                .parent
                .as_ref()
                .and_then(|id| self.by_id.get(id))
            else {
                break;
            };
            from = *parent;
        }
        false
    }

    fn in_batch(node: &Node, message: &str) -> bool {
        node.transparent
            || node.tool_result
            || (node.kind == "assistant" && node.message_id.as_deref() == Some(message))
    }

    fn preceding_batch<'s>(&'s self, node: &'s Node, message: &str) -> Vec<&'s Node> {
        let mut batch = Vec::new();
        let mut seen = HashSet::from([&node.id]);
        let mut cursor = self.parent(node).ok().flatten();
        while let Some(node) = cursor {
            if !Self::in_batch(node, message) || !seen.insert(&node.id) {
                break;
            }
            batch.push(node);
            cursor = self.parent(node).ok().flatten();
        }
        batch
    }

    fn follows_batch(&self, last: usize, pin: usize) -> bool {
        // CLI throughBatch crosses metadata before identifying the pinned tool.
        let mut cursor = Some(&self.nodes[pin]);
        let mut skipped = None;
        let mut seen = HashSet::new();
        while let Some(node) = cursor.filter(|n| n.transparent) {
            if !seen.insert(&node.id) {
                return false;
            }
            skipped = Some(node);
            cursor = self.parent(node).ok().flatten();
        }
        let mut source = Vec::new();
        let mut attachment = false;
        let assistant = if let Some(result) = cursor.filter(|n| n.tool_result) {
            source.clone_from(&result.tool_results);
            self.parent(result).ok().flatten()
        } else if let Some(skipped) = skipped {
            match (cursor, skipped.attachment_tool.as_ref()) {
                (Some(node), Some(tool)) if node.kind == "assistant" => {
                    source.push(tool.clone());
                    attachment = true;
                    node.message_id.as_deref().and_then(|id| {
                        std::iter::once(node)
                            .chain(self.preceding_batch(node, id))
                            .find(|n| n.tool_uses.contains(tool))
                    })
                }
                _ => None,
            }
        } else {
            cursor
        };
        let Some(assistant) =
            assistant.filter(|n| n.kind == "assistant" && !n.tool_uses.is_empty())
        else {
            return false;
        };
        let Some(message) = assistant.message_id.as_deref() else {
            return false;
        };
        let preceding: HashSet<_> = self
            .preceding_batch(assistant, message)
            .into_iter()
            .map(|n| &n.id)
            .collect();
        let mut cursor = Some(&self.nodes[last]);
        let mut seen = HashSet::new();
        let mut child: Option<&Node> = None;
        let mut continuous = false;
        while let Some(node) = cursor {
            if !seen.insert(&node.id) {
                break;
            }
            if continuous && node.id == assistant.id {
                return true;
            }
            if node.id != assistant.id
                && node.kind == "assistant"
                && !node.tool_uses.is_empty()
                && node.message_id.as_deref() == Some(message)
                && node.sidechain == assistant.sidechain
                && node.agent_id == assistant.agent_id
            {
                if preceding.contains(&node.id) {
                    return true;
                }
                continuous = true;
            } else if !Self::in_batch(node, message) {
                continuous = false;
            }
            if node.id == assistant.id
                && source.iter().any(|id| assistant.tool_uses.contains(id))
                && child.is_some_and(|n| {
                    n.tool_results.iter().any(|id| {
                        assistant.tool_uses.contains(id)
                            && if attachment {
                                assistant.tool_uses.len() > 1
                            } else {
                                !source.contains(id)
                            }
                    })
                })
            {
                return true;
            }
            child = Some(node);
            cursor = self.parent(node).ok().flatten();
        }
        false
    }

    fn timestamp_descendant(&self, last: usize) -> usize {
        let newest = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.active && !n.sidechain && n.timestamp.is_some())
            .reduce(|best, n| {
                if n.1.timestamp > best.1.timestamp {
                    n
                } else {
                    best
                }
            });
        if newest.is_none_or(|(i, _)| i == last) {
            return last;
        }
        let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, node) in self.nodes.iter().enumerate().filter(|(_, n)| n.active) {
            if let Some(parent) = node.parent.as_deref() {
                children.entry(parent).or_default().push(i);
            }
        }
        let mut descendants = HashSet::new();
        let mut stack = vec![last];
        while let Some(i) = stack.pop() {
            for child in children
                .get(self.nodes[i].id.as_str())
                .into_iter()
                .flatten()
            {
                if *child != last && descendants.insert(*child) {
                    stack.push(*child);
                }
            }
        }
        // File/map order breaks equal timestamp ties, matching the CLI scanner.
        self.nodes
            .iter()
            .enumerate()
            .filter(|(i, n)| descendants.contains(i) && !n.sidechain && n.timestamp.is_some())
            .max_by_key(|(_, n)| &n.timestamp)
            .map(|(i, _)| i)
            .unwrap_or(last)
    }

    fn recover_batches<'s>(&'s self, path: Vec<&'s Node>) -> Vec<&'s Node> {
        if !path.iter().any(|n| n.kind == "assistant") {
            return path;
        }
        // A message ID defines the batch. Agent/sidechain checks apply only to
        // fallback result association, not to message grouping or direct parents.
        let mut groups: HashMap<&str, Vec<&Node>> = HashMap::new();
        let mut results: HashMap<&str, Vec<&Node>> = HashMap::new();
        let mut children: HashMap<&str, Vec<&Node>> = HashMap::new();
        let mut calls: HashMap<&str, Option<&Node>> = HashMap::new();
        for node in self.nodes.iter().filter(|n| n.active) {
            if let Some(parent) = node.parent.as_deref() {
                children.entry(parent).or_default().push(node);
            }
            if node.kind == "assistant"
                && let Some(message) = node.message_id.as_deref().filter(|id| !id.is_empty())
            {
                groups.entry(message).or_default().push(node);
                for call in &node.tool_uses {
                    // Within one message, the last chunk owns the call. Once
                    // different messages claim it, it stays ambiguous.
                    calls
                        .entry(call)
                        .and_modify(|owner| {
                            if owner.is_some_and(|old| old.message_id == node.message_id) {
                                *owner = Some(node);
                            } else {
                                *owner = None;
                            }
                        })
                        .or_insert(Some(node));
                }
            }
        }
        for result in self.nodes.iter().filter(|n| n.active && n.tool_result) {
            let mut owners = Vec::new();
            if let Some(parent) = result.parent.as_deref() {
                owners.push(parent);
            }
            let parent = result
                .parent
                .as_ref()
                .and_then(|id| self.by_id.get(id))
                .map(|i| &self.nodes[*i]);
            let correctly_parented = !result.tool_results.is_empty()
                && parent.is_some_and(|p| {
                    result
                        .tool_results
                        .iter()
                        .all(|id| p.tool_uses.contains(id))
                });
            if !correctly_parented {
                let source = result
                    .source_assistant
                    .as_ref()
                    .and_then(|id| self.by_id.get(id))
                    .map(|i| &self.nodes[*i]);
                for owner in source.into_iter().chain(
                    result
                        .tool_results
                        .iter()
                        .filter_map(|id| calls.get(id.as_str()).copied().flatten()),
                ) {
                    if same_field(sidechain_scope(owner), sidechain_scope(result))
                        && same_field(owner.agent_field.as_ref(), result.agent_field.as_ref())
                    {
                        owners.push(&owner.id);
                    }
                }
            }
            let mut unique = HashSet::new();
            for owner in owners {
                if unique.insert(owner) {
                    results.entry(owner).or_default().push(result);
                }
            }
        }
        let already_returned: HashSet<_> = path.iter().flat_map(|n| &n.tool_results).collect();
        let mut known: HashSet<_> = path.iter().map(|n| &n.id).collect();
        // Index selected batch ends once: ordinary long histories stay linear.
        let mut group_ends = HashMap::new();
        for (position, node) in path.iter().enumerate() {
            if node.kind == "assistant"
                && let Some(id) = node.message_id.as_deref()
            {
                group_ends.insert(id, position + 1);
            }
        }
        let mut effective = HashMap::new();
        let order = |node: &Node, effective: &HashMap<&str, usize>| {
            let position = self.by_id[&node.id];
            (
                effective
                    .get(node.id.as_str())
                    .copied()
                    .unwrap_or(position * 2),
                position,
            )
        };
        let mut handled = HashSet::new();
        type RecoveryWindow<'n> = (usize, usize, Vec<&'n Node>, HashMap<&'n str, Vec<&'n Node>>);
        let mut windows: Vec<RecoveryWindow<'_>> = Vec::new();
        for (start, node) in path.iter().enumerate() {
            let Some(message) = node
                .message_id
                .as_deref()
                .filter(|id| node.kind == "assistant" && !id.is_empty())
            else {
                continue;
            };
            if !handled.insert(message) {
                continue;
            }
            let group = &groups[message];
            let group_ids: HashSet<_> = group.iter().map(|n| n.id.as_str()).collect();
            let mut recovered: Vec<_> = group
                .iter()
                .copied()
                .filter(|n| !known.contains(&n.id))
                .collect();
            let mut direct = Vec::new();
            let mut stale = Vec::new();
            let mut seen_results = HashSet::new();
            for assistant in group {
                for result in results.get(assistant.id.as_str()).into_iter().flatten() {
                    if known.contains(&result.id) || !seen_results.insert(&result.id) {
                        continue;
                    }
                    if result
                        .parent
                        .as_deref()
                        .is_some_and(|id| group_ids.contains(id))
                    {
                        direct.push(*result);
                    } else {
                        stale.push(*result);
                    }
                }
            }
            // Keep the selected path's call set shared; only batch-local
            // additions need copying. Positions are doubled to represent .5.
            let mut returned: HashSet<_> = direct.iter().flat_map(|n| &n.tool_results).collect();
            stale.sort_by_key(|n| order(n, &effective));
            let floor = group
                .iter()
                .map(|n| self.by_id[&n.id] * 2 + 1)
                .max()
                .unwrap();
            for result in stale {
                if result.tool_results.iter().any(|id| {
                    !already_returned.contains(id)
                        && !returned.contains(id)
                        && calls.get(id.as_str()).is_some_and(|owner| {
                            owner.is_some_and(|n| group_ids.contains(n.id.as_str()))
                        })
                }) {
                    returned.extend(&result.tool_results);
                    effective.insert(result.id.as_str(), (self.by_id[&result.id] * 2).max(floor));
                    direct.push(result);
                }
            }
            recovered.extend(direct);
            known.extend(recovered.iter().map(|n| &n.id));
            let mut end = group_ends[message];
            while end < path.len() && Self::in_batch(path[end], message) {
                end += 1;
            }
            let mut anchored = HashMap::new();
            if group.iter().any(|n| n.tool_use) {
                // Every known result associated with this batch is a root,
                // including results that were already on the selected path.
                let mut roots = group.clone();
                let mut root_ids = group_ids.clone();
                for assistant in group {
                    for result in results.get(assistant.id.as_str()).into_iter().flatten() {
                        if known.contains(&result.id) && root_ids.insert(&result.id) {
                            roots.push(result);
                        }
                    }
                }
                let mut recover_tail = |parent: &Node| {
                    let parent_position = effective.get(parent.id.as_str()).copied();
                    let mut tails = Vec::new();
                    for child in children.get(parent.id.as_str()).into_iter().flatten() {
                        let mut chain = Vec::new();
                        let mut next = Some(*child);
                        let mut seen = HashSet::new();
                        while let Some(n) = next {
                            // Tail acceptance is all-or-nothing. Unlike fallback
                            // association, it compares the raw sidechain fields
                            // (absent differs from false), and ignores agent ID.
                            if known.contains(&n.id)
                                || !n.transparent
                                || !same_field(
                                    n.sidechain_field.as_ref(),
                                    parent.sidechain_field.as_ref(),
                                )
                                || !seen.insert(&n.id)
                            {
                                chain.clear();
                                break;
                            }
                            chain.push(n);
                            let mut remaining = children
                                .get(n.id.as_str())
                                .into_iter()
                                .flatten()
                                .copied()
                                .filter(|n| !known.contains(&n.id));
                            next = remaining.next();
                            if remaining.next().is_some() {
                                chain.clear();
                                break;
                            }
                        }
                        for n in chain {
                            known.insert(&n.id);
                            if let Some(position) = parent_position {
                                effective
                                    .insert(n.id.as_str(), (self.by_id[&n.id] * 2).max(position));
                            }
                            tails.push(n);
                        }
                    }
                    tails
                };
                for root in roots {
                    recovered.extend(recover_tail(root));
                }
                for anchor in &path[start + 1..end] {
                    if anchor.transparent {
                        let tails = recover_tail(anchor);
                        if !tails.is_empty() {
                            anchored.insert(anchor.id.as_str(), tails);
                        }
                    }
                }
            }
            if !recovered.is_empty() || !anchored.is_empty() {
                windows.push((start, end, recovered, anchored));
            }
        }
        // Finish all recovery before merging overlapping intervals. Effective
        // positions may have been assigned while processing a later batch.
        let mut merged: Vec<RecoveryWindow<'_>> = Vec::new();
        for (start, end, recovered, anchored) in windows {
            if let Some(previous) = merged.last_mut().filter(|w| start < w.1) {
                previous.1 = previous.1.max(end);
                previous.2.extend(recovered);
                for (id, tails) in anchored {
                    previous.3.entry(id).or_default().extend(tails);
                }
            } else {
                merged.push((start, end, recovered, anchored));
            }
        }
        let mut output = Vec::new();
        let mut cursor = 0;
        for (start, end, mut recovered, anchored) in merged {
            recovered.sort_by_key(|n| order(n, &effective));
            output.extend_from_slice(&path[cursor..=start]);
            let mut recovered = recovered.into_iter().peekable();
            for node in &path[start + 1..end] {
                while recovered
                    .peek()
                    .is_some_and(|r| order(r, &effective).0 < self.by_id[&node.id] * 2)
                {
                    output.push(recovered.next().unwrap());
                }
                output.push(node);
                if let Some(tails) = anchored.get(node.id.as_str()) {
                    output.extend(tails);
                }
            }
            output.extend(recovered);
            cursor = end;
        }
        output.extend_from_slice(&path[cursor..]);
        output
    }

    /// Select the main conversation and materialize its ordered record references.
    /// This is not an API-message conversion; raw fields remain unchanged.
    pub fn current(&self) -> Result<History<'_>, Error> {
        if self.cleared {
            return Ok(History {
                path: Vec::new(),
                data: self.data,
                cleared: true,
                leaf: None,
            });
        }
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let pinned = self
            .pin
            .as_ref()
            .and_then(|id| self.by_id.get(id))
            .copied()
            .filter(|i| self.nodes[*i].active && !self.nodes[*i].sidechain);
        let latest = self
            .last
            .as_ref()
            .and_then(|id| self.by_id.get(id))
            .copied()
            .filter(|i| self.nodes[*i].active);
        let latest = if !self.has_leaf_metadata {
            latest.map(|last| self.timestamp_descendant(last))
        } else {
            latest
        };
        let selected = match (pinned, latest) {
            (Some(pin), Some(last))
                if !self.explicit
                    && (self.descends(last, pin)
                        || (!self.pin_was_explicit && self.follows_batch(last, pin))) =>
            {
                Some(last)
            }
            (Some(pin), _) => Some(pin),
            (_, last) => last,
        };
        let selected = if !self.explicit {
            match (
                selected,
                self.preserved_tail
                    .as_ref()
                    .and_then(|id| self.by_id.get(id))
                    .copied(),
            ) {
                (Some(selected), Some(tail)) if self.descends(tail, selected) => Some(tail),
                (selected, _) => selected,
            }
        } else {
            selected
        };
        let mut node = selected.map(|i| &self.nodes[i]);
        while let Some(n) = node {
            if matches!(n.kind.as_str(), "user" | "assistant") {
                break;
            }
            if !seen.insert(&n.id) {
                return Err(Error::ParentCycle { uuid: n.id.clone() });
            }
            node = self.parent(n)?;
        }
        if let Some(node) = node
            && node
                .timestamp
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .is_none()
        {
            return Err(Error::InvalidTimestamp {
                uuid: node.id.clone(),
            });
        }
        let leaf = node.map(|n| n.id.as_str());
        seen.clear();
        while let Some(n) = node {
            if !seen.insert(&n.id) {
                return Err(Error::ParentCycle { uuid: n.id.clone() });
            }
            path.push(n);
            node = self.parent(n)?;
        }
        path.reverse();
        path = self.recover_batches(path);
        // Batch recovery may already have inserted the selected leaf's tail.
        seen.extend(path.iter().map(|n| &n.id));
        if let Some(leaf) = leaf {
            let mut children: HashMap<&str, Vec<&Node>> = HashMap::new();
            for n in self.nodes.iter().filter(|n| n.active) {
                if !matches!(n.kind.as_str(), "user" | "assistant")
                    && !n.sidechain
                    && let Some(parent) = n.parent.as_deref()
                {
                    children.entry(parent).or_default().push(n);
                }
            }
            let mut stack = vec![leaf];
            let mut tail = Vec::new();
            while let Some(parent) = stack.pop() {
                for child in children.get(parent).into_iter().flatten() {
                    if seen.insert(&child.id) {
                        tail.push(*child);
                        stack.push(&child.id);
                    }
                }
            }
            tail.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
            path.extend(tail);
        }
        Ok(History {
            path,
            data: self.data,
            cleared: false,
            leaf,
        })
    }
}

/// One selected history, in conversation order.
#[derive(Debug)]
pub struct History<'a> {
    path: Vec<&'a Node>,
    data: &'a [u8],
    cleared: bool,
    leaf: Option<&'a str>,
}
impl<'a> History<'a> {
    /// True only for an explicit empty last-prompt, not for an empty file.
    pub fn is_cleared(&self) -> bool {
        self.cleared
    }
    /// Selected user/assistant UUID. Trailing attachments do not change this value.
    pub fn leaf(&self) -> Option<&'a str> {
        self.leaf
    }
    /// History order after preservation and parallel-reply recovery.
    pub fn ids(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.path.iter().map(|n| n.id.as_str())
    }

    /// Page backward from the tail; each page is ordered oldest to newest.
    /// `before` is exclusive and must belong to this selected history.
    pub fn page(&self, before: Option<&str>, limit: NonZeroUsize) -> Result<Page<'a>, Error> {
        let end =
            match before {
                None => self.path.len(),
                Some(id) => self.path.iter().position(|n| n.id == id).ok_or_else(|| {
                    Error::InvalidCursor {
                        uuid: id.to_owned(),
                    }
                })?,
            };
        let start = end.saturating_sub(limit.get());
        Ok(Page {
            records: self.path[start..end]
                .iter()
                .map(|n| Record::new(n, self.data))
                .collect(),
            next_before: (start > 0).then(|| self.path[start].id.as_str()),
        })
    }
}

/// Original JSONL bytes. The effective parent after compaction may differ from the raw parent.
#[derive(Debug)]
pub struct Record<'a> {
    uuid: &'a str,
    byte_range: Range<usize>,
    raw: &'a [u8],
}
impl<'a> Record<'a> {
    /// The CLI record UUID; distinct from an assistant API message.id.
    pub fn uuid(&self) -> &'a str {
        self.uuid
    }
    /// Original byte range, excluding the line feed (offsets are not character counts).
    pub fn byte_range(&self) -> Range<usize> {
        self.byte_range.clone()
    }
    /// Original unmodified JSON bytes from the indexed snapshot.
    pub fn raw(&self) -> &'a [u8] {
        self.raw
    }

    fn new(node: &'a Node, data: &'a [u8]) -> Self {
        Self {
            uuid: &node.id,
            byte_range: node.range.clone(),
            raw: &data[node.range.clone()],
        }
    }
    /// Decode just this record, retaining unknown fields and content blocks.
    pub fn decode(&self) -> Value {
        serde_json::from_slice(self.raw).expect("indexed JSON is valid")
    }
}

#[derive(Debug)]
pub struct Page<'a> {
    pub records: Vec<Record<'a>>,
    pub next_before: Option<&'a str>,
}

/// An untrustworthy chain must not be used as an exclusivity baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    InvalidTimestamp { uuid: String },
    InvalidRecord { byte_offset: usize },
    BrokenCompaction { uuid: String },
    IncompleteRecord { byte_offset: usize },
    InvalidJson { byte_offset: usize, message: String },
    MissingParent { uuid: String, parent: String },
    ParentCycle { uuid: String },
    InvalidCursor { uuid: String },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTimestamp { uuid } => {
                write!(f, "record {uuid} has no valid RFC3339 timestamp")
            }
            Self::InvalidRecord { byte_offset } => {
                write!(f, "invalid record envelope at byte {byte_offset}")
            }
            Self::BrokenCompaction { uuid } => write!(
                f,
                "incomplete or invalid preserved context at compact boundary {uuid}"
            ),
            Self::IncompleteRecord { byte_offset } => write!(
                f,
                "incomplete record at byte {byte_offset}; retry with a complete snapshot"
            ),
            Self::InvalidJson {
                byte_offset,
                message,
            } => write!(f, "invalid JSON at byte {byte_offset}: {message}"),
            Self::MissingParent { uuid, parent } => write!(
                f,
                "record {uuid} references missing parent {parent}; read a complete snapshot"
            ),
            Self::ParentCycle { uuid } => write!(f, "parent cycle at {uuid}"),
            Self::InvalidCursor { uuid } => write!(f, "cursor {uuid} is outside this history"),
        }
    }
}
impl std::error::Error for Error {}

fn block_ids(row: &Value, kind: &str, key: &str) -> Vec<String> {
    row["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == kind)
        .filter_map(|block| block[key].as_str().map(str::to_owned))
        .collect()
}
