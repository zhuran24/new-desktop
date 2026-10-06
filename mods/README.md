# New Desktop 的两个 mod

日期：2026-10-06。New Desktop 拉起每个 Claude Code 后端进程时，用两个 `--plugin-dir` 带上这两个 mod（ADR 0011）。它们经守护进程的 mod 通道（unix socket 上的 HTTP 长轮询）取命令、回报结果，由 mod 发起、自动重连。通道协议与守护进程一侧见 [Claude 适配](../crates/nd-claude/README.md)。

| mod | 名字（plugin.json） | 挂的钩子 | 职责 |
|---|---|---|---|
| 钩子 mod | `new-desktop` | `session.start`、`classic.SessionStart`、`session.end` | 报到与控制循环；`/clear` 后用新 id 立即重报；把 `session.end` 作为报告交上来。以后挂退役、转接、Codex 子代理、总结的压缩钩子和只读命令。它不发起会引出模型请求的调用 |
| 动作 mod | `new-desktop-actions` | 只有 `session.start` | 起控制循环，替守护进程发起调用、按操作 id 留结果。刻意保持薄 |

两个 mod 的 `userConfig` 声明 `sock`（mod 通道 socket）和 `run`（后端进程编号），由守护进程经 `--settings` 的 `pluginConfigs` 传入。版本号与 `nd-mod-proto` 的 crate 版本一致。

## 文件与静态检查

CLI 装载前按固定规则扫描源码，违反就整个模块拒载（`claude plugin validate <目录>` 按同一规则提前报）。2.1.289 上实测：

- 钩子必须是文件顶层声明的函数（或绑定函数的 const），可以从本 mod 自己的文件 `import`；不用 `import()`。
- `$` 只能传给**同一文件**里的顶层函数，不能跨 `import` 传。
- `on()` 的事件名、`$.env.get/set` 的名字都写字面量。

所以按能力拆文件（N5）的做法是：每个能力文件放它自己的钩子和用 `$` 的辅助函数；`state.ts` 是共享状态的唯一位置，只放数据和纯函数，不碰 `$`；`proto.ts` 是生成的协议类型与常量。钩子 mod 现在有 `channel.ts`（报到、控制循环、命令执行）和 `lifecycle.ts`（会话结束报告）两个能力文件。新增的命令只要用到 `$`，就要写在控制循环所在的 `channel.ts` 里。

模块重载（文件变动后 `reload_plugins`、worker 重生）会清零模块变量、再跑一次 `session.start`：mod 代次重新生成，旧代次的结果查不到。

## 生成的协议类型

`hooks/proto.ts` 由 Rust 类型生成，不要手改：

```bash
cargo run -p nd-mod-proto --bin nd-mod-schema -- .
```

同时更新 `protocol/mod.schema.json`。默认测试和 CI 都会重新生成并比对。CLI 装载时会在 mod 目录写 `.claude-plugin/types/`，该目录不进仓库。
