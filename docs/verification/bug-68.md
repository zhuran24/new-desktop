# #68：聚焦产品窗口的空闲 CPU

日期：2026-10-09。状态：修复已实现，整套检查与最终嵌套验收进行中。真桌面复核为 `OWNER_PENDING`。

## 问题与修复

[工单 #68](https://github.com/zhuran24/new-desktop/issues/68)要求聚焦空输入框、保持拼音预编辑、失焦三种状态的产品进程 CPU 均低于一个核心的 1%。独立复现确认这是产品问题：一轮短历史约 0.40%，历史页填满后约 2.40–2.57%，失焦为 0%。

固定版 Kit 的光标每 500 ms 通知编辑实体重绘。GPUI 将其祖先视图标记为脏，原来的桌面根视图随之重建历史正文。修复将正文滚动区放入独立的 `ConversationPane`，通过 GPUI 的 `Entity::cached` 复用绘制结果；聊天输入框在该视图之外。正文观察桌面实体的事实通知，快照、主题、设置、附件与组件变化仍更新；缓存边界尺寸或文本样式变化也会失效。正文自身的交互继续经过原来的桌面动作与滚动接口。

实现位于 [chat.rs](../../crates/nd-desktop/src/chat.rs) 与 [lib.rs](../../crates/nd-desktop/src/lib.rs)。输入法和光标闪烁继续使用固定版 Kit，依赖与 nd-wire 未变。方案遵守 ADR 0001、0002 和 0014。

## 外部回归入口

```bash
CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-68 \
ND_NATIVE_CPU_OUTPUT=/mnt/wd_external/nd-build/tmp/bug-68/acceptance-new \
ND_NATIVE_CPU_ROUNDS=1000 \
bash scripts/test-idle-cpu.sh
```

输出目录必须是新目录。脚本构建普通 release 桌面、守护进程和看守，然后单独运行 `focused_product_idle_cpu_stays_below_one_percent_with_real_rime`。该测试因需要真实 GPU、KWin/Rime 和三段 60 秒采样而标为 ignored，不使用 debug 构建的 CPU 作性能结论。

测试通过同步副本连接真守护进程，使用钉住的 Claude CLI 2.1.289、两个 mod、看守、systemd 与 SQLite 生成历史；只有模型端点是离线伪端点。私有 KWin 的 fake-input 驱动真实 Fcitx/Rime，不调用产品输入处理函数。默认生成 40 轮，填满公开历史页的 60 条正文；`ND_NATIVE_CPU_ROUNDS=1000` 覆盖原报告的千轮历史。

每种状态静置 15 秒后采样 60 秒，以 `/proc/<pid>/stat` 的进程 CPU ticks 除以实际墙钟时间，百分比按一个核心计。采样期间检查进程身份、实际活动窗口；预编辑状态还持续检查真实候选窗。先经 Rime 上屏「你好」、从公开草稿读回并清空，证明输入框已取得焦点。最后再次真实输入，读回「你好你好」，并从合成器截图差分确认可见光标仍闪烁。

`ND_NATIVE_CPU_SECONDS=5` 仅用于快速诊断，结果标为非正式采样，不能代替 60 秒验收。

隔离使用临时 HOME/CLAUDE_CONFIG_DIR/XDG、断网 bwrap、专用限额 slice 和私有 D-Bus。仅绑定离线测试守护进程的 socket，拒绝网络命名空间未隔离或不属于 `nd-test-` cgroup 的守护进程。输入器只接受 `/sandbox/runtime/nd-test-idle-cpu`，不创建 uinput，不绑定 owner 的显示 socket、输入设备或总线。Rime 使用系统 Luna Pinyin 数据。临时单元与目录由测试回收。

## 已记录的红绿证据

证据根目录：`/mnt/wd_external/nd-build/tmp/bug-68/`。修复前产品来自 `v1 c4632017fdce432fad4bdefd85a03402cc7b1a73`，二进制副本为 `nd-desktop-before`，SHA-256 为 `cca4683764fa9c38513d9db2798ee777a2af022c410ee6c3a11777f35fa955cf`。

| 状态 | 修复前，40 轮，每格 60 秒 | 修复后，40 轮，每格 5 秒，仅快速诊断 |
|---|---:|---:|
| 聚焦空框 | 2.40% | 0.60% |
| 保持预编辑 | 2.57% | 0.20% |
| 失焦 | 0.00% | 0.00% |

`red-ready-fast.log` 与 `red-ready-fast/result.json` 捕获真实输入前置条件通过后，聚焦空框 2.80% 导致的明确失败。`red-ready-60.log` 与 `red-ready-60/result.json` 完成三种状态、真实上屏与光标截图检查，最终因 CPU 门槛失败。`green-fast.log` 与 `green-fast/result.json` 全部通过，光标差分为 2×16 像素。

最终千轮 60 秒回归：待完整通过后填写。一次千轮运行的 CPU 已全部达标，但守护进程夹具的默认 300 秒到期清理中断了最后输入读回；该次运行不计作整体验收。此工单夹具的寿命为 900 秒，其他场景仍采用原有默认值。

环境：KWin 6.7.5、Fcitx 5.1.23、fcitx5-rime 5.1.16、librime 1.17.0；虚拟输出 1400×900，产品内容区 1050×850。证据目录包含 binary.json、逐秒 CPU 样本、候选截图、光标差分截图、窗口身份、owner 进程身份与 cleanup.json。

## 交付检查

`git merge v1` 已执行，基线为 `c4632017fdce432fad4bdefd85a03402cc7b1a73`。检查使用外置 target、6 jobs、12 GiB/零 swap scope；日志在证据根目录。

| 检查 | 日志 | 状态 |
|---|---|---|
| `cargo test --workspace --locked` | workspace.log | 进行中 |
| `bash scripts/test-scenarios.sh` | scenarios.log | 待执行 |
| `cargo clippy --workspace --all-targets --features nd-daemon/scenarios,nd-testkit/scenarios,nd-claude/scenarios,nd-desktop/scenarios --locked -- -D warnings` | clippy.log | 待执行 |
| `cargo fmt --all -- --check` | fmt.log | 通过 |
| `bash scripts/check-schemas.sh` | schemas.log | 待执行 |

## owner_checklist

- `OWNER_PENDING`：在 owner 安排的真桌面验收时段，经 ime-bench 复核普通 release 产品的聚焦空框、保持预编辑和失焦状态，每格至少 60 秒且 CPU <1%，确认输入后光标仍可见、正常闪烁。保留实际使用二进制的 SHA-256、原始采样与输入法状态。工具的真桌面安全缺陷须由其工单先完成验证；参考本机 `research/impl/ime-bench-review.md` 与工具分支的当前验证记录。

嵌套验收覆盖真实 KWin/Fcitx/Rime 软件路径，未替代日常桌面、豆包、物理输入手感或输入到物理显示的延迟验收。本工单执行不向 `wayland-0` 注入事件或打开窗口。
