//! 离线模型端点和使用真实进程的隔离场景运行器。
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
mod endpoint;
pub use endpoint::{ClaudeEndpoint, ModelReply, ModelRequest, ResponseGate, Route};
mod scenario;
pub use scenario::{
    CommandFault, Fifo, Process, Program, ResourceLimits, Scenario, ScenarioOptions,
};
