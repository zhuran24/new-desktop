# #67：取消组词后的 Esc 长按

日期：2026-10-09。状态：**已复现，修复未完成；输入层方案待确定**。基线 `v1=c4632017fdce432fad4bdefd85a03402cc7b1a73`；工作树 `bug-67`，分支 `bug/67`。当前结论是产品输入路径不能满足 #67，不能报告 `status=not-a-product-bug`。

## 已验证的现象

真实 Fcitx 5.1.23、Rime Luna Pinyin、KWin 6.7.5 的私有虚拟显示环境中，产品 Composer 的离线实验窗在行中组词后按住 Esc 0.9 秒：正文仍为“左🙂右”，组词结束，非组词 Esc 从 0 变成 8。独立 EIS 键盘只注入一次按下和一次松开，自动重复由真实输入栈生成。此结果与原复核报告一致；不依赖 `tool/ime-bench` 的场景判定或内部输入注入。

| 实验 | 按键操作 | 实验窗结果 | 判定 |
|---|---|---|---|
| 原始现象复核 | 行中 Rime 组词，Esc 按住 0.9 秒 | 正文保留，非组词计数 0 → 8 | FAIL |
| 当前 v1 回归测试 | 相同操作，独立 EIS 键盘、当前产品构建 | 正文保留，非组词计数 0 → 8 | FAIL，修复前红灯 |
| 单个重复事件对照 | 组词中 Esc 按住约 0.615 秒 | 非组词计数 0 → 1 | FAIL，应保持 0 |
| 真正再次按下对照 | 组词中短按 Esc，松开，等待约 0.565 秒，再短按 | 非组词计数 0 → 1；之后再按一次变成 2 | PASS |

后两个对照使用当前产品构建。取消组词之后，前者的一个重复事件与后者的下一次新按键，都向 GPUI 提供一次 Escape KeyDown 和一次 KeyUp；两者的 Wayland 按键时间戳均为 0。第一组对照的期望值为 0，第二组为 1。GPUI 的公共按键接口未提供这些事件的物理来源。

EIS 在私有合成器的输入边界注入原始按键，不是程序内部合成事件，也不经过 uinput。此验证不能标为 owner 日常桌面或实体键盘验收。

## 输入路径与原因

1. 首次 Esc 被 Rime 用于取消组词，应用收到空预编辑，没有收到这次物理按下。
2. Fcitx 后续重复使用同一原始按键时间，向合成器发送 RELEASED/PRESSED。此行为在 Fcitx 5.1.23 的 V1 和 V2 前端源码中一致。
3. 当前 KWin 的 V1 转发函数没有将输入法提供的 `time` 设置到 seat，独立 EIS 运行的应用端按键时间戳同样全部为 0。因此时间戳为 0 不能仅归因于原工具的 fake-input。
4. 固定 GPUI 的 Wayland 后端对每个收到的 Pressed 构造 `is_held=false`，对 Released 构造 KeyUp。`KeystrokeEvent` 拦截接口进一步只提供 keystroke、action 和 context_stack。
5. `Composer` 在每次 KeyUp 清空 `pressed[Escape]`，下一次转发的 Pressed 因而成为一次新的 `ComposerAction::Escape`。取消组词时应用又没有收到初次按下，故不能仅通过修复组件的 held 布尔值补齐物理按键身份。

焦点丢失假设被本次复核排除：原始测量状态中的 `focused` 一直为 true，独立回归在每次按下与整个长按期间都核对私有 KWin 的活动窗口 PID。原工具“自己多次注入 Esc”的假设也被排除：独立键盘的注入日志中只有一次 Esc 按下和一次松开。

源码依据：

- 产品：[Composer 按键拦截](../../crates/nd-desktop/src/composer.rs)，`new` 中 pressed 的设置与 `Render` 中 `capture_key_up` 的清除。
- 固定依赖：本机 Cargo registry 的 `gpui-pre-linux-0.3.7/src/linux/wayland/client.rs:1943–2072` 与 `gpui-pre-0.3.7/src/app.rs:3205–3214`。本工单没有修改共享 Cargo 缓存。
- [Fcitx 5.1.23 V1 前端](https://github.com/fcitx/fcitx5/blob/5.1.23/src/frontend/waylandim/waylandimserver.cpp#L238)：`repeat()`；[V2 前端](https://github.com/fcitx/fcitx5/blob/5.1.23/src/frontend/waylandim/waylandimserverv2.cpp#L287)：相同重复机制。
- [KWin v6.7.5 输入法转发](https://github.com/KDE/kwin/blob/v6.7.5/src/inputmethod.cpp#L715)：`InputMethod::key`。应用的 registry 日志确认私有 KWin 提供 `zwp_input_method_v1`。本机 `/usr/lib/libkwin.so.6.7.5` 对应函数的反汇编也确认未保留或使用 time 参数，最终直接调用 `SeatInterface::notifyKeyboardKey`；这是已安装二进制的静态核对。

以上是当前环境的运行证据及对应版本静态源码核对。owner 日常桌面的实际输入路径没有运行验证。

## 回归接口与执行

接口：EIS 原生键盘 → 私有 KWin → 真 Fcitx/Rime → 产品 Composer 实验窗。只观察实验窗公开的正文、组词状态和可见计数，不调用内部输入处理函数。该接口由 #67 的“贯穿约定”及本次实施指令指定。

测试文件：

- [native_escape.py](../../crates/nd-desktop/tests/native_escape.py)：隔离环境、Rime 部署、窗口身份检查、真实输入与结果断言。
- [private_keyboard.c](../../crates/nd-desktop/tests/private_keyboard.c)：EIS 键盘，仅接受测试所需键码；拒绝非 `/sandbox/runtime`、非 `nd-test-ime` 的环境；退出时释放按键。
- [nd-composer-lab.rs](../../crates/nd-desktop/src/bin/nd-composer-lab.rs)：增加只读状态输出，观察真实编辑器并报告与可见计数相同的值。实验窗仅在 `scenarios` feature 下构建；生产输入处理逻辑没有改变。

在 bug-67 工作树执行：

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-67
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo build --locked -p nd-desktop --features scenarios
python -B crates/nd-desktop/tests/native_escape.py \
  --bin-dir "$CARGO_TARGET_DIR/debug" \
  --output /mnt/wd_external/nd-build/tmp/bug-67/recheck
```

需要系统的 `libei`、C 编译器、pkg-config、Python dbus/GLib、wl-clipboard、KWin、Fcitx/Rime、rime_deployer、bwrap 与 systemd 用户实例。当前基线该命令应失败，`result.json` 的 `after.escapes` 为 8，期望为 0。此测试尚未接入默认场景套件，修复后需作为真实输入法回归接入。

## 证据目录

全部位于 `/mnt/wd_external/nd-build/tmp/bug-67/`，不写入主目录。

| 路径 | 内容 |
|---|---|
| `red/result.json`、`red/input.jsonl`、`red/lab.wayland.log` | 当前产品的修复前失败结果、独立 EIS 注入回执、真实 Wayland 事件 |
| `red-final/` | 保留实验窗 readiness 输出兼容后的再次复现；回归仍失败 |
| `red/cleanup.json` | 私有 service/slice 无残留，临时根已删除 |
| `interface-hold/`、`interface-repress/` | 当前产品的两个按键操作对照，含输入与协议日志、结果、清理记录 |
| `hold-interface.py`、`repress-interface.py` | 对照实验脚本；只改变操作序列与独立期望值 |
| `reproduction/`、`reproduce.py` | 原测量构建的独立复现，辅助证据 |
| `fcitx-waylandimserver.cpp`、`fcitx-waylandimserverv2.cpp`、`kwin-6.7.5-inputmethod.cpp` | 对应版本的上游源码快照 |
| `kwin-key-disassembly.txt` | 本机已安装 KWin 库的目标函数反汇编 |
| `provenance.json` | 基线 revision 与关键文件 SHA-256 |
| `workspace.log`、`scenarios-service.log`、`clippy.log`、`schemas.log` | 常规检查日志；结果见下一节 |

## 验证状态与未完成项

`git merge v1` 返回 Already up to date，基线仍为 `c463201`。常规检查通过不能表示 #67 已修复；本工单的真实输入回归仍是红灯。

| 检查 | 结果 | 日志 |
|---|---|---|
| `cargo test --workspace --locked` | PASS：246 passed，0 failed，2 ignored | `workspace.log` |
| `scripts/test-scenarios.sh` | PASS：177 passed，0 failed，1 ignored | `scenarios-service.log` |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | PASS | `clippy.log` |
| `cargo fmt --all -- --check` | PASS | `fmt.log` |
| `scripts/check-schemas.sh` | PASS | `schemas.log` |
| `native_smoke.py --composer` | PASS：实验窗首行 readiness 契约与真实 Wayland buffer、冷启动和守护进程恢复 | `native-observer-smoke.log` |
| #67 真实 Rime 回归 | **FAIL：0 → 8，修复未完成** | `red/`、`red-final/` |

场景套件首次从实施 shell 执行时，异 UID 验证被继承的 `NoNewPrivs=1` 阻止，错误为 `newuidmap: Could not set caps`。在独立的 `nd-test-bug67-scenarios` 用户服务中，以正常用户服务环境完整重跑通过；仍使用 bug-67 构建目录、6 jobs、12 GiB/零 swap 限额，场景自身沿用真实隔离实例。服务环境调整只对本次测试生效。记录分别在 `scenarios.log`、`uid-check.log` 与 `scenarios-service.log`。

要完整满足 #67，输入层需要在输入法消费按键之前保留物理按下、重复和松开的身份。候选方向是应用直接对接 Fcitx D-Bus，或修复上游转发协议使这些信息可传到应用；两者均尚未实施验证。单纯给 Composer 增加时间防抖不能可靠区分上述两个操作。

未完成：输入层方案与修复、真实输入回归绿灯、产品窗的面板和回退菜单长按回归、修复后的全量检查。工单保持未解决。

`owner_checklist`：`OWNER_PENDING`。输入层修复及自动回归通过后，才在 owner 日常桌面确认 Rime/豆包的实际路径，以及取消组词后长按、短按后立即再按、延后再按、面板关闭、空闲双 Esc 和回合中断。当前没有在 `wayland-0` 注入事件或打开窗口。
