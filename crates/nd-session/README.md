# 会话组件：名册与持久操作引擎

日期：2026-10-06。状态：已实现第 2 步的最小引擎、名册新建、发送台、持久草稿与附件、对话投影、谱系、按需拉起、闲置回收、恢复闸门与用户重发，以及总结、`!` 命令、fork 型子代理这三种收据等动作有结果的命令和只能聊天的降级提示。审批台、任务账本与转接、子代理、结构操作入口、收场的未决处置由后续工单在同一引擎上补。

后端经 [`nd-backend`](../nd-backend/src/lib.rs) 的 `Backends`（真接缝是 `BackendAdapter`），跨会话的独占经 [`nd-claims`](../nd-claims/README.md) 的 `Exclusivity`，命令收据经 `nd-ledger`。守护进程负责组装（见[守护进程说明](../nd-daemon/README.md#会话与-claude-后端)）。

## 对外接口

| 接口 | 用法 |
|---|---|
| `Sessions::new(store, blobs, claims, backends, config)` | 建表、读回侧栏列表；独占登记一有变化就唤醒装载中的会话 |
| `Sessions::recover()` | 守护进程启动时装载有活进程、进行中操作或未结票的会话，交端口 `adopt` 对账 |
| `Sessions::execute(&Command)` | `session.create`、`session.send`、`session.draft.update`、`session.resend`、`session.withdraw`、`session.interrupt`、`session.configure`、`session.rename`、`session.shell`、`session.subtask`、`session.compact`；不是会话命令时返回 None。后三者的回应等动作有结果（`nd_session::delivery(name)` 判断） |
| `Sessions::subscribe(&SessionId, since)` | `session/<id>` 流：快照或同纪元续上的事件，外加 `WatchGuard`（持有期间算「有人在看」） |
| `Sessions::listing()` | 侧栏列表与一次性的提示（`global` 流里 `sessions` 命名空间的条目） |
| `session_id_for(command_id)` | 新建会话的 id 由建它的命令 id 派生：同一条命令重试落在同一个会话上 |
| `lineage::{Lineage, Event}` | 谱系的纯值 fold；段、承载区间、轮的原生位置、显式边、同步点、拓扑与共同前缀 |
| `projection::{project, Projection, Shown}` | 对话投影的最简版与增量版，见下文差分基准 |
| `scripted::ScriptedAdapter` | 窄接缝一的脚本化适配器（测试用，#32 扩展）；送达时报一条实际回合（一条消息一轮），`Act::Invoke` 的原生步骤记成 `invoke:<种类>:…` |

### 命令

| 命令 | 参数 | 收据 |
|---|---|---|
| `session.create` | `cwd`（绝对路径）、`text`（首条消息）、可选 `attachments`、`model`、`permission_mode`、`backend`（目前只有 `claude`） | `accepted{op, stream:"session/<id>"}`；参数缺失 `invalid`，没有这种后端 `unsupported` |
| `session.send` | `session`、`text`、可选 `attachments` 和 `intent`（`fold` 默认、`after_turn`、`interrupting`） | `done{message:<命令 id>}` 只表示进了发送台；之后的代持、写出、落地看这条消息的条目。撤掉的会话回 `precondition` |
| `session.draft.update` | `args:{session,text,attachments?}`；必须带 `expect:{draft_version}` | `done{draft,saved}`；版本相符替换当前稿，否则另存原文，`saved` 为另存稿 id。参数不合法回 `invalid` |
| `session.resend` | `session`、`message`（确认未送达的原消息 id） | `done{message:<命令 id>,draft}`；资格不满足回 `precondition`，不消费当前草稿 |
| `session.shell` | `nd_wire::ShellArgs{session,command,input?}`；可带 `expect.draft_version` | 收据等动作有结果：`done` 的 value 是 `nd_wire::Invoked{invoke,draft,exit,stdout,stderr,appended}`；见下文 |
| `session.subtask` | `nd_wire::SubtaskArgs{session,prompt,input?}`；可带 `expect.draft_version` | 同上，`Invoked{invoke,draft,agent}` |
| `session.compact` | `nd_wire::CompactArgs{session,message,scope:"from"\|"up_to"}` | 同上，`Invoked{invoke,draft,saved}`；定位不到回 `rejected{anchor_gone, now.reason}` |

### 持久草稿

每个已创建会话有一份 `Draft{version,text,attachments,device,saved[]}`，初始版本为 0。草稿及收据由会话执行器在同一 SQLite 事务落盘，经会话快照和事件公开；不放进设备偏好，不发模型请求。空字符串是合法编辑，用来主动清空。草稿不因切段而另建一份。

编辑须携带实际编辑基准的 `expect.draft_version`。相符时当前稿版本加一；不符时当前稿不变，把这次原文存成 `SavedDraft{id,base_version,text,attachments,device}`，id 等于编辑命令 id。冲突另存也是成功持久化，返回 `Receipt::Done`；它不是“无副作用的拒绝”。重试遵循原有收据契约，同 id 同内容返回原收据、不同内容回 `conflict`，不会再增加一份另存稿。

`session.send` 可带同一个 `expect.draft_version`。受理消息时只有版本、发送正文和附件均等于当前草稿才清空并加一版本，和发送台入账同事务；`Done.value` 除 `message` 外也返回该事务的 `draft`。版本过时仍受理明确发送的正文，但保留其他编辑。旧界面不带版本时不清稿。界面不得在发送后另发无条件空稿。

取回另存稿是带当前编辑基准版本的普通 `session.draft.update`，另存原文继续保留，暂不提供删除入口。已保存的草稿可在界面关闭、守护进程崩溃后从快照恢复；尚未确认持久化的编辑只存在活着的界面中，界面须显示保存状态并保留待确认命令的 id。

`state::Core::update_draft(id, device, base, text, attachments)` 是同 crate 内的回填接入点。#18 撤回已使用该入口；#22 总结、#35 回退处理完成事实时在同一执行器事务调用它；调用方须用原操作账或去重事实保证一次落定，并保存操作开始时的草稿版本，不能拿完成时的最新版冒充基准。并发编辑时回填文字另存。`From` 回填原文、`UpTo` 回填空串的具体规则由对应操作接入，不在本单模拟后端动作。当前稿、另存稿和比较/清空条件均包含 #17 的附件；附件引用随核心事务更新 Blobs 引用。

接口类型和四份 `protocol/draft*.schema.json` 均从 `nd-wire` Rust 类型生成。草稿存正文和附件引用；光标、选区和组词只留在界面。

### 撤回与停止（#18）

- `session.withdraw`：`args:{session,message,draft?:{version,text,attachments?}}`。可选 draft 是界面在操作开始时看到的草稿基准，不是无条件保存。`Done{withdrawal,message}` 只表示持久受理；`prompt.state=withdrawing/withdrawn` 与 `control/<命令 id>` 给最终结果。已不在发送台的消息回 `not_withdrawable`，重复在途撤回回 `withdrawing`。
- `session.interrupt`：`args:{session,queued?:"keep"|"cancel",draft?:{version,text,attachments?}}`，默认 keep。没有活进程回 `Done{idle:true}`；否则 `Done{control}` 是持久受理。`control.state=acknowledged` 只表示停止请求被确认，回合结束仍看 turn/header。Cancel 未获后端能力声明时拒绝；取消成功的消息按发送台到达顺序合成一份回填。
- 成功撤回将原 Send 票结为 `Refused::Withdrawn`，在同事务通过 `Core::update_draft` 回填。保存操作开始时的版本、正文、设备；并发编辑时以稳定 `return/<命令 id>` 另存，不能覆盖新稿。撤回 false 不改变原 Send；未知结果在确证前不回填，始终不自动重投。迟到的撤回错误不能覆盖已经确认的送达；迟到的成功证据可回填一次。确认取消的原 Unknown Send 结为 Withdrawn，迟到 Lost 不能重新赋予重发资格。
- 只要求本会话的来源追平后受理控制，不等待其他会话完成恢复；未知控制仅对账，不能执行到后续回合。在途草稿附件引用保留至控制结果确证，正文分页保留未决提示和控制。
- `header.interaction` 给三种发送意图、withdraw、interrupt、cancel_queued、interrupt_spares_background、immediate_preserves_mcp 和 rewind_menu。界面按这些中立能力呈现，不能解读 Claude 的私有字段。

### 会话流的条目（命名空间 `session`）

每个条目的 `data.seq` 是第一次出现的先后，界面按它排；每个条目都带后备文字。

| id | kind | 内容 |
|---|---|---|
| `header` | `header` | `status`（`preparing` 准备中、`active`、`partial` 部分完成、`withdrawn` 已撤掉）、`note`、`irreversible`、`process{carrier,backend_session,run,alive,readiness,turn_running,drain,features}`、`degraded`、进行中的 `op`、`recovering`。`features` 是端口的能力表（`{id,label,available,why}`），界面按它显示或隐藏总结、`!`、`/subtask`；进程只能聊天时 `degraded{why,unavailable}` 写明原因和用不了的功能名 |
| `draft` | `draft` | 当前草稿 `version,text,attachments,device` 和 `saved[]`；编辑控件使用此项，不作为已发对话显示 |
| `prompt/<消息 id>` | `prompt` | `text`、`attachments`、`intent`、`state`：`held` 代持、`waiting` 等独占、`pending` 已签票、`written` 已写出、`landed` 回显了原编号、`failed`、`unknown` 交付不明、`not_delivered` 已证实未送达、`resent` 已重发、`withdrawing` 撤回中、`withdrawn` 已撤回；`native` 是写出用的原生编号 |
| `block/<API 消息 id>:<块序号>`、`block/result:<工具调用 id>` | `text`、`thinking`、`tool_use`、`tool_result`、`other` | `text`、`complete`；流式增量期间 `complete:false`、文字累积，完整块到了整体替换 |
| `turn/<承载位>/<n>` | `turn` | 后端回合结束的诊断条目：`ok`、`subtype`、`error`；其中 `n` 是后端 result 的计数，导航使用下面的谱系轮索引 |
| `lineage` | `lineage` | `current` 当前段、`rounds` 当前段从 1 起的轮索引、`topology`、`segments`、`carriers`、`edges`、`switches`、`origin`、`summarized`（已被总结、不再是 CLI 对话行的提示消息 id）。轮含 `id`、`n`、`messages`、`positions`、`complete`、`last_assistant` |
| `op/<操作 id>` | `op` | `kind`、`phase`、`reason`、`irreversible` |
| `asked/<请求 id>` | `asked` | 后端在等回答的请求；第 2 步只显示，审批台在第 3 步。`!` 命令自己引出的那一条 Bash 审批由 Claude 适配按规则放行，不出现在这里 |
| `invoke/<命令 id>` | `shell`、`compact`、`subtask` | 收据等动作有结果的命令：`state`（`held` 代持、`waiting` 等独占、`waiting_turn` 等当前回合结束、`pending` 已签票、`running` 已交给动作 mod、`done`、`rejected`、`failed`、`unknown`）、`reason`，及各自的内容与结果：`command`/`exit`/`stdout`/`stderr`/`appended`；`scope`/`message`/`saved`；`prompt`/`agent` |

侧栏条目 `session/<id>`（命名空间 `sessions`，kind `session`）带 `status`、`created_by`、`cwd`、`model`、`process_alive`；撤掉的会话从列表消失，换成一条 `notice/<id>`（只提示一次，不落库）。

## 总结、`!` 命令与 fork 型子代理（#22）

三种命令的效果是一个至多一次、不可重发的后端动作（`Act::Invoke`），收据时点是 Delivery（规格「收据时点」）：

- **受理**：命令的事务里只记意图（核心的 `invokes`，含命令内容散列），不落收据；协议入口把这类命令放到单独的任务里等，同一连接照常处理别的请求。同 id 同内容的重试挂到同一个结果上，不同内容回 `conflict`。等的连接断了，界面按命令 id 查收据，查不到不重发正文。
- **签票**（发送台的另一半，`pump_invokes`）：结构操作中或没有当前承载位时代持；当前承载位没有活进程就起按需拉起；`!` 在回合进行中等这一回合结束（和终端一样）；能力表说做不了（只能聊天）就直接拒绝；其余经独占登记 `Write` 放行后签 `i:<命令 id>#<尝试号>` 的票。`Refused(Withheld)` 回到等待、另发尝试。
- **结果入账**：票的终结结果到了，在同一事务里落收据、更新 `invoke/<命令 id>` 条目、回给还在等的连接。`Unknown` 落 `Unknown{now:{invoke,stream}}` 收据，之后的澄清只改条目，收据不改。
- **总结的定位与回填**：只能选当前段里还是 CLI 对话行的人类提示（在轮索引里、没被总结过）。受理时按「原文与附件完全相同」数出它是第几次出现、共几次（`Anchor{text,attachments,nth,of}`），记下这次覆盖的提示（`from`：它和之后的；`up_to`：它之前的）与受理时的草稿版本。成功后这些提示记进 `summarized`，草稿按 #16 的回填约定在同一事务里改：`from` 放回所选提示的原文与附件，`up_to` 留空；以受理时的版本为基准，之后改过就把回填的原文另存（`up_to` 则不动），被替换掉的非空旧稿另存为 `<命令 id>/displaced`，不丢。一次只允许一个总结在途。
- **`!` 与 `/subtask` 的清稿**：带 `input`（输入框原文）且 `expect.draft_version` 对得上、当前稿正文等于 `input`、没有附件时，受理事务里清稿；结果收据里的 `draft` 是落定那一刻的当前稿。

## 谱系与轮导航

[`lineage.rs`](src/lineage.rs) 是会话执行器内的纯值计算，不另建组件或后端端口。`Lineage::fold(&Event)` 返回新状态，失败时原状态不变；`turns(segment)`、`segment(id)`、`topology()`、`common_prefix(a,b)`、`common_prefix_with(a,other,b)` 是只读计算。`fork(source_session,from,through,target)` 产生新会话的谱系，来源不变。共同前缀比较稳定轮身份，不比较文字、发送时间或消息次数。

- **段与承载区间**：段存从开头到末尾的完整轮路径。根段在 `Done::Opened` 后建立；后端进程回收或续接不另开段、不另开承载区间。`Branch` 的 `through` 含该轮，`None` 表示空前缀；回退创建并切到新段，清空保留旧段但新路径为空，外部续写创建旁支而不抢当前段。不能以进行中的轮为截点。
- **换后端与镜像**：`CarrierKnown` 记后端会话已存在，不改变有效区间；`SwitchBackend` 记录已落定的换后端，保持段 id，关闭旧区间并开启新的一段区间。`Binding::{from,to}` 是从 0 起的轮边界 `[from,to)`，`to=None` 是还在承载的区间。`Imported` 将原生位置追加到既有轮上，并在目标承载位记同步点，允许发生在换后端落定前；`InvalidateSync` 保留压缩、外部续写、版本不兼容、导入失败或不明的原因。操作调用方提供这些已经确认的事实，fold 不自行探测外部状态。
- **轮、消息和票分开**：`Landed` 只记消息 id、票、原生位置的关联，不开轮。`TurnObserved` 用适配器确认的回合身份及用户输入集合开轮或并入；同回合多条提示只有一轮，同一轮可以对应多个原生位置。工具结果、任务通知及没有人类提示落地证据的输入不开人类轮。新票可以使用新 UUID，消息 id 不必变化。重放和迟到的早期输出不重复计轮，也不把最终分叉锚点移回早期输出。
- **Claude 的实际位置**：`system/init.uuid` 是当前进程内的回合身份，端口加后端进程编号；`stream_event/message_start`、`assistant`、`result` 的 `user_message_uuids`（缺少数组时读单数 `user_message_uuid`）给实际提示集合。工具调用后的多次模型请求沿用同一回合。最后一条主对话 `assistant.uuid` 是 `last_assistant.native`，可交只读记录解析库定位；它不是 API 的 `message.id`。`result` 只使轮结束，不冒充 user 回显。缺少归属信息就不生成猜测的索引。
- **界面读取**：桌面、ndctl 和场景都经 `session/<id>` 的 `lineage` 条目读同一份索引。导航使用 `rounds[].n` 和稳定的 `id`，提示定位用 `messages`/`positions`；只有 `complete=true` 且有对应后端锚点的轮才可供后续分叉入口选择。拓扑只在守护进程计算。

谱系和显示缓存、发件结果、适配器检查点同事务保存。旧版核心缺少 `lineage` 时仍能读取；旧的 result 计数不能重建人类轮，旧历史不会被猜数补齐，完整历史导入需要后续导入操作提供真实位置。当前产品接入只消费新建、落地和实际回合事实；回退、切段、换后端、会话分叉、外部续写和同步点的纯函数已可用，实际结构操作由对应工单接入。同步点留在持久谱系里，当前导航条目不公开同步点的内部恢复状态。

测试见 [`tests/lineage.rs`](tests/lineage.rs)、[真 CLI 会话场景](../nd-daemon/tests/sessions.rs) 及 [#15 验证记录](../../docs/verification/ticket-15.md)。没有新增结构或派发操作，现有引擎崩溃矩阵继续覆盖新建、回收和拉起。

## 引擎

- **一个输入一个事务**：命令收据、批次的事实与检查点、发件箱、操作账、独占登记的放行、显示缓存同一个 `Store::write` 提交；提交之后才发事件、给端口确认（`committed`）、把新票交给端口（`act`），最后回命令。纯增量（`Live::Delta`）不开事务，先累加进内存里的进行中条目；后续批次推进检查点时，累计正文同事务写入显示缓存。
- **操作**：`OpSpec::run(&View, &mut Journal) -> Result<Value, Halt>` 是纯函数，不读时钟、不取随机数、不做 I/O；恢复闸门开放后，每个开事务的输入之后从头重跑所有进行中的操作，直到不再产生新步。原语：`act`（动作，第一次求值进操作账和发件箱）、`claim`/`bind`（独占登记放行与晚绑定）、`wait`（结论写进操作账）、`settle`（落定，至多一次）、`id`（由操作 id 与键派生的创建意图编号）。`.undo(..)` 登记补偿；不接补偿的就是不可逆。
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
| 发送＋草稿 | 同版本竞争、清稿、继续编辑、旧发送命令重试 | `draft_conflict_and_send_consumption_recover_at_every_commit_point` | 落败稿恰好一份，原生发送一次，后来编辑保留 |
| 发送＋用户重发 | 确认原消息未送达，带原附件显式重发 | `confirmed_loss_and_user_resend_are_atomic_at_every_commit_point` | 只生效一次，原消息已重发，新旧消息保留附件 |
| 新建＋发送 | 两次都带附件 | `attachment_create_and_send_recover_at_every_commit_point` | 原附件交给端口，消息各生效一次 |
| 新建 | 拉起失败且没有发送附件 | `attachment_creation_compensation_releases_unused_uploads_at_every_commit_point` | 未使用上传可被回收 |
| 发送＋附件草稿 | 附件冲突另存、匹配发送、后来编辑 | `attached_draft_conflict_and_send_consumption_recover_at_every_commit_point` | 附件与正文共同遵守版本、引用和消费规则 |
| 总结（从这里） | 拉起、发两条、改草稿、总结第二条 | `summarize_from_here_backfills_the_draft_once_at_every_commit_point` | 压缩恰好生效一次；草稿回填一次（版本只加一），旧稿恰好另存一份；收据 Done |
| `!` 命令 | 动作 mod 的结论丢了（交付不明） | `a_bang_whose_result_is_lost_is_unknown_and_never_rerun_at_every_commit_point` | 命令至多跑一次、不另发；收据 Unknown |
| `/subtask` | 派 fork 型子代理成功 | `a_subtask_dispatches_one_fork_subagent_at_every_commit_point` | 恰好派出一个；收据 Done 带子代理 id |

#32 在这套矩阵上加三层记录的可重发类别、代持去处与租约结束等断言；后续结构操作按「新增操作就加（操作，场景）行」接入。

## 对话投影的差分基准

`projection::project` 是最简版：每次从头扫一遍全部输入，每个条目取最后一件完整的事，没有就把增量连起来。执行器用的 `Projection` 是增量版。两者在录制回放的输入（`nd-claude/tests/conversation.rs`，每个前缀都比）和随机输入（`tests/projection.rs` 性质测试）上结果相同；以后换更快的实现，用同一组输入对照最简版。

## 存储

`sessions` 表每会话一行：写入代次、状态、建它的命令 id、核心记录（JSON：会话元数据、承载位、进行中的操作账、未结的发件、发送台里没结论的消息）。已结束的对话内容进显示缓存 `session_items`；谱系索引（含已结束轮及落地位置）随核心长期保留。独占登记的状态在它自己的表里，同一个事务提交。

## 已知边界

- 守护进程重启后的恢复走同一条路：执行器装载、端口 `adopt` 对账（写过的等回显、没写过的证明没写出另发、进程不在了的报退出）。恢复闸门、恢复期命令回「暂不可用」、流式中重启不重不漏见 [#19 验证](../../docs/verification/ticket-19.md)。
- 闲置计时在内存里，守护进程重启后重新计。
- 独占登记的 `Write` 放行按原因记账，消息越多登记的状态越大（#12 的结构，见其说明）。
- 装载过的会话执行器目前一直留着（每会话一个线程），还没有按需卸载。


#18 在同一崩溃矩阵追加三行：`withdraw/queued`（撤回并仅回填一次）、`interrupt/queued`（等待中控制仍受理、保留队列）、`interrupt/cancel-queue`（停止并仅回填一次）。另有控制窄接缝测试：Busy 不是终结、未知撤回不回填、迟到的撤回错误不覆盖送达。真 CLI/界面恢复证据见 [#18 验证记录](../../docs/verification/ticket-18.md)。


撤回保存文字与附件引用。操作在途用 `return/<会话>/<命令 id>` 保留基准草稿附件，完成事务再交给当前稿或 `draft-saved` owner，并释放在途引用；原提示的 message 引用仍保留。多条排队输入合回草稿时按到达顺序追加正文、去重相同附件；若合并后超过单条消息的附件上限，需在编辑器删减到限制内再发送，不能静默丢附件。
### 历史页与导航

`get { res:"session/<id>/items", page:{limit:60} }` 读取最近一页；`session/<id>` 是兼容别名。
`before` 传上页的 `next` 向前读，`after` 传 `newer` 向后读，`around` 传当前段的稳定轮 id 直接定位。
三者互斥，limit 为 1–100；每页按显示 seq 升序，边界不含游标条目，around 含目标轮的首条提示。
返回 `Page {items,next,newer,anchor,at}`：anchor 是目标提示的稳定条目 id；at 与会话事件流共用纪元和游标。
游标是不透明字符串，绑定会话和当前段，能跨守护进程重启使用；跨会话、切段或坏游标会失败。
查询只读显示缓存，不发模型请求，不落命令收据。不存在的轮报 `round_not_found`；有轮而无正文锚点报 `history_unavailable`。

冷快照及后续事件只维持最近 60 个正文条目、进行中条目的累积正文、header/draft/lineage，以及：

- `navigation`：`segment` 与 `rounds[{id,n,complete,anchor,preview}]`；每个谱系轮一格，合轮的多条提示共用一格。预览最多 160 个字符。
- `history`：`older` 为快照正文前一页游标；无更早正文时为 null。

`lineage.inactive_messages` 只列谱系明确归入非当前段的提示；轮归属尚不明的已送达提示仍显示。
被移出快照窗口的条目从副本移除，持久正文仍可取页。`lineage` 保留完整拓扑和原生位置，不能用后端 result 计数替代轮索引。
桌面保留一页阅读状态，历史请求在独立查询连接上进行；新输出不强制切回最新，快速跳转的旧回应不会覆盖新选择。

正文来源是会话显示缓存。外部会话导入和结构操作须由各自工单写入实际正文、原生位置与谱系；没有正文时明确不可用，不根据原生 UUID 或相同文字猜锚点。本接口没有新增读取或写入 CLI 原生存储的依赖。

## 恢复与用户重发

`FactBody::Recovered` 表示本承载位已追平且完成未结票对账；闸门内只 fold，不推进操作。全部来源完成后才做独占登记的身份确认和第一次扫描。新命令 `unavailable` 不留收据，已有收据照常可查。

`FactBody::Clarified` 只接受原 Unknown 票的确定送达或 Lost 结论，更新原消息，不修改命令收据、不重新运行已收场的操作。Unknown 票与签发者长期保留；不明不是无效票。

`session.resend {session, message}` 只受理已有 `not_delivered` 消息。正文、意图和附件从原消息取，纯附件消息也可重发；本次命令 id 是新消息 id。旧消息变为 `resent`、新消息及附件引用进发送台、收据同事务；不同命令 id 不能重复消费同一个原消息的资格。当前草稿始终保留，`expect.draft_version` 不参与重发。矩阵行 `confirmed_loss_and_user_resend_are_atomic_at_every_commit_point` 覆盖三个故障位置的全部提交点。


恢复控制以本会话来源追平为准：来源未 Recovered 时 interrupt/withdraw 也回无收据 unavailable；来源已追平即使其他会话或独占登记仍在恢复，也允许这两类控制。普通发送/草稿/重发继续使用全局写入闸门。PendingTicket.unknown 区分对账与执行：Unknown 控制不能重放；连续流水证实未写出时只澄清 Lost。迟到控制回应可经 Clarified 更新原结果，回填及引用只结算一次，未知期间保留基准附件引用。成功撤回不产生 session.resend 资格。
## 会话设置和标题

公共命令均由 `Sessions::execute` 路由，经同一收据账本、会话事务和后端发件箱：

```json
{"id":"model-change-1","device":"desktop","name":"session.configure","args":{"session":"s-…","setting":{"model":"opus"}},"expect":{"settings_revision":0}}
{"id":"rename-1","device":"desktop","name":"session.rename","args":{"session":"s-…","title":"新的标题"},"expect":{"title_revision":0}}
```

`LiveSetting` 一次一项：model、effort、permission_mode 或 ultracode。两条命令返回 Accepted，进展与终态在会话 op 条目中；同 id 同内容重试仍回原收据。修订号不匹配回 conflict；旧命令未带对应修订号时保留兼容入口，新界面必须带当前会话头的值。

Configure 等当前回合结束后应用，期间新消息代持；进程已回收则先续接。只有后端控制接受且回读成功才更新已应用值。权限和模型由元数据保留，effort 意图进入中立 Profile，ultracode 通过 OpenSpec.live_settings 按 CLI 规则恢复。当前模型不支持 effort 时仍保留用户选择，供续接后切回支持的模型。

会话头增加 `title/title_source/title_revision/settings_revision/settings/caps/pending_setting`。settings 只提供运行时 applied、模型目录和权限模式目录；不将 get_settings 的全部配置源广播给界面。caps 经过后端端口约束，Codex 的 ultracode 永远为 false。侧栏的 title 与会话头同一事务更新。

自动标题默认启用，可设 `[sessions] auto_title = false`。第一条至少 10 个字符的非斜杠人类提示完成后尝试一次；失败或 CLI 返回空标题保留首条摘要。自动标题操作不占结构槽位，不阻塞后续对话；手动标题优先。标题可能额外发一次当前模型请求。自定义标题走原生 rename，AI 标题用生成接口的返回值；没有直接写记录文件。

新增崩溃矩阵行均在 `tests/matrix.rs`：

| 操作、场景 | 断言 |
|---|---|
| configure-and-title/success | 改模型与手动标题在每个提交点恢复；原生步骤不重复生效 |
| configure-and-title/refused | 后端拒绝时保留旧模型、旧标题，操作收场 |
| title/generate | 自动生成与落定在每个提交点恢复，不重复生成 |
| title/fallback | 不支持生成时保留首条摘要，不自动再次请求 |
