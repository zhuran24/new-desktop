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

struct PermissionMapping {
    permission: Permission,
    claude: &'static str,
    approval: &'static str,
    sandbox: &'static str,
}
const PERMISSIONS: &[PermissionMapping] = &[
    PermissionMapping {
        permission: Permission::Prompt,
        claude: "default",
        approval: "untrusted",
        sandbox: "workspace-write",
    },
    PermissionMapping {
        permission: Permission::AcceptEdits,
        claude: "acceptEdits",
        approval: "on-request",
        sandbox: "workspace-write",
    },
    PermissionMapping {
        permission: Permission::ReadOnly,
        claude: "plan",
        approval: "on-request",
        sandbox: "read-only",
    },
    PermissionMapping {
        permission: Permission::Unrestricted,
        claude: "bypassPermissions",
        approval: "never",
        sandbox: "danger-full-access",
    },
];
const EFFORTS: &[(Effort, &str)] = &[
    (Effort::Low, "low"),
    (Effort::Medium, "medium"),
    (Effort::High, "high"),
    (Effort::ExtraHigh, "xhigh"),
    (Effort::Maximum, "max"),
];

pub fn decode_profile(native: &NativeProfile) -> Result<Profile, ConvertError> {
    if !native.settings.is_object() {
        return Err(ConvertError::Invalid(
            "profile settings must be an object".into(),
        ));
    }
    let s = &native.settings;
    let uses_defaults = match native.backend {
        BackendKind::Claude => s["permissionMode"].is_null(),
        BackendKind::Codex => s["approvalPolicy"].is_null() && s["sandbox"].is_null(),
    };
    let permission = if uses_defaults {
        Permission::Default
    } else {
        PERMISSIONS
            .iter()
            .find(|mapping| match native.backend {
                BackendKind::Claude => s["permissionMode"].as_str() == Some(mapping.claude),
                BackendKind::Codex => {
                    s["approvalPolicy"].as_str() == Some(mapping.approval)
                        && s["sandbox"].as_str() == Some(mapping.sandbox)
                }
            })
            .map(|m| m.permission.clone())
            .unwrap_or(Permission::Unmapped)
    };
    let effort = EFFORTS
        .iter()
        .find(|(_, name)| s["effort"].as_str() == Some(*name))
        .map(|(effort, _)| effort.clone());
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
        let mapped = PERMISSIONS
            .iter()
            .find(|m| m.permission == profile.permission)
            .map(|mapping| match target.backend {
                BackendKind::Claude => json!({"permissionMode":mapping.claude}),
                BackendKind::Codex => {
                    json!({"approvalPolicy":mapping.approval,"sandbox":mapping.sandbox})
                }
            });
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
    let effort = profile
        .effort
        .as_ref()
        .and_then(|effort| EFFORTS.iter().find(|(e, _)| e == effort))
        .map(|(_, name)| *name);
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
