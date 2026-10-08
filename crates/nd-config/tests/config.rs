use nd_config::{Config, Error, FileSource, Section};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Deserialize, Debug)]
struct Settings {
    enabled: bool,
}
impl Section for Settings {
    const NAME: &'static str = "diagnostics";
}
fn validate(value: &Value) -> nd_config::Result<()> {
    serde_json::from_value::<Settings>(value["diagnostics"].clone())
        .map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(())
}

#[tokio::test]
async fn update_publishes_typed_section_and_rejects_stale_or_invalid_writes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    let config = Config::open(
        Arc::new(FileSource::new(&file)),
        json!({"diagnostics":{"enabled":true}}),
        validate,
    )
    .unwrap();
    let mut watch = config.section::<Settings>().unwrap();
    let initial = watch.get().unwrap();
    assert!(initial.value.enabled);
    let rev = config
        .update(json!({"diagnostics":{"enabled":false}}), &initial.revision)
        .unwrap();
    let changed = watch.changed().await.unwrap();
    assert_eq!(changed.revision, rev);
    assert!(!changed.value.enabled);
    let bytes = std::fs::read(&file).unwrap();
    assert!(matches!(
        config.update(json!({"diagnostics":{"enabled":true}}), &initial.revision),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        config.update(json!({"diagnostics":{"enabled":"invalid"}}), &rev),
        Err(Error::Invalid(_))
    ));
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let reloaded = Config::open(
        Arc::new(FileSource::new(&file)),
        json!({"diagnostics":{"enabled":true}}),
        validate,
    )
    .unwrap();
    assert!(
        !reloaded
            .section::<Settings>()
            .unwrap()
            .get()
            .unwrap()
            .value
            .enabled
    );
    // 外部手改先被观察，不能由仍拿旧修订的界面盖掉。
    std::fs::write(&file, "[diagnostics]\nenabled = true\n").unwrap();
    assert!(matches!(
        config.update(json!({"diagnostics":{"enabled":false}}), &rev),
        Err(Error::Conflict)
    ));
    assert!(watch.changed().await.unwrap().value.enabled);
}

#[tokio::test]
async fn section_watch_ignores_unrelated_sections_but_get_has_latest_revision() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::open(
        Arc::new(FileSource::new(dir.path().join("config.toml"))),
        json!({"diagnostics":{"enabled":true}}),
        validate,
    )
    .unwrap();
    let mut watch = config.section::<Settings>().unwrap();
    let previous = watch.get().unwrap().revision;
    let next = config
        .update(json!({"other":{"value":42}}), &previous)
        .unwrap();
    assert_eq!(watch.get().unwrap().revision, next);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), watch.changed())
            .await
            .is_err()
    );
    config
        .update(json!({"diagnostics":{"enabled":false}}), &next)
        .unwrap();
    assert!(!watch.changed().await.unwrap().value.enabled);
}

#[test]
fn updates_edit_only_user_values_and_null_removes_an_override() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    let defaults = json!({"claude":null,"diagnostics":{"enabled":true},"storage":{"grace":3600}});
    let config = Config::open(Arc::new(FileSource::new(&file)), defaults, validate).unwrap();
    let revision = config
        .update(
            json!({"diagnostics":{"enabled":false}}),
            &config.snapshot().revision,
        )
        .unwrap();
    let written: toml::Value = toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        written.as_table().unwrap().keys().collect::<Vec<_>>(),
        ["diagnostics"]
    );
    assert_eq!(config.snapshot().value["storage"]["grace"], 3600);
    config
        .update(json!({"diagnostics":{"enabled":null}}), &revision)
        .unwrap();
    assert_eq!(config.snapshot().value["diagnostics"]["enabled"], true);
    let next = Config::open(
        Arc::new(FileSource::new(&file)),
        json!({"diagnostics":{"enabled":true},"storage":{"grace":42}}),
        validate,
    )
    .unwrap();
    assert_eq!(next.snapshot().value["storage"]["grace"], 42);
}
