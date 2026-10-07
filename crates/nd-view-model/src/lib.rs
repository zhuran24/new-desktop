//! 不依赖 GPUI 的每设备视图状态、主题变量和显示数据。
mod state;
pub use state::{Panel, TreeView, ViewState, ViewStateFile, WindowState};
mod slots;
pub use slots::{Contribution, Slot, SlotGuard, Slots};
mod chat;
mod history;
pub use history::*;
mod theme;
mod theme_files;
pub use chat::*;
pub use theme::{Colors, Shadow, Spacing, Theme, ThemeDocument, ThemeMode, Typography};
pub use theme_files::{ResolvedTheme, ThemeCatalog, ThemeEntry, ThemeSelection};

#[derive(Clone, Debug, PartialEq)]
pub struct ItemView {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub text: String,
}

mod diff;
pub use diff::*;

mod settings;
pub use settings::*;

mod model_picker;
pub use model_picker::ModelPicker;

mod image_cache;
pub use image_cache::ImageCache;
