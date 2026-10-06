//! 仅 scenarios 构建的单次故障点，不进入生产二进制。
use std::path::Path;

pub struct Fault(serde_json::Value);
impl Fault {
    pub fn take(root: &Path, id: &str) -> Self {
        let path = root.join("command-fault.json");
        let value = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        if let Some(value) = value.filter(|v| v["id"] == id) {
            std::fs::remove_file(path).expect("consume scenario fault");
            Self(value)
        } else {
            Self(serde_json::Value::Null)
        }
    }
    pub fn crash(&self, point: &str) {
        if self.0["point"] == point && self.0["action"] == "crash" {
            // 无析构、无清理；真实进程退出让 SQLite 自行恢复未提交事务。
            std::process::abort();
        }
    }
    pub fn unavailable(&self, point: &str) -> nd_store::Result<()> {
        if self.0["point"] == point && self.0["action"] == "unavailable" {
            return Err(nd_store::Error::Busy);
        }
        Ok(())
    }
}
