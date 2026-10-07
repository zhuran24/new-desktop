use nd_view_model::ThemeDocument;
use nd_view_model::{Theme, ThemeCatalog, ThemeMode, ThemeSelection};

const OCEAN: &str = include_str!("fixtures/ocean.json");

#[test]
fn device_selection_survives_reopen_and_only_system_choice_follows_appearance() {
    use nd_view_model::ViewStateFile;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ui.json");
    let file = ViewStateFile::open(&path).unwrap();
    let mut state = file.load().unwrap();
    assert_eq!(state.theme_selection(), ThemeSelection::System);
    for selection in [
        ThemeSelection::System,
        ThemeSelection::Light,
        ThemeSelection::Dark,
        ThemeSelection::File("海风.json".into()),
    ] {
        state.theme_selection = Some(selection.clone());
        file.save(&state).unwrap();
        assert_eq!(file.load().unwrap().theme_selection(), selection);
    }
    let catalog = ThemeCatalog::default();
    for system in [ThemeMode::Dark, ThemeMode::Light] {
        assert_eq!(
            catalog.resolve(&ThemeSelection::System, system).theme.mode,
            system
        );
        assert_eq!(
            catalog.resolve(&ThemeSelection::Dark, system).theme.mode,
            ThemeMode::Dark
        );
        assert_eq!(
            catalog.resolve(&ThemeSelection::Light, system).theme.mode,
            ThemeMode::Light
        );
    }
    // 旧版本只保存 theme；迁移不能丢失原来的手动选择。
    std::fs::write(path, r#"{"theme":"light"}"#).unwrap();
    assert_eq!(
        file.load().unwrap().theme_selection(),
        ThemeSelection::Light
    );
}

#[test]
fn selected_file_reloads_falls_back_with_filename_and_recovers_after_repair() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ocean.json");
    let choice = ThemeSelection::File("ocean.json".into());
    std::fs::write(&path, OCEAN).unwrap();
    let catalog = ThemeCatalog::read(directory.path());
    assert_eq!(catalog.entries[0].name, "海风");
    assert_eq!(
        catalog
            .resolve(&choice, ThemeMode::Light)
            .theme
            .colors
            .background,
        0x123456ff
    );
    std::fs::write(&path, OCEAN.replace("#123456ff", "#abcdefFF")).unwrap();
    let loaded = ThemeCatalog::read(directory.path()).resolve(&choice, ThemeMode::Light);
    assert_eq!(loaded.theme.colors.background, 0xabcdefff);
    for bad in ["{", "{}", &OCEAN.replace("\"radius\": 12,", "")] {
        std::fs::write(&path, bad).unwrap();
        let loaded = ThemeCatalog::read(directory.path()).resolve(&choice, ThemeMode::Light);
        assert_eq!(loaded.theme, Theme::builtin(ThemeMode::Light));
        assert!(loaded.warning.as_ref().unwrap().contains("ocean.json"));
        assert!(loaded.warning.unwrap().contains("默认"));
    }
    std::fs::remove_file(&path).unwrap();
    assert!(
        ThemeCatalog::read(directory.path())
            .resolve(&choice, ThemeMode::Dark)
            .warning
            .unwrap()
            .contains("ocean.json")
    );
    std::fs::write(&path, OCEAN).unwrap();
    let loaded = ThemeCatalog::read(directory.path()).resolve(&choice, ThemeMode::Light);
    assert!(loaded.warning.is_none());
    assert_eq!(loaded.theme.colors.background, 0x123456ff);
}

#[test]
fn theme_file_defines_the_complete_appearance_without_gpui() {
    let document = ThemeDocument::parse(OCEAN).unwrap();
    assert_eq!(document.name, "海风");
    assert_eq!(document.theme.colors.background, 0x123456ff);
    assert_eq!(document.theme.typography.family, "Noto Sans CJK SC");
    assert_eq!(document.theme.typography.body, 18.);
    assert_eq!(document.theme.spacing.medium, 22.);
    assert_eq!(document.theme.radius, 12.);
    assert_eq!(document.theme.shadow.blur, 6.);
}

#[test]
fn malformed_incomplete_and_unsafe_theme_values_have_actionable_errors() {
    let original: serde_json::Value = serde_json::from_str(OCEAN).unwrap();
    for (pointer, value, reason) in [
        ("/version", serde_json::json!(2), "version"),
        ("/name", serde_json::json!("  "), "name"),
        (
            "/theme/typography/body",
            serde_json::json!(0),
            "typography.body",
        ),
        (
            "/theme/typography/family",
            serde_json::json!(""),
            "typography.family",
        ),
        (
            "/theme/spacing/medium",
            serde_json::json!(-1),
            "spacing.medium",
        ),
        ("/theme/radius", serde_json::json!(1e20), "radius"),
        ("/theme/shadow/blur", serde_json::json!(-2), "shadow.blur"),
    ] {
        let mut value_to_parse = original.clone();
        *value_to_parse.pointer_mut(pointer).unwrap() = value;
        let error = ThemeDocument::parse(&value_to_parse.to_string()).unwrap_err();
        assert!(error.contains(reason), "{pointer}: {error}");
    }
    for section in ["colors", "typography", "spacing", "shadow"] {
        for key in original["theme"][section].as_object().unwrap().keys() {
            let mut incomplete = original.clone();
            incomplete["theme"][section]
                .as_object_mut()
                .unwrap()
                .remove(key);
            let error = ThemeDocument::parse(&incomplete.to_string()).unwrap_err();
            assert!(error.contains(key), "{error}");
        }
    }
    for text in [
        "{",
        "{}",
        &OCEAN.replace("#123456ff", "#oops"),
        &OCEAN.replace("#123456ff", "#+1234567"),
    ] {
        assert!(ThemeDocument::parse(text).is_err());
    }
}
