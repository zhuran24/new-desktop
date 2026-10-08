use nd_wire::Snapshot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingChoice {
    pub value: String,
    pub label: String,
    pub selected: bool,
    pub disabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsView {
    pub title: String,
    pub models: Vec<SettingChoice>,
    pub efforts: Vec<String>,
    pub effort: String,
    pub permission_modes: Vec<String>,
    pub permission_labels: std::collections::BTreeMap<String, String>,
    pub permission_mode: String,
    /// None 表示完全不显示；false 是可用但未生效。
    pub ultracode: Option<bool>,
    pub ultracode_requested: bool,
    pub pending: Option<String>,
}

/// 能力来自后端端口的会话头；界面不按后端名猜测能力，也不缓存另一份设置。
pub fn session_settings(snapshot: &Snapshot) -> SettingsView {
    let Some(header) = snapshot.items.iter().find(|i| i.kind == "header") else {
        return SettingsView::default();
    };
    let h = &header.data;
    let settings: nd_wire::LiveSettings =
        serde_json::from_value(h["settings"].clone()).unwrap_or_default();
    let caps: nd_wire::SettingCaps = serde_json::from_value(h["caps"].clone()).unwrap_or_default();
    let chosen_model = h["model"].as_str();
    let current = settings
        .models
        .iter()
        .find(|m| Some(m.value.as_str()) == chosen_model)
        .or_else(|| {
            settings.models.iter().find(|m| {
                m.resolved_model
                    .as_ref()
                    .is_some_and(|r| Some(r) == settings.applied.model.as_ref())
            })
        });
    SettingsView {
        title: h["title"].as_str().unwrap_or_default().into(),
        models: if caps.model {
            settings
                .models
                .iter()
                .map(|m| SettingChoice {
                    value: m.value.clone(),
                    label: m.label.clone(),
                    selected: Some(m.value.as_str()) == chosen_model,
                    disabled: m.disabled,
                })
                .collect()
        } else {
            vec![]
        },
        efforts: if caps.effort {
            current.map(|m| m.effort_levels.clone()).unwrap_or_default()
        } else {
            vec![]
        },
        effort: settings.applied.effort.unwrap_or_default(),
        permission_modes: if caps.permission_mode {
            settings.permission_modes
        } else {
            vec![]
        },
        permission_labels: settings.permission_labels,
        permission_mode: h["permission_mode"]
            .as_str()
            .map(str::to_owned)
            .or(settings.permission_mode)
            .unwrap_or_else(|| "default".into()),
        ultracode: caps
            .ultracode
            .then_some(settings.applied.ultracode.unwrap_or(false)),
        ultracode_requested: settings.applied.ultracode_requested.unwrap_or(false),
        pending: h["pending_setting"].as_str().map(str::to_owned),
    }
}
