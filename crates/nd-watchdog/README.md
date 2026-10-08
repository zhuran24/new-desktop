# 看守进程与看守托管

日期：2026-10-07。`nd-watchdog` 保管单个后端进程的输入输出；`nd-runs` 提供幂等拉起、身份核对和恢复；`nd-watchdog-proto` 定义独立传输与录制格式。实现遵循 ADR 0005、0013。进程不链接会话引擎、组件内核或 CLI 适配器。

## 拉起与恢复

`nd_runs::Watchdogs::new(Config)` 接收运行目录、看守二进制绝对路径、单元名前缀、slice、单元内存上限和可选沙箱启动前缀。调用方准备带内存上限的 slice；场景运行器使用独立 transient slice。看守是该 slice 中独立的 transient service，`Restart=no`、`KillMode=control-group`，默认单元内存上限 2 GiB、零 swap；不设置 `PartOf`、`BindsTo` 或孤儿时限。

| 接口 | 契约 |
|---|---|
| `launch(run, LaunchSpec)` | 相同 run 和相同规格返回同一身份；规格不同报冲突。并发调用通过文件锁串行化，不抢适配器的控制连接。规格已落盘但看守尚未开始时允许续做；`started` 标记之后绝不重新执行后端命令 |
| `recover()` | 启动时核对单元 cgroup、后端与看守的 PID/启动 ticks/boot id，以及 socket 对端 uid/pid；为存活看守取新控制连接，读取 hello 后交还，不确认流水 |
| `inspect()` | 返回 `Up`、`Gone`、`IdentityMismatch`；`Gone.reason` 为 `NeverLaunched`、`Exited` 或 `ProcGone`。观察失败、单元与身份不符都保留为 `IdentityMismatch`，不能用于释放租约 |
| `inspect_run(run)` / `inspect_run_async(run)` | 核对一个运行；异步版本把 systemctl 等阻塞检查移出执行器线程。紧凑 Gone 墓碑直接返回已核实的退出观察 |
| `collect_unused(read_references)` | 启动持共享文件锁，回收持独占锁；锁内先枚举目录，再调用回调读取持久引用，仅清理已核实 Gone 且无引用的运行。启动竞争时延后回收；保留紧凑幂等墓碑，防止同一 run 再次启动后端 |
| `link(run)` | 仅供适配器。取得唯一控制连接，新连接顶掉旧连接；已经受理的输入写入继续执行 |
| `records(run, after, limit)` | 只读本地流水，供结束后的恢复、诊断和录制使用；不是控制连接，也不推进确认点 |

运行目录和 socket 目录 0700，规格、日志和身份文件 0600。子进程环境从 `LaunchSpec.env` 白名单构造，并始终删除 `BUN_OPTIONS`。后台任务随后端留在看守单元的 cgroup。看守退出时 systemd 清理整个单元；看守自身死亡不重跑，后端意外退出也不自动续接。

后端退出后，最多用 100 ms 排空已可读取的管道，再记录退出状态并结束看守，让 systemd 清掉后代。无法确认读尽的尾部显式标 `LostLines`。守护进程是否在线、是否确认通常的消费水位，不是退出清理的前提。业务任务的停止证据仍须由后续独占登记核实单元清理结果。

守护进程的 `[watchdogs]` 配置表接受上述 `Config` 字段；省略此节就不开放运行诊断命名空间。配置由 `nd-config` 读取；运行中更换或移除此节会报告须重启，现有托管和适配器连接继续使用原配置。nd-wire 的 `Get("runs")` 提供身份、退出原因及 `tail=Available|Unknown`；界面无需访问看守 socket。守护进程启动、会话列表变化及每 30 秒补扫时，根据持久会话和待定 Open 任务的引用回收无引用的 Gone 运行目录及其独立溢出目录。活动运行和身份不明的运行不回收。

## 输入、流水与容量

内部协议是本机 UDS 上的 **4 字节大端长度 + JSON**，单帧上限为 `MAX_FRAME`（128 MiB）。hello 带 `version=1`、run、两份进程身份、流水高水位、已受理输入的高水位 `accepted`、已完整写入的输入序号 `written` 和退出状态。`accepted` 包含排队与正在写管道的输入，用于接回续号；只有 `written` 证明该输入已完整交付。旧看守省略 `accepted` 时，适配器由保留的 In 流水补齐；流水有 LostLines 则拒绝猜测续号。版本只加不改；当前只有 v1。

- `write{in_seq,line}`：单调的非零输入序号，单行上限为 `MAX_FRAME / 4`（32 MiB），先记流水再写 stdin。已写序号的重发只回 `written`；唯一写入任务不会因控制连接断开而取消。部分写失败不会回 `written`，后续相同或更高序号报交付不明。
- `attach{after,limit}`：有界拉取，最多 1000 条且按帧体积截页；按最后一条的 `end_seq` 续读。不推进消费水位。
- `ack{seq}`：仅由适配器在其持久提交完成后调用；不得超过流水高水位。Codex 多消费者取最小可确认水位，属于适配器责任。
- `finish{CloseStdin|Terminate|Kill}`：只用于结束动作。关闭输入或发送信号不被阻塞的 stdin 写入挡住。`release` 和丢弃连接都不结束后端。
- 连接发生传输错误或超时后必须重连；旧连接不能被继续使用，以免把迟到回应错配给下一请求。

流水是带 `seq,end_seq,ms,event` 的 JSONL，正常记录两端序号相同；连续 Gap 可以合成一个闭区间。stdout 每行原文保留，只有顶层 `type=stream_event` 被识别为可丢增量。stderr 按 4 KiB 块读取，总量默认限 1 MiB，之后留截断标记并继续排空管道。

默认软上限 **64 MiB**、硬上限 **256 MiB** 是待 V6 代表性负载测量的暂定值，均可通过 `LaunchSpec.limits` 调整。运行目录按约 256 KiB 分段，不 fsync；确认后删除完全被覆盖的段。

1. 达到软上限：仅将新的 stream_event 换成 `Gap{DeltaDropped}`，连续同类 Gap 合并；事实行、输入和退出记录继续保留。此 Gap 不使收尾变为 Unknown。
2. 达到硬上限：后续流水改写到指定溢出目录；默认 `$XDG_CACHE_HOME/new-desktop/spool-overflow/<run>`，未设 XDG 时用 `$HOME/.cache`。不丢事实行。
3. 实际写入失败、无法解码/超长 stdout 或无法读尽退出尾部：`Gap{LostLines}`，并将此 run 的 `tail` 保持为 `Unknown`。写失败时内存保留紧凑范围，尽力写独立 `lost.json`，不会停止排空 stdout。相关任务不得据缺失流水自然收尾。

`Stats` 记录 stdout 原始行/字节、stream_event 行/字节、保留字节、溢出字节、确认点和 LostLines 状态。软上限不是日志总大小的硬限制：必须保留的事实仍增长，到硬上限转存。

## 录制及验证

`nd_watchdog_proto::{write_fixture,read_fixture}` 将流水写为可移植 JSONL：首行包含 `type=watchdog_fixture`、`meta{format,capability,backend,version,scenario}`，以及 count、first_seq、last_seq；后续行是原始 Record。读取时核对声明范围、数量和序号连续性，缺失必须由 Gap 表示。按 `能力/后端/版本/场景` 组织目录，供后续适配器纯协议回放与 #44 逐行崩溃回放使用。录制不应在消费者确认之后才开始。

真实 2.1.289 Workflow 录制在 `../nd-watchdog-proto/tests/fixtures/watchdog/claude/2.1.289/long-workflow.jsonl`：六个顺序子代理、约 75 秒，场景范围截至 Workflow 成功完成，CLI 此时仍可继续输入。它证明录制和解码；容量定值仍待代表性负载测量，不能由该离线样本关闭 V6。

运行 `scripts/test-scenarios.sh` 验真实服务、PID 连续性、每秒 1000 行下各 50 次 SIGKILL/restart、输入去重、溢出、退出清理、N7 和 V6 离线样本。默认内存验证在 E 盘普通文件上制造可回收页缓存，断言 `memory.events.max > 0`、`memory.current <= memory.max`、`oom=oom_kill=0`。真实匿名内存 OOM 测试被 `#[ignore]`，会触发 KDE 桌面通知，只能显式手动跑。

手动命令和结果见 [验证记录](../../docs/verification/ticket-6.md)，CLI 行为与升级退路见 [CLI 契约清单](../../docs/cli-contracts.md)。
