# 直接讲 Claude Code 的 stream-json 控制协议，不走 ACP 或官方 Agent SDK

守护进程用 Rust 写，官方 Agent SDK 是 TypeScript（要多带一个 Node 运行时），ACP 适配器又会丢掉插话优先级、ultracode 开关等 Claude Code 专有能力，所以守护进程自己拉起 CLI，用 stream-json 双向控制协议对话。CLI 二进制里带着全套协议 schema，规格见 research/protocol.md。代价是 CLI 升级可能改协议，靠录下来的真实会话做回归测试兜底。
