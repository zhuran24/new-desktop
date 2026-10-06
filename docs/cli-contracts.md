# CLI 与 Codex 契约清单

日期：2026-10-06。各工单引入的 CLI/Codex 行为在此各占一个独立条目。#8 的只读记录契约已实现并通过离线验收；#9 的转换契约见下方对应小节。

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
