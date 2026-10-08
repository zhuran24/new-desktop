//! 对话条目的状态词汇及共享判定；开放解码保留未来状态的安全回退。
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! states {
    ($name:ident { $($variant:ident => ($wire:literal, $label:literal)),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant,)+
            #[serde(rename = "other", other)] Other,
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.as_str()) }
        }
        impl $name {
            pub fn from_value(value: &serde_json::Value) -> Self {
                serde_json::from_value(value.clone()).unwrap_or(Self::Other)
            }
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire,)+ Self::Other => "other" }
            }
            pub fn label(self) -> &'static str {
                match self { $(Self::$variant => $label,)+ Self::Other => "" }
            }
        }
    };
}
states!(PromptState {
    Held => ("held", "代持中"),
    Waiting => ("waiting", "等待可写"),
    Pending => ("pending", "等待写出"),
    Written => ("written", "已写出"),
    Withdrawing => ("withdrawing", "撤回中"),
    Withdrawn => ("withdrawn", "已撤回"),
    Landed => ("landed", "已送达"),
    Failed => ("failed", "发送失败"),
    Unknown => ("unknown", "交付不明"),
    NotDelivered => ("not_delivered", "未送达"),
    Resent => ("resent", "已重发"),
});
impl PromptState {
    pub fn unsettled(self) -> bool {
        matches!(
            self,
            Self::Held | Self::Waiting | Self::Pending | Self::Written | Self::Withdrawing
        )
    }
    pub fn withdrawable(self) -> bool {
        self.unsettled() && self != Self::Withdrawing
    }
    pub fn terminal(self) -> bool {
        self != Self::Other && !self.unsettled()
    }
}
states!(ControlState {
    Pending => ("pending", "正在处理"),
    Acknowledged => ("acknowledged", "停止请求已送达"),
    AlreadyEnded => ("already_ended", "目标回合已结束，无需中断"),
    Withdrawn => ("withdrawn", "已撤回，内容已保存在草稿中"),
    NotWithdrawable => ("not_withdrawable", "消息已开始处理，无法撤回"),
    Unknown => ("unknown", "交付不明，请核对会话状态"),
    Failed => ("failed", "操作失败"),
});
impl ControlState {
    pub fn unsettled(self) -> bool {
        self == Self::Pending
    }
}
