//! 按节监视、校验后发布的配置。修订号不跨守护进程纪元复用。
use notify::Watcher;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("配置修订冲突")]
    Conflict,
    #[error("配置无效: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
pub type Revision = String;
pub type Validator = fn(&Value) -> Result<()>;

pub trait ConfigSource: Send + Sync {
    fn read(&self) -> Result<Option<String>>;
    fn compare_and_swap(&self, previous: Option<&str>, next: &str) -> Result<()>;
}
pub struct FileSource {
    path: PathBuf,
}
impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    /// 监视目录以支持编辑器的原子 rename；通知只触发重新读取，不当作配置内容。
    pub fn watch(&self) -> Result<(notify::RecommendedWatcher, tokio::sync::mpsc::Receiver<()>)> {
        let (send, recv) = tokio::sync::mpsc::channel(1);
        let mut watcher =
            notify::recommended_watcher(move |_event: notify::Result<notify::Event>| {
                let _ = send.try_send(());
            })
            .map_err(|e| Error::Invalid(e.to_string()))?;
        watcher
            .watch(
                self.path.parent().unwrap_or(Path::new(".")),
                notify::RecursiveMode::NonRecursive,
            )
            .map_err(|e| Error::Invalid(e.to_string()))?;
        Ok((watcher, recv))
    }
}
impl ConfigSource for FileSource {
    fn read(&self) -> Result<Option<String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn compare_and_swap(&self, previous: Option<&str>, next: &str) -> Result<()> {
        let parent = self.path.parent().unwrap_or(Path::new("."));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(parent.join("config.lock"))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        if self.read()?.as_deref() != previous {
            return Err(Error::Conflict);
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(next.as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(&self.path).map_err(|e| Error::Io(e.error))?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Versioned<T> {
    pub revision: Revision,
    pub value: T,
}
struct State {
    text: Option<String>,
    published: Versioned<Value>,
}
pub struct Config {
    source: Arc<dyn ConfigSource>,
    defaults: Value,
    validate: Validator,
    state: Mutex<State>,
    changed: watch::Sender<Versioned<Value>>,
}
pub trait Section: DeserializeOwned + Clone {
    const NAME: &'static str;
    fn parse(value: &Value) -> Result<Self> {
        let section: Self = serde_json::from_value(value[Self::NAME].clone())
            .map_err(|e| Error::Invalid(format!("{}: {e}", Self::NAME)))?;
        section.check()?;
        Ok(section)
    }
    fn check(&self) -> Result<()> {
        Ok(())
    }
}
pub struct SectionWatch<S> {
    last: Value,
    receiver: watch::Receiver<Versioned<Value>>,
    _type: std::marker::PhantomData<S>,
}
impl<S: Section> SectionWatch<S> {
    pub fn get(&self) -> Result<Versioned<S>> {
        let s = self.receiver.borrow();
        Ok(Versioned {
            revision: s.revision.clone(),
            value: S::parse(&s.value)?,
        })
    }
    pub async fn changed(&mut self) -> Result<Versioned<S>> {
        loop {
            self.receiver
                .changed()
                .await
                .map_err(|e| Error::Invalid(e.to_string()))?;
            let value = self.receiver.borrow().value[S::NAME].clone();
            if value != self.last {
                self.last = value;
                return self.get();
            }
        }
    }
}
impl Config {
    pub fn open(
        source: Arc<dyn ConfigSource>,
        defaults: Value,
        validate: Validator,
    ) -> Result<Self> {
        let text = source.read()?;
        let value = parse(text.as_deref(), &defaults, validate)?;
        let published = Versioned {
            revision: uuid::Uuid::new_v4().to_string(),
            value,
        };
        let (changed, _) = watch::channel(published.clone());
        Ok(Self {
            source,
            defaults,
            validate,
            state: Mutex::new(State { text, published }),
            changed,
        })
    }
    pub fn section<S: Section>(&self) -> Result<SectionWatch<S>> {
        let receiver = self.changed.subscribe();
        let last = receiver.borrow().value[S::NAME].clone();
        let s = SectionWatch {
            last,
            receiver,
            _type: std::marker::PhantomData,
        };
        s.get()?;
        Ok(s)
    }
    pub fn snapshot(&self) -> Versioned<Value> {
        self.state.lock().unwrap().published.clone()
    }
    pub fn refresh(&self) -> Result<bool> {
        let mut state = self.state.lock().unwrap();
        let text = self.source.read()?;
        if text == state.text {
            return Ok(false);
        }
        let value = parse(text.as_deref(), &self.defaults, self.validate)?;
        state.text = text;
        if value == state.published.value {
            return Ok(false);
        }
        state.published = Versioned {
            revision: uuid::Uuid::new_v4().to_string(),
            value,
        };
        self.changed.send_replace(state.published.clone());
        Ok(true)
    }
    pub fn update(&self, patch: Value, expect: &str) -> Result<Revision> {
        self.refresh()?;
        let mut state = self.state.lock().unwrap();
        if state.published.revision != expect {
            return Err(Error::Conflict);
        }
        if !patch.is_object() {
            return Err(Error::Invalid("补丁必须为对象".into()));
        }
        let mut user = match state.text.as_deref() {
            Some(text) => serde_json::to_value(
                toml::from_str::<toml::Value>(text).map_err(|e| Error::Invalid(e.to_string()))?,
            )
            .map_err(|e| Error::Invalid(e.to_string()))?,
            None => serde_json::json!({}),
        };
        merge_patch(&mut user, patch);
        let text = toml::to_string_pretty(&user).map_err(|e| Error::Invalid(e.to_string()))?;
        let value = parse(Some(&text), &self.defaults, self.validate)?;
        self.source.compare_and_swap(state.text.as_deref(), &text)?;
        state.text = Some(text);
        state.published = Versioned {
            revision: uuid::Uuid::new_v4().to_string(),
            value,
        };
        self.changed.send_replace(state.published.clone());
        Ok(state.published.revision.clone())
    }
}
fn parse(text: Option<&str>, defaults: &Value, validate: Validator) -> Result<Value> {
    let mut value = defaults.clone();
    if let Some(text) = text {
        let parsed: toml::Value =
            toml::from_str(text).map_err(|e| Error::Invalid(e.to_string()))?;
        merge(
            &mut value,
            serde_json::to_value(parsed).map_err(|e| Error::Invalid(e.to_string()))?,
        );
    }
    validate(&value)?;
    Ok(value)
}
fn merge(target: &mut Value, patch: Value) {
    if let (Some(a), Some(b)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in b {
            merge(a.entry(key.clone()).or_insert(Value::Null), value.clone());
        }
    } else {
        *target = patch;
    }
}

/// User patches delete explicit overrides with null; defaults are merged only for validation/publication.
fn merge_patch(target: &mut Value, patch: Value) {
    if let Value::Object(patch) = patch {
        if !target.is_object() {
            *target = serde_json::json!({});
        }
        let target = target.as_object_mut().unwrap();
        for (key, value) in patch {
            if value.is_null() {
                target.remove(&key);
            } else {
                merge_patch(target.entry(key).or_insert(Value::Null), value);
            }
        }
    } else {
        *target = patch;
    }
}
