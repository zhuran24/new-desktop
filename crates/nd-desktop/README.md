# 桌面界面外壳

GPUI 原生窗口，依赖锁定为 `gpui-pre 0.3.7` 和 gpui-kit Git 提交 `4c7f1350331562436df868c55ac33bebc4c6406c`。桌面 crate 属于 workspace，但不在 `default-members` 中。没有使用 ime-lab 的观测补丁。

当前显示 `global` 流的快照及未知条目的后备文字。底部提供产品输入框；会话发送和历史视图由后续能力组件接入。输入框接口、测试和离线真机验收见 [桌面输入框](COMPOSER.md)。

## 构建与运行

在自己的工作树内执行；构建资源约定见实施资料 `research/impl/BUILD.md`。

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-7
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo build -p nd-desktop --locked
"$CARGO_TARGET_DIR/debug/nd-desktop"
```

默认连接 `$XDG_RUNTIME_DIR/new-desktop/nd.sock`；每设备视图状态位于 `$XDG_STATE_HOME/new-desktop/ui.json`，未设 XDG_STATE_HOME 时使用 `~/.local/state/new-desktop/ui.json`。可用 `--socket PATH --state PATH` 连接隔离实例；`--quit-after SECONDS` 用于窗口冒烟。第二个使用同一状态文件的界面会被写锁拒绝，独立窗口应使用不同的状态文件。

界面只连接守护进程，不负责拉起或停止它。网络不可达时后台重试；冷启动不使用磁盘游标，每次先订阅完整快照。内存副本仍在的短断线使用 `SyncReplica` 的恢复规则，纪元变化时取新快照。关窗只交还连接。

## 能力视图接口

- `nd-view-model::Slots<T>`：`Sidebar`、`Item(kind)`、`Header`、`RightPanel`、`Settings`、`CommandPalette`。`register` 返回守卫，Drop 同步撤销；外壳等内核变化通知后重绘，不轮询。每帧按顺序和能力名取槽位；同类条目有多个渲染器时取第一个。
- `Desktop::slots` 的值类型是 `Renderer`：`Rc<dyn Fn(&Presentation, &mut Window, &mut App) -> AnyElement>`。`Presentation` 含当前快照、视图状态、主题及可选条目。未知种类没有渲染器时显示 `fallback.title/text`。
- 可选组件使用 `Desktop::configure_component(name, contributions, enabled, cx)`。启用状态写入 `ViewState.components`；卸下撤销全部槽位，名称冲突返回错误并撤销本次部分登记。直接登记的组件自己持有守卫。内置 `overview` 组件提供空会话提示和“本机”徽章。
- `Desktop::update_view_state` 修改窗口/分栏、选中会话、滚动锚点、会话树看法、活动面板等呈现偏好，并通知后台保存。`active_panel="settings"/"commands"` 时呈现对应槽位。这些偏好不改变守护进程事实。
- `Desktop::set_theme` 应用完整主题变量并同步 Kit/Base 主题。外壳渲染的颜色、字体、字号、间距、圆角、边框、阴影都来自 `nd-view-model::Theme`；分栏宽度是用户的视图状态。#23 可从主题文件构造 Theme；文件监视、坏主题回退、跟随系统由 #23 实现。

`nd-ui-core::ReplicaFeed` 在专用 Tokio 线程驱动现有 `SyncReplica`，通过容量为 1 的队列送完整累计快照，供 GPUI 等任意 executor 等待。队列满时等待，不丢弃事实；守护进程因背压断开后仍经副本规则恢复。Drop 请求停止；`close().await` 等到尽力发送 Bye 并退出。现在外壳订阅 global；后续多流和命令队列应扩展这个同步层，继续复用 `SyncReplica::command/receipt/get`，不能在能力视图实现另一套协议或正文重发。

状态文件持有独立锁，原子替换 JSON；写入在单独后台线程串行进行，只合并最新偏好。损坏文件在窗口底部显示提示并使用默认值。文件不保存快照、纪元或游标。

## 自动验证

```sh
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test --workspace --locked
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test -p nd-daemon --features scenarios --test desktop --locked
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo build -p nd-desktop -p nd-daemon --features nd-desktop/scenarios --locked
python crates/nd-desktop/tests/native_smoke.py \
  --bin-dir "$CARGO_TARGET_DIR/debug" \
  --output /mnt/wd_external/nd-build/tmp/ticket-7/native
```

主接缝测试使用真守护进程、SQLite、UDS、systemd 和断网沙盒。原生冒烟另起私有 D-Bus 和虚拟 KWin，运行真 GPUI 窗口；比较渲染时的快照与独立 ndctl 查询，杀界面、重启守护进程，检查 Wayland 缓冲提交并保存明暗截图。所有 HOME/XDG/CLI 配置都隔离，单元以 `nd-test-` 开头，运行后清理。需要本机 KWin、Spectacle、bwrap 和 `/dev/dri`；它不会向日常桌面发送输入。

`scenarios` 仅测试构建启用，提供渲染快照观测 stdout；生产构建不输出会话数据。该冒烟不证明真实输入法、端到端输入延迟、空闲 CPU 或真模型对话。
