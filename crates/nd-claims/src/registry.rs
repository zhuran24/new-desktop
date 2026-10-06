use crate::{BackendSessionId, Identity, error};
use nd_store::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct RegistryConfig {
    pub root: PathBuf,
    pub rescan_interval: std::time::Duration,
}
impl RegistryConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            rescan_interval: std::time::Duration::from_secs(2),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEntry {
    pub source: PathBuf,
    pub identity: Option<Identity>,
    pub bs: Option<BackendSessionId>,
    pub cwd: Option<PathBuf>,
    pub kind: String,
    pub version: Option<String>,
}
pub(crate) fn scan(config: &RegistryConfig) -> Result<Vec<ExternalEntry>> {
    let mut found = vec![];
    for item in read_dir(&config.root.join("sessions"))? {
        let path = item.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        if let Some(entry) = read_entry(&path)? {
            found.push(entry);
        }
    }
    for job in read_dir(&config.root.join("jobs"))? {
        if !job.file_type()?.is_dir() {
            continue;
        }
        if let Some(mut entry) = read_entry(&job.path().join("state.json"))? {
            entry.kind = "background".into();
            // The live sessions registry supplies the full identity for background workers too.
            // A state.json alone must not invent one from a job id or a pid without procStart.
            if !found.iter().any(|other| {
                other.bs == entry.bs && other.identity.is_some() && other.identity == entry.identity
            }) {
                found.push(entry);
            }
        }
    }
    found.sort_by(|a, b| a.source.cmp(&b.source));
    Ok(found)
}
fn read_dir(path: &Path) -> Result<Vec<std::fs::DirEntry>> {
    match std::fs::read_dir(path) {
        Ok(dir) => dir.collect::<std::io::Result<_>>().map_err(Into::into),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.into()),
    }
}
fn read_entry(path: &Path) -> Result<Option<ExternalEntry>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let row: serde_json::Value = serde_json::from_slice(&bytes).map_err(error)?;
    if !row.is_object() {
        return Err(error("registry entry is not an object"));
    }
    if row["sessionId"].as_str().is_none_or(str::is_empty) {
        return Err(error("registry entry has no backend session identity"));
    }
    let domain = std::fs::read_link("/proc/self/ns/pid").ok().map(|p| {
        format!(
            "linux:{}:{}",
            std::fs::read_to_string("/etc/machine-id")
                .unwrap_or_default()
                .trim(),
            p.display()
        )
    });
    let boot_millis = std::fs::read_to_string("/proc/stat").ok().and_then(|stat| {
        stat.lines()
            .find_map(|line| {
                line.strip_prefix("btime ")
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .and_then(|n| n.checked_mul(1000))
    });
    // Native registries have no boot_id. Their creation time must belong to this boot before
    // a same-PID/same-tick process can be assigned the current kernel's boot identity.
    let this_boot = boot_millis
        .zip(row["startedAt"].as_u64())
        .is_some_and(|(boot, started)| started >= boot);
    let identity =
        if this_boot && domain.is_some() && row["pidDomain"].as_str() == domain.as_deref() {
            if let Some(pid) = row["pid"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
            {
                match Identity::read(pid) {
                    Ok(who) => {
                        let start = row["procStart"]
                            .as_str()
                            .and_then(|s| s.parse::<u64>().ok());
                        if start.is_some_and(|s| s != who.start_ticks) {
                            return Ok(None);
                        }
                        match (start, who.matching()) {
                            (Some(_), Some(true)) => Some(who),
                            (Some(_), Some(false)) => return Ok(None),
                            _ => None,
                        }
                    }
                    Err(e)
                        if e.downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                    {
                        return Ok(None);
                    }
                    Err(_) => None,
                }
            } else {
                None
            }
        } else {
            None
        };
    Ok(Some(ExternalEntry {
        source: path.into(),
        identity,
        bs: row["sessionId"].as_str().map(BackendSessionId::claude),
        cwd: row["cwd"].as_str().map(PathBuf::from),
        kind: row["kind"].as_str().unwrap_or("unknown").into(),
        version: row["version"].as_str().map(str::to_string),
    }))
}

pub(crate) fn merge_agents(entries: &mut Vec<ExternalEntry>, bytes: &[u8]) -> Result<()> {
    let rows: Vec<serde_json::Value> = serde_json::from_slice(bytes).map_err(error)?;
    for (index, row) in rows.iter().enumerate() {
        if row["sessionId"].as_str().is_none_or(str::is_empty) {
            return Err(error("agents entry has no backend session identity"));
        }
        let bs = row["sessionId"].as_str().map(BackendSessionId::claude);
        // Join only on BOTH pid and session. A listing alone has no process-start identity.
        if entries.iter().any(|entry| {
            entry.bs == bs
                && entry
                    .identity
                    .as_ref()
                    .is_some_and(|id| Some(id.pid as u64) == row["pid"].as_u64())
        }) {
            continue;
        }
        entries.push(ExternalEntry {
            source: PathBuf::from(format!("agents/{index}")),
            identity: None,
            bs,
            cwd: row["cwd"].as_str().map(PathBuf::from),
            kind: row["kind"].as_str().unwrap_or("unknown").into(),
            version: row["version"].as_str().map(str::to_string),
        });
    }
    Ok(())
}
