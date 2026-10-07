use nd_view_model::{DiffKind, unified_diff};

#[test]
fn unified_diff_preserves_headers_hunks_unicode_and_missing_newline_markers() {
    let input = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -2,2 +2,3 @@ fn main\n 相同🦀\n-旧\n+新\n+多一行\n\\ No newline at end of file\n@@ -9,0 +10,1 @@\n+尾行";
    let lines = unified_diff(input);
    assert_eq!(
        lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
        input.split('\n').collect::<Vec<_>>()
    );
    assert_eq!(
        lines
            .iter()
            .map(|l| (l.kind, l.old, l.new))
            .collect::<Vec<_>>(),
        vec![
            (DiffKind::Header, None, None),
            (DiffKind::Header, None, None),
            (DiffKind::Header, None, None),
            (DiffKind::Hunk, None, None),
            (DiffKind::Context, Some(2), Some(2)),
            (DiffKind::Removed, Some(3), None),
            (DiffKind::Added, None, Some(3)),
            (DiffKind::Added, None, Some(4)),
            (DiffKind::Notice, None, None),
            (DiffKind::Hunk, None, None),
            (DiffKind::Added, None, Some(10)),
        ]
    );
    // 未闭合流式 hunk、损坏输入照显示，不能凭空添行号。
    let broken = unified_diff("@@ -oops\n+仍显示\n");
    assert!(broken.iter().all(|l| l.old.is_none() && l.new.is_none()));
}

#[test]
fn conversation_renders_fenced_and_edit_tool_diffs_without_losing_surrounding_text() {
    use nd_view_model::{MessageBlock, message_blocks};
    use serde_json::json;
    let blocks = message_blocks(
        "text",
        "说明\n```diff\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-旧\n+新\n```\n后文",
        &json!(null),
    );
    assert!(matches!(&blocks[0], MessageBlock::Markdown(t) if t == "说明\n"));
    assert!(
        matches!(&blocks[1], MessageBlock::Diff(lines) if lines.iter().any(|l| l.text == "+新" && l.new == Some(1)))
    );
    assert!(matches!(&blocks[2], MessageBlock::Markdown(t) if t == "后文"));
    let edit = message_blocks(
        "tool_use",
        "Edit",
        &json!({"name":"Edit","input":{"file_path":"a.rs","old_string":"相同\n旧","new_string":"相同\n新\n"}}),
    );
    let MessageBlock::Diff(lines) = &edit[1] else {
        panic!("Edit needs a diff");
    };
    assert!(lines.iter().any(|l| l.text == "-旧"));
    assert!(lines.iter().any(|l| l.text == "+新"));
    assert!(
        !lines
            .iter()
            .any(|l| l.text == "\\ No newline at end of file")
    );
    let literal = "```rust\n```diff\n+literal\n```\n";
    assert_eq!(
        message_blocks("text", literal, &json!(null)),
        vec![MessageBlock::Markdown(literal.into())]
    );
}

#[test]
fn empty_context_lines_advance_both_sides_and_close_the_hunk_before_the_next_file() {
    let rows = unified_diff(
        "@@ -1,4 +1,4 @@\n a\n\n-b\n+c\n d\n--- a/next\n+++ b/next\n@@ -8 +9 @@\n-x\n+y",
    );
    assert_eq!(
        (rows[2].kind, rows[2].old, rows[2].new),
        (DiffKind::Context, Some(2), Some(2))
    );
    assert_eq!(rows[3].old, Some(3));
    assert_eq!(rows[4].new, Some(3));
    assert_eq!((rows[5].old, rows[5].new), (Some(4), Some(4)));
    assert_eq!(rows[6].kind, DiffKind::Header);
    assert_eq!(rows[7].kind, DiffKind::Header);
    assert_eq!(rows[9].old, Some(8));
    assert_eq!(rows[10].new, Some(9));
}
