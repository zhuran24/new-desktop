# 会话组件：名册与持久操作引擎

日期：2026-10-06。状态：#13 实现第 2 步的最小引擎、名册新建、发送台、对话投影、按需拉起与闲置回收。审批台、谱系、任务账本与转接、子代理、收场的未决处置、恢复闸门等由后续工单在同一引擎上补。

后端经 [`nd-backend`](../nd-backend/src/lib.rs) 的 `Backends`（真接缝是 `BackendAdapter`），跨会话的独占经 [`nd-claims`](../nd-claims/README.md) 的 `Exclusivity`，命令收据经 `nd-ledger`。守护进程负责组装（见[守护进程说明](../nd-daemon/README.md#会话与-claude-后端)）。

## 对外接口

| 接口 | 用法 |
|---|---|
| `Sessions::new(store, claims, backends, config)` | 建表、读回侧栏列表；独占登记一有变化就唤醒装载中的会话 |
| `Sessions::recover()` | 守护进程启动时装载有活进程、进行中操作或未结票的会话，交端口 `adopt` 对账 |
| `Sessions::execute(&Command)` | `session.create`、`session.send`；不是会话命令时返回 None |
| `Sessions::subscribe(&SessionId, since)` | `session/<id>` 流：快照或同纪元续上的事件，外加 `WatchGuard`（持有期间算「有人在看」） |
| `Sessions::listing()` | 侧栏列表与一次性的提示（`global` 流里 `sessions` 命名空间的条目） |
| `session_id_for(command_id)` | 新建会话的 id 由建它的命令 id 派生：同一条命令重试落在同一个会话上 |
| `projection::{project, Projection, Shown}` | 对话投影的最简版与增量版，见下文差分基准 |
| `scripted::ScriptedAdapter` | 窄接缝一的脚本化适配器（测试用，#32 扩展） |

### 命令

| 命令 | 参数 | 收据 |
|---|---|---|
| `session.create` | `cwd`（绝对路径）、`text`（首条消息）、可选 `model`、`permission_mode`、`backend`（目前只有 `claude`） | `accepted{op, stream:"session/<id>"}`；参数缺失 `invalid`，没有这种后端 `unsupported` |
| `session.send` | `session`、`text`、可选 `intent`（`fold` 默认、`after_turn`、`interrupting`） | `done{message:<命令 id>}` 只表示进了发送台；之后的代持、写出、落地看这条消息的条目。撤掉的会话回 `precondition` |

### 会话流的条目（命名空间 `session`）

每个条目的 `data.seq` 是第一次出现的先后，界面按它排；每个条目都带后备文字。

| id | kind | 内容 |
|---|---|---|
| `header` | `header` | `status`（`preparing` 准备中、`active`、`partial` 部分完成、`withdrawn` 已撤掉）、`note`、`irreversible`、`process{carrier,backend_session,run,alive,readiness,turn_running,drain}`、进行中的 `op` |
| `prompt/<消息 id>` | `prompt` | `text`、`intent`、`state`：`held` 代持、`waiting` 等独占、`pending` 已签票、`written` 已写出、`landed` 回显了原编号、`failed`、`unknown` 交付不明；`native` 是写出用的原生编号 |
| `block/<API 消息 id>:<块序号>`、`block/result:<工具调用 id>` | `text`、`thinking`、`tool_use`、`tool_result`、`other` | `text`、`complete`；流式增量期间 `complete:false`、文字累积，完整块到了整体替换 |
| `turn/<承载位>/<n>` | `turn` | 回合结束：`ok`、`subtype`、`error` |
| `op/<操作 id>` | `op` | `kind`、`phase`、`reason`、`irreversible` |
| `asked/<请求 id>` | `asked` | 后端在等回答的请求；第 2 步只显示，审批台在第 3 步 |

侧栏条目 `session/<id>`（命名空间 `sessions`，kind `session`）带 `status`、`created_by`、`cwd`、`model`、`process_alive`；撤掉的会话从列表消失，换成一条 `notice/<id>`（只提示一次，不落库）。

## 引擎

- **一个输入一个事务**：命令收据、批次的事实与检查点、发件箱、操作账、独占登记的放行、显示缓存同一个 `Store::write` 提交；提交之后才发事件、给端口确认（`committed`）、把新票交给端口（`act`），最后回命令。纯增量（`Live::Delta`）不开事务，只累加进内存里的进行中条目。
- **操作**：`OpSpec::run(&View, &mut Journal) -> Result<Value, Halt>` 是纯函数，不读时钟、不取随机数、不做 I/O；每个开事务的输入之后从头重跑所有进行中的操作，直到不再产生新步。原语：`act`（动作，第一次求值进操作账和发件箱）、`claim`/`bind`（独占登记放行与晚绑定）、`wait`（结论写进操作账）、`settle`（落定，至多一次）、`id`（由操作 id 与键派生的创建意图编号）。`.undo(..)` 登记补偿；不接补偿的就是不可逆。
- **收场**：`run` 返回 `Fail` 时先等在途的正向动作有结果，再按登记倒序执行「可能已施加」的动作的补偿；做过（或可能做过）不可逆动作的终态 `Partial`，否则 `Compensated`。交付不明而作者要确定结果（`.ok()`）的停在 `Unresolved`，处置入口归 #33。
- **另发尝试**：端口回 `Refused(Withheld)`（证明没写出）时，引擎在同一个键下另发一张票（尝试号加一）；发送台的消息同理，界面上仍是同一条。
- **写入代次**：会话执行器装载时把写入代次加一，每次提交核对，旧实例的提交得 `Fenced`。
- **写类动作自动经独占登记放行**：`Send` 在签票的同一事务里 `admit(Write)`；`Wait` 时不签票，消息标「等独占」，登记一有变化再问。
- **测试构建**：`EngineConfig::check_purity` 每次重跑跑两遍比对；`EngineConfig::faults` 在提交前、提交后交出发件前、交出后结果入账前三处模拟崩溃。

### 操作目录（第 2 步）

| 操作 | 由谁起 | 步骤 | 失败 |
|---|---|---|---|
| 新建（种子） | `session.create`，在新会话里跑 | `claim Open{预定的后端会话 id}` → `Open{Fresh}`（补偿 `End{Discard}`）→ `bind` → 首条 `Send`（不可逆）→ 回显落地 → 落定当前承载位、状态 `active` | 首条消息写出之前失败：补偿后撤掉会话，侧栏提示一次；首条消息可能已到后端：补偿后保留会话，标部分完成并列出「first（可能已做）」 |
| 按需拉起 | 发送台：当前承载位没有活进程 | `claim Open` → `Open{Resume}`（补偿 `End{Discard}`）→ `bind` | 代持的消息报「没能拉起后端进程」，不自动重发 |
| 闲置回收 | 定时检查：没有操作、没有回合、发送台空、没有未结票、后台任务确知已收尾（`Drain::Drained`）、没人在看，闲置到 `idle_reclaim`（默认 15 分钟） | `End{Graceful}` | 进程留着 |

三者都是结构操作：一个会话同时至多一个，进行中给对话的新输入代持，操作到终态后按到达次序放出。

## 引擎崩溃矩阵

`tests/matrix.rs` 对每个（操作，场景）先正常跑一遍，再在三处故障点的每一次提交上杀掉引擎（执行器停下、内存状态丢掉），换新的会话组件与独占登记实例重开、跑完；同 id 重发命令。断言：终态正确；每个原生步骤至多生效一次（脚本化适配器分层记录接纳、生效、重报）；没有留下进行中的操作；适配器里的活进程与会话头一致；每次重跑纯度双跑。

| 操作 | 场景 | 测试 | 终态 |
|---|---|---|---|
| 新建 | 拉起与首条消息都成功 | `create_reaches_active_or_is_compensated_at_every_commit_point` | 活动，首条消息恰好送达一次 |
| 新建 | 拉起失败（没做过不可逆步骤） | `a_create_that_cannot_start_is_withdrawn_at_every_commit_point` | 撤掉，没有原生步骤生效 |
| 新建 | 首条消息交付不明 | `a_create_whose_first_message_is_unknown_is_partial_at_every_commit_point` | 部分完成，列出「first」 |
| 闲置回收＋按需拉起 | 回收后发一条消息 | `idle_reclaim_then_on_demand_launch_deliver_the_held_message_once_at_every_commit_point` | 活动，代持的消息恰好送达一次，两次拉起各一次 |

#32 在这套矩阵上加三层记录的可重发类别、代持去处与租约结束等断言；后续结构操作按「新增操作就加（操作，场景）行」接入。

## 对话投影的差分基准

`projection::project` 是最简版：每次从头扫一遍全部输入，每个条目取最后一件完整的事，没有就把增量连起来。执行器用的 `Projection` 是增量版。两者在录制回放的输入（`nd-claude/tests/conversation.rs`，每个前缀都比）和随机输入（`tests/projection.rs` 性质测试）上结果相同；以后换更快的实现，用同一组输入对照最简版。

## 存储

`sessions` 表每会话一行：写入代次、状态、建它的命令 id、核心记录（JSON：会话元数据、承载位、进行中的操作账、未结的发件、发送台里没结论的消息）。结束了的进显示缓存 `session_items`，不再占核心。独占登记的状态在它自己的表里，同一个事务提交。

## 已知边界

- 守护进程重启后的恢复走同一条路：执行器装载、端口 `adopt` 对账（写过的等回显、没写过的证明没写出另发、进程不在了的报退出）。完整的恢复闸门、恢复期命令回「暂不可用」、流式中重启不重不漏由 #19 验收。
- 闲置计时在内存里，守护进程重启后重新计。
- 独占登记的 `Write` 放行按原因记账，消息越多登记的状态越大（#12 的结构，见其说明）。
- 装载过的会话执行器目前一直留着（每会话一个线程），还没有按需卸载。
