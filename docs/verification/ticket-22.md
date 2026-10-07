# #22 总结、`!` 模式、fork 型子代理与降级提示验收

日期：2026-10-06。状态：实现与自动验收完成；真模型走一遍与桌面上的目视核对留给 owner（见交付报告的 owner 清单）。

## 对外行为

- `session.compact{session,message,scope:"from"|"up_to"}`：在当前对话里还是 CLI 对话行的人类提示上「从这里总结」或「总结到这里」。动作 mod 发 `/compact ND_SUM <参数>`，钩子 mod 的压缩钩子按提示文字的 SHA-256 与次序定位，只把所选范围交给摘要器。成功后 `from` 把所选提示原文放回输入框、`up_to` 让输入框留空（被替换的旧稿另存）；定位不到（文字找不到、同一原文次数对不上）不压缩，收据 `rejected{anchor_gone, now.reason}`。
- `session.shell{session,command,input?}`：`!` 模式。动作 mod 调 Bash 跑这一条命令，再照终端 `!` 模式的格式把命令和输出追加进对话；本身不起回合，模型下一次请求读得到。只延续工作目录，不延续环境变量。回合进行中提交的等这一回合结束再跑。它引出的那一条 Bash 审批由 Claude 适配按规则自动放行（Bash、原文逐字一致、无 agent_id、插件发起），其余审批照常等界面。
- `session.subtask{session,prompt,input?}`：派 fork 型子代理，带父对话在后台跑，收据给子代理 id。
- 三条命令的收据时点是 Delivery：受理时只记意图，动作有结果才落收据（`Done`、`Rejected` 或 `Unknown`）；同 id 同内容的重试等同一个结果，不同内容回 `conflict`；协议入口另起任务等，同一连接照常处理别的请求。
- 只能聊天的降级进程：会话头 `degraded{why,unavailable}` 列出原因和用不了的功能（来自端口能力表 `process.features`）；三条命令回 `rejected{unsupported, now.why}`；桌面隐藏总结入口，`!` 与 `/subtask` 不发出、正文留在输入框。

接口与代码位置见[会话组件](../../crates/nd-session/README.md#总结-命令与-fork-型子代理22)、[Claude 适配](../../crates/nd-claude/README.md)、[两个 mod](../../mods/README.md)、[桌面](../../crates/nd-desktop/README.md)；CLI 依赖见[契约清单](../cli-contracts.md)的 #22 一节。

## 自动验收

主接缝在 `crates/nd-daemon/tests/invocations.rs`：同步副本对真守护进程，真 CLI 2.1.289、两个 mod、看守、systemd、SQLite，只换模型端点；两项另起私有 KWin 真窗口。

| 验收 | 测试与观察 |
|---|---|
| 「从这里总结」只压缩所选范围，原文回到输入框 | `summarize_from_here_compacts_only_the_selected_range_and_returns_the_prompt_to_the_draft`：两条相同原文里选第二条；摘要请求只含它及之后的内容；草稿变成该提示原文，原来写的草稿另存；下一请求里范围外原行仍在、所选范围换成摘要；已被总结的提示再选回 `precondition` |
| 「总结到这里」草稿留空 | `summarize_up_to_here_compacts_what_came_before_and_leaves_the_draft_empty`：摘要请求只含之前的两条；草稿为空、旧稿另存；下一请求只含所选提示之后的原行与摘要；第一条提示上选「总结到这里」回 `precondition` |
| 定位不到不压缩并写明原因 | `a_prompt_the_cli_no_longer_holds_is_not_compacted_and_the_reason_is_shown`：CLI 自己整段 `/compact` 之后选旧提示，回 `anchor_gone`「找不到」；再发同一原文后选新的一条，次数对不上同样拒绝；两次都没有摘要请求、草稿不动 |
| `!` 输出进下一次请求、不起回合、只延续目录 | `bang_runs_one_command_without_a_turn_and_the_next_request_reads_its_output`：`cd sub && export …` 要审批，自动放行；第二条读到 `sub` 而变量为空、退出码 3；两条 `!` 期间零模型请求；下一请求含 `<bash-input>` 行与输出；回合进行中的 `!` 等回合结束才跑；模型自己要跑同一原文的 Bash 不被放行、停在待答；真窗口里经产品输入框提交 `!echo NATIVE_BANG`，跑完、输入框被清空 |
| `!` 跨守护进程重启 | `a_bang_running_across_a_daemon_restart_settles_once_from_the_mod_result`：命令阻塞在 FIFO 时 kill 守护进程；重启后按操作 id 问到「还在跑」，放行后收据 Done、命令只跑一次，同 id 重发回原收据 |
| `/subtask` 派 fork 型子代理 | `subtask_dispatches_a_fork_subagent_that_carries_the_parent_conversation`：收据带子代理 id；子代理的模型请求（带该 agent 头）含父对话标记与任务；后台任务表回到空 |
| 去掉钩子 mod 时列出用不了的功能 | `without_the_hook_mod_the_header_lists_what_cannot_be_used_and_chat_still_works`：钩子 mod 只留清单、不报到；会话头写明原因并列出八项功能；三条命令回 `unsupported`；真窗口里会话头显示提示、`!pwd` 不发出；聊天照常 |

窄接缝一（引擎崩溃矩阵，`crates/nd-session/tests/matrix.rs`）：总结 From 成功、`!` 交付不明、`/subtask` 成功三行，在提交前、提交后交出前、交出后结果入账前的每个提交点崩溃重开，断言原生步骤至多一次、草稿回填一次、旧稿另存一份、收据 Done/Unknown。`crates/nd-session/tests/engine.rs::a_pending_bang_answers_every_identical_retry_with_one_receipt` 断言等待中的重试与冲突。

纯计算直接测：`crates/nd-claude/tests/invoke.rs`（自动批准规则的正反例、压缩钩子看到的行文字、mod 结论到票结果的解释）、`crates/nd-view-model/tests/invocations.rs`（输入框里的 `!`/`/subtask`、降级提示与能力、总结入口资格、条目显示）。

录制回归：`crates/nd-claude/tests/invoke_recording.rs` 录真 CLI 的三种 mod 往返，默认套件 `recorded_invocations_replay_and_none_of_them_is_resendable` 回放 `crates/nd-claude/tests/fixtures/mod/claude/2.1.289/invocations.jsonl`。

变异检查（`/mnt/wd_external/nd-build/tmp/ticket-22/mutations/`）：压缩钩子忽略次序、From 回填空串、不记已总结、UpTo 回填原文、关掉自动批准、重启后不查「还在跑」，各自让对应场景失败。

## 待验证项

本单没有编号的待验证项（INDEX #22 ④）。逐条结论：

- 定位不到或重复原文次序对不上时拒绝压缩：成立，主方案（钩子按散列与次数定位，skip 后不压缩）。
- from 回原提示、up_to 空草稿：成立，按 #16 回填约定实现；回填以受理时的草稿版本为基准，之后改过的另存。
- `!` 本身不起回合、自动批准只放行这一条：成立。注意 2.1.289 终端自己的 `!` 在 `respondToBashCommands`（缺省开）时会接着请求模型，规格要求不起回合，按规格做。
- fork 型子代理带父上下文：成立（需 `CLAUDE_CODE_FORK_SUBAGENT=1`，启动模板已固定设置）。
- 移除 mod 后完整降级清单：成立，主接缝与真窗口都核对。
- 旧待验项 E14（总结收尾行改写成中性说明）：未做，采用设计退路——保留 CLI 原样写入的 `<command-name>/compact…`、`Compacted` 收尾行，界面不显示这些行；定位不到时 CLI 自己的「Not compacted · …」提示照常显示，另有本单的总结条目写明原因。

## 证明边界

离线伪端点不证明真模型的摘要质量与真服务行为；真窗口场景只证明产品输入框经 nd-wire 发出、界面算出的提示文字与拒绝，不证明输入法与界面延迟。钩子 mod 在运行中被替换成不带压缩钩子的版本时会整段压缩（见契约 SUM-NOHOOK），产品不在运行中替换 mod。长轮询拖慢下一条输入的观察记在契约清单 #22 节末，未在本单处理。
