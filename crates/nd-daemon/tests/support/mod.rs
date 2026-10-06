#![allow(dead_code)] // 共用夹具，各场景文件只使用自己需要的入口。
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub struct Daemon {
    pub dir: tempfile::TempDir,
    pub socket: PathBuf,
    unit: String,
    slice: String,
}

fn checked(args: &[&str]) {
    let result = Command::new(args[0]).args(&args[1..]).output().unwrap();
    assert!(
        result.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

impl Daemon {
    pub async fn start() -> Self {
        Self::launch(false, "").await
    }
    pub async fn start_default_paths() -> Self {
        Self::launch(true, "").await
    }
    pub async fn configured(config: &str) -> Self {
        Self::launch(false, config).await
    }
    async fn launch(default_paths: bool, config: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "home", "claude", "config", "data", "state", "cache", "runtime",
        ] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        if !config.is_empty() {
            std::fs::write(dir.path().join("config.toml"), config).unwrap();
        }
        let id = uuid::Uuid::new_v4().simple();
        let unit = format!("nd-test-ticket4-{id}.service");
        let slice = format!("nd-test-ticket4{id}.slice");
        let socket = dir.path().join(if default_paths {
            "runtime/new-desktop/nd.sock"
        } else {
            "runtime/nd.sock"
        });
        let this = Self {
            dir,
            socket,
            unit,
            slice,
        };
        checked(&[
            "systemctl",
            "--user",
            "set-property",
            "--runtime",
            &this.slice,
            "MemoryMax=512M",
            "MemorySwapMax=0",
        ]);
        let mut launch = vec![
            "systemd-run",
            "--user",
            "--quiet",
            "--unit",
            &this.unit,
            "--slice",
            &this.slice,
            "--property",
            "Restart=on-failure",
            "--property",
            "LimitCORE=0",
            "--property",
            "RestartSec=100ms",
            "--property",
            "MemoryMax=512M",
            "--property",
            "MemorySwapMax=0",
            "/usr/bin/bwrap",
            "--unshare-net",
            "--die-with-parent",
            "--new-session",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib",
            "/lib64",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--bind",
            this.dir.path().to_str().unwrap(),
            "/sandbox",
            "--ro-bind",
            env!("CARGO_BIN_EXE_nd-daemon"),
            "/nd-daemon",
            "--clearenv",
            "--setenv",
            "HOME",
            "/sandbox/home",
            "--setenv",
            "CLAUDE_CONFIG_DIR",
            "/sandbox/claude",
            "--setenv",
            "XDG_CONFIG_HOME",
            "/sandbox/config",
            "--setenv",
            "XDG_DATA_HOME",
            "/sandbox/data",
            "--setenv",
            "XDG_STATE_HOME",
            "/sandbox/state",
            "--setenv",
            "XDG_CACHE_HOME",
            "/sandbox/cache",
            "--setenv",
            "XDG_RUNTIME_DIR",
            "/sandbox/runtime",
            "/nd-daemon",
        ];
        if !default_paths {
            launch.extend(["--root", "/sandbox"]);
        }
        checked(&launch);
        this.wait_ready().await;
        this
    }

    pub async fn wait_ready(&self) {
        for _ in 0..100 {
            if tokio::net::UnixStream::connect(&self.socket).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let journal = Command::new("journalctl")
            .args(["--user", "-u", &self.unit, "-n", "20", "--no-pager"])
            .output()
            .unwrap();
        panic!(
            "daemon never listened: {}",
            String::from_utf8_lossy(&journal.stdout)
        );
    }
    pub fn restart(&self) {
        checked(&["systemctl", "--user", "restart", &self.unit]);
    }
    pub fn kill(&self) {
        checked(&["systemctl", "--user", "kill", "--signal=KILL", &self.unit]);
    }
    pub fn assert_limits(&self) {
        let cgroup = Command::new("systemctl")
            .args([
                "--user",
                "show",
                "--property=ControlGroup",
                "--value",
                &self.slice,
            ])
            .output()
            .unwrap();
        assert!(cgroup.status.success());
        let path = PathBuf::from("/sys/fs/cgroup").join(
            String::from_utf8_lossy(&cgroup.stdout)
                .trim()
                .trim_start_matches('/'),
        );
        assert_eq!(
            std::fs::read_to_string(path.join("memory.max"))
                .unwrap()
                .trim(),
            "536870912"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("memory.swap.max"))
                .unwrap()
                .trim(),
            "0"
        );
    }
    pub fn units(&self) -> [String; 2] {
        [self.unit.clone(), self.slice.clone()]
    }
    pub fn root(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &self.unit, &self.slice])
            .output();
        let _ = Command::new("systemctl")
            .args(["--user", "reset-failed", &self.unit])
            .output();
        let _ = Command::new("systemctl")
            .args(["--user", "revert", &self.slice])
            .output();
    }
}
