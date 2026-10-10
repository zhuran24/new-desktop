# #70：Wayland 文件拖放附件

日期：2026-10-09。状态：修复及自动交付检查完成；日常桌面复核为 OWNER_PENDING。

## 结论与触发条件

这是产品问题。GPUI 0.3.7 在外部 `FileDrop` 到达时保留窗口此前的输入模式。若最后一个到达产品的输入是键盘事件，`on_drop` 使用的 `Hitbox::is_hovered` 会因为键盘模式返回 false。Wayland 的文件数据传输已经完成，Qt 源报告 CopyAction，产品却没有调用附件上传。

空草稿是常见复现条件，必要条件是此前的键盘输入模式。原 ime-bench 检查用 Ctrl+A / Backspace 清空草稿，恰好触发这一条件。只经 Rime 组词并上屏的对照能通过，因为组词按键被输入法消费，没有把产品切到键盘模式。清空后先在产品内移动一次真实指针，也能让未修产品通过。

固定依赖的静态依据：`gpui-pre-0.3.7/src/window.rs` 的 `Window::dispatch_event`（5415 行）只为 KeyDown、MouseMove/MouseDown、Touch 切换模式；`HitboxId::is_hovered`（773 行）在键盘模式直接返回 false；`src/elements/div.rs` 的 `on_drop` 分发（2897 行）要求该悬停检查通过。运行时依据是下表的普通 release 失败与仅增加指针移动的成功对照。

工单来源为 [GitHub #70](https://github.com/zhuran24/new-desktop/issues/70)、本机 `research/impl/ime-bench-review.md` 和 `/mnt/wd_external/nd-build/tmp/ime-bench-review-evidence/`。本次独立证据目录为 `/mnt/wd_external/nd-build/tmp/bug-70/`。

## 修复

`crates/nd-desktop/src/attachments.rs` 的 `attachment_drop_target` 接收真实 `ExternalPaths` 拖动。左键松开时用 `Hitbox::is_hovered_at` 对实际落点做命中测试，保留 GPUI 对遮挡和裁剪的判断，避开键盘模式的悬停抑制。命中后结束当前拖放，再进入原有附件上传、格式校验和草稿保存路径。

拖动离开窗口时清除路径；每次左键松开也消费暂存路径。拖放在附件区域外完成，或随后发生普通点击，不会上传这份文件。实现位于界面的 GPUI 适配层，协议及 Schema 不变，符合 ADR 0013、0014。

## 测试接缝与证据

回归测试为 `nd-daemon/tests/sessions.rs` 的 `native_wayland_drag_adds_chinese_space_named_files_and_images_to_the_durable_draft`，驱动脚本为 `nd-desktop/tests/native_drag.py`。

它经真实 Qt QDrag、私有 KWin 的原生指针/键盘接口、真 fcitx5/Rime 操作产品。通过公开 ndctl/nd-wire 接口核对草稿和 blob；不直接调用产品输入处理函数。场景先用 Rime 提交“你好”，再用 Ctrl+A / Backspace 清空草稿，让最后输入明确为键盘。验收包括：

- 中文及空格文件名的 UTF-8 普通文件、PNG 图片各成为一个附件。
- 公开草稿保存正确名称、MIME 类型；blob 接口回读普通文件全文并验证 PNG 签名。
- 草稿正文不变，拖放和 Rime 上屏不新增提示。
- 第三个文件拖出产品窗口后取消，再普通点击输入框，附件仍只有两个。

| 运行 | 结果 | 证据目录或日志 |
| --- | --- | --- |
| 原 ime-bench 定向拖放 | Qt CopyAction，附件为空 | `original-drag/`，含公开草稿快照 |
| 未修普通 release，清空草稿后拖放 | FAIL，公开附件列表为空；协议已有 receive/drop/finish | `plain-v1-empty/` |
| 同一未修 release，清空后先移动指针 | 两类附件成功 | `plain-v1-pointer/` |
| 修复前定向回归测试 | FAIL，`dropped file did not become an attachment` | `red-native-empty.log`、`red-native-empty/` |
| 修复后同一回归测试 | PASS | `green-native-empty.log`、`green-native-empty/` |
| 合并最新 v1 后，公开接口回归及取消拖放 | PASS | `green-merged-native.log`、`green-merged-native/` |
| 最终全场景中的本回归 | PASS | `final-scenarios.log`、`final-native/` |
| 最终普通 release，本回归含取消拖放 | PASS | `final-release-native.log`、`final-native-release/` |

复现基线为 `78959374bd4334a54eb02fdf2194763000cd1674`。集成基线为 `e8840effdfcc05e52bfc0256cf6adaa81dc53912`。基线与修复构建产物、协议日志和截图都保存在上述独立证据目录。

完整场景脚本默认包含本回归。单独复跑：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-70
export CARGO_BUILD_JOBS=6
export ND70_NATIVE_OUTPUT=/mnt/wd_external/nd-build/tmp/bug-70-rerun
scripts/test-scenarios.sh native_wayland_drag_adds_chinese_space_named_files_and_images_to_the_durable_draft -- --exact --nocapture
```

普通 release 定向验证（在上述构建及场景运行准备好守护进程、看守与 ndctl 后）：

```bash
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- cargo build --release --locked -p nd-desktop
export ND_TEST_DAEMON="$CARGO_TARGET_DIR/debug/nd-daemon"
export ND_TEST_WATCHDOG="$CARGO_TARGET_DIR/debug/nd-watchdog"
export ND_TEST_DESKTOP="$CARGO_TARGET_DIR/release/nd-desktop"
export ND70_NATIVE_OUTPUT=/mnt/wd_external/nd-build/tmp/bug-70-release-rerun
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- cargo test --locked -p nd-daemon --features scenarios --test sessions native_wayland_drag_adds_chinese_space_named_files_and_images_to_the_durable_draft -- --exact --nocapture
```

`scripts/test-scenarios.sh` 会把 `ND_TEST_DESKTOP` 设为自己的 scenarios 构建，不能用它覆盖普通 release 的定向验证。

## 交付检查

构建目录为 `/mnt/wd_external/nd-build/target/bug-70`，6 jobs；cargo build/test 在独立的 MemoryMax=12G、MemorySwapMax=0 scope 中运行。检查日志为证据目录的 `final-*.log`，退出码集中在 `validation-status.txt`。

全场景首次运行在异 UID 检查处遇到 `newuidmap: Could not set caps`，原因是调用进程继承 `NoNewPrivs=1`。最终检查从独立 systemd 用户服务启动，实测服务内 `NoNewPrivs=0`，子 cargo 仍按构建约定使用限额 scope。此处没有修改产品、放宽 owner 目录权限或使用 sudo。

| 检查 | 结果 | 日志 |
| --- | --- | --- |
| `cargo test --workspace --locked` | PASS，退出码 0 | `final-workspace.log` |
| `scripts/test-scenarios.sh` | PASS，退出码 0 | `final-scenarios.log` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS，退出码 0 | `final-clippy.log` |
| 上述 clippy 加 `nd-daemon/scenarios,nd-testkit/scenarios,nd-claude/scenarios,nd-desktop/scenarios` | PASS，退出码 0 | `clippy-scenarios.log` |
| `cargo fmt --all -- --check` | PASS，退出码 0 | `final-fmt.log` |
| `scripts/check-schemas.sh` | PASS，退出码 0 | `final-schemas.log` |
| 普通 release 定向回归 | PASS，退出码 0 | `final-release-native.log` |

原有手动 OOM 及日常桌面性能测试保留 ignored；它们不属于 #70 的私有拖放验收。普通 release 截图 `final-native-release/dropped-1.png` 可见两条附件名称和 PNG 预览。`provenance.json` 保存源文件及基线/修复二进制 SHA-256。

## 隔离及 owner_checklist

所有窗口、Rime 数据与输入事件位于 bwrap 中的私有虚拟 KWin、D-Bus、HOME 和 XDG 目录。测试不绑定 owner 的 Wayland/X11/D-Bus socket，不创建 uinput 设备，不访问 owner 的 Claude/Codex 凭据或会话。原生输入 helper 只接受 `/sandbox/runtime` 下的 `nd-test-header` 私有 socket；同名 socket 位于各自独立沙箱，不能连接 wayland-0。后端使用钉住的 Claude CLI、真实守护进程/看守和断网伪模型端点。

每次原生运行的 `cleanup.json` 记录 transient 单元/slice 消失、临时根目录删除。`final-cleanup.json` 还核对诊断用离线实例的根目录和单元已删除，owner KWin/fcitx5 的 PID 与启动时刻均未改变。

- `OWNER_PENDING`：由 owner 安排原 ime-bench 工具在日常 wayland-0 桌面复核中文空格文件名的普通文件和图片，确认附件显示与草稿保存。本次私有运行不声称完成日常桌面验收。
