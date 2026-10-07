use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
}
impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1100.,
            height: 760.,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Panel {
    Settings,
    Commands,
    Themes,
    Rewind,
    #[serde(other)]
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeView {
    Chronological,
    Topological,
    #[serde(other)]
    Unknown,
}

/// 仅本设备的呈现偏好。快照、纪元和游标从不持久到此处。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ViewState {
    pub theme: crate::ThemeMode,
    /// None 只用于读取旧版本的 theme 明暗偏好。
    #[serde(default)]
    pub theme_selection: Option<crate::ThemeSelection>,
    pub components: BTreeMap<String, bool>,
    pub active_panel: Option<Panel>,
    pub window: WindowState,
    pub sidebar_width: f32,
    pub selected_session: Option<String>,
    pub scroll_anchors: BTreeMap<String, String>,
    pub tree_views: BTreeMap<String, TreeView>,
}
impl ViewState {
    pub fn theme_selection(&self) -> crate::ThemeSelection {
        self.theme_selection.clone().unwrap_or(match self.theme {
            crate::ThemeMode::Light => crate::ThemeSelection::Light,
            crate::ThemeMode::Dark => crate::ThemeSelection::Dark,
        })
    }
    pub fn validate(&self) -> io::Result<()> {
        if [self.window.width, self.window.height, self.sidebar_width]
            .iter()
            .any(|v| !v.is_finite() || !(1. ..=16384.).contains(v))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid view geometry",
            ));
        }
        Ok(())
    }
}
impl Default for ViewState {
    fn default() -> Self {
        Self {
            theme: crate::ThemeMode::default(),
            theme_selection: Some(crate::ThemeSelection::System),
            components: BTreeMap::new(),
            active_panel: None,
            window: WindowState::default(),
            sidebar_width: 240.,
            selected_session: None,
            scroll_anchors: BTreeMap::new(),
            tree_views: BTreeMap::new(),
        }
    }
}

/// 锁文件与原子替换分开，rename 后仍保持每设备唯一写入者。
pub struct ViewStateFile {
    path: PathBuf,
    _lock: File,
}
impl ViewStateFile {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_owned();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.with_extension("lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        Ok(Self { path, _lock: lock })
    }
    pub fn load(&self) -> io::Result<ViewState> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let state: ViewState = serde_json::from_slice(&bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                state.validate()?;
                Ok(state)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ViewState::default()),
            Err(e) => Err(e),
        }
    }
    pub fn save(&self, state: &ViewState) -> io::Result<()> {
        state.validate()?;
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, state)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(&self.path).map_err(|e| e.error)?;
        File::open(parent)?.sync_all()
    }
}
