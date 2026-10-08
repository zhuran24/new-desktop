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
| 同 message.id 的回复分块、并行工具结果属于同一回复；旧 progress 桥接；有效链后有附件尾部 | `mAr` @211878079；`Kut` @211874772；`rbn` @211932344 | `recovery.rs` 默认逐例比对 372 条独立参照 UUID 顺序与 1/2/7 条分页；本机提取差分覆盖原有 3525 例及新增 1163 例，方法见下文；旧 progress、叶子附件另有行为测试。构造输入不是模型续接证明 | 不清洗未知块；不按歧义工具 ID 猜关联。升级发现结构变化时停用相关转换/导出，保留原始记录以便修复 |
| 现场回归使用 stream-json initialize、rewind_conversation、export_conversation、/compact、--resume；只能模型端点被替换 | `research/impl/BUILD.md`；`research/round4/switch-branch.md` §1–2；真实夹具生成器 | `cargo test -p nd-claude-records --test cli_live -- --ignored`，真实 CLI 自己写文件，续接请求与解析摘要/保留回复对比 | 场景失败使测试失败；候选版本不通过这条关卡，继续钉住已验证版 |

测试文件都在 [nd-claude-records/tests](../crates/nd-claude-records/tests/)。默认回归与现场回归的区别、完整命令和分页契约见 [库说明](../crates/nd-claude-records/README.md)。

P-24 的恢复契约按消息 ID 归组；工具结果先按父节点关联，旧 parent 再用 source UUID 和无歧义工具 ID 关联。同消息的工具 ID 归最后分块，不同消息重复声明则保持歧义。已在选链上的结果也参与附件尾链恢复。尾链要求整条透明、无分叉，且原始 `isSidechain` 字段相同（缺省与 false 不同）；不额外比较 agent。恢复条目按有效文件位置进入窗口，metadata 子链紧跟其锚点；重叠窗口合并后输出，原始选链的相对顺序不变。

差分夹具 [recovery-orders.jsonl](../crates/nd-claude-records/tests/fixtures/recovery-orders.jsonl) 共 372 例、700775 字节，只含人工构造输入和 CLI 给出的期望 UUID 序列。覆盖连续 metadata、主链结果附件、system/isMeta 尾链、并行结果、旧 parent、侧链、跨消息 ID、原始侧链字段、重复工具 ID、区间重叠及文件顺序。默认测试不需要本机 CLI。期望值由参照生成，不从 Rust 实现更新。

升级时手动运行 `recovery_matches_locally_extracted_cli`（默认 `#[ignore]`，需要本机闭源二进制和完整外部语料）：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/review-fixes
export CARGO_BUILD_JOBS=6
export ND_CLAUDE_RECOVERY_CORPUS=/mnt/wd_external/nd-build/tmp/final-P24-c6e13a4-evidence/corpus.jsonl
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test -p nd-claude-records --test recovery --locked \
  recovery_matches_locally_extracted_cli -- --ignored --exact --nocapture
```

[recovery_oracle.py](../crates/nd-claude-records/tests/cli/recovery_oracle.py) 校验钉版 SHA-256，按已核对偏移和括号边界现场抽出 `mAr` 及其纯函数依赖，仅在 Node 子进程内执行；启用工具 ID 恢复开关，关闭遥测和诊断登记。脚本不运行 CLI、不读取配置或凭据，子进程清空继承环境；仓库不存储抽出的函数或它们的 JavaScript 改写。脚本输出提取函数的散列供留证，将完整外部语料与 [recovery_cases.py](../crates/nd-claude-records/tests/cli/recovery_cases.py) 的 1163 例分支定向输入一起送入参照，Rust 测试逐例核对完整历史和分页。新版必须先复核符号、依赖、开关及二进制散列，再更新抽取清单；抽取或比对失败不放行。该差分只证明记录恢复顺序，实际压缩和续接仍由 `cli_live` 验证。

当前实现对破损文件比 CLI 更严格：不使用它的邻近时间戳猜父节点、忽略坏行或返回部分历史作为登记基线。错误必须由调用方处理，不能转成空历史/空叶子。库只做原生记录顺序和原始内容读取；模型请求的规范化与转换有各自契约。

本条目没有写 CLI 存储例外：产品库是纯读取，真实录制由 CLI 经协议产生；差分夹具另以构造输入标识。`PinLeaf` 写入仍归后续 Claude 适配与独占凭据，不由此库实施。没有新增会话结构/派发操作，无需在引擎崩溃矩阵新增行。

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

## 独占登记（#12）

固定 2.1.289 与 SHA-256 同 #5。注册表解析在 `nd-claims/src/registry.rs`；公开接缝、隔离复现命令和后续边界见 [库说明](../crates/nd-claims/README.md)。

| 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|
| `sessions/<pid>.json` 带完整 sessionId、字符串 procStart、cwd、version、startedAt、pidDomain；PID 域是 `linux:<machine-id>:<pid namespace>` | 真实离线 `session.json`/`identity.json`；固定二进制 `fJo` @201874306；父规格「独占登记」 | `recovery_requires_identities_then_a_complete_scan_and_excludes_own_pid_before_holding`、`stale_local_pids_are_ignored_but_foreign_pid_namespaces_remain_unverified`；`scripts/test-claims-live.sh`：真 CLI 写注册表，Rust 登记读取真 `/proc`；自己被排除，第二条同 id CLI 被判冲突；默认测试在原始副本上造错 ticks、异 PID 域、旧启动纪元和无 pid | 缺身份不猜，`ExternalUnverified`；损坏或读失败 `Checking`，完整重扫成功后再放行 |
| `agents --json --all` 是数组，列表里的 pid 本身不构成启动身份 | 真实离线 `agents.json`；父规格「窄接缝二」 | `scripted_cli_lists_are_unverified_without_registry_identity_and_interest_drop_keeps_detection`、`an_unavailable_optional_cli_list_does_not_block_registry_recovery_or_writes`；`scripts/test-claims-live.sh` 的真实列表 probe | 列表只用于展示，失败只报告列表不可读；独占裁决始终依据注册表扫描与进程身份 |
| `jobs/<8 位 id>/state.json` 可引用完整会话 id 而没有 pid | 真实离线 `job-state.json`（blocked/login required）；生成器仅在隔离目录写入工作目录信任设置 | `real_cli_background_session_without_a_pid_blocks_even_when_no_one_is_listing_external_sessions`；`scripts/test-claims-live.sh` 生成并留存 state.json | 保守等待，不据无 pid 推断退出。完整版本/参数资格与接管退路 R12-X2 留 #64 |
| 外部目录扫描和进程身份确认不足以封闭“检查后才出现写者”的窗口 | 父规格单写者及常开检测；R12 §2.3.1/§2.3.3 | `a_second_process_with_the_same_session_id_blocks_even_an_already_admitted_write`、`unverified_entries_and_incomplete_registry_reads_never_mean_no_external_writer`、`detection_keeps_running_without_a_list_subscriber_and_stops_without_releasing_leases`；`scripts/test-claims-live.sh` | 后续发现后暂停；不增加每条传输路径各自的第二套准入，也不承诺 OS 文件锁 |

本单没有 CLI 存储写入例外，最后自有叶子只接受自有流水证据，文件检查使用 #8 的纯解析库。`CliCommands::stop` 仅定义并实现短 id 命令边界，本单不调用它实施接管；stop/退出/注册项消失的实际状态机与验证属于 #64。完整验证与 owner 边界见 [验证记录](verification/ticket-12.md)。

## mod 与 Claude 后端进程（#11）

适用版本：固定 `/mnt/wd_external/nd-build/cli/claude-2.1.289`，SHA-256 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`。场景测试都在 `crates/nd-claude/tests/`，经 `scripts/test-scenarios.sh` 运行：真看守进程、断网的 bwrap、临时 HOME/`CLAUDE_CONFIG_DIR`/XDG，只有模型端点换成离线伪端点。实现与时序见 [Claude 适配](../crates/nd-claude/README.md)、[两个 mod](../mods/README.md)。

| 编号 | 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|---|
| MOD-LAUNCH | 启动模板被接受：stream-json 双向、`--verbose`、`--permission-prompt-tool stdio`、`--replay-user-messages`、`--include-partial-messages`、两个 `--plugin-dir`、`--settings` 内联 JSON、`--session-id`/`--resume`；不带 `--await-initialize` 时也不等 stdin，约 200 ms 内装载 mod、触发 `session.start` | 规格「进程」L231–232；本版本实跑 | `ready.rs`：`pinned_cli_is_ready_after_both_mods_report_the_preset_session_before_initialize`、`backend_gets_the_spec_template_…`（读 `/proc/<pid>/cmdline`） | 关卡不放行该版本，继续钉旧版；参数被拒的按版本改模板 |
| LAUNCH-PROFILE | `--model` 接受别名和完整名；`--permission-mode` 在新建与 `--resume` 时传递；initialize 的 `current_permission_mode` 回读实际模式 | 固定 2.1.289，`launch.rs` 启动模板及 CLI initialize 回应；权限模式来自对应进程能力 | `backend_gets_the_spec_template_without_preload_and_with_the_four_old_mods_disabled` 核对两参数和回读；`settings_are_read_on_open_and_permissions_and_effort_survive_reclaim` 覆盖续接 | 参数或回读字段不兼容，升级关卡不放行 |
| MOD-ENV | 环境开关按名生效：`CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`、`CLAUDE_CODE_FORK_SUBAGENT=1`、`CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true`、`CLAUDE_CODE_SDK_READS_SESSION_STATE=1`、`DISABLE_UPDATES=1`（关自动更新，B289@202660933）、`CLAUDE_CODE_PLUGIN_DIR_WATCH=0`（三态布尔，显式 false 不监视，B289@206370606）；`BUN_OPTIONS` 去掉 | 规格 L234；`research/impl/cli-protocol.md` §1 | `backend_gets_the_spec_template_…` 读 `/proc/<pid>/environ`；基础环境故意带 `BUN_OPTIONS` 预加载不存在的脚本 | 开关改名或失效时关卡不放行；fork、检查点各自的功能验收归 #22 及后续 |
| MOD-DISABLE | `--settings` 的 `enabledPlugins` 把 `<名>@skills-dir` 设 false，只在本进程关掉四个旧 mod；与用户设置按键合并（用户关掉的仍关，其余 skills-dir mod 照常装载；原 U06） | ADR 0011；规格「两个 mod」 | `backend_gets_the_spec_template_…`：临时 `CLAUDE_CONFIG_DIR/skills/` 放同名小 mod 与对照 mod，按标记文件判断是否装载 | 建议：关卡不放行；不改 owner 的设置或 mod 文件 |
| MOD-OPTIONS | `pluginConfigs.<mod 名>.options` 传给 `register(on, options)`；值须在 plugin.json 的 `userConfig` 声明 | `research/impl/mod-api.md` §3.2 | 所有场景：hello 里的 `run` 来自 options，mod 能连上 options 给的 socket | 建议：关卡不放行；拿不到 options 的 mod 报不了到，进程按 MOD-HELLO 降级只能聊天 |
| MOD-STATIC（N5） | 静态检查：钩子须是文件顶层函数，可从本 mod 文件 `import`；`$` 只能传给同一文件的顶层函数、不能跨 import；不许 `import()`；manifest 带 `author` 无警告 | `claude plugin validate` 实测输出；`research/round9/mod-api-map-evidence/static-rules/` | `mods.rs`：`both_mods_pass_the_pinned_cli_static_check_…`（两个 mod 无警告通过，动作 mod 只挂 session.start）、`the_static_check_refuses_passing_dollar_across_an_import` | 规格 N5 退路：每个 mod 单文件分段，共享状态仍集中 |
| MOD-HELLO（R10-E1） | 先等两个 hello 再写的 initialize 是 CLI 认的第一次：`agents` 进子代理请求的系统提示，`perTaskStopAffordance` 让 Esc 不停后台代理（不声明则同一 Esc 把它停掉：`task_updated killed`、`task_notification stopped`），`forwardSubagentText` 把子代理文字转到 stdout；之后再来的 initialize 不改一次性设置 | 规格 L232、L737；S:L511、S:L993 | `r10_e1.rs` 两个场景（含对照组） | 规格退路：这个进程不派 Codex 子代理，Esc 写明会停后台任务，回退与换后端走降级 |
| MOD-DIALOG | `supportedDialogKinds` 与 agents、perTaskStopAffordance 同属第一次 initialize 的一次性设置（289 处理分支 B289@226802000 起依次应用这几项）；缺省按“画不了”处理 | S:L511；二进制静态读码 | 间接：`r10_e1.rs` 证明这次 initialize 是第一次。`request_user_dialog` 只在拒答回退等联网/特性开关路径出现，离线触发不了，未实测行为 | 第 2 步声明为空，不依赖它；界面加对话框种类时补实测，失败就不声明该种类 |
| MOD-SESSION-ID | `$.session.id()`：新建等于 `--session-id`，续接等于 `--resume` 的 id | 本版本实跑（原 U03 的前两种；分叉的指定 id 归后续工单） | `ready.rs`：新建与 `a_resumed_backend_is_ready_…` | 已实现：hello 的 id 对不上不算报到，进程只能聊天；建议关卡不放行 |
| MOD-CLEAR | `/clear`：`session.end`（`reason:"clear"`、旧 id，此时 `$.session.id()` 仍是旧 id）→ stdout `conversation_reset` → `classic.SessionStart`（`source:"clear"`、新 id，`$.session.id()` 已是新 id）；不再触发 `session.start`；之后 stdout 帧带新 id | 本版本实跑；`mod-api.md` §9（原 U03 的 clear 顺序） | `rebind.rs`：`clear_rebinds_both_mods_to_the_new_session_…`（长轮询 20 s 时 3 s 内完成重绑） | 退路已实现：两个 mod 每轮长轮询前重读 id，重绑最迟延到一个长轮询周期 |
| MOD-HTTP | `$.http.fetch` 经 `socketPath` 走 unix socket，HTTP/1.1 往返正常；守护进程不在时调用报错而不是挂住，mod 每秒重试。单次 fetch 的 30 s 上限来自 `mod-api.md` §5，本单没有重测，长轮询取 25 s 留出余量 | `mod-api.md` §5、§8 | 所有场景；`restart.rs`：通道丢弃后 mod 自动重连 | 上限变短就缩短长轮询；UDS 不可用则 mod 报不了到，按 MOD-HELLO 降级 |
| MOD-BUDGET | `$.clock.after(0, …)` 的后台轮询持续到模块重载；普通钩子 10 秒、`.catch` 1 秒，`$` 调用在途不计钩子预算；`session.end` 另受约 1.5 秒墙钟限制；`$.session.version()` 返回版本信息 | `research/impl/mod-api.md` §8、§9、§15 MOD-09，D289:4917–4952、10502–10504；两个 mod 的 channel/lifecycle | `mods_rebind_after_an_adapter_restart_and_earlier_operations_can_be_queried`；`clear_rebinds_both_mods_to_the_new_session_and_stale_commands_are_refused` 含 20 秒长轮询 | 报到失败按 MOD-HELLO 降级；版本读取或轮询失效不冒称能力可用 |
| MOD-RELOAD | 模块文件变动后 `reload_plugins` 重载该模块：模块变量清零、`session.start` 再跑 | 本版本实跑（`research/round9/VERIFY.md` E1 是开着目录监视时的旧观察） | `reload.rs` | 退路已实现：代次不同的命令被拒，在途命令按可重发类别重排或记 Unknown |

本节不写 CLI 原生存储（mod 只用协议与 `$`，没有用 `$.store`），也没有新增会话结构或派发操作，不加引擎崩溃矩阵行。mod 往返的录制格式与回放见 Claude 适配说明；已提交 2.1.289 的 `clear-rebind` 录制，默认测试套件回放。

## 第一条 Claude 对话（#13）

固定 2.1.289 与 SHA-256 同 #5。主接缝场景在 `crates/nd-daemon/tests/sessions.rs`（真守护进程、真 CLI 与两个 mod、看守、systemd、SQLite，只换模型端点），经 `scripts/test-scenarios.sh` 运行；录制回归在 `crates/nd-claude/tests/conversation.rs`，夹具 `crates/nd-claude/tests/fixtures/conversation/claude/2.1.289/stream-tool-two-turns.jsonl` 由主接缝场景 `a_recorded_conversation_replays_through_the_adapter_state_machine` 录下（设 `ND_RECORD_FIXTURE` 时重写）。实现见 [Claude 适配](../crates/nd-claude/README.md)、[会话组件](../crates/nd-session/README.md)。

| 编号 | 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|---|
| CONV-ECHO | `--replay-user-messages` 下，CLI 读到 stdin 的 user 行才回显 `type:user,isReplay:true,uuid:<原 uuid>`；回显在这条消息的模型请求之前；CLI 记录文件里这一行用同一个 uuid。写进管道、`command_lifecycle`、`result.user_message_uuid` 都不是回显 | 规格 L231；`research/impl/cli-protocol.md` §6（289 对照 `research/round9/verify/runs/e3-289-default/frames-A.jsonl:4,8`）；本版本实跑 | `a_message_counts_as_landed_only_when_the_cli_echoes_its_uuid`（后端停住时只到「已写出」，零模型请求；恢复后回显原 uuid，记录文件含它）；录制回归 `only_an_echo_with_the_original_uuid_counts_as_landing` | 只认回显：没有回显就停在「已写出」，进程退出则交付不明、不自动重发；关卡不放行改了回显语义的版本 |
| CONV-STREAM | `--include-partial-messages`：`stream_event` 的 `message_start.message.id` 给 API 消息 id，`content_block_delta.index` 与随后同一消息的 `assistant` 帧按块的先后一一对应（每个完整块一条 `assistant` 帧，在 `content_block_stop` 之前到）；子代理的帧带 `parent_tool_use_id` | 本版本录制（见夹具）；cli-protocol §4 | `ndctl_creates_a_session_and_streams_the_reply`（增量越来越长、最后整块替换、同一个条目）；`recorded_conversation_replays_to_the_committed_facts`（每块的增量连起来等于完整块） | 对不上时增量仍只是临时显示，完整块到了照样整体替换；关卡跑录制回归 |
| CONV-TURN | 主对话每个回合开头一条 `system/init`（无 `parent_tool_use_id`）；`result` 结束回合，`subtype:"success"` 且 `is_error` 不为真才算成功，`result` 不结任何一条消息；工具结果是不带 `isReplay` 的 user 帧，`content` 里 `tool_result{tool_use_id}` | P §5.1；cli-protocol §4；本版本录制 | `a_recorded_conversation_replays_through_the_adapter_state_machine`、录制回归（两个回合、工具调用与结果配对） | 回合状态只影响显示与闲置判断；看不清时按在跑处理，不回收 |
| CONV-TASKS | 后台 Bash 起来时 stdout 有 `system/task_started{task_id}`，结束有 `task_notification`；任务结束后 CLI 自己把结果交给主对话的模型（多一次模型请求） | 本版本实跑 | `a_backend_with_a_running_background_task_is_not_reclaimed`（有任务时会话头 `drain:busy`、闲置期满不回收；放行 FIFO 后模型收到结果、之后回收） | 任务表拿不到、流水丢过行、用过定时事项（`CronCreate`、`ScheduleWakeup`）时收尾判据是 Unknown，不回收；R10-E3 的钩子判据在第 4 步接 |
| CONV-TASKS-TABLE | `background_tasks_changed.tasks[].task_id` 是整表替换，空数组才表示无任务；`task_updated.patch.status` 的 completed/failed/killed/stopped 移除该 task_id | 真 CLI `nd-watchdog-proto/tests/fixtures/watchdog/claude/2.1.289/long-workflow.jsonl` 的 seq 12、34、35 | `recorded_task_tables_keep_busy_work_and_malformed_tables_cannot_claim_drained` 重放真实非空任务表并破坏 tasks 字段；`recorded_terminal_task_update_drains_without_a_table_or_notification` 屏蔽冗余空表、在通知前断言 recorded completed 的 Busy→Drained（含检查点恢复），另测终态 status 变体及非终态/无关 id；原主接缝后台任务不回收场景 | 字段缺失或任务 id 不可识别判 Unknown、不回收；终态集合变动须重跑关卡 |
| CONV-END | 控制请求 `end_session`：CLI 回成功后退出，看守记退出码、单元清理 | `case"end_session"` 分派 B289@216828319、处理 B289@227273232；本版本实跑 | `an_idle_backend_is_reclaimed_and_the_next_message_resumes_it`（回收后看守报 Gone） | 不退出时回收操作失败、进程留着；建议关卡不放行 |
| CONV-RESUME | 用新建时预定的后端会话 id `--resume` 拉起：同一个后端会话，stdout 不重放旧历史，下一次模型请求带着之前的全部对话 | P §6；本版本实跑 | `an_idle_backend_is_reclaimed_and_the_next_message_resumes_it`（第二次请求含第一轮的提示与回答） | 续接失败时按需拉起失败、代持的消息报失败；会话本身不撤 |
| CONV-REGISTRY | CLI 写进 `CLAUDE_CONFIG_DIR/sessions/<pid>.json` 的 pid、procStart、pidDomain 与看守报的身份一致，守护进程据此把自己的后端进程认作自有，不当同一后端会话的外部写入者 | #12 的注册表条目；本版本实跑 | 全部会话场景：每次发送都经独占登记的 `Write` 放行才写出 | 认不出时发送停在「等独占」，关卡不放行 |
| CONV-PRIORITY | 发送意图映射到 user 行的 `priority`：并入→`next`，本回合后→`later`，打断→`now` | P §5.2；cli-protocol §6 | 本单场景只用默认的并入；三种落点由 #18 验 | #18 定；不成立按规格退路 |

本单没有写 CLI 原生存储。新增的结构操作（新建、按需拉起、闲置回收）的崩溃矩阵行在 [会话组件说明](../crates/nd-session/README.md#引擎崩溃矩阵)。场景构建另有一个故障点：守护进程运行目录下的 `backend-fault.json`（`{"contains":"<标记>"}`）让含标记的那条消息写出前把后端进程停住（SIGSTOP），只在 `scenarios` 构建里读，用一次就删。

## 桌面新建前的模型目录（#14）

适用固定 CLI 2.1.289，二进制指纹同 #13。入口 `scripts/test-scenarios.sh`；实现位于 `nd-claude/src/models.rs`，协议与验证见 [桌面说明](../crates/nd-desktop/README.md)。

| 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|
| UI-MODELS：没有会话时，在目标工作目录运行 CLI，只 initialize 即可取得 `models[]` 的 value/displayName/description/disabled 及可选 unavailable_models；查询不产生模型请求 | `research/impl/cli-protocol.md` §initialize 回应（模型目录，S:L291/L517）；本版本真 CLI 实跑 | `new_session_models_come_from_the_backend_before_any_conversation`：取列表、零模型请求、零产品会话、拒绝无效目录/后端；`native_chat_window_creates_and_recovers_during_streaming_markdown` 用返回的 haiku 选项创建 | 查询失败时显示原因并禁用创建；不填硬编码模型列表。修正候选适配后重新跑关卡 |
| UI-MODEL-HELPER：同一固定启动模板加 `--no-session-persistence` 可用于短命目录查询；两个 hello 后 initialize，退出确认后报告 Gone 和撤回 mod 登记 | 父规格「进程／辅助进程」；CLI 2.1.289 的本次实际运行 | 同上模型目录场景；场景结束清理所有独立单元和临时根。未据此关闭第 7 步 R12-X3/X6 的通用辅助进程问题 | 禁用目录查询并显示错误；不换用 owner 包装 CLI，不降成直接读取凭据或猜模型 |

本单没有写 CLI 原生存储，没有新增结构/派发操作；辅助查询不发送人类提示。`haiku` 在此固定版本的离线场景解析为 `claude-haiku-4-5-20251001`，这是伪端点的版本夹具，产品不硬编码该映射。界面流式恢复依赖沿用 CONV-STREAM，不另造一套 CLI 解码。

## 谱系与轮索引（#15）

固定 Claude Code 2.1.289 与 SHA-256 同 #5。规格依据为 `GLOSSARY.md`「段」「轮」、ADR 0008/0010/0013、`research/round12/DESIGN.md` §2.1.9/§2.2.3，以及 `research/protocol.md` §5.2。验证记录见 [ticket-15](verification/ticket-15.md)。

| 编号 | 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|---|
| LINEAGE-ROUND | 主对话 `system/init.uuid` 标识实际回合；带 `user_message_uuids` 的 `stream_event/message_start`、assistant、result 归属同一回合。紧邻的多条 user 可以合轮，单数 `user_message_uuid` 只作数组缺失时的兼容入口 | 上述研究；本版本真 CLI 隔离实跑及重新录制的 `stream-tool-two-turns` 流水 | `recorded_human_rounds_keep_their_user_uuids_and_final_assistant_anchor`；主接缝 `human_rounds_map_to_cli_uuids_and_survive_restart`、`coalesced_cli_prompts_share_one_navigation_round`、`a_running_round_keeps_its_identity_when_the_daemon_restarts` | 没有回显及实际归属就不生成轮；不按发出次数或文字猜。升级关卡失败则保留钉版，不把不明索引交给分叉/回退入口 |
| LINEAGE-ANCHOR | 带原 UUID 的 user 回显对应 CLI JSONL 中同 UUID 的 user 行；最终主对话 `assistant.uuid` 对应 JSONL 的 assistant 行，和 API `message.id` 分开。工具调用中的多次模型请求仍为一轮 | CONV-ECHO/CONV-TURN；本版本主接缝读 CLI 自己写出的记录实测 | `human_rounds_map_to_cli_uuids_and_survive_restart` 同时核 user 与最终 assistant 行；默认录制回归包括真实 Bash 工具往返；纯函数测试防止迟到早期输出改写终结锚点 | 没有可核的原生位置就不给后续操作锚点；不写 CLI 记录补造位置 |

本单没有编号待验证项，不验证 Codex `clientId` 回显 V3。没有写 CLI 原生存储，没有新增可选组件或结构/派发操作；图与索引均由守护进程通过 nd-wire 提供。回合进行中重启的测试只证明谱系身份与映射持久，不代替 #19 的全部流式恢复验收。

## 三种发送意图、撤回与 Esc（#18）

版本与散列同 #11。主接缝测试在 `crates/nd-daemon/tests/sessions.rs`，完整运行见 `scripts/test-scenarios.sh`；只替换模型端点，Bash/FIFO、MCP 工具、CLI、mod、看守和 SQLite 均实际执行。详细边界见 [验证记录](verification/ticket-18.md)。

| 编号 | 依赖与出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|
| SEND-INTENTS | `next/later/now` 分别是并入、本回合后、打断再发；P §5.2、R12 §2.1.9 | `send_intents_land_at_the_requested_turn_boundary`：前台 Bash 阻塞在 FIFO，实际模型请求和轮归属分别为并入同轮、完整下一轮、未放 FIFO 先到新请求 | 对应能力置为不可用，不能仅改按钮名字；并入没有消费机会时允许落到下一轮 |
| SEND-WITHDRAW | `cancel_async_message{message_uuid}` 的 cancelled bool；cli-protocol §6 | 排队成功、已开始 false、重复撤回、CLI 答复前杀界面/守护进程、并发稿另存 | false 保留原 Send；回应缺失/进程退出记 Unknown，不回填、不重发 |
| SEND-INTERRUPT | 普通 `interrupt`；perTaskStopAffordance 后保后台，ACK 与 result 分离；ADR 0008、R10-E1 | `escape_ends…`、`escape_preserves_background_bash_agent_and_workflow_until_their_results_arrive`；三类后台完成通知均到实际模型请求 | 声明失效时范围标为会停后台任务；不以空白 now 或关闭 stdin 代替 |
| SEND-CANCEL-QUEUE | `system/init.capabilities` 声明 interrupt_cancel_queued_v1；`cancel_queued:true` 回 cancelled UUID 列表 | `explicit_stop_and_cancel_queue_restores_each_queued_message_once`；只恢复明确取消的消息 | 不声明就拒绝并隐藏入口；缺列表是 Unknown，不当成空队列或取消成功 |
| SEND-E2B | `CLAUDE_AUTO_BACKGROUND_TASKS=1` 下立即发送使可后台化的前台 MCP 转后台；规格第 2 步 E2b | `e2b_send_now_moves_foreground_mcp_to_background_and_delivers_its_result`：真实 MCP 已执行且尚未放 FIFO，立即发送收到 background task，放行后结果回主对话，服务端无取消通知；正式启动模板固定启用该变量 | 若升级后此场景不成立，撤掉变量，能力表禁用保留前台 MCP，界面说明会中断；不以超时自动后台化代替 send-now 场景 |
| SEND-E2B-ENV | `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` 或 `CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS=0` 禁用普通前台 MCP 的后台化；`CLAUDE_CODE_DISABLE_MCP_TASK_BACKGROUND=true` 控制服务器 task handle 的后台等待，不能等同于禁止普通调用转后台。应用对这三种配置均保守关闭保留承诺 | 钉版 2.1.289：Vd @205765136、iht @207476353；普通调用/任务 handle 分流 @235008300、@235043450；任务测试使用隔离缓存 tengu_mcp_tasks 和 MCP_PROTOCOL_NEGOTIATION=auto 协商 2026-07-28 | `disabling_mcp_backgrounding_removes_the_send_now_preservation_promise`：真实普通 MCP 执行中立即发送，服务端收到取消；`mcp_task_background_switch_controls_inflight_task_handles`：task 开关关闭时后台任务经中断仍完成，开启时不协商 task 结果，前台调用被中断且服务端收到取消；逐个删掉 CLI 环境变量均须检出。默认正例见 `e2b_send_now_moves_foreground_mcp_to_background_and_delivers_its_result` | 实际调用路径或开关语义改变时升级关卡不放行；字段为 false 只表示不承诺保留，不推断所有 MCP 都会被取消 |
| SEND-E2B-AGENT | 同开关影响 Agent 后台策略；固定二进制的开关读取 @215204979 与 Agent schema 中的异步策略说明 | `auto_background_keeps_an_explicit_foreground_agent_completable`：显式 false 在当前模板仍异步启动，主回合先继续，代理随后正常完成 | 不承诺 Agent 必定同步，也不把源码中的 120000 ms 分支当作该模板实测延迟；升级需重跑并记录实际策略 |

E2b 本轮的无变量对照也能后台化，不能把结果归因为该变量的唯一作用。采用规格主方案是在启用变量的正式模板上验证行为成立。MCP 后台准入仍由 CLI 决定，能力说明只覆盖可后台化调用。没有新增应用写 CLI 原生存储的例外。
## 守护进程恢复与交付澄清（#19）

固定 CLI 2.1.289，散列沿用上文。本单不写 CLI 原生存储。产品级验证见 [#19 验证记录](verification/ticket-19.md)。

| 编号 | 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|---|
| RECOVERY-ECHO | 守护进程重连后原 CLI 继续流式回合，`isReplay` 保留原 uuid；不再次 initialize、不按文本猜送达 | ADR 0005；规格「恢复闸门」「写后记账」；CONV-ECHO、MOD-HTTP | `restart_keeps_a_written_message_pending_until_its_original_echo`、`ambiguous_write_and_crash_before_accounting_never_resend_the_native_input`；真 CLI、两种写后窗口 | 不能确定消费时标 Unknown，保留原票，不自动重发 |
| RECOVERY-STREAM | 已开始的内容块经守护进程重连仍沿用原身份、以完整块结束 | CONV-STREAM；规格第 2 步验收 | `streaming_survives_kill_and_service_restart_with_a_checkpoint_mid_block`，kill -9 与 restart 各一遍 | 不猜块或文本，保留已知内容与不明状态；该版本不能通过升级关卡 |
| RECOVERY-LOSS | 证实未送达须有原票的明确证据；`command_lifecycle` 的 refused/discarded 只归属匹配的原 uuid，不以 completed/result 推断未送达 | `research/impl/cli-protocol.md` §4、§6；规格「结果」「恢复闸门」 | 真流水缺失场景验证 Unknown 退路；真实连续流水对账验证原输入未写出及经 nd-wire 手动重发；脚本适配器验证 Lost/Clarified 与崩溃矩阵。refused/discarded 的具体 CLI 触发路径未在本单新增实测，沿用 #13 的解析边界 | 没有可验证证据就保持 Unknown；旧检查点缺输入记账时不能证明未写出 |
## 附件输入（#17）

固定 CLI 2.1.289，字节指纹同 #13。协议依据：`research/protocol.md` §3.3、`research/impl/INDEX.md` #17，以及本单真实产品主接缝请求。详细验证与平台退路见 [ticket-17](verification/ticket-17.md)。

| 编号 | 依赖 | 自动验证 | 不成立时的退路 |
|---|---|---|---|
| ATTACH-IMAGE | stream-json user.content 的 image/source{type:base64,media_type,data} 接受 PNG/JPEG/GIF/WebP；图片原字节进实际模型请求 | `attachments_reach_the_model_and_remain_in_the_conversation`、`jpeg_gif_and_webp_attachments_reach_the_model_with_their_original_bytes` | 不发送伪路径或静默丢图；受影响候选不放行，保留当前钉版 |
| ATTACH-FILE | PDF 以 document/source{type:base64,media_type:application/pdf,data} 输入；普通 UTF-8 文件以标注名称的 text 块输入；可以没有额外正文 | `files_without_caption_reach_the_model_and_survive_restart_and_collection`：实际请求正文、PDF 原字节、纯附件创建/发送和重启后引用 | 非 UTF-8 或未支持的二进制格式明确拒绝，要求转换文件；候选改变 PDF 支持则不放行 |
| DIFF-EDIT | Claude 的 Edit 工具块保留 name 和 input.file_path/old_string/new_string，供片段 diff 使用（出处：CLI 工具声明与本单实际 Read → Edit 往返） | `real_edit_tool_exposes_the_replaced_text_for_diff_display`；纯视图测试核片段行号与内容 | 缺少字段时保留原工具条目后备文字，不猜文件内容 |
| ATTACH-SIZE | 大于旧看守单行界限的消息仍能经固定 CLI 到模型；正常回显照常结票 | `a_multi_megabyte_attachment_is_not_lost_at_the_watchdog_frame_boundary`：2,400,000 字节全文；原生粘贴场景核对多个内容块 | 编码后超过看守单行界限时在写前明确失败，正文及引用保留；不把确定未写出算成 Unknown |

没有写 CLI 原生记录。新建与发送继续使用原动作、UUID 和收据契约，崩溃矩阵增加 `create-and-send/attachments` 和 `create/attachments-open-fails`；上传正文只经 Blobs HTTP，持久动作只存引用。

## 会话设置与标题（#21）

固定 Claude Code 2.1.289，SHA-256 同 #13。协议出处：`research/impl/cli-protocol.md` §7、`research/round3/CLI-BEHAVIOR.md` §1.2–1.3；以本单真 CLI 离线场景为当前版本结论。完整证据与复验命令见 [ticket-21](verification/ticket-21.md)。

| 编号 | 依赖 | 自动验证 | 不成立时的退路 |
|---|---|---|---|
| SETTINGS-MODEL | `set_model{model}` 后续回合生效；自定义模型会额外发 `max_tokens:1` 的验证请求；`get_settings.applied.model` 返回解析后的模型名 | `settings_model_changes_the_next_turn_and_survives_restart`：扣住当前回合，设置期间代持新输入，放行后核正式请求模型与历史，区分验证请求 | 控制错误保留旧显示值并显示操作失败；接受后回读失败记交付不明，不声称撤回；升级关卡不放行 |
| SETTINGS-FLAGS | `apply_flag_settings{settings:{effortLevel}}` 与 `get_settings.applied.effort`；按模型目录提供的档位展示，实际控制已验 high/medium/max | `settings_effort_changes_clear_ultracode_and_support_max`；实际下一请求 `output_config.effort=max`；`settings_are_read_on_open_and_permissions_and_effort_survive_reclaim` 验 high 与续接 | 拒绝时不更新已应用值；目录或回读缺失时不猜档位。用户已选择的 effort 独立保留，暂不支持的模型不会把它清掉 |
| SETTINGS-ULTRACODE | `apply_flag_settings{settings:{ultracode:true/false}}`；分别读 `ultracodeRequested/ultracodeAvailable/ultracode`。开关本身不改 effort；改变 effort 档位会关闭 ultracode | `settings_effort_and_ultracode_follow_cli_availability_and_preserve_effort`、`settings_effort_changes_clear_ultracode_and_support_max`：Opus 开关、effort 不变、切 Haiku 后 requested=true/available=false/applied=false；初始 Haiku 拒绝操作 | 缺字段或 available=false 时 caps=false、隐藏开关；Codex 在后端端口无条件禁用。候选版本入口或语义不成立时不开放该能力 |
| SETTINGS-PERMISSION | `set_permission_mode{mode}` 使用 CLI 模式名；不改变启动授权、账号资格或策略限制 | `settings_are_read_on_open_and_permissions_and_effort_survive_reclaim`、`settings_stale_clients_conflict_and_cli_rejection_preserves_applied_values`：acceptEdits 生效、续接保留；无效模式被真实 CLI 拒绝且不改旧值 | 失败保留旧值，CLI 原因显示在操作条目；auto/bypass 等资格仍由 CLI 拒绝，不绕过 |
| TITLE-GENERATE | `generate_session_title{description,persist:true}`；当前会话模型返回 JSON `{"title":"…"}`，CLI 回 `{title}`，持久写 ai-title；没有自定义标题事件 | `settings_ai_title_is_generated_once_and_return_value_updates_the_sidebar`、`settings_inflight_ai_title_survives_restart_and_never_overwrites_manual_title`：返回值更新、CLI 原生持久条目、重启不重复请求、手动优先 | 不支持、返回 null、失败、进程退出时保留首条消息摘要；一次自动尝试，不自动重复生成 |
| TITLE-RENAME | `rename_session{title,source:"host"}`，由 `system/session_title_changed` 更新自定义标题 | `settings_manual_title_updates_sidebar_and_survives_resume`：侧栏、重启、同一后端会话续接 | 拒绝空名、控制字符和超过 200 个字符的标题；失败保留原值；不直接写 CLI JSONL |

录制：`crates/nd-claude/tests/fixtures/settings/claude/2.1.289/flags.jsonl`，由设置主接缝的 `ND_RECORD_SETTINGS` 开关保存真实控制往返；`settings_recording.rs` 直接喂协议状态机。特别保留 effective.ultracode=true 而 applied.ultracodeRequested=false 的样本：界面必须读 applied，不能把配置层字段当成运行时状态。

设置是绝对赋值，恢复允许幂等重申后回读。AI 标题可能发模型请求，恢复时先查检查点和真实输入流水：已写出只接回应，证据不完整则报不明，不重发生成。新增操作的四行崩溃矩阵见会话组件说明。本单没有新增直接写 CLI 存储的例外。

## 总结、`!` 模式、fork 型子代理与降级提示（#22）

固定 CLI 2.1.289，SHA-256 同 #5。主接缝场景在 `crates/nd-daemon/tests/invocations.rs`（真守护进程、真 CLI 与两个 mod、看守、systemd、SQLite，只换模型端点；其中两项另起私有 KWin 真窗口），mod 往返录制在 `crates/nd-claude/tests/invoke_recording.rs`，默认套件回放 `crates/nd-claude/tests/fixtures/mod/claude/2.1.289/invocations.jsonl`。探路实验（隔离 bwrap、伪端点）留在 `/mnt/wd_external/nd-build/tmp/ticket-22/spike/`，不进仓库。实现见 [Claude 适配](../crates/nd-claude/README.md)、[两个 mod](../mods/README.md)、[会话组件](../crates/nd-session/README.md)。

| 编号 | 依赖 | 出处 | 自动验证 | 不成立时的退路 |
|---|---|---|---|---|
| SUM-RUN | 动作 mod 的 `$.command.run({command:"compact", args})` 排到会话空闲才跑；成功回 `{}`；压缩钩子 skip 时回 `{text:"Not compacted · <原因>"}`，stdout 多一条同文 assistant 与 `num_turns:0` 的 result。成功时 stdout 依次有 `system/status compacting`、`system/compact_boundary{trigger:"manual"}`、摘要 user 行（不是回显）、`<command-name>/compact…` 与 `<local-command-stdout>Compacted </local-command-stdout>` 两行回显、`num_turns:0` 的 result | `research/impl/mod-api.md` §10；探路 `spike/out/compact`；本版本实跑 | `summarize_from_here_…`、`summarize_up_to_here_…`、`a_prompt_the_cli_no_longer_holds_…`；录制 `invocations.jsonl` 的 `op-compact`；`compaction_results_distinguish_done_unlocatable_and_failed` | 结果形状看不懂按交付不明处理、不自动重发；关卡不放行该版本 |
| SUM-HOOK | `session.compact` 的 `e.messages` 是 `{role,text,toolUses,toolResults?,handle}` 行；用户行的 `text` 是各文字块直接相连（`TWO_A`+`TWO_B`→`TWO_ATWO_B`），图片不进文字，系统提醒不在行里。`next({...e, instructions: undefined, messages: 子集})` 只把子集交给摘要器（主路由、无 agent 头的一次模型请求，最后一条 user 是摘要指令），回 `{messages:[摘要 user, 最后一条 assistant], tokensBefore, tokensAfter, usage}`；带原 handle 拼回的行保持原样。`handle` 对未压缩过的行等于原生 uuid，压缩后保留的行换了新 uuid，所以定位不靠它 | d.ts D289:10252–10313；探路 `spike/out/compact/mods.jsonl`；本版本实跑 | 同上三个场景（摘要请求只含所选范围、下一请求范围外原行仍在）；`the_compaction_hook_sees_text_blocks_joined_without_a_separator` | 定位不到（文字散列找不到、同一原文次数对不上）就 skip、回 `Refused(AnchorGone)`；钩子出错由 `.catch` skip，不落回整段压缩 |
| SUM-NOHOOK | 钩子 mod 不在时，`/compact ND_SUM …` 会把整段对话交给摘要器（参数当作摘要指令） | 探路 `spike/out/nohook`（只装动作 mod，结果 `{}`、对话被整段替换） | 间接：`without_the_hook_mod_…` 证明只能聊天的进程不发总结 | 已实现的防护：只在两个 mod 都报到（能力表 `summarize` 可用）时派发；钩子 mod 运行中被换掉的残余风险见本节末 |
| SUM-SLASH | stream-json 的 user 行 `/compact` 跑 CLI 自己的整段压缩 | 本版本实跑 | `a_prompt_the_cli_no_longer_holds_…` 用它造出「守护进程以为还在、CLI 已总结掉」 | 只用于测试造场景；产品不靠它 |
| BANG-TOOL | `$.tool.call({tool:"Bash", command, description})` 要审批时 CLI 发 `can_use_tool`：`tool_name:"Bash"`、`input.command` 与原文逐字一致、`tool_use_id` 以 `toolu_plugin_` 开头、没有 `agent_id`；回 allow 就跑。成功结果 `{result:{stdout,stderr,interrupted,…},text}`；非零退出 `{isError:true,result:"Error: Exit code N\n…",text:"Exit code N\n…"}`（标准输出与错误合在一起）；只读命令可能不审批直接跑 | 探路 `spike/out/shell`；`research/round9/VERIFY.md` E9 | `bang_runs_one_command_without_a_turn_…`（`cd`+`export` 要审批，自动放行；退出码 3 读出）；`only_the_dispatched_bash_command_is_auto_approved`；录制 `op-shell` | 自动批准规则只放行守护进程刚派发、原文一致、无 agent_id 的这一条；请求形状变了就不放行、回到等界面回答 |
| BANG-CWD | 两次 `$.tool.call` Bash 之间只延续工作目录，不延续环境变量；`cd` 还会改会话的工作目录（下一次请求带 `Environment update: Primary working directory`）。shell 取用户的（沙盒里是 zsh，未加引号的 `[…]` 会被当通配） | VER E9；探路请求体；本版本实跑 | `bang_runs_…`：`pwd` 落在 `sub`、`VAR=[]` | 行为变了按 CLI 的来，在会话里如实显示；不另造持久 shell |
| BANG-APPEND | `$.session.append({message:{type:"user",content:[文字块…]}})` 写一行 isMeta 用户行，回 `{message,uuid}`（被别的插件拒时回 `{deny}`），本身不起回合，下一次模型请求带上 | `research/impl/mod-api.md` §7；VER E9；探路请求体 | `bang_runs_…`：`!` 之后零新请求，下一请求含 `<bash-input>…</bash-input>` 与输出 | 追加被拒：结果 `appended:false`，界面写明「输出没有进对话」；不重跑命令 |
| BANG-FORMAT | 终端自己的 `!` 模式把命令写成 `<bash-input>命令</bash-input>`、输出写成 `<bash-stdout>…</bash-stdout><bash-stderr>…</bash-stderr>`（2.1.289 `processBashCommand`）；New Desktop 照这个格式追加。该版本终端在 `respondToBashCommands`（缺省 true）时会接着请求模型，规格要求 `!` 不起回合，按规格不请求 | 二进制内嵌 JS（`tengu_input_bash` 附近的 `processBashCommand`） | `bang_runs_…` 核对下一请求里的 `<bash-input>` 行 | 格式变了改追加的文字；是否起回合仍按规格 |
| FORK-SPAWN | 设 `CLAUDE_CODE_FORK_SUBAGENT=1` 后 `$.agent.spawn({subagentType:"fork", prompt, description})` 回 `{model, agentId}`，空闲时也能派；stdout 有 `background_tasks_changed`、`task_started{subagent_type:"fork",is_backgrounded:true}`、带 `parent_tool_use_id:toolu_plugin_…` 的子代理文字、`task_notification{status:"completed"}`；子代理的模型请求带 `x-claude-code-agent-id: <agentId>`，消息里有父对话和 `<fork-boilerplate>` 包着的任务 | VER E8；探路 `spike/out/shell`；本版本实跑 | `subtask_dispatches_a_fork_subagent_…`（子代理请求含父对话标记与任务，后台任务表回到空）；录制 `op-fork` | 门控没开或类型找不到时 `$.agent.spawn` 抛错：明确没派出，收据 `failed`；不退回别的子代理类型 |
| INVOKE-QUERY | 动作 mod 按 mod 代次留结果，守护进程重启后 `query{op_ids}` 能问到这条命令还在跑或已结束；结果 POST 到新通道时对不上在途命令，作 Stray 丢掉，靠 query 取回 | `mods/new-desktop-actions/hooks/state.ts`；本版本实跑 | `a_bang_running_across_a_daemon_restart_settles_once_from_the_mod_result`（重启后问到「还在跑」，放行后收据 Done、命令只跑一次） | 问不到（mod 重载丢了）就交付不明，不重发 |

本节没有写 CLI 原生存储：总结、`!`、fork 都经 mod 的公开 `$` 接口由 CLI 自己落记录。新增的三种动作都是至多一次、不可重发的派发类动作，引擎崩溃矩阵加了「总结 From 成功」「`!` 交付不明」「/subtask 成功」三行（见[会话组件说明](../crates/nd-session/README.md#引擎崩溃矩阵)）。

残余风险与观察：

- 钩子 mod 在派发之后、CLI 跑压缩之前被换成不带压缩钩子的版本时，SUM-NOHOOK 会整段压缩；产品不在运行中替换 mod，升级关卡跑本节场景。
- **长轮询拖慢下一条输入（观察，未改）**：探路与录制里都看到，CLI 把上一条排队输入记为 `command_lifecycle completed`、开始下一条（含 `/compact`）之前，要等动作 mod 在途的 `$.http.fetch` 长轮询返回；守护进程给 mod 发命令会让长轮询立刻返回，否则最长要等一个长轮询周期（产品 25 s，场景 5 s）。录制 `invocations.jsonl` 第 8→9 行的约 5 s 就是这段等待。这影响连发消息与总结的等待时间，不影响正确性；需要后续工单在 mod 通道上处理（例如回合结束时让长轮询提前返回）。
