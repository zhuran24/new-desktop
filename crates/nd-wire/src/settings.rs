//! Backend-neutral effective settings and selectable values.
use crate::Model;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SettingCaps {
    pub model: bool,
    pub effort: bool,
    pub permission_mode: bool,
    pub ultracode: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EffectiveSettings {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub ultracode: Option<bool>,
    pub ultracode_requested: Option<bool>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LiveSettings {
    pub applied: EffectiveSettings,
    pub caps: SettingCaps,
    pub models: Vec<Model>,
    pub permission_mode: Option<String>,
    pub permission_modes: Vec<String>,
    pub permission_labels: BTreeMap<String, String>,
    pub error: Option<String>,
}
