use nd_watchdog_proto::{Event, read_fixture};
use std::path::Path;

#[test]
fn recorded_real_workflow_has_numbered_input_start_and_successful_completion() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/watchdog/claude/2.1.289/long-workflow.jsonl");
    let (meta, rows) = read_fixture(&path).unwrap();
    assert_eq!(
        (
            meta.backend.as_str(),
            meta.version.as_str(),
            meta.scenario.as_str()
        ),
        ("claude", "2.1.289", "long-workflow")
    );
    assert!(matches!(rows[0].event, Event::In { in_seq: 1, .. }));
    let frames = rows
        .iter()
        .filter_map(|row| match &row.event {
            Event::Out { line } => Some(serde_json::from_str::<serde_json::Value>(line).unwrap()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        frames
            .iter()
            .any(|v| v["subtype"] == "task_started" && v["task_type"] == "local_workflow")
    );
    let done = frames
        .iter()
        .find(|v| v["subtype"] == "task_notification" && v["status"] == "completed")
        .unwrap();
    assert_eq!(done["tool_use_id"], "workflow_volume");
    assert!(done["usage"]["duration_ms"].as_u64().unwrap() >= 60_000);
    assert!(frames.iter().any(|v| {
        v["workflow_progress"]
            .as_array()
            .is_some_and(|agents| agents.len() == 6 && agents.iter().all(|a| a["state"] == "done"))
    }));
}

#[test]
fn truncated_or_reordered_recording_is_rejected_instead_of_replayed_as_complete() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/watchdog/claude/2.1.289/long-workflow.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let gap = dir.path().join("gap.jsonl");
    std::fs::write(
        &gap,
        text.lines()
            .enumerate()
            .filter(|(n, _)| *n != 2)
            .map(|(_, line)| format!("{line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    assert!(
        read_fixture(&gap)
            .unwrap_err()
            .to_string()
            .contains("non-contiguous")
    );
    let suffix = dir.path().join("suffix.jsonl");
    let mut lines = text.lines().collect::<Vec<_>>();
    lines.pop();
    std::fs::write(&suffix, lines.join("\n")).unwrap();
    assert!(
        read_fixture(&suffix).is_err(),
        "removed final record must not look complete"
    );
    let broken = dir.path().join("broken.jsonl");
    std::fs::write(&broken, format!("{text}{{\"seq\":")).unwrap();
    assert!(read_fixture(&broken).is_err());
}
