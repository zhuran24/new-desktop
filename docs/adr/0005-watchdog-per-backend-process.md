---
status: accepted
---

# 每个后端进程外面套一个看守进程

守护进程在开发期间会频繁改代码、重启；后端进程如果直接是它的子进程，每次重启都会打断正在跑的会话。所以每个后端进程由一个极小、几乎不改动的看守进程持有输入输出并先落盘，守护进程重启后重新连上，会话不断（思路同 containerd 的 shim）。owner 2026-10-05 接受（四份组件方案都建立在它上面）。

## Consequences

- 一个会话可能同时有当前后端进程和几个退役进程（ADR 0008），各有看守进程。
- New Desktop 的 mod 和守护进程之间的通道由 mod 发起、自动重连，不依赖守护进程一直在（research/round9/BRIDGE.md §1.4）。
- 看守进程是独立的 transient systemd 服务单元，不设 PartOf、BindsTo，守护进程重启不连带它；不设孤儿时限。
- 看守进程自己死了不重跑；它记的流水放运行目录、不 fsync，按输入序号去重。Codex app-server 也套看守进程。
- 看守进程和守护进程之间的协议只加不改，守护进程兼容所有还活着的看守进程版本（research/round10/COMPONENTS.md §7.5）。
