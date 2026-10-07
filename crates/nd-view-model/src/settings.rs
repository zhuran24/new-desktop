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
    let s = &h["settings"];
    let applied = &s["applied"];
    let catalog = s["models"].as_array();
    let current = catalog.and_then(|models| {
        models
            .iter()
            .find(|m| m["value"] == h["model"])
            .or_else(|| {
                models
                    .iter()
                    .find(|m| m["resolvedModel"] == applied["model"])
            })
    });
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect()
    };
    SettingsView {
        title: h["title"].as_str().unwrap_or_default().into(),
        models: if h["caps"]["model"] == true {
            catalog
                .into_iter()
                .flatten()
                .map(|m| SettingChoice {
                    value: m["value"].as_str().unwrap_or_default().into(),
                    label: m["displayName"]
                        .as_str()
                        .or_else(|| m["label"].as_str())
                        .unwrap_or_default()
                        .into(),
                    selected: m["value"] == h["model"],
                    disabled: m["disabled"] == true,
                })
                .collect()
        } else {
            vec![]
        },
        efforts: if h["caps"]["effort"] == true {
            current
                .map(|m| strings(&m["supportedEffortLevels"]))
                .unwrap_or_default()
        } else {
            vec![]
        },
        effort: applied["effort"].as_str().unwrap_or_default().into(),
        permission_modes: if h["caps"]["permission_mode"] == true {
            strings(&s["permission_modes"])
        } else {
            vec![]
        },
        permission_mode: h["permission_mode"]
            .as_str()
            .or_else(|| s["permission_mode"].as_str())
            .unwrap_or("default")
            .into(),
        ultracode: (h["caps"]["ultracode"] == true).then(|| applied["ultracode"] == true),
        ultracode_requested: applied["ultracodeRequested"] == true,
        pending: h["pending_setting"].as_str().map(str::to_owned),
    }
}
