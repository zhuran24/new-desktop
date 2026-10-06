//! mod 往返录制的夹具格式与回放（录制回归）。
//!
//! JSONL：首行 `{"type":"mod_fixture","meta":…,"backend_session_id":…,"count":N}`，其后每行一个
//! [`Recorded`]，`seq` 从 1 连续。空闲到时限的长轮询不录（它不改状态、不产生事实）。
//! 回放把事件按序喂给新的 [`ModState`]，逐条比对产生的事实。
use crate::{
    Result,
    channel::Recorded,
    protocol::{Fact, ModState},
};
pub use nd_watchdog_proto::FixtureMeta;
use std::path::Path;

pub const FORMAT: u32 = 1;

/// 写夹具；`backend_session_id` 是录制开始时期望的后端会话 id（回放的初值）。
pub fn write_fixture(
    path: &Path,
    meta: &FixtureMeta,
    backend_session_id: &str,
    records: &[Recorded],
) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)?;
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"mod_fixture","meta":meta,"backend_session_id":backend_session_id,"count":records.len()})
    )?;
    for record in records {
        writeln!(file, "{}", serde_json::to_string(record)?)?;
    }
    Ok(())
}

/// 读夹具并核完整性：格式版本、条数、序号连续；缺行、乱序、截断都报错。
pub fn read_fixture(path: &Path) -> Result<(FixtureMeta, String, Vec<Recorded>)> {
    use std::io::BufRead;
    let mut lines = std::io::BufReader::new(std::fs::File::open(path)?).lines();
    let header: serde_json::Value = serde_json::from_str(&lines.next().ok_or("empty fixture")??)?;
    if header["type"] != "mod_fixture" {
        return Err("not a mod fixture".into());
    }
    let meta: FixtureMeta = serde_json::from_value(header["meta"].clone())?;
    if meta.format != FORMAT {
        return Err("unsupported fixture version".into());
    }
    let session = header["backend_session_id"]
        .as_str()
        .ok_or("fixture lacks the initial backend session id")?
        .to_owned();
    let mut records = vec![];
    for line in lines {
        let record: Recorded = serde_json::from_str(&line?)?;
        if record.seq != records.len() as u64 + 1 {
            return Err(format!("fixture sequence broken at {}", record.seq).into());
        }
        records.push(record);
    }
    if header["count"].as_u64() != Some(records.len() as u64) {
        return Err("truncated fixture or incorrect declared count".into());
    }
    Ok((meta, session, records))
}

/// 按序重放录下的事件，返回每条事件产生的事实。
pub fn replay(backend_session_id: &str, records: &[Recorded]) -> Vec<Vec<Fact>> {
    let mut state = ModState::new(backend_session_id);
    records.iter().map(|r| state.apply(&r.event)).collect()
}
