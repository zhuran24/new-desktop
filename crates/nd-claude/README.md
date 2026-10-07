# Claude 适配：拉起后端进程与 mod 通道

日期：2026-10-06。`nd-claude` 按规格「进程」的启动模板拉起 Claude Code 后端进程（交给看守进程托管），开 mod 通道等两个 mod 报到，再写 initialize 判定就绪。mod 协议类型在 [`nd-mod-proto`](../nd-mod-proto/src/lib.rs)，两个 mod 在仓库根的 [`mods/`](../../mods/README.md)。实现遵循 ADR 0004、0005、0011、0013。`ClaudeBackend` 是后端端口 `BackendAdapter`（[`nd-backend`](../nd-backend/src/lib.rs)）的 Claude 实现，由守护进程装配给会话组件；本 crate 不持有持久状态，检查点由会话执行器随批次提交。

## 接口

| 接口 | 契约 |
|---|---|
| `ClaudeConfig` | CLI 路径、两个 mod 目录、mod 通道 socket（后端进程按同一路径连）、基础环境、看守流水上限，以及 hello（默认 10 s）、长轮询（默认 25 s，须小于 mod 一侧 fetch 的 30 s 上限）、initialize 回应（默认 30 s）三个时限 |
| `launch_spec(config, run, open)` | 纯函数：启动模板，见下节 |
| `Claude::new(config, watchdogs)` | 在 socket 上开 mod 通道。同名旧 socket 会被替换；丢弃时只删自己建的 socket |
| `Claude::open(run, Open, InitOptions)` | 拉起并等就绪，返回 `ClaudeRun`。hello 超时不是失败，结果是只能聊天 |
| `Claude::adopt(run, session, previous_caps)` | 守护进程重启后接回仍在运行的后端进程：不重发 initialize，等两个 mod 重连、重报 hello |
| `ClaudeRun::write/read/wait_frame` | 持有看守连接，是唯一的 stdin 写入者；输入序号连续分配，`read` 从 initialize 回应之后开始 |
| `ClaudeRun::send/result/command/command_as` | 经 mod 通道发命令并等结论；`command_as` 可指定期望的后端会话 id 和 mod 代次 |
| `ClaudeRun::binding/wait_binding` | 当前后端会话 id、绑定代次、两个 mod 最近一次 hello |
| `ClaudeRun::recording`、`fixture::{write_fixture, read_fixture, replay}` | mod 往返录制与回放 |

`Ready` 含后端进程身份（看守报的 pid 与启动标识）、两个 hello（各自的后端会话 id、mod 代次、CLI 版本、报到原因）、`Caps` 和 initialize 回应的原文（开放解码，未知字段保留）。

## 启动模板

argv 逐项传入，不拼 shell：`--output-format stream-json --input-format stream-json --verbose --permission-prompt-tool stdio --replay-user-messages --include-partial-messages`，两个 `--plugin-dir`，`--settings <内联 JSON>`，新建 `--session-id <预定 id>`、续接 `--resume <id>`，可选 `--model`、`--permission-mode`。不带 `--await-initialize`，不传 `--setting-sources`（设置来源用 CLI 默认）。

`--settings` 的 JSON：`pluginConfigs.{new-desktop,new-desktop-actions}.options = {sock, run}`；`enabledPlugins` 把 `codex-direct@skills-dir`、`sendnow@skills-dir`、`cc-quota@skills-dir`、`ultracode-toggle@skills-dir` 设为 false。flag 层的 `enabledPlugins` 与用户设置按键合并：用户自己关掉的仍关着，别的 skills-dir mod 照常装载，旧 mod 的文件不动。

环境从 `ClaudeConfig.env` 构造（生产传守护进程继承的环境，测试从空白构造），去掉 `BUN_OPTIONS`（看守进程也会去掉），再固定加 `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`、`CLAUDE_CODE_FORK_SUBAGENT=1`、`CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true`、`CLAUDE_CODE_SDK_READS_SESSION_STATE=1`、`DISABLE_UPDATES=1`、`CLAUDE_CODE_PLUGIN_DIR_WATCH=0`。这几项不能被配置覆盖。另固定 `CLAUDE_AUTO_BACKGROUND_TASKS=1`：E2b 已用真实 FIFO MCP 验证立即发送时转后台、之后正常交付结果；当前模板下显式 `Agent(run_in_background:false)` 也按 CLI 的策略异步完成，不承诺阻塞或固定两分钟延迟。

## 就绪与降级

1. 登记 run 及期望的后端会话 id（预定或续接的 id），经看守托管拉起，取得看守连接。
2. 等两个 mod 的 hello，且 hello 里的后端会话 id 等于期望值。2.1.289 不等 stdin，约 200 ms 就装载 mod、触发 `session.start`。
3. 写 initialize（stdin 的第一行）：`perTaskStopAffordance:true`、`forwardSubagentText:true`、`supportedDialogKinds`（调用方给界面会画的种类，第 2 步为空）；`InitOptions.hook_agents` 只在两个 mod 都报到时声明。
4. 读看守流水直到这条 initialize 的成功回应。不等 `system/init`；能力按回应实际列出的为准。进程先退出或回应超时是拉起失败。

就绪＝initialize 已回应＋两个 hello＋会话 id 一致。任一 mod 没在时限内报到（或报的 id 不对）：拉起仍算成功，`Readiness::ChatOnly{why}`，`Caps.features` 里退役、Codex 子代理、给子代理发消息、总结、`!` 模式、fork 型子代理、设置行、转接任务操作全部 `Unsupported{why}`；`why` 写明哪个 mod、等了多久，供会话头显示。只能聊天的进程以后即使 mod 迟到也不升级。

`Caps.interrupt_spares_background` 表示 Esc（`interrupt`）不停后台子代理与 Workflow：声明了 `perTaskStopAffordance` 且 stdin 开着时成立（R10-E1 已实测）。

## mod 通道

两个 mod 发起、自动重连；本机 unix socket 上的 HTTP/1.1，只接受同 uid 的连接（对端 pid 核对属第 4 步 E19）。

| 端点 | 作用 |
|---|---|
| `POST /hello` | `Hello{proto, run, mod, mod_version, mod_gen, backend_session_id, cli_version, cause}` → `HelloReply{binding_epoch}` |
| `GET /next?run&mod&mod_gen&backend_session_id` | 长轮询，到时限回空；带的身份不是当前绑定就回 `rehello:true` |
| `POST /result/<op_id>` | `ResultPost{…, outcome}`：`done`、`failed`，或 `rejected`（`stale_session`、`stale_gen`、`unsupported`） |
| `POST /report` | `Report{report_id, body}` → `ReportAck{durable}`；本 crate 没有事实存储，如实回 `durable:false` |

命令是 `Command{op_id, expected_backend_session_id, expected_mod_gen, action}`。mod 执行前先核后端会话 id、再核代次，对不上就回 `rejected`，不执行。动作目前有 `ping` 与 `query{op_ids}`（按操作 id 查本代次的状态和结果）；可重发类别由 `Action::resend` 给出，后续动作按规格分类追加。

- **`/clear` 重绑**：钩子 mod 在 `classic.SessionStart(source=clear)` 里立即用新 id 重报 hello（cause `clear`）；通道见到握手后同一后端进程报来新 id，绑定代次加一，并唤醒动作 mod 挂着的长轮询让它重报（cause `rehello`）。两个 mod 每轮长轮询前也重读 id，作为兜底。钩子 mod 还把 `session.end` 作为报告交上来（`reason:"clear"`、旧 id）。
- **模块重载**：每次装载新生成 mod 代次；代次变了，旧代次留下的结果查不到。在途命令中可重发的按同一操作 id、新代次重新排队，不可重发的结论是 `Unknown`。
- **守护进程重启**：通道丢弃时挂着的长轮询立即结束；mod 每秒重试，连上新通道后被要求重报 hello（同一代次），之前的操作仍可 `query`。

## 录制与回放

通道把每条进出消息（hello、交付了命令或要求重报的长轮询、结果、报告、适配器发出的命令、握手结束）经纯状态机 `ModState` 归一化成事实，连同 Unix 毫秒时间戳录下。夹具是 JSONL：首行 `mod_fixture` 头（能力、后端、版本、场景、初始后端会话 id、条数），其后每行一条，`seq` 从 1 连续；缺行、乱序、截断读取时报错。空闲到时限的长轮询不录。`replay` 把事件按序喂给新的状态机，逐条比对事实。

已提交的真 CLI 夹具：[`tests/fixtures/mod/claude/2.1.289/clear-rebind.jsonl`](tests/fixtures/mod/claude/2.1.289/clear-rebind.jsonl)（两次报到、ping、`/clear` 重绑、旧 id 被拒），由 `clear_rebinds_both_mods_…` 场景在 `ND_TEST_EVIDENCE` 目录导出。

## 后端端口的 Claude 实现（#13）

`ClaudeBackend::new(claude, watchdogs, claims, generation, blobs, config)` 实现 `BackendAdapter`：

- **拉起**（`Act::Open`）：`Claude::open`（新建用预定的后端会话 id，续接用 `--resume`）；成功后向独占登记报 `Up`（看守报的进程身份、这一代守护进程的控制代次），立即重扫一次 CLI 注册表，让自己的进程按身份认作自有；然后起这个承载位的任务，交 `Opened`（能力表原样存进 `adopt`，供重启接回）和 `Tasks{Drained}`（新进程没有后台任务）。失败时把还活着的进程结束掉，等看守单元清理完、独占登记记下它离开，再交 `Failed`。
- **承载位任务**：持有看守连接，是唯一的 stdin 写入者。`Send` 写 `{type:"user", uuid:native_uuid(票), priority, origin:{kind:"human"}}`（并入→`next`，本回合后→`later`，打断→`now`）；看守流水里出现这一行的输入记录报 `Written`，CLI 回显同一 uuid 才报 `Landed`。写看守连接出错时换连接，按看守报的已写高水位判断：没写过就用同一输入序号重写（看守按序号去重），判断不了才交付不明。`End{Graceful|Finish}` 写控制请求 `end_session`，`Kill|Discard` 让看守杀进程。每 20 ms 读一页流水，经 `Conversation` 归一成事实交一批；批次带检查点 `{seq, convo}`（流水位置与状态机快照），会话提交后 `committed` 才给看守 ack。
- **退出**：读到退出记录时，写出了还没回显的票交 `Unknown`，`End` 的票交 `Ended`；等看守单元清理完、向独占登记报 `Gone` 之后才报 `Exited`，所以之后的按需拉起不会撞上旧租约。看守也不在了时读完磁盘上剩下的流水，单元确实没了才按退出（码不明）收。
- **重启接回**（`adopt`）：承载位的命令队列同步先占住，接回期间交来的票排着，任务起来后按序写出。进程还在：`Claude::adopt` 接回（不重发 initialize），从检查点续读，未结的发送在流水里有输入记录的等回显、没有的证明没写出（`Refused(Withheld)`，引擎另发）。进程不在：写过的交付不明、没写过的证明没写出，报 `Exited`。拉起中途被重启打断的票：从没拉起的证明没做，起了一半的结束掉后报失败。
- `record_dir` 设了时，把读到的每页看守流水先追加到 `<run>.jsonl` 再处理（ack 之后流水会被看守回收）。守护进程的 `claude.record` 打开它，场景测试据此做录制回归。

`convo::Conversation` 是对话的纯状态机：输入看守流水的记录，输出 `Convo` 事实——`Written`（我方 user 行的输入记录）、`Echo`（带原 uuid 的回显）、`TurnStarted`（主对话的 `system/init`）、`TurnEnded`（`result`，已核 `is_error`）、`Delta`（主对话 `stream_event` 的文字或思考增量，条目 id 是「API 消息 id:块序号」）、`Block`（完整块；工具结果是 `result:<工具调用 id>`）、`Lifecycle`、`Reply`、`Asked`、`Tasks`、`Exit`、`Gap`。子代理的帧不进主对话。收尾判据按 `task_started`、`task_notification`、`task_updated`、`background_tasks_changed` 记在跑的任务；流水丢过行或用过定时事项（`CronCreate`、`ScheduleWakeup`）时是 `Unknown`，不当成空。状态可序列化，随检查点提交，重启后接着用。

对话的录制：主接缝场景 `a_recorded_conversation_replays_through_the_adapter_state_machine` 录下真 CLI 的一段对话（流式文字、工具调用与结果、两个回合），以看守夹具格式存为 [`tests/fixtures/conversation/claude/2.1.289/stream-tool-two-turns.jsonl`](tests/fixtures/conversation/claude/2.1.289/stream-tool-two-turns.jsonl)，事实存为同名 `.facts.json`（设 `ND_RECORD_FIXTURE=<目录>` 运行该场景时重写）。默认套件 `tests/conversation.rs` 回放比对事实，并用改造的输入证明只有原 uuid 的回显算落地、对话投影的最简版与增量版在每个前缀上一致。

## 总结、`!` 模式、fork 型子代理（#22）

`Act::Invoke` 交到承载位的任务里，按 `invoke.rs` 的纯计算编码成给动作 mod 的命令，操作 id 是 `native_uuid(票)`（重启后能按它查）：

| `Invocation` | 动作 mod 做什么 | 结果 |
|---|---|---|
| `Compact{scope, anchor}` | `$.command.run({command:"compact", args:"ND_SUM "+SummarizeSpec})`；钩子 mod 的 `session.compact` 按参数里的文字散列和次序定位，只把所选范围交给摘要器 | 成功 `Done::Invoked(Compacted)`；定位不到 `Refused(AnchorGone{why})`；摘要失败 `Failed` |
| `Shell{command}` | `$.tool.call({tool:"Bash", command})`，再 `$.session.append` 照终端 `!` 模式的格式追加 `<bash-input>` 与输出 | `Done::Invoked(Shell{exit,stdout,stderr,appended})`；被拒 `Refused`；跑完之后出错或结果丢了 `Unknown` |
| `ForkAgent{prompt}` | `$.agent.spawn({subagentType:"fork", prompt})` | `Done::Invoked(Forked{agent})`；没派出 `Failed`/`Refused` |

- **能力**：拉起时把 `Caps` 换成端口的中立能力表（`Done::Opened.features`）；守护进程重启后接回时 mod 没回来、降为只能聊天的，报 `FactBody::CapsChanged`。能力表说做不了的 `Invoke` 回 `Rejected(Unsupported{why})`。
- **`!` 的自动批准**：派发 `Shell` 时记下命令原文；读流水见到 `can_use_tool`、Bash、`input.command` 与原文逐字一致、没有 `agent_id`、`tool_use_id` 以 `toolu_plugin_` 开头的请求，写一次 allow（不加持久规则）、清掉这条批准，不报 `Asked`。其余审批照常报给会话。规则是 `invoke::auto_approves`，审批台（#24）共用。
- **写后记账**：命令交给通道后报 `Written{ticket, native:操作 id}`，检查点里记着在途命令（含没用掉的批准）。结论由等结果的任务交回承载位任务，报终结结果。进程退出时在途的是 `Unknown`、排队没交出的是 `Withheld`。
- **重启接回**：引擎交来的未结 `Invoke` 票按操作 id 问动作 mod（`query`）：还在跑就过 0.5 s 再问，做完了取它留下的结论，问不到（mod 重载丢了）就是 `Unknown`，不重发。
- 录制回归：`tests/invoke_recording.rs` 录真 CLI 的三种往返，默认套件 `recorded_invocations_replay_and_none_of_them_is_resendable` 回放 `invocations.jsonl`。

## 测试

默认套件（`cargo test --workspace`）跑协议生成物一致性、录制回放与夹具完整性，不启动 CLI。场景测试用真守护进程场景（独立 slice、bwrap 断网、临时 HOME/`CLAUDE_CONFIG_DIR`/XDG）、真看守进程、钉住的 CLI 2.1.289 和仓库里的两个 mod，只有模型端点换成离线伪端点：

```bash
scripts/test-scenarios.sh            # 全部场景，含本 crate 的
scripts/test-scenarios.sh -- clear_rebinds   # 也可按测试名过滤
```

| 场景 | 证明 |
|---|---|
| `ready.rs` | 就绪顺序与 hello 会话 id（新建、续接）；模板的 argv/环境实测（读 `/proc`）；四个旧 mod 关掉、其余照常；hello 超时只能聊天 |
| `rebind.rs` | `/clear` 后两个 mod 重绑、旧 id 与旧代次被拒，录制回放一致 |
| `r10_e1.rs` | hello 之后才写的 initialize 声明仍生效，含不声明 `perTaskStopAffordance` 的对照组 |
| `restart.rs`、`reload.rs` | 适配器重启后 mod 重连并可按操作 id 查结果；模块重载换代次 |
| `mods.rs` | 两个 mod 过 `claude plugin validate`；`$` 跨文件传递被静态检查拒绝（N5 的边界） |

后端端口这一层的场景在守护进程的 `crates/nd-daemon/tests/sessions.rs`（经 nd-wire 驱动，见[会话组件说明](../nd-session/README.md)）。

CLI 依赖逐条登记在 [CLI 契约清单](../../docs/cli-contracts.md) 的「mod 与 Claude 后端进程（#11）」「第一条 Claude 对话（#13）」两节。


## 发送、撤回和停止回合

`Act::Send` 的中立 `Fold/AfterTurn/Interrupting` 映射到 user 的 `next/later/now`。回显与实际轮归属仍分别处理：活动模型尚无输出时被打断，可能缺少可确认的原生轮归属，不能猜轮。

`Act::Withdraw{send}` 用原 Send 票派生 UUID，发 `cancel_async_message`。`cancelled:true` 才结撤回成功；false 保留原 Send 的后续结果。`Act::Interrupt{queued:Keep}` 发普通 `interrupt`，ACK 只结控制票，后续 result 才结束回合。Cancel 必须先观察 `system/init.capabilities` 的 `interrupt_cancel_queued_v1`，再发 `cancel_queued:true`，仅返回明确列出的取消票；缺少列表记交付不明。

单一写入者优先处理控制和流水确认；普通输入最多 128 条，满时同步 Busy，由引擎保留同票重试。撤回和取消排队先完成目标输入的因果前置写出。检查点保存未答控制请求；恢复时已写请求只等重放回应，证实未写才派发。进程退出时已写未答记 Unknown，尚未写出的普通输入记 Withheld。没有把 ACK、空 success 或进程存活当作停止完成。


#19 集成后，检查点的 writes 覆盖用户输入和控制请求的实际输入序号，controls 只保留回应配对。对旧版不含 writes 的控制检查点按旧格式兼容；无法证明未写出时保持 Unknown。接回先追平并报 Recovered，写过的控制继续等原回应；已终结 Unknown 的票只对账，不重新执行。明确回应同时提供 Clarified，供引擎更新原 Unknown 的当前结论。
