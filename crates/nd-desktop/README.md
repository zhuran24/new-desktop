# 桌面聊天界面

日期：2026-10-06。状态：Claude 会话创建、侧栏、流式 Markdown/代码块、持久草稿、附件、diff 和界面冷启动恢复已实现。真实输入法、上屏延迟、空闲 CPU 与真模型对话保留为人工验收。

依赖锁定为 `gpui-pre 0.3.7` 和 gpui-kit Git 提交 `4c7f1350331562436df868c55ac33bebc4c6406c`。桌面 crate 属于 workspace，位于 `default-members` 之外；没有使用 ime-lab 观测补丁。

## 构建与使用

在自己的工作树内执行；构建资源照实施资料 `research/impl/BUILD.md`。

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-14
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo build -p nd-desktop --locked
"$CARGO_TARGET_DIR/debug/nd-desktop" --socket /path/to/nd.sock --state /path/to/ui.json
```

守护进程的 Claude 和看守配置见[守护进程说明](../nd-daemon/README.md#会话与-claude-后端)。桌面不负责启动或停止守护进程。

1. 点击「新建会话」，填写守护进程可见的工作目录绝对路径。
2. 点击「获取后端模型列表」，选择 CLI 返回的可用模型。修改目录后旧列表失效，需要重新获取；目录不存在、后端未配置或查询失败时提示原因并禁止创建，不填入猜测的模型。
3. 在下方输入首条消息，Enter 或发送按钮提交。等待明确受理时保留正文；新建成功自动选中会话。侧栏显示准备中、可对话、部分完成，以及撤掉会话的一次性失败提示。
4. 点击侧栏切换会话，继续输入消息。正文按 `data.seq` 排列；增量和完整块共用身份，完整块替换累计文字。Markdown 支持未闭合的流式代码围栏；未知条目保留后备文字。停在底部时跟随新输出，上滚阅读时保持位置。

Enter 组词保护和 Shift/Alt+Enter 换行复用产品输入框，见 [COMPOSER.md](COMPOSER.md)。当前发送使用默认意图 `fold`；三种意图、撤回与停止回合的交互归 #18。部分完成的创建保留原因，当前禁止继续发送，未决处置入口归 #33。长历史分页、虚拟化与滚动锚点归 #20。

已创建会话的输入会自动保存到守护进程，输入框旁显示保存状态。另一界面更新时，空闲编辑器同步到最新稿；本地未保存文字和输入法组词受保护。两台基于同一版本修改时，落败稿另存，在输入框上方可查看原文并点击「载入这份草稿」。保存结果不明时文字留在窗口，重连或点击「重试保存草稿」只查询原收据，查不到仍标未确认；只有明确未受理的 `unavailable` 才可用原命令重试。窗口崩溃前尚未送达守护进程的文字不属于已保存草稿。新建表单尚未有会话身份，首条消息提交前的文字仍只在该窗口。

输入后立即发送会先等对应草稿保存，再受理发送与清稿；等待期间修改正文或发生版本冲突，会取消这次待发送意图，提示核对后再发。发送已经受理时，原草稿清空来自同一守护进程事务，不会清除后来的编辑。组词中不向守护进程保存 preedit，也不能切换会话或载入另存稿；完成组词后再操作。

默认 socket 为 `$XDG_RUNTIME_DIR/new-desktop/nd.sock`。设备偏好位于 `$XDG_STATE_HOME/new-desktop/ui.json`，未设时为 `~/.local/state/new-desktop/ui.json`。`--socket`、`--state` 可连接隔离实例；`--quit-after` 用于冒烟。不同窗口应使用不同状态文件，文件锁保证每份偏好只有一个写入者。偏好不包含快照、纪元、游标或消息正文。

## 接口与寿命

- `nd-ui-core::ReplicaFeed`：专用 Tokio 线程驱动 `SyncReplica`；容量为 1 的通道传完整累计快照，满了就等待。外壳分别订阅 `global` 和选中会话的 `session/<id>`；切走会话即撤回旧订阅。冷启动必须取快照；已有副本的短断线由同步副本按游标恢复。关闭界面不结束后端。
- `nd-ui-core::CommandClient`：容量为 8 的请求队列、独立 Tokio 线程；`models`、`command` 和 `receipt` 可在任意 executor 等待。所有命令复用 `SyncReplica::command` 的收据与断线规则；没有界面私有协议。模型查询不阻塞流式接收。
- `nd-view-model::{sidebar, conversation}`：从公开快照生成显示数据；简单的完整投影保留作以后优化的差分基准。`Draft` 用修订号保护受理后的清空；新编辑、不同会话和未提交组词不能被旧收据清掉。
- `Desktop::{begin_create, select_session}`：切换视图、草稿及订阅。会话选择应使用此入口；`update_view_state` 用于其他设备偏好。`composer()` 返回已接好发送者的输入框，不应重复订阅发送。
- `Desktop::slots` 的 `Renderer` 接收 `Presentation{snapshot,state,theme,item}`。会话条目的快照为会话流；外壳槽位为 global。`Sidebar`、`Item(kind)`、`Header`、`RightPanel`、`Settings`、`CommandPalette` 保留，条目渲染器优先于内置 Markdown/后备文本。
- 可选组件用 `configure_component(name, contributions, enabled, cx)`，卸下全部撤销；内置 `overview` 只提供「本机」徽章。聊天是常驻外壳能力。本单不新增会话结构操作，创建与发送沿用 #13 的引擎和崩溃矩阵。
- `set_theme` 同时投影应用与 Kit/Base 主题。颜色、字体、字号、间距、圆角、边框都取主题变量。主题文件加载和错误回退由 #23 实现。

## 模型目录

新增 nd-wire 请求 `models{id,backend,cwd}`，返回 `Reply.value: Model[]`。`Model{value,label,description,disabled}` 的 Rust 定义在 `nd-wire`，Schema 为 `protocol/models.schema.json`；`value` 必须原样用于创建，不能把 label 当模型 ID。首次创建前就可查询，无需现有会话。

守护进程经 `Sessions → Backends → BackendAdapter::models` 查询。Claude 适配在目标目录启动短命辅助进程，使用固定启动环境和两个 mod；等待 hello 后仅发送 initialize，读取 `models[]`/`unavailable_models[]`。辅助进程在守护进程 cgroup 内，报 Up/Gone，查询结束确认退出并撤回 mod 登记；不发提示、不走看守、不写产品会话。关闭查询连接不打断清理。完整账号/额度和通用辅助进程管理归 #30 及辅助进程相关工单。

## 自动验证

```sh
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test --workspace --locked
bash scripts/test-scenarios.sh
```

`nd-daemon/tests/sessions.rs` 的 #14 场景通过真实守护进程、CLI 2.1.289、两个 mod、看守、systemd 与 SQLite 验证模型目录、流式冷启动和第二轮对话。`native_chat_window_creates_and_recovers_during_streaming_markdown` 另起私有 D-Bus/虚拟 KWin，通过真实 GPUI 创建表单和 Composer 提交消息，流式代码块期间 SIGKILL 界面并重开，保存明暗截图和实际 Wayland 缓冲提交证据。

脚本自动构建 scenarios 版桌面并设置 `ND_TEST_DESKTOP`。可设 `ND_NATIVE_OUTPUT=/mnt/wd_external/nd-build/tmp/ticket-14/native` 保留截图和清理记录。测试需要 KWin、Spectacle、bwrap、systemd 用户实例及 `/dev/dri`，不操作 owner 的显示会话。每场景断网、临时 HOME/CLAUDE_CONFIG_DIR/XDG、独立限额 slice；模型只访问离线伪端点。

`draft_native_windows_save_reopen_follow_and_recover` 验证真实输入、SIGKILL 后恢复、双窗口同步、载入落败稿和发送时清稿。`ND_NATIVE_DRAFT_OUTPUT` 可保留其截图和清理证据，接口及验证详见 [#16 记录](../../docs/verification/ticket-16.md)。

`scenarios` 下的 stdout 副本/编辑器观测、`--scenario-create` 和 `--scenario-draft` 仅用于上述隔离测试；生产构建没有自动输入入口、不打印对话。原生冒烟不能证明豆包/Rime、真实上屏性能、静止 CPU 或真模型服务。
## 附件与 diff

粘贴图片、复制文件后粘贴，或拖到输入区，上传完成后可发送。支持 PNG/JPEG/GIF/WebP、PDF、UTF-8 文本，每个最多 5 MiB、每条最多 8 个且总计不超过 16 MiB。只有附件也能发送。失败保留草稿；移除只影响当前草稿。发送后的图片从守护进程加载，其他文件保留名称和散列引用。Wayland 复制文件粘贴需要 `wl-clipboard`（`/usr/bin/wl-paste`），图片/文字由 Kit 处理，拖放无需该程序。

正文中的 diff/patch 围栏与 Edit 替换片段显示增删、行号和无末尾换行标记。Edit 行号是片段内的位置。diff 颜色由 `Theme.colors.diff_added/diff_removed` 提供。详情、协议字段和证明边界见 [#17 验证](../../docs/verification/ticket-17.md)。

`CommandClient::upload(AttachmentSource)` 与 `blob` 使用同一个 nd-wire UDS 的 HTTP 通道；没有桌面私有文件发送路径。输入框只发一次 `ComposerEvent::Attach`，宿主仍只有原来的一个 Submit 接收者。已有会话的草稿与冲突另存稿都持久保存附件；发送只清匹配文字、附件和版本的那份草稿。创建会话前的临时草稿尚未持久化。

## 总结、`!` 模式、/subtask 与降级提示

- **`!` 命令**：已创建会话的输入框里以 `!` 开头的一段文字是一条 shell 命令（`! pwd` 与 `!pwd` 相同），提交时发 `session.shell`（`nd_wire::ShellArgs`，`input` 带输入框原文与草稿版本）。守护进程受理时清掉匹配的草稿，输入框跟着快照变空；命令跑完在对话里显示一条「! 命令」，带命令、输出和退出码，模型下一次请求读得到。它本身不起回合；当前回合进行中提交的，等这一回合结束再跑。
- **`/subtask <要做的事>`**：派 fork 型子代理（`session.subtask`），带着当前对话在后台跑；对话里显示子代理 id，子代理面板归 #28/#34。
- **总结**：还在当前对话里的人类提示下方有「从这里总结」「总结到这里」（`session.compact`，`nd_wire::CompactArgs`）。成功后前者把该提示原文放回输入框，后者让输入框留空；被替换的旧稿另存，可在另存稿里取回。提示定位不到或已被总结时不压缩，界面提示原因。
- **降级提示**：后端进程只能聊天（mod 没装上等）时，会话头下方列出原因和这时用不了的功能（名单来自端口能力表），总结按钮隐藏，`!` 与 `/subtask` 不发出、正文留在输入框并提示原因。
- 三个命令的收据等动作有结果：经 `CommandClient::deliver` 另开连接等（最长 15 分钟），等的时候草稿保存等别的命令照常走；到时限没有回应只查收据、不重发正文。`!`/`/subtask` 带附件时提示先移除附件。
- 视图计算在 `nd_view_model::{composer_input, conversation}`（`ConversationView.degraded/abilities`、`MessageView.summarize`），纯函数测试在 `crates/nd-view-model/tests/invocations.rs`。真窗口场景 `bang_runs_one_command_…` 与 `without_the_hook_mod_…`（`crates/nd-daemon/tests/invocations.rs`，驱动 `tests/native_chat.py --invoke`）核对产品输入框里的 `!` 跑完并清空、降级进程的会话头提示与拒绝。
