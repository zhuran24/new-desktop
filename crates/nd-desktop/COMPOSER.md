# 桌面输入框

状态：输入框状态机、GPUI 控件与离线验收入口已实现。真实豆包/Rime、候选框位置和性能由 owner 在日常桌面验收；合成事件不能替代它们。GPUI 和 Kit 沿用工作区锁定版本。

## 接入

`Desktop::composer()` 返回外壳底部的 `Entity<Composer>`。外壳已经订阅 `ComposerEvent::Action(Submit { text })`，经 `CommandClient` 和同步副本提交创建或发送命令；其他能力不能再登记第二个发送者。输入框本身没有后端通道。新建表单取得有效模型，或所选会话的快照允许发送时才启用发送；等待收据时仍可编辑。

- `Composer::set_send_enabled` 由会话能力根据准入更新；默认 false。按钮、命令面板都调用 `submit`，不能自行读取正文并绕过组词保护。
- `Composer::snapshot(window, cx)` 直接读取编辑器正文、焦点、marked range 与发送准入。`Changed` 供草稿组件观察编辑和焦点变化；它不是完整的 IME 生命周期通知，发送或 Esc 必须重新采样。
- 鼠标发送和会话导航在按下时调用 `begin_pointer_click`，点击完成时先检查 `finish_pointer_click`。守卫锁存最近一次 Composer 渲染中的组词状态，避免合成器先提交预编辑、再交付鼠标事件时放行同一次点击；被拦截时发出 `CompositionClickBlocked`，外壳提示先完成组词。KWin 6.7.5 自己提交拼音、重置候选的行为仍存在，范围与证据见 [#66 验证记录](../../docs/verification/issue-66-ime-clicks.md)。
- `editor()` 提供真正的 `TextareaState`，供平台输入、附件粘贴、草稿恢复、选择区操作。Kit `set_selected_range` 的单位为 UTF-8 字节；`EntityInputHandler` 的 range 为 UTF-16，不能混用。
- 提交动作保留正文，不代表后端受理。外壳先等草稿保存，再由守护进程在发送事务里按版本和正文清稿；界面只应用明确受理的对应修订结果，新编辑或正在组词时不清空。交付不明保留正文，不自动重投。已创建会话的草稿经 `session.draft.update` 持久化，版本冲突原文另存，详见 [桌面说明](README.md)；组词只留在编辑器。
- Esc 取消组词时删除 marked range、结束标记，并消费按键；不会向会话再发一个 Esc。非组词时发出中立 `Escape`，外壳按面板、活动回合、空闲双 Esc 分派；它经 nd-wire 发停止命令。长按同一个键只处理一次，释放后才处理下一次。
- 只对所属窗口里聚焦的输入框安装前置按键保护。订阅由实体持有，实体释放即撤回。父容器只接未被输入框消费的 Esc，不能绕过组词守卫或另装 Enter 发送器。
- `set_theme` 更新输入框的应用主题；外壳 `set_theme` 同时更新 Kit/Base 的字体、光标和选择区等颜色。视图不读 CLI 数据或解析 stdout。

当前平台不能可靠识别“同一确认键先 commit/清 preedit，再转交 Enter”的物理身份；没有用毫秒去抖猜测。若真机复现，记录该输入法、版本和按键顺序，按 ADR 0001 判定 GPUI 是否退到 gtk4-rs。

## 自动验证

在 ticket 工作树设置 E 盘 `CARGO_TARGET_DIR` 和 `CARGO_BUILD_JOBS=6`，每个 Cargo 命令套 BUILD.md 的 12 GiB scope。

```sh
cargo test -p nd-composer --locked
cargo test -p nd-desktop --test composer --locked
cargo build -p nd-desktop -p nd-daemon --features nd-desktop/scenarios --locked
python crates/nd-desktop/tests/native_smoke.py --composer \
  --bin-dir "$CARGO_TARGET_DIR/debug" \
  --output /mnt/wd_external/nd-build/tmp/ticket-10/native
```

纯函数测试验证键位决策；GPUI 测试经真实控件、平台输入 trait 和合成按键/鼠标验证组词守卫、UTF-16 范围、选择区换行、按钮保护与长按。原生冒烟在断网、临时 HOME/XDG、私有 D-Bus/虚拟 KWin 和独立限额 slice 中打开桌面与离线窗口，保存截图并验证实际 Wayland 缓冲提交。它们不声称豆包/Rime、候选框、上屏延迟或 CPU 已达标。

`scripts/test-scenarios.sh` 还运行 `composing_clicks_do_not_send_or_navigate_with_real_rime`：真实 fcitx5/Rime 在私有 KWin 中处理拼音与确认键，私有 fake-input 协议驱动鼠标和键盘；断言三处点击不误发、不导航，正文、编辑焦点和提示保持，重新完成组词后正常执行。场景不创建 uinput 设备，不连接日常 `wayland-0`。这个回归没有证明被 KWin 重置的候选可以保留。

## owner 真机入口

验收程序 `nd-composer-lab` 仅在 `scenarios` feature 下提供，复用产品控件。Enter/按钮将正文显示到窗口中的本地提交列表并清空；关窗丢弃。程序不连接守护进程或模型，不持久化文字。窗口同时显示本地提交数和非组词 Esc 数，便于观察误提交/重复动作。

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-10
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo build --release -p nd-desktop --bin nd-composer-lab --features scenarios --locked
systemd-run --user --scope --quiet -p MemoryMax=2G -p MemorySwapMax=0 -- \
  bash crates/nd-desktop/scripts/composer-lab.sh "$CARGO_TARGET_DIR/release/nd-composer-lab"
```

最后一条由 owner 在真实 Wayland 桌面运行。脚本只共享该桌面的 Wayland socket 和 GPU；使用断网沙盒、空环境、临时 HOME/XDG、私有 D-Bus，退出删除临时目录。真实输入法仍由桌面 compositor 的 text-input-v3 链路提供。脚本不会自动切换输入法或向日常窗口注入按键。

1. 记录二进制 SHA-256、Cargo.lock、输入法版本/方案、GPU、桌面缩放和显示器。豆包、Rime 分别做以下检查；Rime 的左右 Shift 分别测。
2. 输入拼音形成候选，按 Enter：正文上屏，本地提交数不变；松开后再按 Enter：只增加 1，正文清空。分别在组词中按 Shift+Enter、Alt+Enter：不能误提交或额外插入换行。非组词时这两个键各插入一个换行；鼠标选中中文/emoji 再换行，周围文本保持原样。
3. 在已有中文和 emoji 中间组词，按 Esc：候选消失、未提交文字删除，左右正文保留，两个计数均不增。长按 Esc 不能增加非组词 Esc 数；松开后再按一次才增加 1。组词中点击发送不能提交，候选和焦点不应被按钮破坏。
4. 检查候选框始终贴近输入位置：第二行、行中、自动折行、超过 9 行后滚动、窗口移动/缩放、100%/150% 或日常分数缩放、双屏切换。再检查粘贴长中文/emoji、长按删除、两窗口切焦点和豆包语音（若日常使用）；无残留 preedit 或吞键。明暗主题各做一轮。
5. 输入延迟用至少 240 fps 的相机同时拍物理按键与屏幕，或用同等可校准的输入事件与实际呈现帧记录。每个输入法/负载至少 100 个样本，记录 `input_ms,present_ms`，按 `present_ms-input_ms` 排序取第 `ceil(0.95*N)` 项，要求 ≤50 ms。仅量到 `notify`/`render` 不算上屏；接入 #14 后还需在后台流式输出时复测。
6. release 程序静置 60 秒，分别测有焦点空文本、有焦点非空 preedit、失焦三种状态。读取该程序 `/proc/<pid>/stat` 的 utime+stime，CPU% = 差值 / `getconf CLK_TCK` / 实际秒数 ×100，按一个核心计，要求每种 <1%。采样只读，不要在测量期间打开连续诊断日志。可直接执行 `python crates/nd-desktop/scripts/composer-cpu.py "$(pgrep -n -x nd-composer-lab)" --seconds 60`；保留输出 JSON 并注明三种状态。
7. 把每项结果、样本、截图/录像与失败复现写入实施记录的 owner_checklist。中文输入法验收不通过时按 `docs/adr/0001-rust-native-ui.md` 改用 gtk4-rs；未执行不能记为通过。
