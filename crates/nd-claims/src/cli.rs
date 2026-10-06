//! The only replaceable external boundary in the exclusivity seam.
use std::{
    collections::BTreeMap,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub trait CliCommands: Send + Sync {
    fn agents(&self) -> Result<Vec<u8>, String>;
    fn stop(&self, short_id: &str) -> Result<(), String>;
}
pub(crate) struct Unconfigured;
impl CliCommands for Unconfigured {
    fn agents(&self) -> Result<Vec<u8>, String> {
        Err("approved CLI is not configured".into())
    }
    fn stop(&self, _: &str) -> Result<(), String> {
        Err("approved CLI is not configured".into())
    }
}
/// The release owner supplies a pinned executable and a complete environment whitelist.
/// Construction neither reads credentials nor falls back to a PATH-resolved CLI.
pub struct PinnedCli {
    pub executable: PathBuf,
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
}
impl PinnedCli {
    fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        if !self.executable.is_absolute() || self.timeout.is_zero() {
            return Err("invalid pinned CLI configuration".into());
        }
        let mut child = Command::new(&self.executable)
            .args(args)
            .env_clear()
            .envs(&self.env)
            .env_remove("BUN_OPTIONS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut bytes = vec![];
            stdout
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("CLI timed out".into());
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(e.to_string());
                }
            }
        };
        let bytes = reader
            .join()
            .map_err(|_| "CLI output reader failed")?
            .map_err(|e| e.to_string())?;
        if !status?.success() {
            return Err("CLI command failed".into());
        }
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("CLI output exceeds limit".into());
        }
        Ok(bytes)
    }
}
impl CliCommands for PinnedCli {
    fn agents(&self) -> Result<Vec<u8>, String> {
        self.run(&["agents", "--json", "--all"])
    }
    fn stop(&self, short_id: &str) -> Result<(), String> {
        if short_id.len() != 8 || !short_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("stop requires an 8-digit hexadecimal id".into());
        }
        self.run(&["stop", short_id]).map(|_| ())
    }
}
