//! Append-only watchdog protocol. No dependency on daemon, CLI adapter or session engine.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const VERSION: u32 = 1;
// 附件最多 16 MiB；base64、JSON 转义和看守信封仍须装得下。
// 单行界限为 MAX_FRAME / 4（32 MiB），读回批次仍受此帧界限约束。
pub const MAX_FRAME: usize = 128 * 1024 * 1024;
pub const MAX_RECORDS_PER_READ: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub start_ticks: u64,
    pub boot_id: String,
}
impl Identity {
    pub fn read(pid: u32) -> Result<Self> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let fields = stat
            .rsplit_once(')')
            .ok_or("invalid proc stat")?
            .1
            .split_whitespace()
            .collect::<Vec<_>>();
        Ok(Self {
            pid,
            start_ticks: fields.get(19).ok_or("missing starttime")?.parse()?,
            boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
                .trim()
                .into(),
        })
    }
    /// None is an observation failure, not evidence that a process exited.
    pub fn matching(&self) -> Option<bool> {
        match Self::read(self.pid) {
            Ok(now) if now != *self => Some(false),
            Ok(_) => std::fs::read_to_string(format!("/proc/{}/stat", self.pid))
                .ok()
                .and_then(|s| {
                    s.rsplit_once(')')
                        .map(|(_, rest)| !rest.trim_start().starts_with('Z'))
                }),
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Some(false)
            }
            Err(_) => None,
        }
    }
    pub fn alive(&self) -> bool {
        self.matching() == Some(true)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub limits: Limits,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub soft_bytes: u64,
    pub hard_bytes: u64,
    pub overflow: PathBuf,
    pub stderr_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            soft_bytes: 64 * 1024 * 1024,
            hard_bytes: 256 * 1024 * 1024,
            overflow: PathBuf::new(),
            stderr_bytes: 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub stdout_lines: u64,
    pub stdout_bytes: u64,
    pub stream_lines: u64,
    pub stream_bytes: u64,
    pub retained_bytes: u64,
    pub overflow_bytes: u64,
    pub lost_lines: bool,
    pub acked: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WatchSpec {
    pub run: String,
    pub directory: PathBuf,
    pub launch: LaunchSpec,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub run: String,
    pub identity: Identity,
    pub watchdog: Identity,
    pub high: u64,
    pub written: u64,
    /// 已接受的输入高水位，包括排队中和正在写的行。旧看守缺省为 0。
    #[serde(default)]
    pub accepted: u64,
    pub exit: Option<i32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub seq: u64,
    pub end_seq: u64,
    pub ms: u64,
    pub event: Event,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Out { line: String },
    Err { chunk: String },
    In { in_seq: u64, line: String },
    Exit { code: i32 },
    Gap { reason: GapReason },
    StderrTruncated,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GapReason {
    DeltaDropped,
    LostLines,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Finish {
    CloseStdin,
    Terminate,
    Kill,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Attach { after: u64, limit: usize },
    Write { in_seq: u64, line: String },
    Ack { seq: u64 },
    Stats,
    Finish { action: Finish },
    Release,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello { hello: Hello },
    Records { records: Vec<Record> },
    Written { in_seq: u64 },
    Acked { seq: u64 },
    Stats { stats: Stats },
    Finished,
    Error { message: String },
}
pub async fn send<W: AsyncWrite + Unpin, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        return Err("watchdog frame too large".into());
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    Ok(())
}
pub async fn recv<R: AsyncRead + Unpin, T: DeserializeOwned>(reader: &mut R) -> Result<T> {
    let len = reader.read_u32().await? as usize;
    if len > MAX_FRAME {
        return Err("watchdog frame too large".into());
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
/// Runtime journal reader. A partial trailing write is retried on the next read.
pub fn read_records(directory: &Path, after: u64, limit: usize) -> Result<Vec<Record>> {
    use std::io::BufRead;
    let manifest: JournalManifest =
        serde_json::from_slice(&std::fs::read(directory.join("journal.json"))?)?;
    let mut files = vec![];
    for root in [directory.join("spool"), manifest.overflow] {
        if !root.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if let Some(seq) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.strip_suffix(".jsonl"))
                .and_then(|s| s.parse::<u64>().ok())
            {
                files.push((seq, entry.path()));
            }
        }
    }
    files.sort_by_key(|(seq, _)| *seq);
    let mut records = vec![];
    let mut size = 0;
    for (i, (_, path)) in files.iter().enumerate() {
        if files
            .get(i + 1)
            .is_some_and(|(start, _)| *start <= after.saturating_add(1))
        {
            continue;
        }
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for line in std::io::BufReader::new(file).split(b'\n') {
            let line = line?;
            let Ok(mut record) = serde_json::from_slice::<Record>(&line) else {
                break;
            };
            if record.end_seq > after {
                if record.seq <= after {
                    record.seq = after + 1;
                }
                if !records.is_empty() && size + line.len() > MAX_FRAME / 2 {
                    return Ok(records);
                }
                size += line.len();
                records.push(record);
                if records.len() >= limit.clamp(1, MAX_RECORDS_PER_READ) {
                    return Ok(records);
                }
            }
        }
    }
    // A full disk can leave only this compact emergency range; keep it visible after process exit.
    if let Ok(bytes) = std::fs::read(directory.join("lost.json")) {
        let gap: Record = serde_json::from_slice(&bytes)?;
        append_gap_tail(&mut records, after, limit, gap);
    }
    Ok(records)
}
/// Append only the part of an emergency gap beyond the returned cursor.
pub fn append_gap_tail(records: &mut Vec<Record>, after: u64, limit: usize, mut gap: Record) {
    let cursor = records.last().map_or(after, |r| r.end_seq);
    if gap.end_seq > cursor && records.len() < limit.clamp(1, MAX_RECORDS_PER_READ) {
        gap.seq = gap.seq.max(cursor + 1);
        records.push(gap);
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalManifest {
    pub overflow: PathBuf,
}
pub fn private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let temp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

/// Portable JSONL recording, indexed by capability/backend/version/scenario by the caller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureMeta {
    pub format: u32,
    pub capability: String,
    pub backend: String,
    pub version: String,
    pub scenario: String,
}
pub fn write_fixture(path: &Path, meta: &FixtureMeta, records: &[Record]) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"watchdog_fixture","meta":meta,"count":records.len(),"first_seq":records.first().map(|r|r.seq),"last_seq":records.last().map(|r|r.end_seq)})
    )?;
    for record in records {
        writeln!(file, "{}", serde_json::to_string(record)?)?;
    }
    Ok(())
}
pub fn read_fixture(path: &Path) -> Result<(FixtureMeta, Vec<Record>)> {
    use std::io::BufRead;
    let mut lines = std::io::BufReader::new(std::fs::File::open(path)?).lines();
    let header: serde_json::Value = serde_json::from_str(&lines.next().ok_or("empty fixture")??)?;
    if header["type"] != "watchdog_fixture" {
        return Err("not a watchdog fixture".into());
    }
    let meta: FixtureMeta = serde_json::from_value(header["meta"].clone())?;
    if meta.format != 1 {
        return Err("unsupported fixture version".into());
    }
    let mut records = vec![];
    let mut previous: Option<u64> = None;
    for line in lines {
        let record: Record = serde_json::from_str(&line?)?;
        if record.end_seq < record.seq
            || previous.is_some_and(|p| Some(record.seq) != p.checked_add(1))
        {
            return Err("non-contiguous fixture; missing Gap marker".into());
        }
        previous = Some(record.end_seq);
        records.push(record);
    }
    if header["count"].as_u64() != Some(records.len() as u64)
        || header["first_seq"].as_u64() != records.first().map(|r| r.seq)
        || header["last_seq"].as_u64() != records.last().map(|r| r.end_seq)
    {
        return Err("truncated fixture or incorrect declared range".into());
    }
    Ok((meta, records))
}
