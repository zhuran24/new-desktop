# 守护进程、同步流与命令收据

日期：2026-10-06。状态：#3、#4、#13 已实现。当前提供本机 `global` 流、配置与存储底座、持久命令收据、可选诊断组件、Claude 会话（新建、流式对话、按需拉起与闲置回收）和 `ndctl`。

## 启动与路径

`nd-daemon` 不带参数时使用以下路径。`HOME` 和 `XDG_RUNTIME_DIR` 必须存在于启动环境。

| 内容 | 默认路径 |
|---|---|
| 配置 | `$XDG_CONFIG_HOME/new-desktop/config.toml`；未设置时为 `~/.config/new-desktop/config.toml` |
| SQLite 与附件 | `$XDG_STATE_HOME/new-desktop/`；未设置时为 `~/.local/state/new-desktop/` |
| socket、单实例锁 | `$XDG_RUNTIME_DIR/new-desktop/{nd.sock,daemon.lock}` |

systemd 用户服务文件在 [packaging/systemd/nd-daemon.service](../../packaging/systemd/nd-daemon.service)。安装时把编译后的 `nd-daemon`、`ndctl` 放到 `~/.local/bin/`，把服务文件放到 `~/.config/systemd/user/`，再运行 `systemctl --user daemon-reload` 和 `systemctl --user enable --now nd-daemon.service`。服务重启不复用事件纪元。SIGTERM 按 `HandBack` 停止组件；本单没有后端进程。

临时实例可显式传 `nd-daemon --root /path/to/instance`：配置、数据库和附件直接放在该目录，socket 在 `runtime/nd.sock`。运行时目录强制 0700、必须归当前 uid 所有且不能是符号链接；单实例锁在恢复之前取得，恢复完成后才监听。旧 socket 只在取得锁后移除，普通文件不替换。HTTP 和 WebSocket 的每个连接都检查 `SO_PEERCRED` 的 uid。

```bash
ndctl get global
ndctl watch global
ndctl page system
ndctl --socket /path/to/instance/runtime/nd.sock get global
```

`get` 打印完整快照后退出；`watch` 首行也是完整快照，之后每行是更新后的完整副本。`page` 经同一协议的只读查询路径。无需 GPUI。

## 会话与 Claude 后端

配了 `watchdogs` 与 `claude` 两节时，守护进程有 Claude 后端：每个后端进程由看守进程托管，会话由会话组件持有（[会话组件说明](../nd-session/README.md)）。没配时会话命令回 `unsupported`。

```toml
[watchdogs]                       # 看守托管（#6）
root = "/run/user/1000/new-desktop/runs"
watchdog = "/home/me/.local/bin/nd-watchdog"
unit_prefix = "nd-run"
slice = "nd-backends.slice"

[claude]
cli = "/path/to/pinned/claude"    # 钉住的 CLI，按后端进程看到的路径写
hook_mod = "/path/to/mods/new-desktop"
action_mod = "/path/to/mods/new-desktop-actions"
config_dir = "/home/me/.claude"   # CLI 的配置目录：独占登记扫描它的 sessions/ 与 jobs/
# socket = "…/mod.sock"           # mod 通道，默认运行目录下的 mod.sock（约 100 字节以内）
inherit_env = true                # 后端进程的基础环境取守护进程继承的环境；BUN_OPTIONS 总会去掉
# env = { KEY = "value" }         # 追加或覆盖的环境变量
hello_timeout_ms = 10000          # 等两个 mod 报到；超时算拉起成功、只能聊天
record = false                    # 录 mod 往返与看守流水（场景测试用）

[sessions]
idle_reclaim_ms = 900000          # 当前进程闲置多久回收，默认 15 分钟
tick_ms = 1000                    # 闲置检查间隔
```

`claude`、`watchdogs` 两节在启动时读，改了要重启守护进程才生效。启动次序：独占登记进入恢复中 → 看守托管报每个还在的后端进程的身份 → 名册装载有活进程或未完操作的会话、适配器接回（续读流水、对账未结的票）→ 独占登记身份已知、第一次扫描完成 → 放行；放行之前起操作的命令照常受理，操作等着。

服务单元用 `RuntimeDirectoryPreserve=yes` 保留运行目录：停止、重启和崩溃重启都不删除看守的 socket、身份与流水。更新已安装的服务文件后须执行 `systemctl --user daemon-reload`；只有全部看守和后端进程都退出后才能手动清理该目录。

```bash
ndctl new --cwd ~/proj --model claude-haiku-4-5 --follow "你好"
ndctl send <会话 id> --follow "接着说"
ndctl send <会话 id> --intent after_turn "这一回合结束再看这条"
ndctl watch session/<会话 id>
ndctl page sessions
```

`new` 发 `session.create`（命令 id 自动生成），首行打印 `{command, session, reply}`；`--follow` 之后订阅 `session/<id>`，每个变了的条目打一行 `{"item":…}`（流式文字会以 `complete:false`、越来越长的同一条目出现），直到这一轮安静下来（没有没结论的消息、回合结束），或会话撤掉、部分完成。`--timeout` 秒数默认 120。命令与条目格式见会话组件说明。

协议上：`hello` 多协商 `sessions`、`session` 两个命名空间；`global` 快照带侧栏的会话条目和撤掉会话的一次性提示；`get sessions` 分页取列表；`subscribe {stream:"session/<id>"}` 每条连接每个会话流一个转发任务，没有这个会话回 `error{code:"not_found"}`，转发落后或队列满就断开连接；会话命令不经守护进程的全局锁，等会话执行器提交后回收据。订阅某个会话流期间算「有人在看」，这个会话的后端进程不闲置回收。

## 配置与组件

配置文件缺失时使用默认值；启动时文件无效则报错退出。运行期间，原位编辑与原子替换都触发重新读取；无效配置保留上一份有效值，通过 system 条目的 `config_error` 和后备文字报告，文件修复后自动清除。

```toml
[diagnostics]
enabled = true

[storage]
blob_grace_seconds = 86400
gc_interval_seconds = 3600

[commands]
receipt_keep_ms = 604800000 # 新收据保留七天，范围 1ms..一年

[wire]
send_queue = 128           # 每连接发送队列，范围 1..4096
send_timeout_ms = 5000    # 单次写超时，范围 1..60000ms
```

`diagnostics` 是真实内核可选组件，提供同名命名空间、无副作用的 `diagnostics.inspect` 和持久备注命令 `diagnostics.set_note`。关闭后提供者、订阅和命令登记一起撤回，global 移除其条目，之后的命令返回 `not_found`；重新启用重新登记。system 条目保留，并报告组件的 running、disabled、waiting、unavailable、failed 或 stopping 状态。生命周期驱动使用内核变化通知与最近期限，不轮询内核。

`nd-config::Config` 的公开接口：

- `section::<S>()`：`S: Section` 指定节名和反序列化类型。`get()` 返回该节及当前全局修订；`changed()` 只在该节的内容改变时返回。
- `update(patch, expect)`：先读取外部手改、核对修订和校验整份候选配置，再用同目录临时文件、fsync 和 rename 写入。过期修订回 `Conflict`，无效补丁不写文件。补丁按对象递归合并。
- 修订是不可解释的字符串；进程重开时换代，旧进程拿到的修订不能用于新进程。`FileSource` 对遵守配置锁的写者作比较交换；外部编辑器不遵守该锁，不能把任意编辑器和 `update` 的并发写入当成跨进程原子事务。
- 文件监视仅作重新读取的提示。`ConfigSource` 隔离配置来源，`FileSource` 是当前生产实现。配置更新通过库 API 提供。文件配置的写入不能放进 SQLite 命令事务；设置能力接入时需要持久操作记录和文件写回恢复，不能先改文件再补收据。

## nd-wire 与同步副本

Rust 类型在 `nd-wire/src/lib.rs`，生成物在仓库 `protocol/`。`nd-wire-schema protocol` 重新导出 request、response 两份 JSON Schema。CI 重生成后检查差异，测试也比对生成物。字段只添加；namespace、kind 和条目数据中的未来取值保留，未知条目仍有 `fallback{title,text}`。

UDS 的 `/wire` 是 HTTP WebSocket 升级入口，随后使用 JSON 文本帧：

1. `hello`：协议大版本 1，命名空间分别协商小版本。空 namespace 表表示接受守护进程当前版本；未知命名空间不宣称支持。
2. `subscribe {stream:"global", since:null}`：冷启动一定得到 snapshot，包含当前所有条目，包括零事件情形。
3. `event` 带 epoch、cursor、upsert、remove。单次状态发布中的条目变化一起应用。只在内存保留最近 128 个事件；守护进程重启换纪元。
4. 持有副本的重连可以给 `since {epoch,seq}`。同纪元且游标仍在缓冲内时先重放事件，再发 `resumed`；旧纪元、过期或未来游标都重新取快照。快照和事件订阅在同一个协调锁下取得，避免其间漏事件。
5. `get {id,res,page}` 经 `NamespaceProvider::page`；当前 system 和 diagnostics 都是单页，不支持历史 `before`。#20 在同一路径扩展历史分页。
6. `execute {id,command}` 提交持久命令，`receipt {id,command_id,content_hash?}` 查询收据；数字 id 仅关联 RPC，持久身份是 `command.id`。旧 `command {id,name}` 保留为只读诊断查询，不受理写命令。

每条连接独立有界发送队列，命令回应、快照和事件都经这一队列。持久事件不合并、不改序；队列满、广播滞后或写超时即关闭连接。能写出时附 `bye{resume:true}`，无法写出则直接断开。配置对新连接生效，单个慢连接不阻塞其他连接。重放所需消息超过队列容量时直接发完整快照，防止重连反复溢出。当前没有流式 delta，后续增量不能放入持久事件环。

`nd-ui-core::SyncReplica` 提供 `connect`、`subscribe`、`get`、`current`、`next`、`command`、`receipt`、只读 `query` 及附件读写。调用方持续驱动 `next()` 来收事件和自动重连，可以取消等待；`current()` 是最近应用的副本。查询和命令等待期间也应用流事件。请求编号单调增长，取消的请求迟到后不会冒充下一次回应。副本只由快照与事件修改。`ndctl`、场景测试均使用这个库。

## 命令契约

```bash
ndctl command '{"id":"note-001","device":"desktop-local","name":"diagnostics.set_note","args":{"text":"检查连接"},"expect":{"revision":0}}'
ndctl receipt note-001
ndctl page diagnostics
```

示例必须在当前备注修订号为 0 时首次执行。之后先从 `diagnostics` 条目的 `note.revision` 读取修订号，新编辑使用新的命令 id。`diagnostics.set_note` 是无外部派发的持久命令：最多 64 KiB UTF-8 备注，修订号每次成功编辑加一，重启后保留。禁用诊断组件后新命令回 `not_found`，已有收据仍能查询和判重，重新启用后备注仍在。

命令信封含 `id`、`device`、`name`、`args`、`expect`；前三项须为 1..256 字节字符串。id 在整个守护进程内唯一，设备名是来源标识，本机鉴权仍以 socket 对端 uid 为准，不能把自报 device 当成远程授权。`Command::content_hash` 将完整已知信封的对象键递归排序后计算 SHA-256，JSON 字段顺序不影响身份；设备、操作、参数、前置条件改变均视为不同内容。

| 回应 | 含义与副本行为 |
|---|---|
| `receipt {receipt:{status:"done",value}}` | 命令在同一事务内完成；只表示这个命令定义的受理时点 |
| `receipt {receipt:{status:"accepted",op}}` | 长操作已受理；后续进度从对象事件读取，不改原收据 |
| `receipt {receipt:{status:"rejected",code,now}}` | 持久拒绝；缺必填前置条件为 `invalid`，不符为 `precondition` 并带当前值 |
| `receipt {receipt:{status:"unknown",now}}` | 动作结果不明，`now` 指向待澄清对象，收据不改 |
| `conflict` | 同 id 不同散列，不执行，不覆盖原收据 |
| `expired` | 正文已过期，id 与散列墓碑仍保留，不执行 |
| `unavailable {reason}` | 本次事务未提交，不生成新收据；副本仅对此使用同 id、同正文退避重试 |
| `delivery_unknown` | 副本在传输中断后无法查明收据，禁止自动重发正文 |

同 id 同散列在检查当前前置条件、提供者是否启用之前返回原收据。效果和收据由 `Store::write` 同事务提交，成功后才发布事件和回应；拒绝收据也保持不可变。保留期限在提交时固定，重试、查询和后续配置更改不延长它；启动及每秒清理到期正文，墓碑不删除。查询只用只读 WAL 连接，未清理的到期正文也直接回 `expired`。

断线或等待回应超过 5 秒后，同步副本只查询命令 id 和原内容散列；拿到原收据、`conflict` 或 `expired` 就返回相应结论，查不到或无法连接则报告 `delivery_unknown`。独立的 `receipt(id)` 返回 `found`、`missing`、`expired` 或查询 `unavailable`，查询本身可重连重试。`missing` 只表示当前查不到，不能证明正文未曾送达。调用方取消 `command()` 后应保留 id 并查询，不另造 id 重送。

明确未受理的 `CommandReply::Unavailable` 最多发送五次，间隔 50、100、200、400 ms；用尽后把最后的 unavailable 交给调用方。`Receipt::Rejected` 即使错误码叫 unavailable 也不会触发这一重试。收据查询默认不发送正文，带散列的恢复查询还防止误认同 id 的另一条命令。

新增单事务能力实现 `NamespaceProvider::execute(tx,command)`，在账本去重之后核对该能力的必填条件并修改业务状态；可复用 `commands::{migrate,execute,lookup,expire}`。不得在闭包里写文件、发网络请求或派发后端动作。结构操作和 Delivery 时点的命令由后续会话引擎持久保存意图、追踪动作结果；本次实际业务命令只有 Commit 时点的诊断备注，不把本单测试视为后端执行至多一次的证明。只读查询走 `query/get`，不落命令收据。

## SQLite 与附件

`nd-store::Store::open(path, pool_size)` 建立一个写连接和固定大小的只读连接池，开启 WAL、外键和 FULL 同步。各业务模块负责自己的表与迁移。

- `write(|tx| ...)`：一个同步闭包一个 immediate 事务；返回错误时回滚。闭包禁止外部 I/O 和等待其他进程。`Tx::on_commit` 在提交成功后、释放写连接前运行，钩子不能重入写事务。
- `read()`：从有界池取得真正以 READ_ONLY 打开的读连接；句柄期间保持一份读事务，Drop 回滚读事务并归还。长时间持有会保留旧 WAL 快照，因此读完及时释放。
- SQLite 的 busy、full、corrupt 归为公开错误；失败不伪造收据。
- `Blobs::put(bytes)` 在事务外持久化文件，再登记 SHA-256 元数据；HTTP `PUT /blobs/<sha256>` 检查内容与地址相符。上传本身不创建业务引用。
- 业务行的主人在同一个 `Store::write` 里修改自己的行并调用 `Blobs::hold/release(tx,id,owner)`。同一附件和 owner 的重复 hold 幂等。引用数是引用表的行数，不另外维护可能失配的计数。
- 零引用保留宽限期。守护进程定时调用 `collect`；先在事务内标记待删，再在事务外删文件，最后清元数据。待删附件拒绝新增引用，下次清理可继续未完成删除。重新上传会刷新未引用附件的宽限期。当前定时清理处理已登记的附件；文件写完、元数据未登记时崩溃留下的孤立文件尚不自动扫描。
- `GET /blobs/<sha256>` 读回时再验散列。HTTP 与 WebSocket 使用同一个 uid 检查；上传上限 32 MiB。大文件流式传输、附件业务引用的界面入口由 #17 扩展。

## 自动验证

普通 `cargo test --workspace --locked` 包括内核性质、真 SQLite、真配置文件与协议演进测试。真实 systemd 场景显式启用 `nd-daemon/scenarios`，缺工具或用户实例时直接失败，不静默跳过：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-4
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test --workspace --features nd-daemon/scenarios --locked
```

场景需要 Linux cgroup v2、systemd 用户实例、bwrap、Python 3、unshare、newuidmap/newgidmap 和当前用户的 subuid/subgid 配置。每个场景使用真守护进程、独立 `nd-test-` 单元和 512 MiB slice、空环境、临时 HOME/CLAUDE_CONFIG_DIR/XDG、bwrap 断网；退出时撤销单元属性并删临时目录。测试不会启动 CLI、模型端点或用户的生产服务。`scenarios` 构建包含临时实例目录 `command-fault.json` 的单次故障点，生产构建不读取该文件；场景服务禁用 core dump。异 uid 场景只放宽其临时 socket 目录，用从属 uid 验证内核身份拒连。

GitHub 容器作业执行普通测试与 Schema 比对；真实 systemd 场景由具备上述条件的 Linux 席位运行。场景底座当前位于 `nd-daemon/tests/support`，#5 可在保留隔离与清理约定的前提下迁入通用 testkit。

命令场景在 `tests/commands.rs`：去重与散列冲突、无效/过期前置条件、墓碑重启、三个提交窗口崩溃、未受理退避、慢连接和正常连接保序、ndctl 提交与查询、冲突回应丢失、并发设备、组件卸载和 JSON 键顺序。所有断言经同步副本、真实进程、事件、快照和收据完成，不读内部表。原有 `tests/read_stream.rs` 继续验证只读流与隔离。

## 桌面模型目录查询

nd-wire 的 `models{id,backend,cwd}` 是只读查询，返回 `Reply.value` 中的 `Model[]`，类型与 Schema 由 `nd-wire` 生成。`SyncReplica::models` 和 `CommandClient::models` 是界面公共入口。目录须为已存在的绝对路径；当前只安装 Claude 适配，未安装后端返回错误。此查询不创建会话、不发送提示、不占全局引擎锁。

Claude 适配在守护进程 cgroup 内直接拉起短命辅助进程并报告 Up/Gone，查询时使用 `[claude]` 的固定 CLI、mod 与环境。隔离测试的守护进程沙盒也只读挂入钉住的 `/cli`，无须改用假进程。协议、桌面流程与实测范围见[桌面说明](../nd-desktop/README.md)。
