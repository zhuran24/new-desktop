//! 每设备图片预览缓存。请求、加载完成和读取不依赖 GPUI。
use std::collections::{BTreeMap, VecDeque};

pub struct ImageCache<T> {
    entries: BTreeMap<String, Option<T>>,
    capacity: usize,
    recent: VecDeque<String>,
}
impl<T> Default for ImageCache<T> {
    fn default() -> Self {
        Self::new(64)
    }
}
impl<T> ImageCache<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            entries: BTreeMap::new(),
            capacity,
            recent: VecDeque::new(),
        }
    }
    /// 标记一次使用；只在需要启动下载时返回 true。
    pub fn request(&mut self, id: &str) -> bool {
        self.recent.retain(|key| key != id);
        self.recent.push_back(id.to_owned());
        if self.entries.contains_key(id) {
            return false;
        }
        if self.entries.len() >= self.capacity
            && let Some(oldest) = self.recent.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(id.to_owned(), None);
        true
    }
    /// 被淘汰的下载不复活缓存项。
    pub fn complete(&mut self, id: &str, value: T) {
        if let Some(entry) = self.entries.get_mut(id) {
            *entry = Some(value);
        }
    }
    pub fn get(&self, id: &str) -> Option<&T> {
        self.entries.get(id).and_then(Option::as_ref)
    }
    pub fn remove(&mut self, id: &str) {
        self.entries.remove(id);
        self.recent.retain(|key| key != id);
    }
}
