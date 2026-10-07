use crate::{Theme, ThemeDocument, ThemeMode};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "file", rename_all = "snake_case")]
pub enum ThemeSelection {
    #[default]
    System,
    Light,
    Dark,
    File(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThemeEntry {
    /// 目录中的文件名，是选择的身份；显示名允许重复。
    pub file: String,
    pub name: String,
    pub theme: Result<Theme, String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeCatalog {
    pub entries: Vec<ThemeEntry>,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTheme {
    pub theme: Theme,
    pub warning: Option<String>,
}

impl ThemeCatalog {
    /// I/O 边界。调用方在后台执行，再向视图发布完整快照。
    pub fn read(directory: &Path) -> Self {
        let files = match std::fs::read_dir(directory) {
            Ok(files) => files,
            Err(error) => {
                return Self {
                    entries: vec![],
                    warning: Some(format!("主题目录 {}：{error}", directory.display())),
                };
            }
        };
        let mut catalog = Self::default();
        for file in files {
            let file = match file {
                Ok(file) => file,
                Err(error) => {
                    catalog.warning = Some(format!("主题目录 {}：{error}", directory.display()));
                    continue;
                }
            };
            let path = file.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(name) = file.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let document = read_document(&path);
            catalog.entries.push(ThemeEntry {
                file: name.clone(),
                name: document.as_ref().map(|d| d.name.clone()).unwrap_or(name),
                theme: document.map(|d| d.theme),
            });
        }
        catalog.entries.sort_by(|a, b| a.file.cmp(&b.file));
        catalog
    }

    pub fn resolve(&self, selection: &ThemeSelection, system: ThemeMode) -> ResolvedTheme {
        let mode = match selection {
            ThemeSelection::Light => ThemeMode::Light,
            ThemeSelection::Dark => ThemeMode::Dark,
            ThemeSelection::System | ThemeSelection::File(_) => system,
        };
        let mut result = ResolvedTheme {
            theme: Theme::builtin(mode),
            warning: self.warning.clone(),
        };
        if let ThemeSelection::File(name) = selection {
            match self
                .entries
                .iter()
                .find(|e| &e.file == name)
                .map(|e| &e.theme)
            {
                Some(Ok(theme)) => result.theme = theme.clone(),
                other => {
                    let error = match other {
                        Some(Err(error)) => error.as_str(),
                        _ => "文件不存在或不在主题目录中",
                    };
                    result.warning = Some(format!(
                        "主题 {name}：{error}；已回到系统明暗对应的默认主题"
                    ));
                }
            }
        }
        result
    }
}

fn read_document(path: &Path) -> Result<ThemeDocument, String> {
    // 不阻塞在目录、FIFO 或设备上；主题是普通 JSON 文件。
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.file_type().is_file() {
        return Err("须为普通文件，不支持目录或符号链接".into());
    }
    const LIMIT: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("主题文件不能超过 64 KiB".into());
    }
    ThemeDocument::parse(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)
}
