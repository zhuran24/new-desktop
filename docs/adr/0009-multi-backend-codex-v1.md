# 后端可替换，第一版同时接 Claude Code 和 Codex

会话和后端之间按可替换的接口设计，不把 Claude Code 写死在会话模型里；第一版就接 Codex（走 Codex app-server 协议，codex-direct 时已摸清），Gemini 预留位置，Grok 待定。这样会话树、分叉、插话、审批这些概念要按「后端无关」来定义，各后端做不到的再单独标出。

## Consequences

- Codex 会话的 Esc 照 Codex TUI：发 `turn/interrupt`；这一回合属于活动目标时，另发 `thread/goal/set paused`。中断只停这一回合（模型请求、在途工具、还没派成功的子代理），已派出的子代理和已登记的后台终端照跑（research/round11/ANSWER.md §1）。
- 各后端做不到的由后端端口的能力表说明（ADR 0013）；第三家 CLI 怎么接、第一版先做哪几件见 ADR 0017。
