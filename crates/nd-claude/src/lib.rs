//! Claude 适配：按启动模板拉起 Claude Code 后端进程、两个 mod 的通道与就绪判定。
//!
//! 后端进程由看守进程托管（nd-runs）；本 crate 持有看守连接（唯一的 stdin 写入者）
//! 和 mod 通道（unix socket 上的 HTTP 长轮询）。会话引擎、后端端口由后续工单接入。
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub mod channel;
pub mod fixture;
pub mod launch;
pub mod protocol;
pub mod run;

pub use channel::{Binding, CommandResult, ModChannel, Recorded};
pub use launch::{ClaudeConfig, OLD_MODS, Open, Start, launch_spec, settings};
pub use protocol::{Fact, ModEvent, ModState};
pub use run::{Availability, Caps, Claude, ClaudeRun, Feature, InitOptions, Readiness, Ready};
