//! 操作目录（第 2 步）：新建（种子）、按需拉起、闲置回收。都占结构槽位，进行中代持给对话的新输入。
//!
//! 每个操作是一个纯函数 `run(&View, &mut Journal)`；改了已有键的含义或先后就把 VERSION 加一。
use crate::journal::{Change, Halt, Journal, View};
use crate::state::Status;
use nd_backend::{
    Act, BackendKind, BackendSessionId, CarrierId, Done, EndHow, Intent, Msg, OpenSpec, Origin,
    Outcome, Profile, RunId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OpSpec {
    Create(Create),
    Launch(Launch),
    Reclaim(Reclaim),
}
impl OpSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            OpSpec::Create(_) => "create",
            OpSpec::Launch(_) => "launch",
            OpSpec::Reclaim(_) => "reclaim",
        }
    }
    pub fn version(&self) -> u32 {
        match self {
            OpSpec::Create(_) => Create::VERSION,
            OpSpec::Launch(_) => Launch::VERSION,
            OpSpec::Reclaim(_) => Reclaim::VERSION,
        }
    }
    /// 结构操作：一个会话同时至多一个，进行中代持给对话的新输入。
    pub fn structural(&self) -> bool {
        true
    }
    pub fn run(&self, v: &View<'_>, j: &mut Journal<'_>) -> Result<Value, Halt> {
        match self {
            OpSpec::Create(op) => op.run(v, j),
            OpSpec::Launch(op) => op.run(v, j),
            OpSpec::Reclaim(op) => op.run(v, j),
        }
    }
}

fn uuid_from_hex(hex: &str) -> String {
    let mut h: Vec<u8> = hex.bytes().take(32).collect();
    h[12] = b'4';
    h[16] = b"89ab"[(h[16] as usize) % 4];
    let h = String::from_utf8(h).unwrap();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn profile(v: &View<'_>) -> Profile {
    let meta = v.meta();
    Profile {
        kind: meta.kind.clone(),
        model: meta.model.clone(),
        permission_mode: meta.permission_mode.clone(),
        cwd: meta.cwd.clone(),
    }
}

/// 新建：名册种下，在新会话里跑。拉起后端进程 → 发首条消息 → 首条消息回显落地才落定。
/// 首条消息没有补偿：它写给后端之后就算做过不可逆步骤；之前的失败只撤掉会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Create {
    #[serde(default)]
    pub attachments: Vec<nd_wire::Attachment>,
    pub text: String,
}
impl Create {
    pub const VERSION: u32 = 1;
    fn run(&self, v: &View<'_>, j: &mut Journal<'_>) -> Result<Value, Halt> {
        let carrier = CarrierId(format!("c-{}", j.id("carrier")));
        let run = RunId(format!("r-{}", j.id("run")));
        let bs = match v.meta().kind {
            BackendKind::Claude => BackendSessionId::claude(uuid_from_hex(&j.id("bs"))),
            BackendKind::Codex => {
                return Err(j.reject("unsupported", json!({"backend":"codex"})));
            }
        };
        j.claim(
            "claim",
            nd_claims::Act::Open {
                session: v.session().0.clone(),
                bs: nd_claims::NewBs::Known(bs.clone()),
                via: run.0.clone(),
            },
        )?;
        let opened = j
            .act(
                "open",
                &carrier,
                Act::Open {
                    carrier: carrier.clone(),
                    run: run.clone(),
                    spec: OpenSpec {
                        origin: Origin::Fresh { id: bs.clone() },
                        profile: profile(v),
                    },
                },
            )
            .undo(Act::End {
                carrier: carrier.clone(),
                how: EndHow::Discard,
            })
            .outcome()?;
        if !matches!(
            opened,
            Outcome::Ok {
                done: Done::Opened { .. }
            }
        ) {
            return Err(j.fail(format!("没能拉起后端进程：{}", opened.reason())));
        }
        j.bind("claim", &bs)?;
        let first = j
            .act(
                "first",
                &carrier,
                Act::Send {
                    to: carrier.clone(),
                    msg: Msg {
                        text: self.text.clone(),
                        attachments: self.attachments.clone(),
                        intent: Intent::Fold,
                    },
                },
            )
            .outcome()?;
        if !matches!(
            first,
            Outcome::Ok {
                done: Done::Landed { .. }
            }
        ) {
            return Err(j.fail(format!("首条消息没有确认送达：{}", first.reason())));
        }
        j.settle(vec![
            Change::Current {
                carrier: carrier.clone(),
            },
            Change::Status {
                status: Status::Active,
            },
        ])?;
        Ok(json!({"carrier": carrier, "backend_session": bs.id}))
    }
}

/// 按需拉起：给对话的输入找不到活进程时，引擎起它，续接当前后端会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub carrier: CarrierId,
}
impl Launch {
    pub const VERSION: u32 = 1;
    fn run(&self, v: &View<'_>, j: &mut Journal<'_>) -> Result<Value, Halt> {
        let Some(carrier) = v.carrier(&self.carrier) else {
            return Err(j.fail("承载位不见了"));
        };
        let bs = carrier.bs.clone();
        let run = RunId(format!("r-{}", j.id("run")));
        j.claim(
            "claim",
            nd_claims::Act::Open {
                session: v.session().0.clone(),
                bs: nd_claims::NewBs::Known(bs.clone()),
                via: run.0.clone(),
            },
        )?;
        let opened = j
            .act(
                "open",
                &self.carrier,
                Act::Open {
                    carrier: self.carrier.clone(),
                    run: run.clone(),
                    spec: OpenSpec {
                        origin: Origin::Resume { bs: bs.clone() },
                        profile: profile(v),
                    },
                },
            )
            .undo(Act::End {
                carrier: self.carrier.clone(),
                how: EndHow::Discard,
            })
            .outcome()?;
        if !matches!(
            opened,
            Outcome::Ok {
                done: Done::Opened { .. }
            }
        ) {
            return Err(j.fail(format!("没能拉起后端进程：{}", opened.reason())));
        }
        j.bind("claim", &bs)?;
        Ok(json!({"run": run}))
    }
}

/// 闲置回收：当前承载位无回合、无任务、无待答、没人在看，闲置到时限后自然收尾。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reclaim {
    pub carrier: CarrierId,
}
impl Reclaim {
    pub const VERSION: u32 = 1;
    fn run(&self, _v: &View<'_>, j: &mut Journal<'_>) -> Result<Value, Halt> {
        let ended = j
            .act(
                "end",
                &self.carrier,
                Act::End {
                    carrier: self.carrier.clone(),
                    how: EndHow::Graceful,
                },
            )
            .outcome()?;
        match ended {
            Outcome::Ok {
                done: Done::Ended { code },
            } => Ok(json!({"code": code})),
            other => Err(j.fail(format!("后端进程没有收尾：{}", other.reason()))),
        }
    }
}
