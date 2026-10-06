//! Live acceptance: true CLI, real rewind/compact/resume, only a local model endpoint.
use nd_claude_records::RecordIndex;
use serde_json::Value;
use std::{fs, num::NonZeroUsize, path::PathBuf, process::Command};

#[test]
#[ignore = "requires Linux user systemd, bwrap and the pinned CLI; see README"]
fn real_cli_records_with_a_branch_and_two_compactions_match_resumed_context() {
    let destination = PathBuf::from(
        std::env::var_os("ND_RECORDS_EVIDENCE_DIR")
            .expect("set ND_RECORDS_EVIDENCE_DIR to an empty evidence directory on the build disk"),
    );
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cli/generate.py");
    let output = Command::new("python3")
        .arg(script)
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "generator failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let data = fs::read(destination.join("history.jsonl")).unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(destination.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["cli_version"], "2.1.289 (Claude Code)");
    let frames: Vec<Value> = fs::read_to_string(destination.join("write.frames.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let expected_leaf = frames
        .iter()
        .rev()
        .find(|f| f["type"] == "assistant")
        .unwrap()["uuid"]
        .as_str()
        .unwrap();
    let index = RecordIndex::parse(&data).unwrap();
    let history = index.current().unwrap();
    assert_eq!(history.leaf(), Some(expected_leaf));
    assert_eq!(manifest["compact_boundaries"].as_array().unwrap().len(), 2);
    let ids: Vec<_> = history.ids().collect();
    for discarded in ["alpha", "beta", "gamma", "delta"] {
        let id = manifest["prompts"][discarded].as_str().unwrap();
        assert!(
            index.record(id).is_some(),
            "CLI must actually retain the original record"
        );
        assert!(!ids.contains(&id));
    }
    assert!(ids.contains(&manifest["prompts"]["omega"].as_str().unwrap()));
    let mut before = None;
    let mut pages = Vec::new();
    loop {
        let page = history.page(before, NonZeroUsize::new(3).unwrap()).unwrap();
        for record in &page.records {
            assert_eq!(&data[record.byte_range()], record.raw());
        }
        before = page.next_before;
        pages.push(page.records);
        if before.is_none() {
            break;
        }
    }
    let records: Vec<_> = pages.into_iter().rev().flatten().collect();
    assert_eq!(records.iter().map(|r| r.uuid()).collect::<Vec<_>>(), ids);
    let messages: Vec<_> = records
        .iter()
        .map(|r| r.decode())
        .filter(|r| matches!(r["type"].as_str(), Some("user" | "assistant")) && r["isMeta"] != true)
        .collect();
    let requests: Value =
        serde_json::from_slice(&fs::read(destination.join("requests.json")).unwrap()).unwrap();
    let resumed =
        &requests[manifest["resumed_model_request"].as_u64().unwrap() as usize]["messages"];
    // The actual resumed request starts with the newest summary and the preserved assistant.
    assert_eq!(messages[0]["message"]["content"], resumed[0]["content"]);
    assert_eq!(messages[1]["message"]["content"], resumed[1]["content"]);
    assert_eq!(
        messages.last().unwrap()["message"]["content"],
        resumed[3]["content"]
    );
    assert!(resumed.to_string().contains("OMEGA after two compactions"));
    assert!(!resumed.to_string().contains("BETA discarded branch"));
    assert!(!resumed.to_string().contains("GAMMA discarded branch"));
}
