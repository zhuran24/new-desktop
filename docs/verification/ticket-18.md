# #18 三种发送意图、撤回与 Esc

日期：2026-10-06。状态：实现与自动验收完成；真机输入法、性能和真模型验证为 OWNER_PENDING。

## 范围与基线

工作树 `ticket-18`、分支 `ticket/18`；已合入 `v1=9f1a69dac08484b8e910830b0c2d6c83100522c7`，包含 #14–#17、#19–#21。实现源码截至 `5f78dca`；恢复闸门、历史分页、会话设置与标题均已集成。固定 CLI 2.1.289 的 SHA-256 为 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`；Cargo.lock 为 `9c0b546d361109bc36e581453de57e5dc8229c75f7ea0e3447976a164d6e818b`。

主接缝是 SyncReplica/真实 GPUI 对真守护进程；CLI、两个 mod、看守、systemd、SQLite、FIFO Bash 和本地 MCP 工具实际运行，仅替换模型端点。窄接缝使用真会话组件/独占登记/SQLite，只替换 BackendAdapter；Esc 分派直接测纯视图计算。

## 实现与验收映射

| 验收 | 自动证据 | 结论 |
|---|---|---|
| 三种实际落点 | `nd-daemon/tests/sessions.rs::send_intents_land_at_the_requested_turn_boundary` | PASS：并入在工具边界吸收且同轮；本回合后到下一轮；打断再发不等前台 FIFO 放行即起新请求。核对实际模型内容和谱系轮归属 |
| 撤回回输入框 | `withdrawn_queued_text_returns_to_a_durable_draft_after_ui_disappears`、`withdrawing_a_started_message_never_restores_or_resends_it` | PASS：成功、重复、已开始 false；失败保留原 Send，成功只回填一次 |
| 撤回中杀界面 | `native_controls_send_withdraw_reopen_and_dispatch_escape` | PASS：CLI 在处理撤回前被测试故障点 SIGSTOP，界面看到 withdrawing 后 SIGKILL；恢复 CLI 后重开真实 GPUI，草稿恢复 |
| 撤回中重启守护进程 | `withdrawal_survives_daemon_restart_before_the_cli_reply` | PASS：后端 PID 不变，接回原控制票，草稿一份，无额外模型请求 |
| 并发稿与附件 | `a_concurrent_draft_edit_saves_the_withdrawal_as_an_alternative` | PASS：保留操作开始时的草稿版本；新设备编辑保留，撤回正文和附件另存。等待中旧稿附件被替换且 GC 已收掉对照孤儿 Blob，回填附件仍可读取；重启后也保留 |
| Esc 保后台 | `escape_preserves_background_bash_agent_and_workflow_until_their_results_arrive` | PASS：Bash、子代理、Workflow 三个独立场景；Esc 后仍 busy，放出 FIFO/模型响应后正常完成，结果通知到主模型 |
| ACK 与回合结束分开 | `escape_ends_the_turn_without_ending_the_backend` | PASS：请求 ACK 不修改回合状态；等待 result 后回合结束，后端仍活着 |
| 取消排队 | `explicit_stop_and_cancel_queue_restores_each_queued_message_once` | PASS：只对声明能力的 CLI 使用 cancel_queued；按实际 cancelled 列表恢复，消息保持稳定 id，顺序不变 |
| Esc 分派 | `nd-view-model/tests/chat.rs::escape_closes_panels_before_stopping_and_idle_double_escape_opens_rewind`；上述原生控制场景；已有 composer 组词/长按测试 | PASS：消息输入框、目录和标题输入框都先消费组词 Esc，之后依次面板、活动回合、空闲双 Esc；原生场景验证实际 #21 设置面板先收起。回退菜单可打开，执行回退属于 #32，界面标明尚不可用 |
| E2b | `e2b_send_now_moves_foreground_mcp_to_background_and_delivers_its_result` | PASS：真实前台 MCP 已运行且 FIFO 未释放，立即发送使其转后台；随后主回合继续、MCP 完成并投递，服务端未收到取消通知 |
| E2b 的 Agent 行为 | `auto_background_keeps_an_explicit_foreground_agent_completable` | PASS：当前固定构建/模板下显式 run_in_background:false 仍异步启动，主回合先继续，代理正常交付。未以源码里的两分钟分支冒充这个模板的实测延迟 |
| 崩溃与不明 | `nd-session/tests/matrix.rs` 的 withdraw/queued、interrupt/queued、interrupt/cancel-queue；`tests/controls.rs` 五项 | PASS：三个提交故障点逐提交重开；文字/附件回填一次；等待中控制可受理；Busy 保留同票重试；Unknown 未确证前不回填、不重投；迟到撤回错误不覆盖已送达 |
| 来源恢复期间的 Esc | `a_recovered_source_accepts_escape_while_another_session_is_still_recovering` | PASS：本会话来源追平即可停止，不被另一个会话恢复阻塞；未追平时不受理 |
| 未知控制与发送 | `an_uncertain_unwritten_interrupt_is_clarified_without_executing_it`、`confirmed_queue_cancellation_returns_an_unknown_send_and_revokes_resend`、`late_withdrawal_confirmation_restores_the_original_draft_once` | PASS：未知控制只对账；未写控制不执行；迟到确证可回填一次，已取消发送不再取得重发资格 |
| 分页与未决操作 | `nd-session/tests/history.rs::queued_messages_and_pending_controls_stay_visible_outside_the_body_window` | PASS：在途提示/控制始终保留在快照中，历史正文限窗不隐藏撤回入口 |

## 持久接口

- `session.send` 保留中立 `intent=fold|after_turn|interrupting`，文字和附件继续走 #17；保存草稿期间固定提交当刻的发送意图。真实落点由后端事实建立，不能按按钮选择猜轮。
- `session.withdraw{session,message,draft?:{version,text,attachments?}}` 的 Done 只表示持久受理。撤回自身有控制票；cancelled=true 才结原 Send 为 Refused::Withdrawn，并在同事务回填。false 不回填；Unknown 在得到确证前不回填。
- `session.interrupt{session,queued?:keep|cancel,draft?:...}` 默认 keep。控制 ACK 与后续 result 独立；Cancel 只在实际 system/init 声明 interrupt_cancel_queued_v1 后可用。未知/缺少取消列表不当作空队列。
- `header.interaction` 给界面中立能力；`control/<命令 id>` 给持久状态。普通输入队列上限 128，控制和确认不占该容量；唯一写入者优先控制，撤回/取消队列保留目标输入的因果前置关系。
- #16 的 `Core::update_draft` 是唯一回填入口。开始版本、正文、附件和设备随票持久化；并发编辑以 `return/<命令 id>` 另存。在途引用 owner 为 `return/<会话>/<命令 id>`，完成事务交给当前稿/另存稿 owner 并释放在途引用。原提示附件不删除。
- #19 的恢复闸门按本会话来源判定控制准入。未知控制只对账：检查点 `writes` 保留原生 UUID/request id 和输入序号，`controls` 关联停止/撤回回复，`settings_controls` 关联设置/标题回复，并兼读 #21 原有的 controls 标题检查点；不能把旧控制执行到新回合。确认取消后原未知 Send 结为 Withdrawn，迟到 Lost 不能重新开放重发。#20 正文分页始终保留在途提示和控制条目。
- #21 设置面板纳入 Esc 面板优先级；目录/标题输入框在 Kit 清除 marked range 前拦截 Esc，先取消组词，并抑制长按重复。
- 空闲双 Esc 的菜单仅提供位置列表；#32 接入执行回退与可回退锚点。没有伪造回退成功或发 rewind_conversation。

## E2b 判定与退路

主方案成立，正式启动模板固定启用 `CLAUDE_AUTO_BACKGROUND_TASKS=1`。受测 MCP 是真实 stdio 服务，其工具等待 FIFO；没有靠缩短自动后台化超时制造正例。无变量对照本轮也成功，不能把结果归因为该变量的唯一作用。

当前二进制中，开关读取 @215204979 的 `$o()` 包含 120000 ms 分支，Agent schema 的后台策略说明在 @215206055。但本模板实测显式 false 也提前异步启动，不能宣称已走两分钟路径。后台准入依旧由 CLI 判断，界面明确限定为“可后台化”的 MCP。

若以后钉版关卡不再通过 E2b，按规格撤掉该变量，能力表禁用保留前台 MCP，界面说明立即发送会中断前台 MCP；本版没有触发退路。R10-E1 采用 #11 的声明结论，并在本单重新验证三类后台任务保留。

## 验证记录

产物根 `/mnt/wd_external/nd-build/tmp/ticket-18/`，构建根 `/mnt/wd_external/nd-build/target/ticket-18/`。全部 Cargo build/test/clippy 在 6 jobs、独立 MemoryMax=12G/MemorySwapMax=0 scope 中运行。没有使用 /tmp 构建退路或主动触发 OOM。

- 最终源码默认工作区：199 passed、0 failed、1 ignored，`logs/workspace-integrated21.log`。忽略项是 #8 的真 CLI 分支/双压缩记录测试，需要显式现场运行；不属于 #18 的未测验收。
- 最终源码完整场景：132 passed、0 failed、1 ignored，`logs/scenarios-integrated21.log`；其中会话场景 62 项全过，含本单所有主接缝、#19 恢复闸门、#20 千轮历史和 #21 设置/标题。唯一忽略项是真 OOM，按 BUILD 禁止自动执行。
- 引擎矩阵：15 个测试全部通过（其中一个测试覆盖两行），包含在最终 workspace 日志中；#18 新增三行，撤回行含基准稿和返回消息的两份附件。控制窄接缝 5 passed，含 Unknown 原发送被确认取消后不能重发。
- 原生最终验收：`native-merge21/result.json` pass=true；`withdrawn-draft.png`、`rewind-menu.png` 已查看；`cleanup.json` 的 remaining 为空、temporary_root_removed=true。同目录先定向验收、再完整场景均通过；标题 preedit 的 Esc 不收设置面板、不停回合，下一次先收面板。
- `cargo fmt --all -- --check`、Clippy workspace/all-targets/scenarios（`-D warnings`）、nd-wire/nd-mod Schema 再生成无差异、`git diff --check` 均通过。日志为 `logs/clippy-integrated21.log`、`logs/schema-integrated21.log`。最终 release 构建通过，见 `logs/release-integrated21.log`；三个程序散列在 `release-hashes.txt`。
- 红绿日志保存在 `logs/red-*`、`green-*`。实际失败覆盖 Busy 错误结票、迟到错误覆盖已送达、全局恢复闸门阻挡已追平来源、未知控制无法澄清、分页隐藏未决条目、未知 Send 取消不回填。另外，合入 #21 后原生回归先复现设置面板 Esc 误停回合（red-16），补上标题组词接线缺口（red-17），定向及整套转绿。每项均先复现再修复。E2b 初次沿用旧报告预期的失败属于探索记录，结论以通过场景为准。
- 一次原生复跑失败来自旧协调标记被复用（`logs/native-delivery-final.log`）。驱动现清理自己拥有的阶段标记，相同目录两次复验通过；失败运行的单元和临时目录也已核对清理。共享原生驱动的 attachments/history 等参数已适配。
- 未修改 owner 的会话、登录、活动桌面、mod、Chrome 或代理。测试环境为空白 HOME/CLAUDE_CONFIG_DIR/XDG、bwrap 断网、独立 nd-test 单元与 slice；原生窗口在私有 KWin/D-Bus。

## owner_checklist

真实豆包/Rime 组词、候选位置、实际呈现 p95/空闲 CPU、真模型与 owner 选择的真实 MCP 服务仍为 OWNER_PENDING。可执行的独立实例准备、步骤和清理命令在不入库交付记录 `research/impl/tickets/18.md`。上述自动场景不替代这些真机验证。
