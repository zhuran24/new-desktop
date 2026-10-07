use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
}

/// 颜色均为 RRGGBBAA；尺寸为逻辑像素。文件加载由主题组件提供。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub mode: ThemeMode,
    pub colors: Colors,
    pub typography: Typography,
    pub spacing: Spacing,
    pub radius: f32,
    pub border_width: f32,
    pub shadow: Shadow,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colors {
    #[serde(with = "hex_color")]
    pub background: u32,
    #[serde(with = "hex_color")]
    pub surface: u32,
    #[serde(with = "hex_color")]
    pub foreground: u32,
    #[serde(with = "hex_color")]
    pub muted: u32,
    #[serde(with = "hex_color")]
    pub border: u32,
    #[serde(with = "hex_color")]
    pub accent: u32,
    #[serde(with = "hex_color")]
    pub diff_added: u32,
    #[serde(with = "hex_color")]
    pub diff_removed: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Typography {
    pub family: String,
    pub mono_family: String,
    pub body: f32,
    pub title: f32,
    pub small: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spacing {
    pub small: f32,
    pub medium: f32,
    pub large: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shadow {
    pub inset: bool,
    #[serde(with = "hex_color")]
    pub color: u32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub spread: f32,
}
impl Theme {
    pub fn builtin(mode: ThemeMode) -> Self {
        let colors = match mode {
            ThemeMode::Light => Colors {
                background: 0xf5f6f8ff,
                surface: 0xffffffff,
                foreground: 0x20242cff,
                muted: 0x555e6eff,
                border: 0xd5dbe3ff,
                accent: 0x235ac9ff,
                diff_added: 0x126936ff,
                diff_removed: 0xb42332ff,
            },
            ThemeMode::Dark => Colors {
                background: 0x171a20ff,
                surface: 0x222630ff,
                foreground: 0xeff2f7ff,
                muted: 0xadb6c6ff,
                border: 0x3a4250ff,
                accent: 0x9bbaffff,
                diff_added: 0x8bdda1ff,
                diff_removed: 0xffa0a8ff,
            },
        };
        Self {
            mode,
            colors,
            typography: Typography {
                family: "Noto Sans CJK SC".into(),
                mono_family: "monospace".into(),
                body: 14.,
                title: 20.,
                small: 12.,
            },
            spacing: Spacing {
                small: 8.,
                medium: 16.,
                large: 24.,
            },
            radius: 8.,
            border_width: 1.,
            shadow: Shadow {
                inset: false,
                color: 0x00000018,
                offset_x: 0.,
                offset_y: 2.,
                blur: 8.,
                spread: 0.,
            },
        }
    }
}

/// 独立于 GPUI Kit 的版本化文件契约，所有 token 必填。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeDocument {
    pub version: u32,
    pub name: String,
    pub theme: Theme,
}
impl ThemeDocument {
    pub fn parse(text: &str) -> Result<Self, String> {
        let document: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if document.version != 1 {
            return Err("version：只支持主题格式 1".into());
        }
        for (key, value) in [
            ("name", &document.name),
            ("typography.family", &document.theme.typography.family),
            (
                "typography.mono_family",
                &document.theme.typography.mono_family,
            ),
        ] {
            if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(format!("{key}：须为 1–256 字节的非空文本，不能含控制字符"));
            }
        }
        let t = &document.theme;
        for (key, value, min, max) in [
            ("typography.body", t.typography.body, 8., 72.),
            ("typography.title", t.typography.title, 8., 96.),
            ("typography.small", t.typography.small, 8., 72.),
            ("spacing.small", t.spacing.small, 0., 128.),
            ("spacing.medium", t.spacing.medium, 0., 128.),
            ("spacing.large", t.spacing.large, 0., 128.),
            ("radius", t.radius, 0., 64.),
            ("border_width", t.border_width, 0., 8.),
            ("shadow.offset_x", t.shadow.offset_x, -64., 64.),
            ("shadow.offset_y", t.shadow.offset_y, -64., 64.),
            ("shadow.blur", t.shadow.blur, 0., 128.),
            ("shadow.spread", t.shadow.spread, -64., 64.),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(format!("{key}：须为 {min}–{max} 之间的有限数值"));
            }
        }
        Ok(document)
    }
}
mod hex_color {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(color: &u32, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("#{color:08x}"))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
        let text = String::deserialize(deserializer)?;
        let digits = text
            .strip_prefix('#')
            .filter(|s| s.len() == 8 && s.is_ascii());
        digits
            .and_then(|s| u32::from_str_radix(s, 16).ok())
            .ok_or_else(|| D::Error::custom("颜色必须为 #RRGGBBAA"))
    }
}
