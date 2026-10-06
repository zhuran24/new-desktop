//! Idempotent systemd launch and identity-checked recovery. Dropping a link never stops a run.
use nd_watchdog_proto::*;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Command, time::Duration};
use tokio::net::UnixStream;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub root: PathBuf,
    pub watchdog: PathBuf,
    pub unit_prefix: String,
    pub slice: String,
    #[serde(default = "default_memory_max")]
    pub memory_max: u64,
    #[serde(default)]
    pub launcher: Vec<String>,
}
fn default_memory_max() -> u64 {
    2 * 1024 * 1024 * 1024
}
pub struct Watchdogs {
    config: Config,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launched {
    pub identity: Identity,
    pub unit: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Found {
    pub run: String,
    pub identity: Option<Identity>,
    pub state: String,
    pub high: u64,
    pub exit: Option<i32>,
    pub tail: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
}
fn valid(s: &str) -> bool {
    !s.is_empty() && s.len() < 100 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn command(program: &str) -> Command {
    let mut c = Command::new(program);
    c.env_clear().env("PATH", "/usr/bin:/bin").env(
        "XDG_RUNTIME_DIR",
        format!("/run/user/{}", rustix::process::geteuid().as_raw()),
    );
    c.env(
        "DBUS_SESSION_BUS_ADDRESS",
        format!(
            "unix:path=/run/user/{}/bus",
            rustix::process::geteuid().as_raw()
        ),
    );
    c
}
impl Watchdogs {
    pub fn new(config: Config) -> Result<Self> {
        if !valid(&config.unit_prefix)
            || !config.slice.ends_with(".slice")
            || config.memory_max == 0
        {
            return Err("invalid unit configuration".into());
        }
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        if !config.root.is_absolute()
            || !config.watchdog.is_absolute()
            || !valid(config.slice.trim_end_matches(".slice"))
        {
            return Err("paths must be absolute and slice must be a unit name".into());
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.root)?;
        let meta = std::fs::symlink_metadata(&config.root)?;
        if !meta.is_dir() || meta.uid() != rustix::process::geteuid().as_raw() {
            return Err("run root must be owned by current uid and not a symlink".into());
        }
        std::fs::set_permissions(&config.root, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self { config })
    }
    pub fn directory(&self, run: &str) -> Result<PathBuf> {
        if !valid(run) {
            return Err("invalid run id".into());
        }
        Ok(self.config.root.join(run))
    }
    pub fn unit(&self, run: &str) -> Result<String> {
        self.directory(run)?;
        Ok(format!("{}-{run}.service", self.config.unit_prefix))
    }
    pub async fn launch(&self, run: &str, spec: LaunchSpec) -> Result<Launched> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let directory = self.directory(run)?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(directory.join("launch.lock"))?;
        let _lock = tokio::task::spawn_blocking(move || {
            fs2::FileExt::lock_exclusive(&lock)?;
            Ok::<_, std::io::Error>(lock)
        })
        .await??;
        let path = directory.join("spec.json");
        if path.exists() {
            let prior: WatchSpec = serde_json::from_slice(&std::fs::read(&path)?)?;
            if prior.launch != spec {
                return Err("run spec conflict".into());
            }
        } else {
            private_json(
                &path,
                &WatchSpec {
                    run: run.into(),
                    directory: directory.clone(),
                    launch: spec,
                },
            )?;
        }
        // Spec publication can precede a daemon crash. Retry only before any watchdog started.
        if !directory.join("started").exists() && self.unit_group(run)?.is_none() {
            let mut cmd = command("systemd-run");
            cmd.args([
                "--user",
                "--quiet",
                "--collect",
                "--unit",
                &self.unit(run)?,
                "--slice",
                &self.config.slice,
                "-p",
                "Restart=no",
                "-p",
                "KillMode=control-group",
                "-p",
                "TimeoutStopSec=2s",
                "-p",
                &format!("MemoryMax={}", self.config.memory_max),
                "-p",
                "MemorySwapMax=0",
                "-p",
                "LimitCORE=0",
                "--",
            ]);
            cmd.args(&self.config.launcher)
                .arg(&self.config.watchdog)
                .arg("--spec")
                .arg(&path);
            let out = cmd.output()?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).to_string().into());
            }
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(found) = self
                    .inspect()?
                    .into_iter()
                    .find(|f| f.run == run && f.state == "Up")
                {
                    return Ok(Launched {
                        identity: found.identity.ok_or("missing live identity")?,
                        unit: self.unit(run)?,
                    });
                }
                if directory.join("failed.json").exists() {
                    return Err(format!(
                        "Gone(NeverLaunched): {}",
                        std::fs::read_to_string(directory.join("failed.json"))?
                    )
                    .into());
                }
                if directory.join("hello.json").exists() && self.unit_group(run)?.is_none() {
                    return Err("run already ended; use recover".into());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?
    }
    pub async fn link(&self, run: &str) -> Result<WatchLink> {
        let directory = self.directory(run)?;
        let expected: Hello =
            serde_json::from_slice(&std::fs::read(directory.join("hello.json"))?)?;
        let group = self
            .unit_group(run)?
            .ok_or("IdentityMismatch: unit absent")?;
        if !in_group(&expected.identity, &group) || !in_group(&expected.watchdog, &group) {
            return Err("IdentityMismatch: cgroup".into());
        }
        let mut socket = UnixStream::connect(directory.join("watchdog.sock")).await?;
        if socket.peer_cred()?.uid() != rustix::process::geteuid().as_raw()
            || socket.peer_cred()?.pid() != Some(expected.watchdog.pid as i32)
        {
            return Err("watchdog uid mismatch".into());
        }
        let response: Response =
            tokio::time::timeout(Duration::from_secs(3), recv(&mut socket)).await??;
        let Response::Hello { hello } = response else {
            return Err("missing watchdog hello".into());
        };
        if hello.version != VERSION
            || hello.run != run
            || hello.identity != expected.identity
            || hello.watchdog != expected.watchdog
            || !hello.identity.alive()
            || !hello.watchdog.alive()
        {
            return Err("IdentityMismatch".into());
        }
        Ok(WatchLink {
            socket: Some(socket),
            hello,
        })
    }
    /// Startup claims each still-live controller once, reads hello, and never acknowledges output.
    pub async fn recover(&self) -> Result<Vec<Found>> {
        let mut found = self.inspect()?;
        for run in &mut found {
            if run.state == "Up" {
                match self.link(&run.run).await {
                    Ok(link) => run.high = link.hello.high,
                    Err(_) => run.state = "IdentityMismatch".into(),
                }
            }
        }
        Ok(found)
    }
    fn unit_group(&self, run: &str) -> Result<Option<String>> {
        let out = command("systemctl")
            .args([
                "--user",
                "show",
                &self.unit(run)?,
                "-p",
                "ControlGroup",
                "--value",
            ])
            .output()?;
        if !out.status.success() {
            return Err(format!(
                "cannot inspect systemd unit: {}",
                String::from_utf8_lossy(&out.stderr)
            )
            .into());
        }
        let group = String::from_utf8(out.stdout)?.trim().to_owned();
        Ok((!group.is_empty()).then_some(group))
    }
    /// Read-only diagnostics never acquire the adapter's control connection.
    pub fn inspect(&self) -> Result<Vec<Found>> {
        let mut found = vec![];
        for entry in std::fs::read_dir(&self.config.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let run = entry.file_name().to_string_lossy().into_owned();
            let saved = std::fs::read(entry.path().join("hello.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<Hello>(&b).ok());
            let group = self.unit_group(&run);
            let state = match (&saved, &group) {
                (Some(h), Ok(Some(group)))
                    if h.identity.alive()
                        && h.watchdog.alive()
                        && in_group(&h.identity, group)
                        && in_group(&h.watchdog, group) =>
                {
                    "Up"
                }
                (Some(h), Ok(None))
                    if h.identity.matching() == Some(false)
                        && h.watchdog.matching() == Some(false) =>
                {
                    "Gone"
                }
                (None, Ok(None))
                    if !entry.path().join("started").exists()
                        || entry.path().join("failed.json").exists() =>
                {
                    "Gone"
                }
                _ => "IdentityMismatch",
            };
            let reason = if state == "Gone" {
                Some(
                    match &saved {
                        None => "NeverLaunched",
                        Some(h) if h.exit.is_some() => "Exited",
                        Some(_) => "ProcGone",
                    }
                    .into(),
                )
            } else {
                None
            };
            found.push(Found {
                reason,
                detail: group.as_ref().err().map(ToString::to_string),
                tail: tail_state(&entry.path()),
                run,
                identity: saved.as_ref().map(|h| h.identity.clone()),
                state: state.into(),
                high: saved.as_ref().map_or(0, |h| h.high),
                exit: saved.and_then(|h| h.exit),
            });
        }
        found.sort_by(|a, b| a.run.cmp(&b.run));
        Ok(found)
    }
    pub fn records(&self, run: &str, after: u64, limit: usize) -> Result<Vec<Record>> {
        read_records(&self.directory(run)?, after, limit)
    }
}
pub struct WatchLink {
    socket: Option<UnixStream>,
    pub hello: Hello,
}
impl WatchLink {
    async fn request(&mut self, request: Request) -> Result<Response> {
        let mut socket = self
            .socket
            .take()
            .ok_or("link disconnected; reconnect before retry")?;
        let response = tokio::time::timeout(Duration::from_secs(5), async {
            send(&mut socket, &request).await?;
            recv(&mut socket).await
        })
        .await??;
        self.socket = Some(socket);
        Ok(response)
    }
    pub async fn write(&mut self, in_seq: u64, line: &str) -> Result<()> {
        match self
            .request(Request::Write {
                in_seq,
                line: line.into(),
            })
            .await?
        {
            Response::Written { in_seq: n } if n == in_seq => Ok(()),
            other => Err(format!("write: {other:?}").into()),
        }
    }
    pub async fn read(&mut self, after: u64, limit: usize) -> Result<Vec<Record>> {
        match self.request(Request::Attach { after, limit }).await? {
            Response::Records { records } => Ok(records),
            other => Err(format!("attach: {other:?}").into()),
        }
    }
    pub async fn ack(&mut self, seq: u64) -> Result<()> {
        match self.request(Request::Ack { seq }).await? {
            Response::Acked { .. } => Ok(()),
            r => Err(format!("ack: {r:?}").into()),
        }
    }
    pub async fn stats(&mut self) -> Result<Stats> {
        match self.request(Request::Stats).await? {
            Response::Stats { stats } => Ok(stats),
            r => Err(format!("stats: {r:?}").into()),
        }
    }
    pub async fn finish(&mut self, action: Finish) -> Result<()> {
        match self.request(Request::Finish { action }).await? {
            Response::Finished => Ok(()),
            r => Err(format!("finish: {r:?}").into()),
        }
    }
    pub async fn release(mut self) -> Result<()> {
        send(
            self.socket.as_mut().ok_or("link disconnected")?,
            &Request::Release,
        )
        .await
    }
}

fn tail_state(directory: &std::path::Path) -> String {
    if directory.join("lost.json").exists()
        || std::fs::read(directory.join("stats.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Stats>(&b).ok())
            .is_some_and(|s| s.lost_lines)
    {
        "Unknown"
    } else {
        "Available"
    }
    .into()
}

fn in_group(identity: &Identity, group: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{}/cgroup", identity.pid)).is_ok_and(|text| {
        text.lines().any(|line| {
            line.strip_prefix("0::")
                .is_some_and(|g| g == group || g.starts_with(&format!("{group}/")))
        })
    })
}
