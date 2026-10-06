use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
}

/// 颜色均为 RRGGBBAA；尺寸为逻辑像素。文件加载由主题组件提供。
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub mode: ThemeMode,
    pub colors: Colors,
    pub typography: Typography,
    pub spacing: Spacing,
    pub radius: f32,
    pub border_width: f32,
    pub shadow: Shadow,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Colors {
    pub background: u32,
    pub surface: u32,
    pub foreground: u32,
    pub muted: u32,
    pub border: u32,
    pub accent: u32,
    pub diff_added: u32,
    pub diff_removed: u32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Typography {
    pub family: String,
    pub mono_family: String,
    pub body: f32,
    pub title: f32,
    pub small: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Spacing {
    pub small: f32,
    pub medium: f32,
    pub large: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Shadow {
    pub inset: bool,
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
