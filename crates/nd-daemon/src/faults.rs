//! 仅 scenarios 构建的提交点崩溃；故障文件只在实际崩溃时消费。
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Point {
    AfterEffect,
    BeforeCommit,
    AfterCommit,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Crash,
}
#[derive(Deserialize)]
struct Spec {
    point: Point,
    action: Action,
}
pub struct Fault {
    spec: Option<Spec>,
    path: PathBuf,
}
impl Fault {
    pub fn take(root: &Path, id: &str) -> Self {
        let path = root.join("command-fault.json");
        let value = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        let spec = value
            .filter(|v| v["id"] == id)
            .map(|value| serde_json::from_value(value).expect("invalid scenario crash point"));
        Self { spec, path }
    }
    pub fn crash(&self, point: Point) {
        if self
            .spec
            .as_ref()
            .is_some_and(|spec| spec.point == point && matches!(spec.action, Action::Crash))
        {
            std::fs::remove_file(&self.path).expect("consume reached crash point");
            std::process::abort();
        }
    }
}
