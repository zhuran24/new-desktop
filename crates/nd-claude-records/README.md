# Claude 记录解析库

状态：已实现；验收版本为 Claude Code 2.1.289。日期：2026-10-06。

`nd-claude-records` 为 Claude 适配与独占登记提供同一个只读解析入口。库接收调用方提供的完整、不可变 JSONL 字节快照，计算当前叶子和历史顺序，建立 UUID 到原始字节范围的索引。它不打开文件、不写 CLI 存储、不启动进程，不依赖 GPUI 或组件内核。

## 接口

| 接口 | 含义 |
|---|---|
| `RecordIndex::parse(&[u8])` | 扫描快照并建立索引；正文只在扫描当前行时暂时解码，不在索引里留整份 JSON |
| `RecordIndex::current()` | 选择 CLI 当前主对话，返回 `History`；格式损坏、选中链断裂或循环时返回 `Error` |
| `RecordIndex::record(uuid)` | 读取该 UUID 最后一次出现的原始记录；也能读被回退或压缩切掉的行 |
| `History::leaf()` | 最近的 user/assistant 记录 UUID；附件尾部可以在历史中排在它后面 |
| `History::is_cleared()` | 区分 explicit 空叶子和空文件；两者的 `leaf()` 都是 `None` |
| `History::ids()` | 当前历史顺序，含沿链 system/attachment、恢复的回复分块和并行工具结果 |
| `History::page(before, limit)` | 从末尾向前分页；`before` 为独占上界 UUID，`limit` 为 `NonZeroUsize`，页内从旧到新 |
| `Record::{uuid,byte_range,raw,decode}` | UUID、原始字节范围、原始 JSON 字节、按需解码；未知内容块和字段保留 |

`Page::next_before` 是下一页的 `before`；为 `None` 表示已到开头。未知或不属于这条历史的游标返回 `InvalidCursor`。页属于当前借用的快照；调用方不能在文件追加或替换后继续使用旧快照的偏移去读新文件。跨 nd-wire 暴露游标时应绑定调用方的快照身份，不能只发送裸 UUID。

偏移以字节计，范围不含 LF；CRLF 的 CR 是原始行的一部分。重复 UUID 使用最后一份字节内容，逻辑位置按第一次插入保留。正文和原始 `parentUuid` 不改写；压缩保留段重接、progress 桥接、并行分块恢复影响的是 `History` 顺序。消费者应使用这个顺序，不要从 `Record::decode()` 的原始父指针重新推链，也不要把库输出直接等同于 CLI 最终模型请求。

## 选择规则与边界

- `last-prompt`、explicit 回退/清空、随后新增的主对话、父子关系和时间戳共同选叶子。标题或没有 `leafUuid` 的 `last-prompt` 不表示新对话；sidechain 和 `fork_briefing` 不撤销主对话的显式选择。
- 压缩边界重置旧选择；最新边界的 `preservedMessages` 优先，旧格式 `preservedSegment` 沿尾到头求出保留序列，再接到摘要。压缩前未保留的行不进入当前历史，也不能通过回复分块恢复重新混入。`logicalParentUuid` 不用于续接父链。
- 同一 `message.id` 的 assistant 分块、直接相连的并行工具结果恢复到历史中。父指针陈旧的工具结果只按同一 agent 内唯一的工具调用 ID 恢复；歧义不猜。API message ID 和记录 UUID 是不同身份。
- 当前选择针对主对话，sidechain 只保留原始索引。模型请求的块清洗、工具缺失结果合成、附件渲染、用量归零与转换不在这个库里。
- 使用 CLI 正常写出的 RFC3339 时间戳。缺失或非法的选中叶子时间戳报错，不把它作为独占登记基线。
- JSON 错误、无效对话信封、未写完的末行、缺父节点、循环和破损保留段均明确报错。CLI 对损坏记录有跳行、按邻近时间戳猜父节点和返回部分历史等容错；本库不把这些猜测作为可授权写入的叶子。调用方先扩大读取到完整快照/等待写完，仍失败则标记记录不可判定并停止基于它的续接放行。

索引保留全文件 UUID/父节点/偏移等元数据，空间随记录数增长；当前历史选择与批次恢复不复制正文。当前实现全量建立不可变索引，后续文件监视器负责重读和缓存失效，库不内置增量追尾。长历史翻页只解码请求的 `Record`；先取 `current()` 并复用其返回值。

## 验证与复现

默认工作区测试使用已录制的真 CLI 原始文件和明确标注的合成边界案例：

- [history.rs](tests/history.rs)：真实记录的回退/新旁支前缀、两次压缩、保留尾段、旧格式保留段、显式清空、时间戳、分块与工具结果、错误处理、UTF-8 偏移、超过 5 MiB 的 8,192 条历史分页。
- [claude-2.1.289.jsonl](tests/fixtures/claude-2.1.289.jsonl) 是 CLI 原样写出的记录，未手工追加或改 UUID；[manifest.json](tests/fixtures/manifest.json) 记录 CLI/文件 SHA-256、会话/提示 UUID 与压缩边界。期望值来自真实 `rewind_conversation`、CLI 帧、`export_conversation` 和续接后的 Messages 请求。
- `legacy_preserved_segment_*` 从真记录删除 `preservedMessages` 得到旧格式衍生输入；大记录、异常、并行分块等案例是纯函数构造输入，不冒充真 CLI 录制。

现场验收测试默认忽略，需要 Linux 用户 systemd、bwrap、Python 3 和钉版 CLI `/mnt/wd_external/nd-build/cli/claude-2.1.289`。测试只替换模型端点：在断网 namespace 内运行真实 CLI 与本地 HTTP 服务，经协议执行三轮对话、回退、旁支、两次 `/compact`，随后续接核对实际模型请求。无真实凭据和真实模型服务；失败会使测试失败，不按环境不足跳过。

在工作树内运行（证据目录必须不存在或为空）：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-8
export CARGO_BUILD_JOBS=6
export ND_RECORDS_EVIDENCE_DIR=/mnt/wd_external/nd-build/tmp/ticket-8-cli-check
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test -p nd-claude-records --test cli_live --locked -- --ignored --nocapture
```

[generate.py](tests/cli/generate.py) 从空环境配置临时 HOME、CLAUDE_CONFIG_DIR 和 XDG 路径；bwrap 只挂载 `/usr`、CLI、专用临时目录和私有 `/proc`、`/dev`，不挂 owner 的 home/run；每次使用唯一 `nd-test-records-*.service` 和 `nd-test-records*.slice`，服务与 slice 都限额 12 GiB、禁用 swap。结束后回收单元和临时配置，只保留证据目录里的记录、帧、stderr、导出、模型请求和 manifest。

完整依赖契约及失败退路见 [CLI 契约清单](../../docs/cli-contracts.md)。
