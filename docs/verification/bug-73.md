# #73：窗口变化后的输入法候选位置

日期：2026-10-10。分支 `bug/73`。集成基线 `v1=48e9bc20925119f5ac3aebc7c7322cd019257e0e`。状态：修复及全部自动交付检查通过；日常桌面复核为 `OWNER_PENDING`。

## 原因与修复

GPUI 0.3.7 的布局与平台输入法状态之间缺少自动坐标刷新。Wayland 输入法事件会查询候选位置，但单独缩放窗口不会重新提交光标矩形。Kit 已经在绘制时更新编辑器布局并通知几何变化；产品此前没有调用 GPUI 的显式坐标失效接口。

[Composer](../../crates/nd-desktop/src/composer.rs) 在原有编辑器观察回调里，对当前获得焦点的编辑器调用 `Window::invalidate_character_coordinates()`。Kit 绘制后的几何通知使下一帧使用新布局上报位置。输入法的 marked range、光标所在视觉行、窗口坐标和 Wayland 提交仍由 GPUI 处理，产品不自行偏移候选框。

修复位于产品 GPUI 适配层，无需修改依赖或协议。上游报告为 [GPUI 候选位置草稿](../upstream/gpui-ime-position-after-layout.md)，未发布。报告引用的 GPUI 源码与已发布 crate、固定 Zed 快照逐字节一致；未对较新 GPUI 版本作运行结论。

## 回归边界与结果

[native_ime_position.py](../../crates/nd-desktop/tests/native_ime_position.py) 经私有双输出 KWin 的原生键盘/指针接口操作真 Fcitx/Rime 产品窗。测试接入 [sessions.rs](../../crates/nd-daemon/tests/sessions.rs) 的 `real_rime_candidates_follow_window_resize_and_cross_output_drag`，由 `scripts/test-scenarios.sh` 自动执行；守护进程、固定 Claude CLI 和看守是真进程，只有模型端点为断网离线端点。

候选框预期位置来自合成器截图中的预编辑字形：在同尺寸、同主题窗口中取消组词取得对照图，只搜索整个 textarea 的布局范围，排除真实候选面板和有颜色的闪烁光标。光标矩形和预编辑矩形均不参与预期位置计算。截图前后必须保持窗口、编辑区和候选面板几何一致；字形框尺寸约束拒绝把整条候选栏当成文字。取消组词后核对正文，场景结束再通过公开 nd-wire 核对草稿清空且没有新增提示。

| 检查 | 修复前 | 修复后 |
| --- | --- | --- |
| 1050 × 850 窗口中的初始预编辑 | 候选与字形相距 3 px | 相距 3 px |
| 窗口变为 970 × 810，保留组词 | FAIL：文字上移 40 px，候选不动，间距 39 px | PASS：间距 3 px |
| 保留组词，将窗口放大回原尺寸 | 未验证 | PASS：间距 3 px |
| 真标题栏拖动到 Virtual-1，再拖回 Virtual-0 | 未验证 | 两次均 PASS：间距 3 px |
| 中文与 emoji 长草稿滚动后，保留组词缩窄到 770 px，触发折行重排 | 未验证 | PASS：间距 3 px，取消组词后正文完整 |
| 撤掉修复，用最终独立截图判据重跑 | 再次 FAIL：缩小后间距 39 px | 修复代码已恢复 |

原失败请求为 `set_cursor_rectangle(291, 649, 0, 18)`，窗口缩小后没有新请求；修复后出现 `set_cursor_rectangle(291, 609, 0, 18)`。跨屏测试实际按住标题栏并分步移动原生指针，确认产品窗口的输出身份改变，未以直接设置窗口位置代替拖动。

复跑单个场景：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-73
export CARGO_BUILD_JOBS=6
export ND73_NATIVE_OUTPUT=/mnt/wd_external/nd-build/tmp/bug-73-recheck
bash scripts/test-scenarios.sh \
  real_rime_candidates_follow_window_resize_and_cross_output_drag -- --exact --nocapture
```

也可直接运行 Python 驱动；需提供隔离场景的 `--desktop`、`--socket`、`--session`、唯一的 `--output`。默认执行完整矩阵，`--only shrink` 只执行初始定位和缩小窗口。

## 交付检查

构建目录为 `/mnt/wd_external/nd-build/target/bug-73`，6 jobs；构建和测试使用独立的 `MemoryMax=12G`、`MemorySwapMax=0` 用户服务或 scope。交付日志及退出码位于 `/mnt/wd_external/nd-build/tmp/bug-73/final-*.log`、`checks.status`。

交付前执行 `git merge v1`，结果为已包含该基线。

| 检查 | 结果 | 日志 |
| --- | --- | --- |
| `cargo test --workspace --locked` | PASS，退出码 0 | `final-workspace.log` |
| `scripts/test-scenarios.sh`，含 #73 与四组真 Rime Esc 回归 | PASS，退出码 0 | `final-scenarios.log`、`final-native/`、`final-escape/` |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | PASS，退出码 0 | `final-clippy.log` |
| `cargo fmt --all -- --check` | PASS，退出码 0 | `final-fmt.log` |
| `scripts/check-schemas.sh` | PASS，退出码 0 | `final-schemas.log` |
| 普通 `release` 产品与 ndctl 构建 | PASS，退出码 0 | `final-release.log`、`final-release-tools.log` |
| 普通 release、私有真 Rime 空闲 CPU 回归 | PASS，退出码 0 | `final-idle.log`、`release-idle2/` |

补充 CPU 回归使用既有的 `focused_product_idle_cpu_stays_below_one_percent_with_real_rime`，三种状态各采样 60 秒。单核 CPU 占用：空输入框 0.35%、持续 Rime 组词 0.38%、失焦 0%；恢复输入上屏为“你好你好”，闪烁光标仍可见。它验证普通 release 输入链路与空闲负载，不代替日常桌面的定位或性能验收。

常规套件保留已有的手动 CLI 语料/夹具验证和真实 OOM 等 ignored 项；没有新增忽略项。常规场景中被忽略的 release CPU 回归在上述补充步骤实际执行。补充步骤首次缺少 release 同目录的 ndctl，在打开测试窗口前退出；补齐工具后重跑通过，准备失败记录保存在 `idle-preparation.log`。

## 隔离与证据

原生场景使用私有 HOME/XDG、D-Bus、Rime 部署目录和断网 bwrap。输入 helper 拒绝 `/sandbox/runtime` 以外的运行目录、`nd-test-native` 以外的显示，以及继承的 `WAYLAND_SOCKET`；沙箱内没有 uinput。没有向日常 `wayland-0` 注入事件、打开窗口或加载 KWin 模块，也没有访问真实模型端点、owner 登录凭据或会话。

每次原生运行的 `cleanup.json` 记录临时 service/slice 消失、临时根目录删除；`owner-processes.json` 核对 owner KWin/Fcitx 的 PID 与启动时刻未变。证据根为 `/mnt/wd_external/nd-build/tmp/bug-73/`：

| 路径 | 内容 |
| --- | --- |
| `red2/`、`red-final-oracle/` | 修复前失败及最终判据撤销修复后的失败 |
| `matrix2/` | 修复后完整矩阵，含成对截图、标注图、几何测量、原生事件与 Wayland 日志 |
| `final-native/` | 合入 v1 后全场景中的候选定位回归 |
| `upstream-*.rs` | 固定上游源码核实 |
| `checks.status`、`final-*.log` | 自动交付检查 |
| `release-idle2/` | 普通 release 的三态 CPU 样本、恢复输入与闪烁光标截图 |
| `final-cleanup.json`、`source-provenance.json` | 汇总清理证据与构建/源码身份 |

## owner_checklist

`OWNER_PENDING`：owner 有时间后，通过 `tool/ime-bench` 在日常桌面复核 Rime 与豆包组词中的放大、缩小、跨屏拖动和内容重排；确认实际双屏及分数缩放下候选贴着当前输入位置。私有双虚拟输出的真 Rime 验证已自动完成，不代表日常桌面和物理输出验收。
