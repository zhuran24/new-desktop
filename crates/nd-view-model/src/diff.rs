//! 完整重算的简单 diff 显示基准；后续优化须对同一输入比较全部行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Header,
    Hunk,
    Context,
    Added,
    Removed,
    Notice,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    /// 保留原来的前缀、空白和缺少行尾的标记。
    pub text: String,
    pub kind: DiffKind,
    pub old: Option<u64>,
    pub new: Option<u64>,
}

pub fn unified_diff(text: &str) -> Vec<DiffLine> {
    let mut range: Option<(u64, u64, u64, u64)> = None;
    text.split('\n')
        .map(|line| {
            let mut row = DiffLine {
                text: line.into(),
                kind: DiffKind::Notice,
                old: None,
                new: None,
            };
            if line.starts_with("@@") {
                range = hunk(line);
                row.kind = DiffKind::Hunk;
            } else if line.starts_with("diff ")
                || line.starts_with("index ")
                || (range.is_none() && (line.starts_with("--- ") || line.starts_with("+++ ")))
            {
                range = None;
                row.kind = DiffKind::Header;
            } else if let Some((old, new, left, right)) = range.as_mut() {
                match line.as_bytes().first() {
                    Some(b' ') | None if *left > 0 && *right > 0 => {
                        row.kind = DiffKind::Context;
                        row.old = Some(*old);
                        row.new = Some(*new);
                        *old = old.saturating_add(1);
                        *new = new.saturating_add(1);
                        *left -= 1;
                        *right -= 1;
                    }
                    Some(b'-') if *left > 0 => {
                        row.kind = DiffKind::Removed;
                        row.old = Some(*old);
                        *old = old.saturating_add(1);
                        *left -= 1;
                    }
                    Some(b'+') if *right > 0 => {
                        row.kind = DiffKind::Added;
                        row.new = Some(*new);
                        *new = new.saturating_add(1);
                        *right -= 1;
                    }
                    _ => {}
                }
                if *left == 0 && *right == 0 {
                    range = None;
                }
            }
            row
        })
        .collect()
}
fn hunk(line: &str) -> Option<(u64, u64, u64, u64)> {
    fn part(s: &str, prefix: char) -> Option<(u64, u64)> {
        let s = s.strip_prefix(prefix)?;
        let (start, count) = s.split_once(',').unwrap_or((s, "1"));
        Some((start.parse().ok()?, count.parse().ok()?))
    }
    let mut words = line.split_whitespace();
    if words.next()? != "@@" {
        return None;
    }
    let (old, left) = part(words.next()?, '-')?;
    let (new, right) = part(words.next()?, '+')?;
    if words.next()? != "@@" {
        return None;
    }
    Some((old, new, left, right))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageBlock {
    Markdown(String),
    Plain(String),
    Diff(Vec<DiffLine>),
}

/// 简单替换基准：整段旧内容删除、整段新内容加入，保留缺少末尾换行的信息。
/// 不猜测 Edit 在文件中的绝对行号；这里的行号从被替换的片段开头计。
pub fn replacement_diff(before: &str, after: &str) -> Vec<DiffLine> {
    let old: Vec<_> = before.split_inclusive('\n').collect();
    let new: Vec<_> = after.split_inclusive('\n').collect();
    let mut patch = format!(
        "@@ -{},{} +{},{} @@\n",
        usize::from(!old.is_empty()),
        old.len(),
        usize::from(!new.is_empty()),
        new.len()
    );
    for (prefix, lines) in [('-', old), ('+', new)] {
        for line in lines {
            patch.push(prefix);
            patch.push_str(line);
            if !line.ends_with('\n') {
                patch.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    unified_diff(patch.trim_end_matches('\n'))
}

pub fn message_blocks(kind: &str, text: &str, raw: &serde_json::Value) -> Vec<MessageBlock> {
    if kind == "tool_use"
        && raw["name"] == "Edit"
        && let (Some(before), Some(after)) = (
            raw["input"]["old_string"].as_str(),
            raw["input"]["new_string"].as_str(),
        )
    {
        return vec![
            MessageBlock::Plain(format!(
                "Edit · {}（片段行号）",
                raw["input"]["file_path"].as_str().unwrap_or_default()
            )),
            MessageBlock::Diff(replacement_diff(before, after)),
        ];
    }
    if kind == "diff" {
        return vec![MessageBlock::Diff(unified_diff(text))];
    }
    if kind != "text" {
        return vec![MessageBlock::Plain(text.into())];
    }
    let mut blocks = Vec::new();
    let mut pending = String::new();
    let mut fence: Option<(char, usize, bool)> = None;
    for line in text.split_inclusive('\n') {
        let clean = line.trim_end_matches(['\r', '\n']);
        let trimmed = clean.trim_start_matches(' ');
        let indent = clean.len() - trimmed.len();
        let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
        let count = marker
            .map(|c| trimmed.chars().take_while(|x| *x == c).count())
            .unwrap_or(0);
        if let Some((ch, len, diff)) = fence {
            if indent <= 3
                && marker == Some(ch)
                && count >= len
                && trimmed[count..].trim().is_empty()
            {
                if diff {
                    blocks.push(MessageBlock::Diff(unified_diff(
                        pending.strip_suffix('\n').unwrap_or(&pending),
                    )));
                    pending.clear();
                } else {
                    pending.push_str(line);
                }
                fence = None;
            } else {
                pending.push_str(line);
            }
        } else if indent <= 3 && count >= 3 {
            let diff = matches!(trimmed[count..].trim(), "diff" | "patch");
            if diff {
                if !pending.is_empty() {
                    blocks.push(MessageBlock::Markdown(std::mem::take(&mut pending)));
                }
            } else {
                pending.push_str(line);
            }
            fence = Some((marker.unwrap(), count, diff));
        } else {
            pending.push_str(line);
        }
    }
    if fence.is_some_and(|(_, _, diff)| diff) {
        blocks.push(MessageBlock::Diff(unified_diff(&pending)));
    } else if !pending.is_empty() {
        blocks.push(MessageBlock::Markdown(pending));
    }
    blocks
}
