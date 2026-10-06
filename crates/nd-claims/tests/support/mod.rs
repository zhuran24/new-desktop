use std::process::{Child, Command, Stdio};
pub struct ShortProcess(Child);
impl ShortProcess {
    pub fn start() -> Self {
        Self(
            Command::new("/usr/bin/sleep")
                .arg("120")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
    pub fn identity(&self) -> nd_claims::Identity {
        nd_claims::Identity::read(self.0.id()).unwrap()
    }
    pub fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for ShortProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn registry(
    root: &std::path::Path,
    who: &nd_claims::Identity,
    session: &str,
) -> std::path::PathBuf {
    let mut row: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/session.json")).unwrap();
    row["startedAt"] = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64)
        .into();
    row["pid"] = who.pid.into();
    row["procStart"] = who.start_ticks.to_string().into();
    row["sessionId"] = session.into();
    row["pidDomain"] = format!(
        "linux:{}:{}",
        std::fs::read_to_string("/etc/machine-id")
            .unwrap_or_default()
            .trim(),
        std::fs::read_link("/proc/self/ns/pid").unwrap().display()
    )
    .into();
    std::fs::create_dir_all(root.join("sessions")).unwrap();
    let path = root.join("sessions").join(format!("{}.json", who.pid));
    std::fs::write(&path, serde_json::to_vec(&row).unwrap()).unwrap();
    path
}
pub fn ready(
    store: std::sync::Arc<nd_store::Store>,
    root: &std::path::Path,
) -> nd_claims::Exclusivity {
    let claims =
        nd_claims::Exclusivity::open(store, nd_claims::RegistryConfig::new(root.join("cli")))
            .unwrap();
    claims.observe(nd_claims::Observed::Recovered).unwrap();
    claims.refresh().unwrap();
    claims
}
pub fn eventually(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "observation did not converge"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
pub struct ScriptedCli {
    list: std::sync::Mutex<Vec<u8>>,
}
impl ScriptedCli {
    pub fn new(list: Vec<u8>) -> Self {
        Self {
            list: std::sync::Mutex::new(list),
        }
    }
}
impl nd_claims::CliCommands for ScriptedCli {
    fn agents(&self) -> std::result::Result<Vec<u8>, String> {
        Ok(self.list.lock().unwrap().clone())
    }
    fn stop(&self, _short_id: &str) -> std::result::Result<(), String> {
        Ok(())
    }
}
