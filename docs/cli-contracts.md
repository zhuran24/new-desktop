# CLI 与 Codex 契约清单

日期：2026-10-06。各工单引入的 CLI/Codex 行为在此各占一个独立条目。#5 的离线端点、#6 的 Workflow 流水和 #8 的只读记录契约已通过真 CLI 离线验收；#9 的转换契约区分格式回归与后续真实后端验证。

## 条目格式

规格「版本与升级」要求 New Desktop 用到的每一条 CLI 与 Codex 行为各写一条，升级关卡每次都跑这份清单。每条至少写清四件事：

| 栏 | 写什么 |
|---|---|
| 依赖 | 依赖的具体行为，含没公开的内部机制；写到能判断“还成立吗”的粒度 |
| 出处 | 适用的钉住版本及散列，规格行、研究报告或二进制内嵌 JS 的字节偏移，实测记录的位置 |
| 自动验证 | 复验这条的测试名（默认套件或场景套件）；只有静态证据的写明“未实测”及原因 |
| 不成立时的退路 | 规格给的退路；规格没定的写“建议”，不能写成已验收的恢复保证 |

条目可以带编号（如 `MOD-CLEAR`），供关卡报告和后续工单引用；写 CLI 原生存储的例外另带“写完 CLI 能读回、能续接”的往返测试。按工单分节追加，只加不改；某条因升级失效时在原条目下注明版本与处理结果。

## 离线场景端点与真 CLI（#5）

适用版本：固定 `/mnt/wd_external/nd-build/cli/claude-2.1.289`，SHA-256 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`。测试入口为 `scripts/test-scenarios.sh`，实现与范围见 [nd-testkit](../crates/nd-testkit/README.md)。

| 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|
| `ANTHROPIC_BASE_URL` 和假 key 可让固定 CLI 只访问本地 Messages 端点，SSE 回答产生成功 result | 父规格「测试决定／主接缝」；`research/round9/verify/mock_base.py` 的消息 SSE；本版本实跑 | `pinned_real_claude_receives_held_streaming_reply_without_network_or_credentials`：实际请求被扣住、放出后 result 为预定文字、总请求为 1 | 离线场景失败，修正对应版本的端点契约；不改连真服务、不读取 owner 认证、不换假 CLI |
| API 按正文 model 与 `x-claude-code-agent-id` 路由；缺 agent 头为主对话 | 2.1.289 二进制内嵌 JS 字节偏移 206526432（`J?.agentId` 设置头）；R12 §3.2；主对话已实跑 | `replies_route_by_agent_and_model_and_unplanned_requests_fail_closed` 在端点 HTTP 边界验证精确路由；真 CLI 场景验证主对话。真实子代理 id 的生产/时序仍由后续子代理场景验证 | 未知组合返回 409 并保留请求，不回缺省成功；升级若改请求头，先修夹具再恢复子代理场景 |
| SSE `tool_use` / `input_json_delta` 能触发真 Bash，结果出现在下一次模型请求 | `research/round9/verify/mock_base.py` 的工具 SSE；父规格 FIFO 控时约定；本版本实跑 | `real_claude_executes_scripted_tool_and_reports_fifo_result_to_endpoint`：真实 Bash 阻塞 FIFO，放行后实际 tool_result 带回指定文字 | 离线场景失败，保留 CLI 输出与请求检查契约；不直接伪造 tool_result 或代写 CLI 记录 |

本条目不写 CLI 原生存储、不新增产品结构/派发操作；命令故障点沿用 #4，后端操作的引擎崩溃矩阵随实际操作工单增加。以上验证全在断网临时环境中完成，不代表真模型服务或产品看守保活验证。

## 看守流水与 Workflow（#6）

版本及二进制指纹同 #5；默认复验入口 `scripts/test-scenarios.sh`。N7 的 systemd/高吞吐验证不依赖 CLI 格式，V6 使用同一固定 CLI 和离线端点。

| 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|
| 顶层 `type=stream_event` 是可从后续事实重建的流式增量；完整 assistant、result、system、控制回应不能丢 | 父规格「进程」L227；`research/impl/cli-protocol.md` 流式输出小节 | `soft_limit_marks_only_deltas_and_hard_limit_preserves_facts_in_overflow`；真实 V6 流水的主对话 stream_event 与 Workflow 事实原样录制 | 未识别类型全部作为事实保留；候选若改 stream_event 语义，禁用丢增量（提高软限）并修正分类；硬限仍转存，真损失仍报 LostLines |
| 双向 stdin 在回合后保持开放，可 initialize 后执行后台 Workflow，并从 stdout 得到 task_started/progress/notification | 父规格 L226、L231–234；`research/round9/VERIFY.md` E3；本版本 V6 实跑 | `v6_real_workflow_records_versioned_fixture_and_measures_stream_share`：六个真 Workflow 子代理实际请求、约 75 秒后完成，零真实模型请求，录制读回一致 | 该版本的场景关卡失败，继续用已钉版本；不伪造 CLI 任务结果、不把看守的 PID 存活当作 Workflow 成功 |
| Workflow agent 请求带实际 agent id；本版本 `sonnet` 解析为 `claude-sonnet-5-5`；子代理模型 SSE 不全部出现在看守 stdout | 固定 2.1.289 的实际请求与任务进度帧；录制 `crates/nd-watchdog-proto/tests/fixtures/watchdog/claude/2.1.289/long-workflow.jsonl` | V6 核对六个 agent 请求与完成通知，统计实际 stdout 的行数/字节/stream_event 比例，所有原始行保留 | 重新测量候选版本的实际路由与体积，调整软/硬限值；不能按模型端点 SSE 数量冒充看守流水吞吐，也不改变溢出策略 |

本单没有写 CLI 原生存储；输入/输出录制是看守自己的运行数据。没有新增会话结构或派发操作，因此引擎崩溃矩阵由 #13 及实际操作工单接入；本单真实故障测试覆盖看守的输入去重、守护进程重启和结束清理。N7/V6 的证据、默认无 OOM 验证与手动 OOM 结果见 [验证记录](verification/ticket-6.md)。

## Claude 记录解析（#8）

适用版本：Claude Code 2.1.289，二进制 SHA-256 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`。源码位置为此二进制内嵌 JavaScript 的字节偏移，不是仓库 Rust 行号。规格为 [#8](https://github.com/zhuran24/new-desktop/issues/8) 和 [#1](https://github.com/zhuran24/new-desktop/issues/1)，遵循 ADR 0004、0006、0013。

| 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|
| 主记录是 JSONL，user/assistant/system/attachment 带 UUID 和父指针；元数据不入对话链；重复 UUID 后写覆盖，未知字段保留 | `rbn` @211932344；既有 `research/round7/cc-import.md` §3.1–3.4 | `history.rs` 的原始记录、偏移、重复 UUID、错误信封案例 | 解析失败返回字节偏移；完整重读仍失败则标记录不可判定，不绕过登记继续写 |
| 主对话叶子受 `last-prompt`、explicit、后续对话、父链、时间戳影响；空 explicit 清空；无叶子标题不等于新对话 | `rbn` @211932344；`Okn` @211930929；`AN` @211874654、`Kut` @211874772；`FAr` @211958891 | `history.rs` 的真实回退前缀、标题/sidechain、清空、时钟倒序、并行结果选择、fork briefing | 叶子无法确定时不给独占登记一个猜出的 UUID；等待写完/扩大读取，仍不确定则不给续接放行 |
| 最新 compact 边界、摘要、保留尾段重接；`preservedMessages` 优先于旧 `preservedSegment`；旧上下文不能经分块恢复复活 | `dAr` @211872778；`cAr` @211874082；既有 `research/round4/switch-branch.md` §1、`research/round9/VERIFY.md` E4/E5 | 真 CLI 两次 `/compact` + `--resume` 的 `cli_live`；录制默认回归；旧段格式衍生输入 | 保留列表或父链不全返回 `BrokenCompaction`/`MissingParent`；完整重读仍失败则不导出/不放行续接，不伪造摘要或手写父指针 |
| 同 message.id 的回复分块、并行工具结果属于同一回复；旧 progress 桥接；有效链后有附件尾部 | `mAr` @211878079；`Kut` @211874772；`rbn` @211932344 | 分块/工具结果、旧 progress、附件尾部直接行为测试；这些边界案例是构造输入，非真模型证明 | 不清洗未知块；不按歧义工具 ID 猜关联。升级发现结构变化时停用相关转换/导出，保留原始记录以便修复 |
| 现场回归使用 stream-json initialize、rewind_conversation、export_conversation、/compact、--resume；只能模型端点被替换 | `research/impl/BUILD.md`；`research/round4/switch-branch.md` §1–2；真实夹具生成器 | `cargo test -p nd-claude-records --test cli_live -- --ignored`，真实 CLI 自己写文件，续接请求与解析摘要/保留回复对比 | 场景失败使测试失败；候选版本不通过这条关卡，继续钉住已验证版 |

测试文件都在 [nd-claude-records/tests](../crates/nd-claude-records/tests/)。默认回归与现场回归的区别、完整命令和分页契约见 [库说明](../crates/nd-claude-records/README.md)。

当前实现对破损文件比 CLI 更严格：不使用它的邻近时间戳猜父节点、忽略坏行或返回部分历史作为登记基线。错误必须由调用方处理，不能转成空历史/空叶子。库只做原生记录顺序和原始内容读取；模型请求的规范化与转换有各自契约。

本条目没有写 CLI 存储例外：产品库是纯读取，测试记录也由真实 CLI 经协议产生。`PinLeaf` 写入仍归后续 Claude 适配与独占凭据，不由此库实施。没有新增会话结构/派发操作，无需在引擎崩溃矩阵新增行。

## 对话转换（#9）

每条记录区分格式回归与真实后端行为验证。升级关卡应先跑引用的自动测试，再由对应适配器跑真实 CLI/app-server 场景；纯函数通过不能替代后者。

| ID | 依赖及范围 | 出处 | 本仓库自动测试 | 真实场景接入与失效退路 |
|---|---|---|---|---|
| CONVERT-CLAUDE-EXPORT | `session.messages({as:"api"})` 的有效消息为 role/content 块；包含压缩保留段、并行工具结果。不是最终 API 请求体 | `research/round9/VERIFY.md` E4，2.1.289 的 `e4-289-final/http.jsonl` L14/33/52/65；夹具来源散列见 `crates/nd-convert/tests/fixtures/sources.json` | `recorded_effective_exports_keep_preserved_compaction_and_parallel_results`；`a_complete_export_larger_than_the_mod_limit_is_not_truncated` | Claude Export 适配器（#11/#50/#51）验证物化与完整性；超过 4096 条或无 mod 用 #8 记录解析。不完整则拒绝转换，不能以空页冒充空历史 |
| CONVERT-CLAUDE-REPLAY | stdin 支持 user/assistant 回放；user 为 `shouldQuery:false/client_composed:true`；assistant 含 id/type/model/usage | `research/round9/VERIFY.md` E6；`research/round9/verify/host.py` 的 `replay`；ADR 0010 | `replay_envelopes_are_stable_complete_and_set_no_query_flags`、`recorded_codex_turn_replays_commands_and_file_changes_as_paired_history`；这里只断言输出帧，不声称本次运行了 CLI | #50/#51 主接缝验证回放期间零模型请求、不重跑工具、再次续接带全历史；不成立走同段重建，仍失败禁用方向，不能改写主记录文件 |
| CONVERT-CODEX-HISTORY | paginated `ThreadItem` 的 userMessage/agentMessage/reasoning/commandExecution/fileChange；其余原文保留；异步问题转普通对话 | 固定 0.160.0 录制 `research/round7/convert/results/x1.json`；`research/round3/codex-schema/typescript/v2/ThreadItem.ts`；父规格「产品规则」第 1 条 | `recorded_codex_turn_replays_commands_and_file_changes_as_paired_history`、`codex_async_questions_become_conversation_text`、未知条目和 diff 降级测试 | Codex 适配器读完分页、保留自己的注入账本；历史或调用结果缺失标不完整。未知格式保留为文字并报告，损坏配对拒绝，不伪造成功 |
| CONVERT-CODEX-INJECT | `thread/inject_items` 接受 Responses message 和内嵌图片 data URL；导入在回合之间进行 | ADR 0010；`research/round7/CONVERT.md` §2.1/2.3，固定源 `turn_processor.rs` | `user_and_assistant_text_convert_in_both_directions`、`inline_images_round_trip_without_reading_any_file_or_url`、工具降级测试 | #50/#51 在真 app-server 上验证已加载线程的注入、后续请求和重启读回；不确认则不推进目标同步点，弃置本次新目标后整段重建，失败禁用方向 |
| CONVERT-NATIVE-REASONING | 签名 thinking、redacted_thinking、Codex reasoning 仅保留给本方；不能伪装成另一家原生推理 | ADR 0010 Consequences；`research/round9/VERIFY.md` E6；父规格 E21 | `signed_thinking_is_kept_for_its_own_backend_and_never_sent_to_the_other`、`codex_reasoning_is_a_native_item_and_never_claude_thinking` | 本地伪签名、密文往返不证明真实 API 接受。E21 经 owner 同意做真服务验收；失败同段整段转换带损失，再失败禁用该方向 |
| CONVERT-PROFILE | Claude 权限模式和 Codex approvalPolicy/sandbox 是不同机制；effort 支持集合按模型动态提供 | ADR 0010/0017；`research/impl/cli-protocol.md` §7；固定 schema `v2/AskForApproval.ts`、`v2/SandboxMode.ts` | `tests/profile.rs`，实际产品映射表见 `crates/nd-convert/README.md` | #13/#21 对接设置与能力表；无法对应时使用目标默认值并报告，不能把 max 猜成 xhigh，不能把源模型名发往目标 |

本清单由 #9 引入转换相关行。没有新增写 CLI 存储的例外，没有会话结构或派发操作；操作崩溃矩阵由实际执行 `Open{Seeded}`/`Import` 的后续工单添加。

## mod 与 Claude 后端进程（#11）

适用版本：固定 `/mnt/wd_external/nd-build/cli/claude-2.1.289`，SHA-256 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`。场景测试都在 `crates/nd-claude/tests/`，经 `scripts/test-scenarios.sh` 运行：真看守进程、断网的 bwrap、临时 HOME/`CLAUDE_CONFIG_DIR`/XDG，只有模型端点换成离线伪端点。实现与时序见 [Claude 适配](../crates/nd-claude/README.md)、[两个 mod](../mods/README.md)。

| 编号 | 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|---|
| MOD-LAUNCH | 启动模板被接受：stream-json 双向、`--verbose`、`--permission-prompt-tool stdio`、`--replay-user-messages`、`--include-partial-messages`、两个 `--plugin-dir`、`--settings` 内联 JSON、`--session-id`/`--resume`；不带 `--await-initialize` 时也不等 stdin，约 200 ms 内装载 mod、触发 `session.start` | 规格「进程」L231–232；本版本实跑 | `ready.rs`：`pinned_cli_is_ready_after_both_mods_report_the_preset_session_before_initialize`、`backend_gets_the_spec_template_…`（读 `/proc/<pid>/cmdline`） | 关卡不放行该版本，继续钉旧版；参数被拒的按版本改模板 |
| MOD-ENV | 环境开关按名生效：`CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`、`CLAUDE_CODE_FORK_SUBAGENT=1`、`CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true`、`CLAUDE_CODE_SDK_READS_SESSION_STATE=1`、`DISABLE_UPDATES=1`（关自动更新，B289@202660933）、`CLAUDE_CODE_PLUGIN_DIR_WATCH=0`（三态布尔，显式 false 不监视，B289@206370606）；`BUN_OPTIONS` 去掉 | 规格 L234；`research/impl/cli-protocol.md` §1 | `backend_gets_the_spec_template_…` 读 `/proc/<pid>/environ`；基础环境故意带 `BUN_OPTIONS` 预加载不存在的脚本 | 开关改名或失效时关卡不放行；fork、检查点各自的功能验收归 #22 及后续 |
| MOD-DISABLE | `--settings` 的 `enabledPlugins` 把 `<名>@skills-dir` 设 false，只在本进程关掉四个旧 mod；与用户设置按键合并（用户关掉的仍关，其余 skills-dir mod 照常装载；原 U06） | ADR 0011；规格「两个 mod」 | `backend_gets_the_spec_template_…`：临时 `CLAUDE_CONFIG_DIR/skills/` 放同名小 mod 与对照 mod，按标记文件判断是否装载 | 建议：关卡不放行；不改 owner 的设置或 mod 文件 |
| MOD-OPTIONS | `pluginConfigs.<mod 名>.options` 传给 `register(on, options)`；值须在 plugin.json 的 `userConfig` 声明 | `research/impl/mod-api.md` §3.2 | 所有场景：hello 里的 `run` 来自 options，mod 能连上 options 给的 socket | 建议：关卡不放行；拿不到 options 的 mod 报不了到，进程按 MOD-HELLO 降级只能聊天 |
| MOD-STATIC（N5） | 静态检查：钩子须是文件顶层函数，可从本 mod 文件 `import`；`$` 只能传给同一文件的顶层函数、不能跨 import；不许 `import()`；manifest 带 `author` 无警告 | `claude plugin validate` 实测输出；`research/round9/mod-api-map-evidence/static-rules/` | `mods.rs`：`both_mods_pass_the_pinned_cli_static_check_…`（两个 mod 无警告通过，动作 mod 只挂 session.start）、`the_static_check_refuses_passing_dollar_across_an_import` | 规格 N5 退路：每个 mod 单文件分段，共享状态仍集中 |
| MOD-HELLO（R10-E1） | 先等两个 hello 再写的 initialize 是 CLI 认的第一次：`agents` 进子代理请求的系统提示，`perTaskStopAffordance` 让 Esc 不停后台代理（不声明则同一 Esc 把它停掉：`task_updated killed`、`task_notification stopped`），`forwardSubagentText` 把子代理文字转到 stdout；之后再来的 initialize 不改一次性设置 | 规格 L232、L737；S:L511、S:L993 | `r10_e1.rs` 两个场景（含对照组） | 规格退路：这个进程不派 Codex 子代理，Esc 写明会停后台任务，回退与换后端走降级 |
| MOD-DIALOG | `supportedDialogKinds` 与 agents、perTaskStopAffordance 同属第一次 initialize 的一次性设置（289 处理分支 B289@226802000 起依次应用这几项）；缺省按“画不了”处理 | S:L511；二进制静态读码 | 间接：`r10_e1.rs` 证明这次 initialize 是第一次。`request_user_dialog` 只在拒答回退等联网/特性开关路径出现，离线触发不了，未实测行为 | 第 2 步声明为空，不依赖它；界面加对话框种类时补实测，失败就不声明该种类 |
| MOD-SESSION-ID | `$.session.id()`：新建等于 `--session-id`，续接等于 `--resume` 的 id | 本版本实跑（原 U03 的前两种；分叉的指定 id 归后续工单） | `ready.rs`：新建与 `a_resumed_backend_is_ready_…` | 已实现：hello 的 id 对不上不算报到，进程只能聊天；建议关卡不放行 |
| MOD-CLEAR | `/clear`：`session.end`（`reason:"clear"`、旧 id，此时 `$.session.id()` 仍是旧 id）→ stdout `conversation_reset` → `classic.SessionStart`（`source:"clear"`、新 id，`$.session.id()` 已是新 id）；不再触发 `session.start`；之后 stdout 帧带新 id | 本版本实跑；`mod-api.md` §9（原 U03 的 clear 顺序） | `rebind.rs`：`clear_rebinds_both_mods_to_the_new_session_…`（长轮询 20 s 时 3 s 内完成重绑） | 退路已实现：两个 mod 每轮长轮询前重读 id，重绑最迟延到一个长轮询周期 |
| MOD-HTTP | `$.http.fetch` 经 `socketPath` 走 unix socket，HTTP/1.1 往返正常；守护进程不在时调用报错而不是挂住，mod 每秒重试。单次 fetch 的 30 s 上限来自 `mod-api.md` §5，本单没有重测，长轮询取 25 s 留出余量 | `mod-api.md` §5、§8 | 所有场景；`restart.rs`：通道丢弃后 mod 自动重连 | 上限变短就缩短长轮询；UDS 不可用则 mod 报不了到，按 MOD-HELLO 降级 |
| MOD-RELOAD | 模块文件变动后 `reload_plugins` 重载该模块：模块变量清零、`session.start` 再跑 | 本版本实跑（`research/round9/VERIFY.md` E1 是开着目录监视时的旧观察） | `reload.rs` | 退路已实现：代次不同的命令被拒，在途命令按可重发类别重排或记 Unknown |

本节不写 CLI 原生存储（mod 只用协议与 `$`，没有用 `$.store`），也没有新增会话结构或派发操作，不加引擎崩溃矩阵行。mod 往返的录制格式与回放见 Claude 适配说明；已提交 2.1.289 的 `clear-rebind` 录制，默认测试套件回放。
