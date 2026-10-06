# #18 三种发送意图、撤回与 Esc

日期：2026-10-06。状态：实现与集成完成，最终整套自动检查进行中；真机输入法、性能和真模型验证为 OWNER_PENDING。

## 范围与基线

工作树 `ticket-18`、分支 `ticket/18`；已合入 `v1=6491f921ce08b880fdb03cc200c7ec8b6f9b120c`，包含 #14、#15、#16、#17。实现提交 `0e4a5eb`，草稿集成 `6d559c6`，附件集成 `793b058`。固定 CLI 2.1.289 的 SHA-256 为 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`；Cargo.lock 为 `9c0b546d361109bc36e581453de57e5dc8229c75f7ea0e3447976a164d6e818b`。

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
| Esc 分派 | `nd-view-model/tests/chat.rs::escape_closes_panels_before_stopping_and_idle_double_escape_opens_rewind`；上述原生控制场景；已有 composer 组词/长按测试 | PASS：组词由 Composer 消费，之后依次面板、活动回合、空闲双 Esc。回退菜单可打开，执行回退属于 #32，界面标明尚不可用 |
| E2b | `e2b_send_now_moves_foreground_mcp_to_background_and_delivers_its_result` | PASS：真实前台 MCP 已运行且 FIFO 未释放，立即发送使其转后台；随后主回合继续、MCP 完成并投递，服务端未收到取消通知 |
| E2b 的 Agent 行为 | `auto_background_keeps_an_explicit_foreground_agent_completable` | PASS：当前固定构建/模板下显式 run_in_background:false 仍异步启动，主回合先继续，代理正常交付。未以源码里的两分钟分支冒充这个模板的实测延迟 |
| 崩溃与不明 | `nd-session/tests/matrix.rs` 的 withdraw/queued、interrupt/queued、interrupt/cancel-queue；`tests/controls.rs` 三项 | PASS：三个提交故障点逐提交重开；文字/附件回填一次；等待中控制可受理；Busy 保留同票重试；Unknown 不回填、不重投；迟到撤回错误不覆盖已送达 |

## 持久接口

- `session.send` 保留中立 `intent=fold|after_turn|interrupting`，文字和附件继续走 #17；保存草稿期间固定提交当刻的发送意图。真实落点由后端事实建立，不能按按钮选择猜轮。
- `session.withdraw{session,message,draft?:{version,text,attachments?}}` 的 Done 只表示持久受理。撤回自身有控制票；cancelled=true 才结原 Send 为 Refused::Withdrawn，并在同事务回填。false/Unknown 不回填。
- `session.interrupt{session,queued?:keep|cancel,draft?:...}` 默认 keep。控制 ACK 与后续 result 独立；Cancel 只在实际 system/init 声明 interrupt_cancel_queued_v1 后可用。未知/缺少取消列表不当作空队列。
- `header.interaction` 给界面中立能力；`control/<命令 id>` 给持久状态。普通输入队列上限 128，控制和确认不占该容量；唯一写入者优先控制，撤回/取消队列保留目标输入的因果前置关系。
- #16 的 `Core::update_draft` 是唯一回填入口。开始版本、正文、附件和设备随票持久化；并发编辑以 `return/<命令 id>` 另存。在途引用 owner 为 `return/<会话>/<命令 id>`，完成事务交给当前稿/另存稿 owner 并释放在途引用。原提示附件不删除。
- 空闲双 Esc 的菜单仅提供位置列表；#32 接入执行回退与可回退锚点。没有伪造回退成功或发 rewind_conversation。

## E2b 判定与退路

主方案成立，正式启动模板固定启用 `CLAUDE_AUTO_BACKGROUND_TASKS=1`。受测 MCP 是真实 stdio 服务，其工具等待 FIFO；没有靠缩短自动后台化超时制造正例。无变量对照本轮也成功，不能把结果归因为该变量的唯一作用。

当前二进制中，开关读取 @215204979 的 `$o()` 包含 120000 ms 分支，Agent schema 的后台策略说明在 @215206055。但本模板实测显式 false 也提前异步启动，不能宣称已走两分钟路径。后台准入依旧由 CLI 判断，界面明确限定为“可后台化”的 MCP。

若以后钉版关卡不再通过 E2b，按规格撤掉该变量，能力表禁用保留前台 MCP，界面说明立即发送会中断前台 MCP；本版没有触发退路。R10-E1 采用 #11 的声明结论，并在本单重新验证三类后台任务保留。

## 验证记录

产物根 `/mnt/wd_external/nd-build/tmp/ticket-18/`，构建根 `/mnt/wd_external/nd-build/target/ticket-18/`。全部 Cargo build/test/clippy 在 6 jobs、独立 MemoryMax=12G/MemorySwapMax=0 scope 中运行。没有使用 /tmp 构建退路或主动触发 OOM。

- 默认工作区：179 passed、0 failed、1 ignored（继承的 #8 手工现场项），`logs/workspace-final.log`。
- 完整场景、Clippy、release：最终汇总待填写。
- 引擎矩阵：11 passed，`logs/matrix-final.log`；#18 新增三行，撤回行含基准稿和返回消息的两份附件。
- 原生最终定向验收：`native-final-ok/result.json` pass=true；`withdrawn-draft.png`、`rewind-menu.png` 已查看；`cleanup.json` 的 remaining 为空、temporary_root_removed=true。
- 红绿日志保存在 logs/red-*、green-*。包含命令缺失、纯接口编译缺口、原生驱动缺失及实际行为失败。实际修复包括 Busy 被错误结票、迟到撤回错误覆盖已送达；E2b 初次沿用旧报告预期的失败保留为探索证据，最终结论来自通过场景。
- #16/#17 集成保留两单的全部自动测试。最终脚本修复了共享原生夹具新增 attachments 参数的调用适配；它不改变产品行为。
- 未修改 owner 的会话、登录、活动桌面、mod、Chrome 或代理。测试环境为空白 HOME/CLAUDE_CONFIG_DIR/XDG、bwrap 断网、独立 nd-test 单元与 slice；原生窗口在私有 KWin/D-Bus。

## owner_checklist

真实豆包/Rime 组词、候选位置、实际呈现 p95/空闲 CPU、真模型与 owner 选择的真实 MCP 服务仍为 OWNER_PENDING。可执行的独立实例准备、步骤和清理命令在不入库交付记录 `research/impl/tickets/18.md`。上述自动场景不替代这些真机验证。
