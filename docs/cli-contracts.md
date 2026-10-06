# CLI 契约清单

日期：2026-10-06。状态：#8 引入的只读记录契约已实现并通过离线验收。其他工单引入的 CLI/Codex 行为在此追加独立条目。

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
