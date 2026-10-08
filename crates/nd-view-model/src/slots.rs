use nd_kernel::{Guard, Kernel, Registration, RegistrationKind, ScopeId};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    Sidebar,
    Item(String),
    Header,
    RightPanel,
    Settings,
    CommandPalette,
}
type Entries<T> = RefCell<BTreeMap<(Slot, String), (i32, T)>>;

/// 内核守卫管理调用权；呈现值可以含 GPUI 实体，不要求 Send。
pub struct Contribution<T> {
    pub slot: Slot,
    pub order: i32,
    pub value: T,
}

pub struct Slots<T> {
    components: BTreeMap<String, Vec<SlotGuard<T>>>,
    kernel: Kernel,
    scope: ScopeId,
    entries: Rc<Entries<T>>,
}
impl<T> Default for Slots<T> {
    fn default() -> Self {
        let kernel = Kernel::new();
        let scope = kernel.scope();
        Self {
            components: BTreeMap::new(),
            kernel,
            scope,
            entries: Rc::default(),
        }
    }
}
#[must_use = "组件应持有槽位守卫；丢弃即撤销"]
pub struct SlotGuard<T> {
    key: (Slot, String),
    entries: Weak<Entries<T>>,
    _guard: Guard,
}
impl<T> Drop for SlotGuard<T> {
    fn drop(&mut self) {
        if let Some(entries) = self.entries.upgrade() {
            let removed = entries.borrow_mut().remove(&self.key);
            drop(removed);
        }
    }
}
impl<T: Clone> Slots<T> {
    pub fn changed(&self) -> nd_kernel::Changed {
        self.kernel.changed_since(self.kernel.change_count())
    }
    pub fn configure(
        &mut self,
        name: &str,
        entries: &[Contribution<T>],
        enabled: bool,
    ) -> Result<(), nd_kernel::Error> {
        self.components.remove(name);
        if enabled {
            let mut guards = Vec::new();
            for entry in entries {
                guards.push(self.register(
                    entry.slot.clone(),
                    name,
                    entry.order,
                    entry.value.clone(),
                )?);
            }
            self.components.insert(name.into(), guards);
        }
        Ok(())
    }
    pub fn register(
        &mut self,
        slot: Slot,
        name: impl Into<String>,
        order: i32,
        value: T,
    ) -> Result<SlotGuard<T>, nd_kernel::Error> {
        let name = name.into();
        let guard = self.kernel.register(Registration::new(
            self.scope,
            RegistrationKind::Slot,
            format!("{slot:?}/{name}"),
        ))?;
        let key = (slot, name);
        self.entries
            .borrow_mut()
            .insert(key.clone(), (order, value));
        Ok(SlotGuard {
            key,
            entries: Rc::downgrade(&self.entries),
            _guard: guard,
        })
    }
    /// 依次按 order 和能力名排序；每帧重新取，撤销当帧可见。
    pub fn values(&self, slot: &Slot) -> Vec<T> {
        let entries = self.entries.borrow();
        let mut selected: Vec<_> = entries.iter().filter(|((s, _), _)| s == slot).collect();
        selected.sort_by_key(|((_, name), (order, _))| (*order, name));
        selected
            .into_iter()
            .map(|(_, (_, value))| value.clone())
            .collect()
    }
}
