use super::*;

impl Executor {
    // —— 闲置回收 ——

    /// 当前进程闲置：没有操作、没有回合、发送台空、没有未结的票、后台任务确知已收尾、没人在看。
    pub(super) fn idle_now(&self) -> bool {
        let Some(meta) = &self.core.meta else {
            return false;
        };
        let Some(carrier) = self.core.current_carrier() else {
            return false;
        };
        matches!(meta.status, Status::Active | Status::Partial)
            && self.core.ops.is_empty()
            && self.core.messages.is_empty()
            && self.core.invokes.is_empty()
            && self.core.outbox.is_empty()
            && carrier.alive
            && !carrier.turn_running
            && carrier.drain == Drain::Drained
            && self.shared.watchers.load(Ordering::Acquire) == 0
    }

    pub(super) fn idle_due(&mut self) -> bool {
        if !self.idle_now() {
            self.idle_since = None;
            return false;
        }
        let since = *self.idle_since.get_or_insert_with(Instant::now);
        since.elapsed() >= self.deps.config.idle_reclaim
    }
}
