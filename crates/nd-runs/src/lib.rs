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
#[derive(Clone)]
pub struct Watchdogs {
    config: Config,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launched {
    pub identity: Identity,
    pub unit: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum RunState {
    Up,
    Gone { reason: GoneReason },
    IdentityMismatch,
}
impl std::fmt::Display for RunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Up => "Up",
            Self::Gone { .. } => "Gone",
            Self::IdentityMismatch => "IdentityMismatch",
        })
    }
}
impl RunState {
    pub fn is_gone(self) -> bool {
        matches!(self, Self::Gone { .. })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoneReason {
    NeverLaunched,
    Exited,
    ProcGone,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tail {
    Available,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Found {
    pub run: String,
    pub identity: Option<Identity>,
    #[serde(flatten)]
    pub state: RunState,
    pub high: u64,
    pub exit: Option<i32>,
    pub tail: Tail,
    pub detail: Option<String>,
}
impl Found {
    /// 看守报告方把已核实的进程事实交给独占登记；无法核实的身份继续拒写。
    pub fn observation(
        &self,
        generation: u64,
        kind: nd_claims::BackendKind,
    ) -> nd_claims::Observed {
        use nd_claims::{GoneHow, Observed};
        let run = self.run.clone();
        match (self.state, self.identity.as_ref()) {
            (RunState::Up, Some(identity)) if identity.matching() == Some(true) => Observed::Up {
                run,
                identity: identity.clone(),
                generation,
                kind,
            },
            (
                RunState::Gone {
                    reason: reason @ (GoneReason::ProcGone | GoneReason::Exited),
                },
                Some(identity),
            ) if identity.matching() == Some(false) => Observed::Gone {
                run,
                identity: Some(identity.clone()),
                how: match reason {
                    GoneReason::Exited => GoneHow::Exited,
                    _ => GoneHow::ProcGone,
                },
            },
            (
                RunState::Gone {
                    reason: GoneReason::NeverLaunched,
                },
                None,
            ) => Observed::Gone {
                run,
                identity: None,
                how: GoneHow::NeverLaunched,
            },
            _ => Observed::IdentityMismatch { run },
        }
    }
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
        if self.archive(run)?.exists() {
            return Err("run already collected; cannot replay launch".into());
        }
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
        if !directory.join("started").exists() && self.unit_group_async(run).await?.is_none() {
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
            let out = tokio::task::spawn_blocking(move || cmd.output()).await??;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).to_string().into());
            }
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(found) = self
                    .inspect_run_async(run)
                    .await?
                    .filter(|f| f.state == RunState::Up)
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
                if directory.join("hello.json").exists()
                    && self.unit_group_async(run).await?.is_none()
                {
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
            .unit_group_async(run)
            .await?
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
        let Response::Hello { mut hello } = response else {
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
        // 旧看守尚无 accepted；从保留流水补齐在途输入，不能只信 written。
        if hello.accepted.is_none() {
            let mut accepted = hello.written;
            let mut cursor = 0;
            loop {
                let rows = read_records(&directory, cursor, MAX_RECORDS_PER_READ)?;
                if rows.is_empty() {
                    break;
                }
                for row in &rows {
                    match row.event {
                        Event::In { in_seq, .. } => accepted = accepted.max(in_seq),
                        Event::Gap {
                            reason: GapReason::LostLines,
                        } => return Err("legacy watchdog input watermark unavailable".into()),
                        _ => {}
                    }
                    cursor = row.end_seq;
                }
            }
            hello.accepted = Some(accepted);
        }
        Ok(WatchLink {
            socket: Some(socket),
            hello,
        })
    }
    /// Startup claims each still-live controller once, reads hello, and never acknowledges output.
    pub async fn recover(&self) -> Result<Vec<Found>> {
        let this = self.clone();
        let mut found = tokio::task::spawn_blocking(move || this.inspect()).await??;
        for run in &mut found {
            if run.state == RunState::Up {
                match self.link(&run.run).await {
                    Ok(link) => run.high = link.hello.high,
                    Err(_) => run.state = RunState::IdentityMismatch,
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
    async fn unit_group_async(&self, run: &str) -> Result<Option<String>> {
        let this = self.clone();
        let run = run.to_owned();
        tokio::task::spawn_blocking(move || this.unit_group(&run)).await?
    }
    pub async fn inspect_run_async(&self, run: &str) -> Result<Option<Found>> {
        let this = self.clone();
        let run = run.to_owned();
        tokio::task::spawn_blocking(move || this.inspect_run(&run)).await?
    }
    fn archive(&self, run: &str) -> Result<PathBuf> {
        self.directory(run)?;
        Ok(self.config.root.join(format!("{run}.gone.json")))
    }
    /// Read-only diagnostics never acquire the adapter's control connection.
    pub fn inspect(&self) -> Result<Vec<Found>> {
        let mut runs = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(&self.config.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() {
                runs.insert(name);
            } else if let Some(run) = name.strip_suffix(".gone.json") {
                runs.insert(run.to_owned());
            }
        }
        runs.into_iter()
            .filter_map(|run| self.inspect_run(&run).transpose())
            .collect()
    }
    pub fn inspect_run(&self, run: &str) -> Result<Option<Found>> {
        let directory = self.directory(run)?;
        if self.archive(run)?.exists() {
            return Ok(Some(serde_json::from_slice(&std::fs::read(
                self.archive(run)?,
            )?)?));
        }
        if !directory.exists() {
            return Ok(None);
        }
        let saved = std::fs::read(directory.join("hello.json"))
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
                RunState::Up
            }
            (Some(h), Ok(None))
                if h.identity.matching() == Some(false) && h.watchdog.matching() == Some(false) =>
            {
                RunState::Gone {
                    reason: if h.exit.is_some() {
                        GoneReason::Exited
                    } else {
                        GoneReason::ProcGone
                    },
                }
            }
            (None, Ok(None))
                if !directory.join("started").exists()
                    || directory.join("failed.json").exists() =>
            {
                RunState::Gone {
                    reason: GoneReason::NeverLaunched,
                }
            }
            _ => RunState::IdentityMismatch,
        };
        Ok(Some(Found {
            detail: group.as_ref().err().map(ToString::to_string),
            tail: tail_state(&directory),
            run: run.to_owned(),
            identity: saved.as_ref().map(|h| h.identity.clone()),
            state,
            high: saved.as_ref().map_or(0, |h| h.high),
            exit: saved.and_then(|h| h.exit),
        }))
    }
    /// 调用方提供持久会话状态的引用集合；仅回收已 Gone 且无人引用的流水。
    /// 小墓碑保留诊断和 run 幂等性；它不需要 systemctl，也不保存流水。
    pub fn collect_unused(&self, referenced: &std::collections::BTreeSet<String>) -> Result<usize> {
        let mut collected = 0;
        for entry in std::fs::read_dir(&self.config.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let run = entry.file_name().to_string_lossy().into_owned();
            if referenced.contains(&run) {
                continue;
            }
            let Some(found) = self.inspect_run(&run)? else {
                continue;
            };
            if !found.state.is_gone() {
                continue;
            }
            private_json(&self.archive(&run)?, &found)?;
            if let Ok(bytes) = std::fs::read(entry.path().join("journal.json")) {
                let manifest: JournalManifest = serde_json::from_slice(&bytes)?;
                // 只删除这个 run 专属目录；自定义共享目录不递归删除。
                if manifest
                    .overflow
                    .file_name()
                    .is_some_and(|name| name == run.as_str())
                    && manifest.overflow.exists()
                {
                    std::fs::remove_dir_all(manifest.overflow)?;
                }
            }
            std::fs::remove_dir_all(entry.path())?;
            collected += 1;
        }
        Ok(collected)
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

fn tail_state(directory: &std::path::Path) -> Tail {
    if directory.join("lost.json").exists()
        || std::fs::read(directory.join("stats.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Stats>(&b).ok())
            .is_some_and(|s| s.lost_lines)
    {
        Tail::Unknown
    } else {
        Tail::Available
    }
}

fn in_group(identity: &Identity, group: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{}/cgroup", identity.pid)).is_ok_and(|text| {
        text.lines().any(|line| {
            line.strip_prefix("0::")
                .is_some_and(|g| g == group || g.starts_with(&format!("{group}/")))
        })
    })
}
