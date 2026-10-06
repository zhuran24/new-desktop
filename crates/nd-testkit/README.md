# 离线场景运行器与 Claude 端点

日期：2026-10-06。#5 已实现：每场景一个真守护进程、真实 systemd/bwrap 隔离、同步副本、伪 Claude Messages API、扣放与计数、FIFO 和命令提交故障点。产品会话、两个 mod 和看守进程的接入由对应工单提供。

## 运行

开发机需有 systemd 用户实例、cgroup v2、bwrap、Python 3，以及 BUILD.md 钉住的 `/mnt/wd_external/nd-build/cli/claude-2.1.289`。端点本身由 Rust 实现；Python 只负责隔离内的 TCP→UDS 字节转接和子进程输出收集。

在工作树根目录运行：

```bash
scripts/test-scenarios.sh
```

脚本先构建带 `scenarios` 的真 nd-daemon，再运行 #3/#4 的场景回归和本库的场景。构建目录默认按当前 `ticket/N` 分支选择 E 盘 `target/ticket-N`；build/test 各用 12 GiB、零 swap 的独立 scope、6 个构建 jobs。`CARGO_TARGET_DIR` 可显式指定 BUILD.md 允许的退路。所有模型请求留在离线伪端点内，不消费真实模型额度。

`cargo test --workspace` 包含端点 HTTP 行为测试。需要开发机 systemd 和固定 CLI 的场景由 `nd-testkit/scenarios` 启用；直接运行时还须设置 `ND_TEST_DAEMON` 为带故障点的 nd-daemon 绝对路径。缺依赖时报错，不把跳过当通过。

## 接口

| 接口 | 用法与边界 |
|---|---|
| `Scenario::start(ScenarioOptions)` | 名称限小写字母、数字、连字符；每次追加 UUID。同名场景也能并行。参数含真 daemon 路径、config.toml 内容、slice 内存上限、连接超时 |
| `Scenario::connect()` | 返回产品的 `nd_ui_core::SyncReplica`；命令、快照、订阅、分页与收据均走 nd-wire |
| `endpoint().enqueue(Route, ModelReply)` | 先编排后发请求；同 route 按接收顺序逐个取回复。文本用 `text`，工具用 `tool(id,name,input)` |
| `endpoint().enqueue_held(...)` | 返回一次性 `ResponseGate`；`release()` 放行整条应答，可以在请求前放行；丢弃 gate 或关闭端点使等待请求以 503 结束 |
| `endpoint().requests()/count()/wait_for_requests()` | 观察已收到的请求，包括扣住、取消、未编排的请求。记录实际正文、route 和递增 id，不记录认证头；等待有显式超时 |
| `Scenario::fifo(name)` | 创建真实 FIFO；`sandbox_path()` 供 Bash 使用，`release(text)` 非阻塞写入一行，最多 4095 字节。保留 FIFO 句柄到任务结束 |
| `Scenario::spawn(name, Program)` | 同 slice 内启动独立真实服务；只读挂入选定程序，参数按数组传递，不拼 shell。相同场景内进程名不可复用 |
| `Program::claude()` | 固定 CLI 副本、固定假 API key、隔离内 `127.0.0.1:8765`。没有继承 owner 的 `BUN_OPTIONS` 或认证环境 |
| `Program::new(path)` | 用于真实 Bash、探针或后续真实看守；只带基础隔离环境。输入为 `/dev/null`，stdout/stderr 由 `Process` 读取 |
| `Process::{wait,wait_for_stdout,kill}` | 有界等待退出/输出；kill 对整个进程服务发 SIGKILL。kill 会连输出监督进程一起杀掉，此后不能依赖 `.exit` 文件，应由 nd-wire/看守观察恢复结果 |
| `Scenario::{kill_daemon,restart_daemon}` | 分别调用真实 `systemctl kill --signal=KILL` 与 `systemctl restart`；随后用 `connect()` 重新取同步副本 |
| `arm_command_fault(id, CommandFault)` | 复用 #4 的测试构建故障点：效果后、提交前、提交后崩溃，以及效果后 unavailable。文件先完整写出再原子发布；不覆盖未消费故障 |
| `command_fault_consumed()` | 在已 arm 的测试中核对故障确实被消费，避免未命中却判通过；生产 daemon 不含故障逻辑 |
| `Scenario::{root,units,limits,close}` | root 是宿主侧临时目录，隔离内固定 `/sandbox`；limits 实读内核 cgroup。close 返回清理错误，Drop 是失败/取消时的后备清理 |

`Route::new(None, model)` 匹配没有 `x-claude-code-agent-id` 的主对话，`Some(id)` 精确匹配该请求头。模型名也精确匹配，不做别名归一化。未知组合和用尽的计划返回 409，仍计数；不能用缺省成功掩盖多请求。子代理 id 由 CLI 决定，场景应从真实协议事件取得 id 后为该 route 编排，不能拿显示名猜 id。

端点支持 `POST /v1/messages`（允许 query string）、非流式 JSON 与 SSE 的 text/tool_use；usage 是固定夹具值，不用于额度或 token 准确性测试。其他 API 未实现。工具应答中的程序必须由场景显式安排，真实工具结果通过下一次真实 CLI 请求观察。

## 隔离与清理

每个场景使用 `nd-test-<名称>-<UUID>-*.service` 和独立 transient slice（默认 2 GiB，零 swap）。daemon 崩溃后自动重启，其他服务没有 `PartOf`/`BindsTo`，不会被 daemon 重启带走。正常退出、启动失败和 Rust unwind/取消都会停止整个场景 slice，清掉其中所有后代，然后关闭端点和删除临时目录。成功路径应显式调用 `close()`，使清理错误进入测试结果。

每个服务使用独立的 `bwrap --unshare-all`，仅挂入 `/usr`、选定程序和本场景目录；HOME、CLAUDE_CONFIG_DIR、全部 XDG 目录都是临时目录，环境从空白构造。owner 的 home、会话、凭据、用户 D-Bus 和桌面 socket 不在沙箱里。systemd 操作由隔离外的运行器对自己创建的单元发起。

伪端点只监听场景 UDS；CLI 所在的独立网络命名空间有一个环回 TCP 代理，转到这个 UDS。场景不共享端口、目录或端点状态。两个同名场景用同一命令 id 写出不同结果的并行测试验证这一边界。

Rust 进程被 SIGKILL 或机器掉电时不能运行 Drop；这类中断后按失败日志/`Scenario::units()` 记录的**完整单元名**停止对应 slice，再删除对应临时目录。不要批量停止其他席位的 `nd-test-*` 单元。关闭一次场景不会删除 systemd journal；journal 是排障证据。

## 验证范围与后续接入

- [端点测试](tests/endpoint.rs)：route、真实 HTTP 请求记录、扣放不阻塞其他 agent、取消与关闭。
- [场景测试](tests/scenarios.rs)：真 daemon + SyncReplica 样例、同名并行、故障与重启隔离、cgroup 限额、断网/环境隔离、FIFO、启动失败及进程树清理；真 CLI 的 SSE 回答和 Bash 工具结果往返。
- 本单的 CLI 场景验证模型端点和运行器，不证明尚未实现的产品会话/看守/mod 集成。没有假 CLI、假看守、假 app-server 或 SQLite mock。
- 后续 #6/#11/#13 应将真实看守和适配器接入同一主接缝。当前 daemon 沙箱没有用户 D-Bus、额外程序挂载或 CLI 启动配置；这些能力随真实后端接入明确增加，不能通过暴露整个 host 文件系统来绕过隔离。
- N7/V6 归 #6；R10-E1/N5/E2b 归两个 mod 和 Claude 适配相关工单。本库的基础设施测试不替代那些产品待验。
- 依赖 CLI 的行为和失败退路见 [CLI 契约清单](../../docs/cli-contracts.md)。本单不写 CLI 原生存储，没有新增会话结构或派发操作。
