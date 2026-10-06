# #13 第一条 Claude 对话（ndctl）验证

日期：2026-10-06。实现包括后端端口骨架（`nd-backend`）、最小持久操作引擎与会话名册（`nd-session`）、Claude 适配的后端端口实现（`nd-claude` 的 `ClaudeBackend`、`Conversation`）、守护进程装配与 `ndctl new/send`。接口与边界见 [会话组件](../../crates/nd-session/README.md)、[Claude 适配](../../crates/nd-claude/README.md)、[守护进程](../../crates/nd-daemon/README.md#会话与-claude-后端)。

测试按父规格「测试决定」已定的接缝：主接缝（同步副本与 ndctl 对真守护进程，只换模型端点）、窄接缝一（引擎崩溃矩阵，只把 `BackendAdapter` 换成脚本化适配器）、纯计算直接测与录制回归。没有假 CLI、假看守或对数据库的 mock。

## 主接缝：`crates/nd-daemon/tests/sessions.rs`

真守护进程（独立 transient 服务与限额 slice、bwrap 断网、临时 HOME/`CLAUDE_CONFIG_DIR`/XDG）、真看守进程、钉住的 CLI 2.1.289、仓库里的两个 mod、真 SQLite；模型端点是离线伪端点。经 `scripts/test-scenarios.sh` 运行。

| 验收 | 测试 | 观察到的外部行为 |
|---|---|---|
| ndctl 新建会话并流式对话 | `ndctl_creates_a_session_and_streams_the_reply` | `ndctl new --follow` 首行收据 `accepted`、带 `session/<id>`；同一个文字条目先以 `complete:false` 出现且越来越长，最后整块替换成完整回答；模型请求含首条消息；`ndctl send` 第二轮的模型请求带着第一轮；`global` 列出会话 |
| 回显带原 uuid 才算落地 | `a_message_counts_as_landed_only_when_the_cli_echoes_its_uuid` | 后端进程 SIGSTOP 时发的消息停在 `written`（看守记下输入、零模型请求，1 秒后仍未落地）；SIGCONT 后 `landed`，`native` 不变且出现在 CLI 自己的记录文件里 |
| 创建失败：没做过不可逆步骤就撤掉 | `a_create_that_never_started_a_backend_is_withdrawn_with_one_notice` | 工作目录不存在：看守报 NeverLaunched，会话 `withdrawn`、从列表消失，`global` 恰好一条提示；零模型请求，看守托管里没有活进程 |
| 创建失败：做过的保留、标部分完成 | `a_create_whose_backend_died_after_the_first_message_was_written_is_partial` | 场景故障点让后端在首条消息写出前停住，消息 `written`；杀掉后端进程后会话 `partial`，`irreversible` 列出「first（可能已做）」，首条消息 `unknown`，列表里标部分完成 |
| 闲置回收与按需拉起 | `an_idle_backend_is_reclaimed_and_the_next_message_resumes_it` | 没人订阅时闲置到时限回收，看守报原后端进程 Gone；再发消息按需拉起新的后端进程，续接同一个后端会话，模型请求带着第一轮 |
| 闲置不等于可回收 | `a_backend_with_a_running_background_task_is_not_reclaimed` | 后台 Bash 阻塞在 FIFO 上：会话头 `drain:busy`，超过闲置时限 4 倍仍不回收；放行后 CLI 把结果交给模型，之后回收 |
| 后端进程意外退出后按需拉起 | `a_backend_that_died_is_relaunched_on_the_next_message` | 杀掉 CLI 后，看守记下退出、单元清理完，会话头报进程不在；下一条消息按需拉起新进程，续接同一后端会话、带着第一轮 |
| 守护进程重启后接着用 | `the_session_keeps_its_backend_process_across_a_daemon_restart` | SIGKILL 守护进程后自动重启，接回同一个后端进程（同 pid、同后端进程编号），重启后发的消息落地、得到回答 |
| 录制回归的来源 | `a_recorded_conversation_replays_through_the_adapter_state_machine` | 真对话（流式、工具调用与结果、两回合）的看守流水录成夹具，读回完整；回放的写出与回显一一对应且等于会话里落地消息的原生编号，完整块等于守护进程显示的内容 |

## 窄接缝一：`crates/nd-session/tests/`

真的会话组件、独占登记、名册与 SQLite，经名册命令与会话流观察；后端是脚本化适配器（分层记录接纳、原生步骤生效、重报）。每次重跑都双跑比对纯度。

| 行为 | 测试 |
|---|---|
| 首条消息回显才落定，列表标活动 | `a_created_session_becomes_active_once_its_first_message_lands` |
| 拉起失败撤掉、提示一次，撤掉后的消息回 `precondition` | `a_create_that_never_started_a_backend_is_withdrawn_and_noticed_once` |
| 首条消息交付不明：保留、部分完成，补偿弃置后端进程 | `a_create_whose_first_message_may_have_reached_the_backend_is_kept_as_partial` |
| 创建中的消息代持，落定后按到达次序写出 | `messages_sent_while_the_session_is_being_created_are_held_then_sent_in_order` |
| 闲置回收与按需拉起续接同一后端会话、换后端进程编号 | `an_idle_process_is_reclaimed_and_the_next_message_launches_it_again` |
| 任务在跑或任务表看不清不回收；有人在看不回收 | `a_process_with_busy_or_unknown_background_work_is_not_reclaimed`、`a_watched_session_is_not_reclaimed_until_nobody_is_watching` |

引擎崩溃矩阵（`tests/matrix.rs`）在提交前、提交后交出发件前、交出后结果入账前三处的每一次提交上杀引擎、换新实例重开跑完。各（操作，场景）覆盖的提交点数：新建成功 6、新建拉起失败 4、新建首条消息不明 8、闲置回收＋按需拉起 15（三处故障点各一遍）。断言终态、原生步骤至多一次、没有留下进行中的操作、活进程与会话头一致。把「证明没写出就另发」的那一处改掉后，矩阵里三个场景失败，证明它能抓到漏另发与重复执行。

## 纯计算与录制回归

| 内容 | 测试 |
|---|---|
| 录制的真对话回放得到与录制时相同的事实；每块的增量连起来等于完整块；两回合都成功；新进程的收尾判据是已收尾 | `crates/nd-claude/tests/conversation.rs`：`recorded_conversation_replays_to_the_committed_facts` |
| 回显换成别的 uuid、或回显那一行没了：这条消息没有回显；`result` 与 `started` 照样在，不算落地 | `only_an_echo_with_the_original_uuid_counts_as_landing` |
| 对话投影的差分基准：最简版与增量版在录制输入的每个前缀、以及随机输入上一致 | `simple_and_incremental_projections_agree_on_the_recorded_conversation`、`crates/nd-session/tests/projection.rs` |

## 待验证项

#13 没有编号的待验证项（INDEX #13：「无新增编号项，消费 #11 R10-E1 结论」）。本单用到的 CLI 行为均已在钉住的 2.1.289 上实测，逐条登记在 [CLI 契约清单](../cli-contracts.md#第一条-claude-对话13)：回显语义（CONV-ECHO）、流式增量与块序（CONV-STREAM）、回合边界（CONV-TURN）、后台任务的 stdout 事件（CONV-TASKS）、`end_session` 退出（CONV-END）、续接带历史（CONV-RESUME）、注册表的自有身份（CONV-REGISTRY）。发送意图的三种落点（CONV-PRIORITY）只做了映射，验收归 #18。

## 证明边界

- 证明了：经 nd-wire 的新建与流式对话、落地判据、创建失败的两种结果、闲置回收与按需拉起、有任务不回收、守护进程重启后接回同一后端进程；三个结构操作的引擎崩溃语义。
- 没有证明：真模型服务的行为（全程离线伪端点）；流式中重启守护进程不重不漏、恢复期命令回暂不可用（#19）；三种发送意图的实际落点与撤回、Esc（#18）；审批（第 3 步）；界面（#14）。
