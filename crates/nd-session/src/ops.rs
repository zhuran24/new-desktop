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
    Configure(Configure),
    Title(Title),
}
impl OpSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            OpSpec::Create(_) => "create",
            OpSpec::Launch(_) => "launch",
            OpSpec::Reclaim(_) => "reclaim",
            OpSpec::Configure(_) => "configure",
            OpSpec::Title(_) => "title",
        }
    }
    pub fn version(&self) -> u32 {
        match self {
            OpSpec::Create(_) => Create::VERSION,
            OpSpec::Launch(_) => Launch::VERSION,
            OpSpec::Reclaim(_) => Reclaim::VERSION,
            OpSpec::Configure(_) | OpSpec::Title(_) => 1,
        }
    }
    /// 结构操作：一个会话同时至多一个，进行中代持给对话的新输入。
    pub fn structural(&self) -> bool {
        !matches!(
            self,
            Self::Title(Title {
                request: TitleRequest::Generate { .. },
                ..
            })
        )
    }
    pub fn run(&self, v: &View<'_>, j: &mut Journal<'_>) -> Result<Value, Halt> {
        match self {
            OpSpec::Create(op) => op.run(v, j),
            OpSpec::Launch(op) => op.run(v, j),
            OpSpec::Reclaim(op) => op.run(v, j),
            OpSpec::Configure(op) => op.run(v, j),
            OpSpec::Title(op) => op.run(v, j),
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
        effort: meta.effort.clone(),
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
                        live_settings: v
                            .meta()
                            .settings
                            .applied
                            .ultracode_requested
                            .map(nd_wire::LiveSetting::Ultracode)
                            .into_iter()
                            .collect(),
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
                        live_settings: v
                            .meta()
                            .settings
                            .applied
                            .ultracode_requested
                            .map(nd_wire::LiveSetting::Ultracode)
                            .into_iter()
                            .collect(),
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Configure {
    pub carrier: CarrierId,
    pub setting: nd_wire::LiveSetting,
}
impl Configure {
    fn run(&self, v: &View<'_>, j: &mut Journal<'_>) -> Result<Value, Halt> {
        if matches!(self.setting, nd_wire::LiveSetting::Model(_)) {
            j.wait("between-turns", v, |v| {
                v.carrier(&self.carrier)
                    .filter(|c| !c.turn_running)
                    .map(|_| true)
            })?;
        }
        if !v.carrier(&self.carrier).is_some_and(|c| c.alive) {
            Launch {
                carrier: self.carrier.clone(),
            }
            .run(v, j)?;
        }
        let outcome = j
            .act(
                "configure",
                &self.carrier,
                Act::Configure {
                    to: self.carrier.clone(),
                    setting: self.setting.clone(),
                },
            )
            .outcome()?;
        match outcome {
            Outcome::Ok {
                done: Done::Configured { settings },
            } => Ok(serde_json::to_value(settings).expect("neutral settings")),
            other => Err(j.fail(other.reason())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TitleRequest {
    Rename { title: String },
    Generate { description: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "TitleRecord")]
pub struct Title {
    pub carrier: CarrierId,
    pub request: TitleRequest,
}
#[derive(Deserialize)]
struct TitleRecord {
    carrier: CarrierId,
    #[serde(default)]
    request: Option<TitleRequest>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    generate: bool,
}
impl From<TitleRecord> for Title {
    fn from(record: TitleRecord) -> Self {
        Self {
            carrier: record.carrier,
            request: record.request.unwrap_or_else(|| {
                if record.generate {
                    TitleRequest::Generate {
                        description: record.title,
                    }
                } else {
                    TitleRequest::Rename {
                        title: record.title,
                    }
                }
            }),
        }
    }
}
impl Title {
    fn run(&self, v: &View<'_>, j: &mut Journal<'_>) -> Result<Value, Halt> {
        if !v.carrier(&self.carrier).is_some_and(|c| c.alive) {
            Launch {
                carrier: self.carrier.clone(),
            }
            .run(v, j)?;
        }
        let outcome = j
            .act(
                "title",
                &self.carrier,
                Act::Invoke {
                    to: self.carrier.clone(),
                    invocation: match &self.request {
                        TitleRequest::Generate { description } => {
                            nd_backend::Invocation::GenerateTitle {
                                description: description.clone(),
                            }
                        }
                        TitleRequest::Rename { title } => nd_backend::Invocation::Title {
                            title: title.clone(),
                        },
                    },
                },
            )
            .outcome()?;
        match outcome {
            Outcome::Ok {
                done: Done::Titled { title },
            } => Ok(json!({"title":title})),
            other => Err(j.fail(other.reason())),
        }
    }
}
