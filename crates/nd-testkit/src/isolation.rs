//! Rust 与 Python 隔离器共用的只读策略数据。
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path, sync::LazyLock};
#[derive(Deserialize)]
struct Policy {
    pinned_cli: String,
    environment: BTreeMap<String, String>,
    offline_claude: BTreeMap<String, String>,
}
static POLICY: LazyLock<Policy> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../python/isolation.json"))
        .expect("checked-in isolation policy")
});
pub fn pinned_cli() -> &'static Path {
    Path::new(&POLICY.pinned_cli)
}
pub fn environment() -> BTreeMap<String, String> {
    POLICY.environment.clone()
}
pub fn offline_claude() -> BTreeMap<String, String> {
    POLICY.offline_claude.clone()
}
#[derive(Clone, Copy)]
pub(crate) enum Isolation {
    PrivatePid,
    HostPid,
}
impl Isolation {
    pub fn flags(self) -> &'static [&'static str] {
        match self {
            Self::PrivatePid => &["--unshare-all"],
            Self::HostPid => &[
                "--unshare-user",
                "--unshare-ipc",
                "--unshare-net",
                "--unshare-uts",
            ],
        }
    }
}
