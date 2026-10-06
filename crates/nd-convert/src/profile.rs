use crate::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeProfile {
    pub backend: BackendKind,
    pub model: String,
    pub cwd: String,
    /// 适配器提供的设置快照；不是可直接发送的协议帧。
    pub settings: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    Default,
    Prompt,
    AcceptEdits,
    ReadOnly,
    Unrestricted,
    Unmapped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effort {
    Low,
    Medium,
    High,
    ExtraHigh,
    Maximum,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub cwd: String,
    pub permission: Permission,
    pub effort: Option<Effort>,
    /// 源端启动设置等原样保留，跨后端不自动带过去。
    pub native: NativeProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileTarget {
    pub backend: BackendKind,
    /// 由用户选择，绝不把源端模型名猜成目标模型。
    pub model: String,
    pub defaults: Value,
    /// 适配器从该版本、该模型的能力表取得；空集不表示全支持。
    pub supported_efforts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappedProfile {
    pub profile: NativeProfile,
    pub loss: LossReport,
}

pub fn decode_profile(native: &NativeProfile) -> Result<Profile, ConvertError> {
    if !native.settings.is_object() {
        return Err(ConvertError::Invalid(
            "profile settings must be an object".into(),
        ));
    }
    let s = &native.settings;
    let permission = match native.backend {
        BackendKind::Claude => match s["permissionMode"].as_str() {
            None if s["permissionMode"].is_null() => Permission::Default,
            Some("default") => Permission::Prompt,
            Some("acceptEdits") => Permission::AcceptEdits,
            Some("plan") => Permission::ReadOnly,
            Some("bypassPermissions") => Permission::Unrestricted,
            _ => Permission::Unmapped,
        },
        BackendKind::Codex => match (s["approvalPolicy"].as_str(), s["sandbox"].as_str()) {
            (None, None) if s["approvalPolicy"].is_null() && s["sandbox"].is_null() => {
                Permission::Default
            }
            (Some("untrusted"), Some("workspace-write")) => Permission::Prompt,
            (Some("on-request"), Some("workspace-write")) => Permission::AcceptEdits,
            (Some("on-request"), Some("read-only")) => Permission::ReadOnly,
            (Some("never"), Some("danger-full-access")) => Permission::Unrestricted,
            _ => Permission::Unmapped,
        },
    };
    let effort = match s["effort"].as_str() {
        Some("low") => Some(Effort::Low),
        Some("medium") => Some(Effort::Medium),
        Some("high") => Some(Effort::High),
        Some("xhigh") => Some(Effort::ExtraHigh),
        Some("max") => Some(Effort::Maximum),
        _ => None,
    };
    Ok(Profile {
        cwd: native.cwd.clone(),
        permission,
        effort,
        native: native.clone(),
    })
}

pub fn encode_profile(
    profile: &Profile,
    target: &ProfileTarget,
) -> Result<MappedProfile, ConvertError> {
    if target.model.is_empty() || !target.defaults.is_object() {
        return Err(ConvertError::Invalid(
            "target model and default settings are required".into(),
        ));
    }
    let same = profile.native.backend == target.backend;
    let mut settings = if same {
        profile.native.settings.clone()
    } else {
        target.defaults.clone()
    };
    let mut loss = LossReport::default();
    let original = decode_profile(&profile.native)?;
    if !same {
        for key in profile.native.settings.as_object().unwrap().keys() {
            if !matches!(
                key.as_str(),
                "effort" | "permissionMode" | "approvalPolicy" | "sandbox"
            ) {
                loss.entries.push(Loss {
                    position: format!("profile/{key}"),
                    reason: "backend_setting_defaulted".into(),
                });
            }
        }
    }
    if !same || profile.permission != original.permission {
        let mapped = match target.backend {
            BackendKind::Claude => match profile.permission {
                Permission::Prompt => Some(json!({"permissionMode":"default"})),
                Permission::AcceptEdits => Some(json!({"permissionMode":"acceptEdits"})),
                Permission::ReadOnly => Some(json!({"permissionMode":"plan"})),
                Permission::Unrestricted => Some(json!({"permissionMode":"bypassPermissions"})),
                _ => None,
            },
            BackendKind::Codex => match profile.permission {
                Permission::Prompt => {
                    Some(json!({"approvalPolicy":"untrusted","sandbox":"workspace-write"}))
                }
                Permission::AcceptEdits => {
                    Some(json!({"approvalPolicy":"on-request","sandbox":"workspace-write"}))
                }
                Permission::ReadOnly => {
                    Some(json!({"approvalPolicy":"on-request","sandbox":"read-only"}))
                }
                Permission::Unrestricted => {
                    Some(json!({"approvalPolicy":"never","sandbox":"danger-full-access"}))
                }
                _ => None,
            },
        };
        if let Some(mapped) = mapped {
            settings
                .as_object_mut()
                .unwrap()
                .extend(mapped.as_object().unwrap().clone());
            if !same {
                loss.entries.push(Loss {
                    position: "profile/permission".into(),
                    reason: "permission_semantics_mapped".into(),
                });
            }
        } else {
            let keys: &[&str] = match target.backend {
                BackendKind::Claude => &["permissionMode"],
                BackendKind::Codex => &["approvalPolicy", "sandbox"],
            };
            for key in keys {
                settings.as_object_mut().unwrap().remove(*key);
                if let Some(value) = target.defaults.get(*key) {
                    settings[*key] = value.clone();
                }
            }
            if profile.permission == Permission::Unmapped {
                loss.entries.push(Loss {
                    position: "profile/permission".into(),
                    reason: "unsupported_permission_defaulted".into(),
                });
            }
        }
    }
    let effort = profile.effort.as_ref().map(|e| match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::ExtraHigh => "xhigh",
        Effort::Maximum => "max",
    });
    if let Some(e) = effort.filter(|e| target.supported_efforts.iter().any(|s| s == e)) {
        settings["effort"] = json!(e);
    } else if profile.effort.is_some() || !profile.native.settings["effort"].is_null() {
        let object = settings.as_object_mut().unwrap();
        object.remove("effort");
        if let Some(default) = target.defaults.get("effort") {
            object.insert("effort".into(), default.clone());
        }
        loss.entries.push(Loss {
            position: "profile/effort".into(),
            reason: "unsupported_effort_defaulted".into(),
        });
    }
    Ok(MappedProfile {
        profile: NativeProfile {
            backend: target.backend,
            model: target.model.clone(),
            cwd: profile.cwd.clone(),
            settings,
        },
        loss,
    })
}
