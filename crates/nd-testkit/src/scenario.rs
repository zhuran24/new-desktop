use crate::{ClaudeEndpoint, Result};
use nd_ui_core::SyncReplica;
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

pub struct Program {
    binary: PathBuf,
    args: Vec<String>,
    claude: bool,
}
impl Program {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            args: vec![],
            claude: false,
        }
    }
    /// The pinned BUILD.md CLI, with a fake key and only the local offline endpoint.
    pub fn claude() -> Self {
        Self {
            binary: "/mnt/wd_external/nd-build/cli/claude-2.1.289".into(),
            args: vec![],
            claude: true,
        }
    }
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
}

/// Handle to a real child service; the containing Scenario owns cleanup.
pub struct Process {
    unit: String,
    output: PathBuf,
}
impl Process {
    pub fn unit(&self) -> &str {
        &self.unit
    }
    pub fn stdout(&self) -> Result<String> {
        Ok(std::fs::read_to_string(
            self.output.with_extension("stdout"),
        )?)
    }
    pub fn stderr(&self) -> Result<String> {
        Ok(std::fs::read_to_string(
            self.output.with_extension("stderr"),
        )?)
    }
    pub async fn wait_for_stdout(&self, text: &str, timeout: Duration) -> Result<()> {
        tokio::time::timeout(timeout, async {
            loop {
                if self.stdout().is_ok_and(|out| out.contains(text)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            format!(
                "{} missing stdout {text:?}: {}",
                self.unit,
                self.stderr().unwrap_or_default()
            )
        })?;
        Ok(())
    }
    pub async fn wait(&self, timeout: Duration) -> Result<i32> {
        tokio::time::timeout(timeout, async {
            loop {
                if let Ok(exit) = std::fs::read_to_string(self.output.with_extension("exit")) {
                    return exit.parse::<i32>();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            format!(
                "{} timed out: {}",
                self.unit,
                self.stderr().unwrap_or_default()
            )
        })?
        .map_err(Into::into)
    }
    pub fn kill(&self) -> Result<()> {
        checked(
            "systemctl",
            &["--user", "kill", "--signal=KILL", &self.unit],
        )?;
        Ok(())
    }
}

pub struct Fifo {
    path: PathBuf,
    file: std::fs::File,
}
impl Fifo {
    /// Path as seen by sandboxed Bash, CLI tools and other processes.
    pub fn sandbox_path(&self) -> PathBuf {
        Path::new("/sandbox/fifos").join(self.path.file_name().unwrap())
    }
    pub fn release(&mut self, text: &str) -> Result<()> {
        if text.len() > 4095 || text.contains('\n') {
            return Err("FIFO release must be a single line of at most 4095 bytes".into());
        }
        self.file.write_all(format!("{text}\n").as_bytes())?;
        Ok(())
    }
}

pub struct ResourceLimits {
    pub memory_max: u64,
    pub memory_swap_max: u64,
}

pub struct ScenarioOptions {
    pub name: String,
    pub daemon: PathBuf,
    pub config: String,
    pub memory_max: u64,
    pub timeout: Duration,
    pub watchdog: Option<PathBuf>,
    pub disk_scratch: bool,
}
impl ScenarioOptions {
    pub fn new(name: &str, daemon: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            daemon: daemon.into(),
            config: String::new(),
            memory_max: 2 * 1024 * 1024 * 1024,
            timeout: Duration::from_secs(10),
            watchdog: None,
            disk_scratch: false,
        }
    }
}

pub struct Scenario {
    dir: Option<tempfile::TempDir>,
    disk: Option<tempfile::TempDir>,
    slice: String,
    services: Vec<String>,
    daemon_args: Vec<String>,
    timeout: Duration,
    endpoint: Option<ClaudeEndpoint>,
    watchdog_config: Option<nd_runs::Config>,
}

/// The command commit windows supplied by the daemon's `scenarios` build.
pub enum CommandFault {
    CrashAfterEffect,
    CrashBeforeCommit,
    CrashAfterCommit,
    UnavailableAfterEffect,
}

fn command(binary: &str) -> Command {
    let mut cmd = Command::new(binary);
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env(
            "XDG_RUNTIME_DIR",
            format!("/run/user/{}", rustix::process::geteuid().as_raw()),
        );
    cmd
}
fn checked(binary: &str, args: &[&str]) -> Result<Output> {
    let out = command(binary).args(args).output()?;
    if !out.status.success() {
        return Err(format!(
            "{binary} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(out)
}

impl Scenario {
    pub async fn start(options: ScenarioOptions) -> Result<Self> {
        if options.name.is_empty()
            || options.name.len() > 32
            || !options
                .name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(
                "scenario name must be 1..32 lowercase ASCII letters, digits or hyphens".into(),
            );
        }
        let daemon = options.daemon.canonicalize()?;
        // systemd must own the same runtime directory as the packaged service.
        // A unique parent keeps each scenario away from the owner's instance.
        let runtime = PathBuf::from(format!("/run/user/{}", rustix::process::geteuid().as_raw()));
        let dir = tempfile::Builder::new()
            .prefix("nd-test-")
            .tempdir_in(runtime)?;
        for name in [
            "home", "claude", "config", "data", "state", "cache", "runtime", "project", "out",
            "fifos", "programs",
        ] {
            std::fs::create_dir(dir.path().join(name))?;
        }
        // `{root}` 换成本场景的根目录（宿主路径，守护进程在沙盒里同一路径可见）。
        std::fs::write(
            dir.path().join("config.toml"),
            options.config.replace(
                "{root}",
                dir.path().to_str().ok_or("non-UTF8 scenario root")?,
            ),
        )?;
        std::fs::write(dir.path().join("sandbox.py"), include_str!("sandbox.py"))?;
        let id = uuid::Uuid::new_v4().simple();
        let slice = format!("nd-test-{}{id}.slice", options.name.replace('-', ""));
        let unit = format!("nd-test-{}-{id}-daemon.service", options.name);
        let mut this = Self {
            dir: Some(dir),
            disk: if options.disk_scratch {
                Some(
                    tempfile::Builder::new()
                        .prefix("nd-test-disk-")
                        .tempdir_in("/mnt/wd_external/nd-build/tmp")?,
                )
            } else {
                None
            },
            slice,
            services: vec![unit],
            daemon_args: Vec::new(),
            timeout: options.timeout,
            endpoint: None,
            watchdog_config: None,
        };
        // A transient slice leaves no runtime drop-ins behind on success or failure.
        checked(
            "busctl",
            &[
                "--user",
                "call",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                "StartTransientUnit",
                "ssa(sv)a(sa(sv))",
                &this.slice,
                "fail",
                "3",
                "MemoryMax",
                "t",
                &options.memory_max.to_string(),
                "MemorySwapMax",
                "t",
                "0",
                "CollectMode",
                "s",
                "inactive-or-failed",
                "0",
            ],
        )?;
        if let Some(watchdog) = options.watchdog {
            let watchdog = watchdog.canonicalize()?;
            // Keep host PIDs for /proc identity checks. Network and home remain isolated.
            let mut launcher = this.sandbox_args(&watchdog)?;
            launcher.retain(|s| s != "--unshare-all");
            launcher.splice(
                1..1,
                [
                    "--unshare-user",
                    "--unshare-ipc",
                    "--unshare-net",
                    "--unshare-uts",
                ]
                .map(str::to_owned),
            );
            launcher.extend([
                "--ro-bind".into(),
                watchdog.to_string_lossy().into_owned(),
                watchdog.to_string_lossy().into_owned(),
            ]);
            launcher.extend(
                [
                    "--ro-bind",
                    "/mnt/wd_external/nd-build/cli/claude-2.1.289",
                    "/cli",
                    "/usr/bin/python3",
                    "/sandbox/sandbox.py",
                    "--watchdog",
                ]
                .map(str::to_owned),
            );
            let config = nd_runs::Config {
                root: this.root().join("runtime/runs"),
                watchdog,
                unit_prefix: this.services[0].trim_end_matches("-daemon.service").into(),
                slice: this.slice.clone(),
                memory_max: 2 * 1024 * 1024 * 1024,
                launcher,
            };
            let section = toml::to_string(&serde_json::json!({"watchdogs":config}))?;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(this.root().join("config.toml"))?;
            writeln!(file, "\n{section}")?;
            this.watchdog_config = Some(config);
        }
        this.endpoint = Some(ClaudeEndpoint::bind(this.root().join("model.sock")).await?);
        let mut args = this.service_args(&this.services[0], true);
        args.extend(this.sandbox_args(&daemon)?);
        if this.watchdog_config.is_some() {
            // 短命的模型目录查询由真适配器在守护进程 cgroup 里直接拉起。
            args.extend(
                [
                    "--ro-bind",
                    "/mnt/wd_external/nd-build/cli/claude-2.1.289",
                    "/cli",
                ]
                .map(str::to_owned),
            );
            let bus = format!("/run/user/{}/bus", rustix::process::geteuid().as_raw());
            args.retain(|s| s != "--unshare-all");
            let at = args.iter().position(|s| s == "/usr/bin/bwrap").unwrap() + 1;
            args.splice(
                at..at,
                [
                    "--unshare-user",
                    "--unshare-ipc",
                    "--unshare-net",
                    "--unshare-uts",
                ]
                .map(str::to_owned),
            );
            args.extend(["--ro-bind".into(), bus.clone(), bus]);
            let private = format!(
                "/run/user/{}/systemd/private",
                rustix::process::geteuid().as_raw()
            );
            args.extend(["--ro-bind".into(), private.clone(), private]);
        }
        args.extend([
            "/program".into(),
            "--root".into(),
            this.root().to_string_lossy().into_owned(),
        ]);
        checked(
            "systemd-run",
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
        )?;
        this.daemon_args = args;
        this.connect().await?;
        Ok(this)
    }

    fn service_args(&self, unit: &str, restart: bool) -> Vec<String> {
        let mut args = vec![
            "--user",
            "--quiet",
            "--collect",
            "--unit",
            unit,
            "--slice",
            &self.slice,
            "-p",
            "LimitCORE=0",
            "-p",
            "MemorySwapMax=0",
            "-p",
            "TimeoutStopSec=3s",
            "-p",
            "KillMode=control-group",
            "-p",
            "RuntimeMaxSec=300",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        if restart {
            for property in include_str!("../../../packaging/systemd/nd-daemon.service").lines() {
                if let Some((key, value)) = property.split_once('=') {
                    if key == "RuntimeDirectory" {
                        args.extend([
                            "-p".into(),
                            format!(
                                "RuntimeDirectory={}/runtime",
                                self.root().file_name().unwrap().to_string_lossy()
                            ),
                        ]);
                    } else if matches!(key, "RuntimeDirectoryMode" | "RuntimeDirectoryPreserve") {
                        args.extend(["-p".into(), format!("{key}={value}")]);
                    }
                }
            }
            args.extend(
                [
                    "-p",
                    "StartLimitIntervalSec=0",
                    "-p",
                    "Restart=on-failure",
                    "-p",
                    "RestartSec=100ms",
                ]
                .map(str::to_owned),
            );
        }
        args
    }
    fn sandbox_args(&self, binary: &Path) -> Result<Vec<String>> {
        let mut args = vec![
            "/usr/bin/bwrap",
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/bin",
            "/bin",
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
            "--dir",
            "/etc",
            "--bind",
            self.root().to_str().ok_or("non-UTF8 scenario root")?,
            "/sandbox",
            "--bind",
            self.root().to_str().ok_or("non-UTF8 scenario root")?,
            self.root().to_str().ok_or("non-UTF8 scenario root")?,
            "--ro-bind",
            binary.to_str().ok_or("non-UTF8 program path")?,
            "/program",
            "--chdir",
            "/sandbox/project",
            "--clearenv",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        if let Some(disk) = &self.disk {
            args.extend([
                "--bind".into(),
                disk.path().to_string_lossy().into_owned(),
                "/scratch".into(),
            ]);
        }
        for (key, value) in [
            ("PATH", "/usr/bin:/bin"),
            ("LANG", "C.UTF-8"),
            ("HOME", "/sandbox/home"),
            ("CLAUDE_CONFIG_DIR", "/sandbox/claude"),
            ("XDG_CONFIG_HOME", "/sandbox/config"),
            ("XDG_DATA_HOME", "/sandbox/data"),
            ("XDG_STATE_HOME", "/sandbox/state"),
            ("XDG_CACHE_HOME", "/sandbox/cache"),
            ("XDG_RUNTIME_DIR", "/sandbox/runtime"),
        ] {
            args.extend(["--setenv", key, value].map(str::to_owned));
        }
        Ok(args)
    }
    pub fn watchdog_config(&self) -> Result<nd_runs::Config> {
        self.watchdog_config
            .clone()
            .ok_or_else(|| "watchdog not configured".into())
    }
    pub fn watchdogs(&self) -> Result<nd_runs::Watchdogs> {
        nd_runs::Watchdogs::new(
            self.watchdog_config
                .clone()
                .ok_or("watchdog not configured")?,
        )
    }
    pub fn claude_watchdog_spec(&self, run: &str) -> Result<nd_watchdog_proto::LaunchSpec> {
        let mut spec = self.watchdog_spec(
            run,
            "/cli",
            &[
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--model",
                "claude-haiku-4-5",
                "--permission-mode",
                "bypassPermissions",
                "--dangerously-skip-permissions",
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--replay-user-messages",
                "--include-partial-messages",
                "--settings",
                r#"{"enableWorkflows":true}"#,
            ],
        )?;
        for (key, value) in [
            ("ANTHROPIC_BASE_URL", "http://127.0.0.1:8765"),
            ("ANTHROPIC_API_KEY", "offline-fixture"),
            ("DISABLE_AUTOUPDATER", "1"),
            ("DISABLE_TELEMETRY", "1"),
            ("DISABLE_ERROR_REPORTING", "1"),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ("CLAUDE_CODE_EAGER_FLUSH", "1"),
            ("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1"),
            ("TERM", "dumb"),
        ] {
            spec.env.insert(key.into(), value.into());
        }
        Ok(spec)
    }
    pub fn watchdog_spec(
        &self,
        run: &str,
        binary: &str,
        args: &[&str],
    ) -> Result<nd_watchdog_proto::LaunchSpec> {
        Ok(nd_watchdog_proto::LaunchSpec {
            argv: std::iter::once(binary.to_owned())
                .chain(args.iter().map(|s| (*s).to_owned()))
                .collect(),
            cwd: "/sandbox/project".into(),
            limits: nd_watchdog_proto::Limits {
                overflow: self.root().join("cache/spool-overflow").join(run),
                ..Default::default()
            },
            env: [
                ("PATH", "/usr/bin:/bin"),
                ("HOME", "/sandbox/home"),
                ("CLAUDE_CONFIG_DIR", "/sandbox/claude"),
                ("XDG_CONFIG_HOME", "/sandbox/config"),
                ("XDG_DATA_HOME", "/sandbox/data"),
                ("XDG_STATE_HOME", "/sandbox/state"),
                ("XDG_CACHE_HOME", "/sandbox/cache"),
                ("XDG_RUNTIME_DIR", "/sandbox/runtime"),
                ("LANG", "C.UTF-8"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
        })
    }
    pub fn disk_root(&self) -> Option<&Path> {
        self.disk.as_ref().map(|d| d.path())
    }
    pub fn root(&self) -> &Path {
        self.dir.as_ref().expect("open scenario").path()
    }
    pub fn endpoint(&self) -> &ClaudeEndpoint {
        self.endpoint.as_ref().expect("open scenario")
    }
    pub fn spawn(&mut self, name: &str, program: Program) -> Result<Process> {
        if name.is_empty()
            || name.len() > 32
            || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err("process name must be 1..32 ASCII letters, digits or hyphens".into());
        }
        let config = self.root().join("programs").join(format!("{name}.json"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config)?;
        write!(
            file,
            "{}",
            serde_json::json!({"name":name,"args":program.args,"claude":program.claude})
        )?;
        let unit = self.services[0].replace("-daemon.service", &format!("-{name}.service"));
        if self.services.contains(&unit) {
            return Err("process name already used".into());
        }
        let mut args = self.service_args(&unit, false);
        args.extend(self.sandbox_args(&program.binary.canonicalize()?)?);
        args.extend([
            "/usr/bin/python3".into(),
            "/sandbox/sandbox.py".into(),
            format!("/sandbox/programs/{name}.json"),
        ]);
        // Register before launch so even a partial launch is owned by cleanup.
        self.services.push(unit.clone());
        checked(
            "systemd-run",
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
        )?;
        Ok(Process {
            unit,
            output: self.root().join("out").join(name),
        })
    }
    pub fn fifo(&self, name: &str) -> Result<Fifo> {
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return Err("invalid FIFO name".into());
        }
        let path = self.root().join("fifos").join(name);
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &path,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )?;
        // RDWR keeps a reader present; NONBLOCK prevents a stalled task blocking the harness.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(&path)?;
        Ok(Fifo { path, file })
    }
    pub fn limits(&self) -> Result<ResourceLimits> {
        let out = checked(
            "systemctl",
            &[
                "--user",
                "show",
                &self.slice,
                "-p",
                "ControlGroup",
                "--value",
            ],
        )?;
        let path = Path::new("/sys/fs/cgroup").join(
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .trim_start_matches('/'),
        );
        Ok(ResourceLimits {
            memory_max: std::fs::read_to_string(path.join("memory.max"))?
                .trim()
                .parse()?,
            memory_swap_max: std::fs::read_to_string(path.join("memory.swap.max"))?
                .trim()
                .parse()?,
        })
    }
    pub fn units(&self) -> Vec<String> {
        self.services
            .iter()
            .cloned()
            .chain([self.slice.clone()])
            .collect()
    }
    pub fn stop_daemon(&self) -> Result<()> {
        checked("systemctl", &["--user", "stop", &self.services[0]])?;
        Ok(())
    }
    pub fn start_daemon(&self) -> Result<()> {
        checked(
            "systemd-run",
            &self
                .daemon_args
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )?;
        Ok(())
    }
    pub fn restart_daemon(&self) -> Result<()> {
        checked("systemctl", &["--user", "restart", &self.services[0]])?;
        Ok(())
    }
    pub fn kill_daemon(&self) -> Result<()> {
        checked(
            "systemctl",
            &["--user", "kill", "--signal=KILL", &self.services[0]],
        )?;
        Ok(())
    }
    pub fn arm_command_fault(&self, id: &str, fault: CommandFault) -> Result<()> {
        let (point, action) = match fault {
            CommandFault::CrashAfterEffect => ("after_effect", "crash"),
            CommandFault::CrashBeforeCommit => ("before_commit", "crash"),
            CommandFault::CrashAfterCommit => ("after_commit", "crash"),
            CommandFault::UnavailableAfterEffect => ("after_effect", "unavailable"),
        };
        let dest = self.root().join("command-fault.json");
        if dest.exists() {
            return Err("an unconsumed command fault is already armed".into());
        }
        let temp = self.root().join("command-fault.tmp");
        std::fs::write(
            &temp,
            serde_json::json!({"id":id,"point":point,"action":action}).to_string(),
        )?;
        std::fs::rename(temp, dest)?;
        Ok(())
    }
    pub fn command_fault_consumed(&self) -> bool {
        !self.root().join("command-fault.json").exists()
    }
    pub async fn connect(&self) -> Result<SyncReplica> {
        let socket = self.root().join("runtime/nd.sock");
        tokio::time::timeout(self.timeout, async {
            loop {
                if let Ok(ui) = SyncReplica::connect(&socket).await {
                    return ui;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| format!("daemon did not become ready: {}", self.journal()).into())
    }
    fn journal(&self) -> String {
        command("journalctl")
            .args(["--user", "-u", &self.services[0], "-n", "20", "--no-pager"])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default()
    }
    fn cleanup(&mut self) -> Result<()> {
        if self.dir.is_none() {
            return Ok(());
        }
        // Stopping the slice also kills any descendant unit launched by the product.
        checked("systemctl", &["--user", "stop", &self.slice])?;
        self.endpoint.take();
        self.dir.take().unwrap().close()?;
        if let Some(disk) = self.disk.take() {
            disk.close()?;
        }
        Ok(())
    }
    pub fn close(mut self) -> Result<()> {
        self.cleanup()
    }
}
impl Drop for Scenario {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("scenario cleanup failed: {error}");
        }
    }
}
