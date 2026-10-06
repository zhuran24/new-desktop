# 对话转换库

`nd-convert` 提供 Claude 与 Codex 的对话编解码、中间条目、损失清单、增量同步点和中立设置档 `Profile`。输入、输出均为内存值；没有文件、网络、进程或存储依赖。

## 接口

```rust
use nd_convert::{BackendKind, FrozenInput, NativeItem, convert};
use serde_json::json;

let input = FrozenInput {
    backend: BackendKind::Claude,
    source_id: "backend-session-a".into(),
    epoch: "effective-history-1".into(),
    complete: true,
    images: Default::default(),
    items: vec![NativeItem {
        position: "message-uuid/block-0".into(),
        payload: json!({"role":"user", "content":"你好"}),
    }],
};
let converted = convert(&input, BackendKind::Codex, None)?;
assert_eq!(converted.items[0]["content"][0]["text"], "你好");
// 只有目标导入已确认并与目标位置一起入账，才能把这个同步点用于下一次追加。
let unchanged = convert(&input, BackendKind::Codex, Some(&converted.sync))?;
assert!(unchanged.items.is_empty());
# Ok::<(), nd_convert::ConvertError>(())
```

| 接口 | 用途 |
|---|---|
| `convert(&FrozenInput, BackendKind, Option<&SyncPoint>)` | 先验证完整历史，再对同步点之后的条目做源解码、目标编码；返回 `Converted { items, sync, loss }` |
| `decode(&FrozenInput)` | 返回 `Decoded`；每个 `Entry` 含中立角色、内容块以及完整 `native` 来源值 |
| `encode(&Decoded, BackendKind)` | 将中间条目编码成目标条目；可直接测试两家编解码的往返 |
| `decode_profile(&NativeProfile)`、`encode_profile(&Profile, &ProfileTarget)` | 设置经中立权限档和 effort 档换算，返回目标设置快照与损失清单 |
| `FrozenImage::from_bytes(media_type, bytes)` | 用已读取的字节建立图片快照及 SHA-256；读取责任在调用方 |

这些值都可用 serde 序列化。`ConvertError` 区分 `Incomplete`、`Invalid(reason)`、`SyncInvalid`；失败不返回可推进的同步点，也不产生部分导入。

## 导出方的契约

- `FrozenInput` 必须是**已物化的当前有效对话**。Claude 接受 `session.messages({as:"api"})` 的消息，也接受已选链的消息信封；不接受未经选链的整份 JSONL。压缩保留段、旁支选择、并行结果的物化由导出适配器和记录解析库负责。
- Codex 接受按对话顺序排列的 `ThreadItem`，也可解码本库输出的 Responses 消息和原生 reasoning 条目。适配器须读完所有历史页，补入自己的注入账本；空的 `thread/read.items` 不代表没有历史。原生回合失败、中断等回合级信息不在 `ThreadItem` 内，导出方可附加带 `type` 的说明条目，它会完整转成文字。
- `complete` 必须由导出方明确给出；缺页、缺失流水、未完成导出不得标成完整。没有 4096 条截断逻辑，超过 mod 上限后的完整记录解析结果可以直接传入。
- `source_id` 表示来源后端会话，`epoch` 表示有效历史的代次，追加期间保持不变。`NativeItem.position` 是导出方提供的唯一稳定位置，如 Claude UUID 加块序号、Codex 回合 ID 加条目 ID；不把它当作 CLI 的落盘叶子。
- 图片引用在 `images` 中按原路径或 URL 精确查找。不存在时生成占位及损失；转换器不读取文件或访问 URL。散列不符属于损坏输入，返回错误。只转 PNG/JPEG/GIF/WebP，单张解码后不超过 5 MiB；此检查不解析图片像素，也不证明模型端点接受该图片。
- 工具调用和结果必须完整配对，支持同一批并行调用和乱序返回；孤立结果、缺结果、重复调用 ID、重复位置被拒绝。适配器应在回合结束后冻结，不能通过伪造结果补齐。

## 输出与损失

Claude 输出是可供适配器逐帧发送的 stream-json 回放信封：user 固定 `shouldQuery:false`、`client_composed:true`，assistant 补齐 `id/type/model/usage/stop_reason`，两种都有确定性 UUID、空 `session_id` 和空 `parent_tool_use_id`。适配器须使用回放模式，按原生回应推进导入；库本身不发送帧，也不声称已经验证零请求、零工具重跑。跨后端 assistant 用 `nd-import` 标识历史来源；保留本方历史时保留原 `model` 和签名推理。

Codex 输出是 `thread/inject_items` 的 `items` 数组。普通文字和用户图片分别编码成 Responses message 和 data URL；Claude 工具调用、结果按顺序转成 assistant 文字，完整保留参数、结果和错误标志，不截断正文。来源原有的 Codex reasoning 仅在编码回 Codex 时输出为顶层 reasoning，不能变成 Claude thinking。

Codex `commandExecution` 转成带工作目录的 Bash 历史调用和结果；已经完成的文件新增转 Write，能完整解析且行数一致的更新 diff 转 Edit。新文件内容即使以 `@@` 开头也原样保留。删除、改名、失败变更、无法无歧义重建的 diff 保留完整文字。MCP、动态工具、子代理等其余条目保留原始 JSON 文字，异步问题转成普通对话。未知类型也如此处理，不静默丢掉。

`Entry.native` 保留整个源对象，包含未来字段、签名、密文、phase、usage 等。跨后端编码不会把签名或密文作为对方的推理发送。同端 Claude 已识别块的元字段保留，跨后端不能表达的元字段报告损失。中立内容的往返保证适用于文字、图片及本方可表达的工具块；工具降为文字后不能凭文字恢复原生工具结构。

损失条目用 `position` 定位，`reason` 是稳定代码：

| `reason` | 含义 |
|---|---|
| `native_reasoning_omitted` | 跨后端不发送来源推理；原值仍在中间条目 |
| `native_metadata_not_transferred` | 来源模型、用量、phase、引用等原生元信息未进入目标格式 |
| `tool_call_as_text`、`tool_result_as_text` | 结构化工具历史转成完整文字 |
| `file_change_as_text` | 文件变更不能完整映射到 Write/Edit，保留原说明 |
| `unsupported_item_as_text`、`unsupported_block_as_text` | 条目或内容块保留为完整 JSON 文字 |
| `interaction_as_text` | 异步问题保留为对话，交互能力不跨后端继承 |
| `image_unavailable` | 未冻结、格式不支持、base64 无效或超过大小上限 |
| `backend_setting_defaulted` | 后端特有设置不跨后端传递，使用目标默认值 |
| `permission_semantics_mapped` | 两家权限机制有差异，按下表映射，不能视为完全等价 |
| `unsupported_permission_defaulted`、`unsupported_effort_defaulted` | 没有受支持的对应设置，使用目标默认值 |

## 同步点的责任边界

同步点绑定编解码修订、来源后端会话、源/目标后端、历史代次、前缀长度、末位置，以及前缀和所引用附件的规范 JSON SHA-256。新增未引用的附件不改变旧前缀；原来缺失的图片后来得到字节也会改变旧前缀。对象键顺序不影响散列。

`SyncInvalid` 的处理是**在同一段新建同种目标后端会话，用 `since=None` 整段转换**，不得清掉同步点后向旧目标硬追加。只在目标 `Open{Seeded}`/`Import` 确认后，才把候选同步点、目标真实落点及损失清单一起写入操作账。当前函数没有目标承载位或目标位置参数；目标身份、目标前缀仍有效、重试去重和双向位置对应由会话引擎及适配器负责。它不把计算完成当成交付完成。

切回已有镜像时只追加新增部分，镜像里本方原生推理因此可以原样留下。纯库不执行退役、转接或重建，也不直接改 CLI 记录文件。

## 设置换算

`ProfileTarget.model` 必须由用户选择。工作目录来自中立 `Profile.cwd`；`defaults` 和 `supported_efforts` 来自目标后端、版本和所选模型的能力表。空的支持列表意味着不发送来源 effort；不猜测 `max` 等同于 `xhigh`。输出是适配器的设置快照，实际控制请求/启动参数编码由后端适配器完成。

| 中立权限档 | Claude `permissionMode` | Codex `approvalPolicy` / `sandbox` |
|---|---|---|
| `Prompt` | `default` | `untrusted` / `workspace-write` |
| `AcceptEdits` | `acceptEdits` | `on-request` / `workspace-write` |
| `ReadOnly` | `plan` | `on-request` / `read-only` |
| `Unrestricted` | `bypassPermissions` | `never` / `danger-full-access` |
| `Default` | 目标默认值 | 目标默认值 |
| `Unmapped` | 目标默认值并报损失 | 目标默认值并报损失 |

此表是产品映射策略，不是两家机制等价的断言，所有跨后端权限映射都会报告该差异；Claude plan 的交互语义与 Codex sandbox 尤其不同。Codex granular 审批、Claude auto/dontAsk 等未匹配组合使用默认值。源端设置原值留在 `Profile.native`；同端换算保留启动设置，修改中立权限/effort 时覆盖对应字段，恢复默认会清掉旧值。

## 验证与后续接入

`cargo test -p nd-convert --locked` 包含公开函数行为测试、录制数据回归，以及两组 proptest：中间条目经两家编解码往返、分批和整段转换一致。录制夹具的来源、选取方式与 SHA-256 在 `tests/fixtures/sources.json`。CLI/Codex 格式依赖和升级退路见仓库的 `docs/cli-contracts.md`。

这些测试没有启动 CLI 或模型端点。真实导入、再次续接、转投和模型端点接受历史的主接缝验收由后端适配器及换后端工单接入；E21 保留为第 6 步验收，失败时同段整段重建并展示损失，仍不成立则禁用该方向。
