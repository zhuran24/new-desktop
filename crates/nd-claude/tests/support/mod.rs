//! 场景夹具：真守护进程场景（独立 slice、bwrap 断网、临时 HOME/CLAUDE_CONFIG_DIR/XDG）里，
//! 真看守进程拉起钉住的 CLI 与仓库里的两个 mod；只有模型端点换成离线伪端点。
#![allow(dead_code)]
use nd_claude::{Claude, ClaudeConfig, Open, Start};
use nd_testkit::{Scenario, ScenarioOptions};
use std::{path::Path, sync::Arc, time::Duration};

pub const MODEL: &str = "claude-haiku-4-5";

pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "types" {
                copy_dir(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

pub fn repo_mods() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods")
}

pub struct Fixture {
    pub scenario: Scenario,
}
impl Fixture {
    pub async fn start(name: &str) -> Self {
        let mut options = ScenarioOptions::new(name, std::env::var_os("ND_TEST_DAEMON").unwrap());
        options.watchdog = Some(std::env::var_os("ND_TEST_WATCHDOG").unwrap().into());
        let scenario = Scenario::start(options).await.unwrap();
        for module in ["new-desktop", "new-desktop-actions"] {
            copy_dir(
                &repo_mods().join(module),
                &scenario.root().join("mods").join(module),
            );
        }
        Self { scenario }
    }
    /// 后端进程在看守的 bwrap 里：场景根目录在宿主路径和 /sandbox 两处可见。
    pub fn config(&self) -> ClaudeConfig {
        let root = self.scenario.root();
        let mut config = ClaudeConfig::new(
            "/cli",
            root.join("mods/new-desktop"),
            root.join("mods/new-desktop-actions"),
            root.join("runtime/mod.sock"),
        );
        config.env = [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/sandbox/home"),
            ("CLAUDE_CONFIG_DIR", "/sandbox/claude"),
            ("XDG_CONFIG_HOME", "/sandbox/config"),
            ("XDG_DATA_HOME", "/sandbox/data"),
            ("XDG_STATE_HOME", "/sandbox/state"),
            ("XDG_CACHE_HOME", "/sandbox/cache"),
            ("XDG_RUNTIME_DIR", "/sandbox/runtime"),
            ("LANG", "C.UTF-8"),
            ("TERM", "dumb"),
            // 只换模型端点：环回地址由沙盒里的代理接到伪端点，假 key 不是任何真凭据。
            ("ANTHROPIC_BASE_URL", "http://127.0.0.1:8765"),
            ("ANTHROPIC_API_KEY", "offline-fixture"),
            ("DISABLE_TELEMETRY", "1"),
            ("DISABLE_ERROR_REPORTING", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        config.limits.overflow = root.join("cache/spool-overflow");
        config.hello_timeout = Duration::from_secs(10);
        config.poll_timeout = Duration::from_secs(5);
        config.record = true;
        config
    }
    pub fn claude(&self, config: ClaudeConfig) -> Claude {
        Claude::new(config, Arc::new(self.scenario.watchdogs().unwrap())).unwrap()
    }
    pub fn fresh(&self, session: &str) -> Open {
        Open {
            start: Start::Fresh {
                session: session.to_owned(),
            },
            cwd: "/sandbox/project".into(),
            model: Some(MODEL.into()),
            permission_mode: None,
        }
    }
    pub fn close(self) {
        self.scenario.close().unwrap();
    }
}

pub fn session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
