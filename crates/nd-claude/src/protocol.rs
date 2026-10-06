//! mod 通道的协议状态机：绑定、重绑、代次、结果与报告。纯计算，不做 I/O。
//!
//! 通道把每条进出消息喂给 [`ModState::apply`]，得到归一化的事实；同一串消息录下来，
//! 回放时喂给新的状态机，事实应完全相同（录制回归）。
use nd_mod_proto::{
    Command, Hello, HelloCause, ModName, NextQuery, Outcome, Report, ReportBody, Resend, ResultPost,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// 通道里发生的一件事：mod 发来的消息、适配器发出的命令，或握手阶段的变化。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ModEvent {
    Hello {
        hello: Hello,
    },
    Poll {
        query: NextQuery,
    },
    Result {
        op_id: String,
        post: ResultPost,
    },
    Report {
        report: Report,
    },
    /// 适配器把命令放进这个 mod 的队列。
    Send {
        module: ModName,
        command: Command,
    },
    /// 握手结束：此后同一后端进程报来新的后端会话 id 算 `/clear` 重绑。
    Settled,
}

/// 归一化的事实，回放比对的单位。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum Fact {
    /// mod 绑定到后端会话。`matches` 表示与当前期望的后端会话 id 一致。
    Bound {
        module: ModName,
        backend_session_id: String,
        mod_gen: String,
        cause: HelloCause,
        matches: bool,
    },
    /// 后端会话 id 换了（`/clear`），绑定代次加一。
    Rebound {
        from: String,
        to: String,
        binding_epoch: u64,
    },
    /// 同一个 mod 换了代次（模块重载），旧代次留下的结果查不到了。
    Reloaded {
        module: ModName,
        from_gen: String,
        to_gen: String,
    },
    /// 轮询带的绑定不是当前的，要 mod 重报 hello。
    Rehello {
        module: ModName,
    },
    /// 命令进了队列。
    Sent {
        module: ModName,
        op_id: String,
        resend: Resend,
    },
    /// 命令已交给 mod。
    Delivered {
        module: ModName,
        op_id: String,
    },
    /// mod 回了结果。
    Finished {
        module: ModName,
        op_id: String,
        outcome: Outcome,
    },
    /// 发出的命令随旧代次丢了结果，可以重发：同一个操作 id 按新代次重新排队。
    Resent {
        module: ModName,
        op_id: String,
    },
    /// 发出的命令随旧代次丢了结果，不可重发：结论是 Unknown。
    Unknown {
        module: ModName,
        op_id: String,
    },
    /// 结果对不上在途的命令（重复、未知操作或不是发给这个 mod 的），丢弃。
    Stray {
        op_id: String,
    },
    Reported {
        module: ModName,
        report_id: String,
        body: ReportBody,
    },
    Settled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    pub hello: Hello,
}

/// 一个后端进程的 mod 通道状态。
#[derive(Clone, Debug)]
pub struct ModState {
    /// 期望的后端会话 id：握手时是预定或续接的 id，`/clear` 之后跟着换。
    pub session: String,
    pub binding_epoch: u64,
    pub settled: bool,
    pub bound: BTreeMap<ModName, Bound>,
    /// 已发出、还没有结果的命令。
    pub outstanding: BTreeMap<String, (ModName, Command, bool)>,
    /// 最近见过的报告 id，用于去重；只留最近 [`KEEP_REPORT_IDS`] 个。
    reports: BTreeSet<String>,
    report_order: VecDeque<String>,
}

pub const KEEP_REPORT_IDS: usize = 4096;

impl ModState {
    pub fn new(expected_session: &str) -> Self {
        Self {
            session: expected_session.to_owned(),
            binding_epoch: 0,
            settled: false,
            bound: BTreeMap::new(),
            outstanding: BTreeMap::new(),
            reports: BTreeSet::new(),
            report_order: VecDeque::new(),
        }
    }

    /// 这个 mod 当前是否绑定在期望的后端会话上。
    pub fn current(&self, module: ModName) -> Option<&Hello> {
        self.bound
            .get(&module)
            .map(|b| &b.hello)
            .filter(|h| h.backend_session_id == self.session)
    }

    /// 轮询带的身份是否就是当前绑定。
    pub fn polls_current(&self, query: &NextQuery) -> bool {
        self.current(query.module).is_some_and(|h| {
            h.mod_gen == query.mod_gen && h.backend_session_id == query.backend_session_id
        })
    }

    /// 某个 mod 队列里该交出去的命令（已发出、还没交付）。
    pub fn deliverable(&self, module: ModName) -> Vec<Command> {
        self.outstanding
            .values()
            .filter(|(m, _, delivered)| *m == module && !delivered)
            .map(|(_, c, _)| c.clone())
            .collect()
    }

    pub fn apply(&mut self, event: &ModEvent) -> Vec<Fact> {
        match event {
            ModEvent::Settled => {
                self.settled = true;
                vec![Fact::Settled]
            }
            ModEvent::Hello { hello } => self.hello(hello),
            ModEvent::Poll { query } => {
                if !self.polls_current(query) {
                    return vec![Fact::Rehello {
                        module: query.module,
                    }];
                }
                let mut facts = vec![];
                for (op_id, (module, _, delivered)) in self.outstanding.iter_mut() {
                    if *module == query.module && !*delivered {
                        *delivered = true;
                        facts.push(Fact::Delivered {
                            module: *module,
                            op_id: op_id.clone(),
                        });
                    }
                }
                facts
            }
            ModEvent::Send { module, command } => {
                self.outstanding
                    .insert(command.op_id.clone(), (*module, command.clone(), false));
                vec![Fact::Sent {
                    module: *module,
                    op_id: command.op_id.clone(),
                    resend: command.action.resend(),
                }]
            }
            ModEvent::Result { op_id, post } => match self.outstanding.get(op_id) {
                Some((module, _, _)) if *module == post.module => {
                    self.outstanding.remove(op_id);
                    vec![Fact::Finished {
                        module: post.module,
                        op_id: op_id.clone(),
                        outcome: post.outcome.clone(),
                    }]
                }
                _ => vec![Fact::Stray {
                    op_id: op_id.clone(),
                }],
            },
            ModEvent::Report { report } => {
                if !self.reports.insert(report.report_id.clone()) {
                    return vec![];
                }
                self.report_order.push_back(report.report_id.clone());
                while self.report_order.len() > KEEP_REPORT_IDS {
                    if let Some(oldest) = self.report_order.pop_front() {
                        self.reports.remove(&oldest);
                    }
                }
                vec![Fact::Reported {
                    module: report.module,
                    report_id: report.report_id.clone(),
                    body: report.body.clone(),
                }]
            }
        }
    }

    fn hello(&mut self, hello: &Hello) -> Vec<Fact> {
        let mut facts = vec![];
        if self.settled && hello.backend_session_id != self.session {
            // 握手之后同一后端进程报来新 id：`/clear` 换了后端会话。
            let from = std::mem::replace(&mut self.session, hello.backend_session_id.clone());
            self.binding_epoch += 1;
            facts.push(Fact::Rebound {
                from,
                to: hello.backend_session_id.clone(),
                binding_epoch: self.binding_epoch,
            });
        }
        if let Some(previous) = self.bound.get(&hello.module)
            && previous.hello.mod_gen != hello.mod_gen
        {
            facts.push(Fact::Reloaded {
                module: hello.module,
                from_gen: previous.hello.mod_gen.clone(),
                to_gen: hello.mod_gen.clone(),
            });
            // 旧代次的结果随模块变量一起没了：按可重发类别处理在途命令。
            let session = self.session.clone();
            let mut unknown = vec![];
            for (op_id, (module, command, delivered)) in self.outstanding.iter_mut() {
                if *module != hello.module || command.expected_mod_gen == hello.mod_gen {
                    continue;
                }
                match command.action.resend() {
                    Resend::Resendable => {
                        command.expected_mod_gen = hello.mod_gen.clone();
                        command.expected_backend_session_id = session.clone();
                        *delivered = false;
                        facts.push(Fact::Resent {
                            module: *module,
                            op_id: op_id.clone(),
                        });
                    }
                    Resend::NotResendable => unknown.push(op_id.clone()),
                }
            }
            for op_id in unknown {
                self.outstanding.remove(&op_id);
                facts.push(Fact::Unknown {
                    module: hello.module,
                    op_id,
                });
            }
        }
        facts.push(Fact::Bound {
            module: hello.module,
            backend_session_id: hello.backend_session_id.clone(),
            mod_gen: hello.mod_gen.clone(),
            cause: hello.cause,
            matches: hello.backend_session_id == self.session,
        });
        self.bound.insert(
            hello.module,
            Bound {
                hello: hello.clone(),
            },
        );
        facts
    }
}
