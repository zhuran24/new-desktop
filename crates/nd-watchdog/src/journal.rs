use nd_watchdog_proto::*;
use std::{
    collections::VecDeque,
    fs::File,
    io::{Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
struct Segment {
    path: PathBuf,
    end: u64,
    size: u64,
    overflow: bool,
}
pub struct Journal {
    directory: PathBuf,
    limits: Limits,
    segments: VecDeque<Segment>,
    active: Option<File>,
    tail: Option<(u64, Record)>,
    emergency: Option<Record>,
    pub high: u64,
    pub stats: Stats,
    stderr: u64,
}
impl Journal {
    pub fn new(directory: &Path, mut limits: Limits) -> Result<Self> {
        if limits.hard_bytes < limits.soft_bytes {
            return Err("hard limit below soft limit".into());
        }
        if limits.overflow.as_os_str().is_empty() {
            let cache = std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
                .ok_or("HOME or XDG_CACHE_HOME required for overflow")?;
            limits.overflow = cache
                .join("new-desktop/spool-overflow")
                .join(directory.file_name().ok_or("run directory")?);
        }
        if limits.overflow == directory.join("spool") {
            return Err("overflow equals runtime spool".into());
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory.join("spool"))?;
        private_json(
            &directory.join("journal.json"),
            &JournalManifest {
                overflow: limits.overflow.clone(),
            },
        )?;
        Ok(Self {
            directory: directory.into(),
            limits,
            segments: VecDeque::new(),
            active: None,
            tail: None,
            emergency: None,
            high: 0,
            stats: Stats::default(),
            stderr: 0,
        })
    }
    pub fn append(&mut self, mut event: Event) -> Result<()> {
        if let Event::Out { line } = &event {
            self.stats.stdout_lines += 1;
            self.stats.stdout_bytes += line.len() as u64 + 1;
            let delta = serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|v| v["type"] == "stream_event");
            if delta {
                self.stats.stream_lines += 1;
                self.stats.stream_bytes += line.len() as u64 + 1;
                if self.stats.retained_bytes >= self.limits.soft_bytes {
                    event = Event::Gap {
                        reason: GapReason::DeltaDropped,
                    };
                }
            }
        }
        if let Event::Err { chunk } = &event {
            if self.stderr >= self.limits.stderr_bytes {
                return Ok(());
            }
            self.stderr += chunk.len() as u64;
            if self.stderr > self.limits.stderr_bytes {
                event = Event::StderrTruncated;
            }
        }
        if matches!(
            event,
            Event::Gap {
                reason: GapReason::LostLines
            }
        ) {
            self.stats.lost_lines = true;
        }
        self.high += 1;
        let record = Record {
            seq: self.high,
            end_seq: self.high,
            ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as u64,
            event,
        };
        if let Some(gap) = self.emergency.take() {
            if self.store(gap.clone()).is_err() {
                self.emergency = Some(gap);
            } else {
                let _ = std::fs::remove_file(self.directory.join("lost.json"));
            }
        }
        if self.emergency.is_some() || self.store(record).is_err() {
            self.stats.lost_lines = true;
            let gap = self.emergency.get_or_insert(Record {
                seq: self.high,
                end_seq: self.high,
                ms: 0,
                event: Event::Gap {
                    reason: GapReason::LostLines,
                },
            });
            gap.end_seq = self.high;
            // Best effort outside the quota. The live protocol always retains this bounded range.
            let _ = private_json(&self.directory.join("lost.json"), gap);
            return Err("journal storage failed: LostLines".into());
        }
        Ok(())
    }
    fn store(&mut self, record: Record) -> Result<()> {
        let overflow = self.stats.retained_bytes >= self.limits.hard_bytes;
        if self.active.is_none()
            || self
                .segments
                .back()
                .is_some_and(|s| s.size >= 256 * 1024 || s.overflow != overflow)
        {
            self.active = None;
            self.tail = None;
            let root = if overflow {
                self.limits.overflow.clone()
            } else {
                self.directory.join("spool")
            };
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&root)?;
            let path = root.join(format!("{:020}.jsonl", record.seq));
            let file = std::fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(&path)?;
            self.segments.push_back(Segment {
                path,
                end: record.end_seq,
                size: 0,
                overflow,
            });
            self.active = Some(file);
        }
        let segment = self.segments.back_mut().unwrap();
        let file = self.active.as_mut().unwrap();
        let mut row = record;
        let offset = if let Some((offset, previous)) = &self.tail {
            if matches!(row.event, Event::Gap { .. })
                && previous.event == row.event
                && previous.end_seq + 1 == row.seq
            {
                row.seq = previous.seq;
                row.ms = previous.ms;
                *offset
            } else {
                segment.size
            }
        } else {
            segment.size
        };
        let bytes = serde_json::to_vec(&row)?;
        file.seek(SeekFrom::Start(offset))?;
        if let Err(e) = file.write_all(&bytes).and_then(|_| file.write_all(b"\n")) {
            let _ = file.set_len(offset);
            self.active = None;
            self.tail = None;
            return Err(e.into());
        }
        let size = offset + bytes.len() as u64 + 1;
        file.set_len(size)?;
        self.stats.retained_bytes = self.stats.retained_bytes - segment.size + size;
        if segment.overflow {
            self.stats.overflow_bytes = self.stats.overflow_bytes - segment.size + size;
        }
        segment.size = size;
        segment.end = row.end_seq;
        self.tail = Some((offset, row));
        Ok(())
    }
    pub fn read(&self, after: u64, limit: usize) -> Result<Vec<Record>> {
        if after < self.stats.acked {
            return Err("cursor already acknowledged".into());
        }
        if after > self.high {
            return Err("cursor beyond high watermark".into());
        }
        let mut rows = read_records(&self.directory, after, limit)?;
        if let Some(gap) = &self.emergency
            && gap.end_seq > rows.last().map_or(after, |r| r.end_seq)
            && rows.len() < limit.clamp(1, 1000)
        {
            let mut gap = gap.clone();
            gap.seq = gap.seq.max(after + 1);
            rows.push(gap);
        }
        Ok(rows)
    }
    pub fn ack(&mut self, seq: u64) -> Result<()> {
        if seq > self.high {
            return Err("ack beyond high watermark".into());
        }
        if seq <= self.stats.acked {
            return Ok(());
        }
        self.active = None;
        self.tail = None;
        while self.segments.front().is_some_and(|s| s.end <= seq) {
            let s = self.segments.front().unwrap();
            std::fs::remove_file(&s.path)?;
            self.stats.retained_bytes -= s.size;
            if s.overflow {
                self.stats.overflow_bytes -= s.size;
            }
            self.segments.pop_front();
        }
        self.stats.acked = seq;
        Ok(())
    }
}
