use nd_claude_records::RecordIndex;
use serde_json::Value;
use std::num::NonZeroUsize;

fn compare(corpus: &str) {
    let mut failures = Vec::new();
    let mut count = 0;
    for line in corpus.lines() {
        let case: Value = serde_json::from_str(line).unwrap();
        let mut bytes = Vec::new();
        for row in case["rows"].as_array().unwrap() {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        let index = RecordIndex::parse(&bytes).unwrap();
        let history = index.current().unwrap();
        let actual: Vec<_> = history.ids().collect();
        let expected: Vec<_> = case["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect();
        if actual != expected {
            failures.push(format!(
                "{}: expected {expected:?}, got {actual:?}",
                case["name"]
            ));
        }
        for size in [1, 2, 7] {
            let mut pages = Vec::new();
            let mut cursor = None;
            loop {
                let page = history
                    .page(cursor, NonZeroUsize::new(size).unwrap())
                    .unwrap();
                pages.push(page.records.iter().map(|r| r.uuid()).collect::<Vec<_>>());
                cursor = page.next_before;
                if cursor.is_none() {
                    break;
                }
            }
            assert_eq!(
                pages.into_iter().rev().flatten().collect::<Vec<_>>(),
                actual
            );
        }
        count += 1;
    }
    assert!(count > 0);
    assert!(
        failures.is_empty(),
        "{}/{count} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("{count} cases: 0 mismatches; full history and pages of 1, 2, 7");
}

#[test]
fn recovery_matches_pinned_cli_orders() {
    compare(include_str!("fixtures/recovery-orders.jsonl"));
}

// Requires the locally pinned proprietary CLI and the external full corpus.
// Extracted code exists only in the child process; it must never be committed.
#[test]
#[ignore = "local CLI extraction and full corpus; see docs/cli-contracts.md"]
fn recovery_matches_locally_extracted_cli() {
    let corpus = std::env::var_os("ND_CLAUDE_RECOVERY_CORPUS")
        .expect("set ND_CLAUDE_RECOVERY_CORPUS to the full corpus.jsonl");
    let result = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/cli/recovery_oracle.py"
        ))
        .arg("--corpus")
        .arg(corpus)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&result.stderr));
    assert!(result.status.success());
    compare(std::str::from_utf8(&result.stdout).unwrap());
}
