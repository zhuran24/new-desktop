# 用 Rust 自写原生界面，不用 Electron、网页界面或现成客户端

官方桌面端的外壳实测常驻约 1.26 GiB，而 owner 的优先级是快、稳、省资源，写代码的工作量不算成本，所以桌面界面用 Rust 的 GPUI 自绘；若中文输入法（fcitx5 上的豆包、Rime）验收不过，改用 gtk4-rs。GitHub 上的现成客户端（Paseo、Waku、Zed、Zeron 等）都不能原样替换桌面端，评审见 research/github/REPORT.md。

## Considered Options

- Electron / 系统 WebView 的网页界面：中文输入无忧，但正是 owner 要摆脱的分量。
- 直接用 Paseo（Electron，功能最全）或 fork Waku / Zeron（GPUI，直连 CLI）：前者不轻，后两者缺功能（会话列表、审批卡片等），也绑定别人的取舍。
