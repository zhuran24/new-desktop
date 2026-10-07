//! 不依赖 GPUI 的每设备视图状态、主题变量和显示数据。
mod state;
pub use state::{ViewState, ViewStateFile, WindowState};
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
    pub selected: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub items: Vec<ItemView>,
}
/// 未识别条目始终保留后备文字；渲染器可按 kind 覆盖呈现。
pub fn project(snapshot: &nd_wire::Snapshot, state: &ViewState) -> View {
    View {
        items: snapshot
            .items
            .iter()
            .map(|item| ItemView {
                id: item.id.clone(),
                kind: item.kind.clone(),
                title: item.fallback.title.clone(),
                text: item.fallback.text.clone(),
                selected: state.selected_session.as_ref() == Some(&item.id),
            })
            .collect(),
    }
}

mod diff;
pub use diff::*;

mod settings;
pub use settings::*;
