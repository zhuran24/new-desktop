//! 由主接缝记录的控制往返；只验证协议归一化，产品行为由 nd-daemon 场景覆盖。
use nd_claude::convo::{Convo, replay};
use nd_watchdog_proto::Record;

#[test]
fn pinned_settings_recording_preserves_requested_available_and_applied_values() {
    let records: Vec<Record> = include_str!("fixtures/settings/claude/2.1.289/flags.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let values: Vec<_> = replay(&records)
        .into_iter()
        .flatten()
        .filter_map(|event| match event {
            Convo::Reply { ok: true, body, .. } if body["response"]["applied"].is_object() => {
                let a = &body["response"]["applied"];
                Some((
                    a["effort"].as_str().map(str::to_owned),
                    a["ultracodeRequested"].as_bool().unwrap(),
                    a["ultracodeAvailable"].as_bool().unwrap(),
                    a["ultracode"].as_bool().unwrap(),
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        values,
        vec![
            (Some("medium".into()), false, true, false),
            (Some("high".into()), false, true, false),
            (Some("high".into()), true, true, true),
            (Some("medium".into()), false, true, false),
            (Some("max".into()), false, true, false),
            (Some("max".into()), true, true, true),
            (None, true, false, false),
        ]
    );
}
