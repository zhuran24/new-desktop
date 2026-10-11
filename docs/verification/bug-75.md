# #75：取消组词后的真实 Esc 再按

日期：2026-10-11。分支 `bug/75`，工作树 `.worktrees/bug-75`。状态：**修复与全部自动交付检查通过**；日常桌面与实体键盘验收 `OWNER_PENDING`。

## 原因与当前规则

取消未提交预编辑后，Composer 没有收到被输入法消费的首次 Esc。原规则把第一个应用端 Esc 的重复识别窗口设为 `[0,D+2P]`，其中 `D`、`P=1/R` 来自当前 Wayland seat 的 `wl_keyboard.repeat_info`。因此 25 Hz/600 ms 设置下，取消后约 0.33 秒的真实再按也被吞掉。

[Composer](../../crates/nd-desktop/src/composer.rs) 将首个取消重复窗口收紧为 `[max(0,D−2P),D+2P]`，与普通 Esc 首次重复的时间窗口一致。默认设置对应 520–680 ms；约 330 ms 的真实再按正常分派。已识别的重复链仍使用 `2P` 连续窗口。普通 Esc 的合成松开/按下邻接条件、失焦与其他键清除保护、关闭重复时不猜测节奏、运行中读取设置更新均保持原规则。

这仍是节奏识别。`D` 是输入法开始计时的延迟，应用锚点是观察到取消预编辑的时间，两者并不相同。[Fcitx 5.1.23 首次计时](https://github.com/fcitx/fcitx5/blob/27cca6e0239938b2d53937061d0980c087856b22/src/frontend/waylandim/waylandimserver.cpp#L507)还减去 [3000 微秒](https://github.com/fcitx/fcitx5/blob/27cca6e0239938b2d53937061d0980c087856b22/src/frontend/waylandim/waylandim.h#L53)。真实协议中，600 ms 配置的首个重复距客户端取消消息只有约 594 ms。精确用 `D` 作应用侧下界会漏掉首个重复，破坏长按保护；两个周期的既有余量覆盖提前与观察迟到，没有增加固定毫秒阈值。

接近 `D` 的真实再按仍可能与自动重复无法区分；本修复不能承诺任何小于 `D` 的应用侧间隔都被识别为新按键。恢复物理按键身份的输入法接入层方案属于 #72。

## 测试接缝与红绿证据

接缝为 #75 和实施指令指定的真实链路：独立 EIS 键盘 → 私有 KWin → 真 Fcitx5/Rime Luna Pinyin → 产品 Composer/产品窗。观察公开编辑缓冲、非组词 Esc 计数、产品面板及 nd-wire 会话状态。测试没有直接调用内部输入函数。

[native_escape.py](../../crates/nd-desktop/tests/native_escape.py) 在组词中点按 Esc，确认这次取消不分派其他动作，再约 0.33 秒真实松开、重新按下 Esc。测试记录实际间隔并核对该次输入早于配置的重复延迟，防止机器迟到到原死区之外而假通过。非默认短延迟配置采用更早的真实再按。

产品回归沿用 [sessions.rs](../../crates/nd-daemon/tests/sessions.rs) 中 `native_rime_escape_hold_preserves_turn_and_real_double_escape_opens_rewind`：真实守护进程、固定 Claude CLI 和离线端点建立活动回合，再由原生输入停止回合、收起产品面板。长按期间持续检查回合和面板，空闲真实双 Esc 仍打开回退菜单。[test-scenarios.sh](../../scripts/test-scenarios.sh) 自动执行产品场景以及四组 Composer 设置回归，无新增手动接法或忽略项。

证据根：`/mnt/wd_external/nd-build/tmp/bug-75/`。

| 证据 | 输入与结果 |
|---|---|
| `red-lab-final/` | 修复前真实再按间隔 331.6 ms，计数仍为 9，等待增加到 10 失败 |
| `red-product/` | 修复前 331.6 ms 再按后活动回合仍运行，停止回合断言失败 |
| `green-cadence-lab/` | 修复后 331.5 ms 再按正确增加一次；原长按、双按、提交预编辑和动态设置回归通过 |
| `green-cadence-product/` | 修复后 331.4 ms 再按停止回合，另一轮 331.4 ms 再按收起面板；长按与双 Esc 均通过 |
| `strict-delay-lab/`、`strict-delay-product/` | 下界严格设为 `D` 的对照尝试：原长按分别泄漏 8 个 Esc、误停活动回合，未保留该实现 |
| `fcitx-v1.cpp`、`fcitx-waylandim.h` | 经 GitHub contents API 重新取得的准确提交源码，核对提前计时常量 |

每个原生结果目录包含 `input.jsonl`、`lab.wayland.log`、`lab.jsonl`、`result.json`、`cleanup.json`。红灯在产品修改前产生，修复后运行同一测试；没有修改测试期望来接受被吞掉的 Esc。

## 隔离与构建

全部构建位于 `/mnt/wd_external/nd-build/target/bug-75`，6 jobs，独立 systemd 用户单元或 scope，内存上限 12 GiB、零 swap。所有共用该 target 的 Cargo 操作串行执行。场景启动器在 `NoNewPrivileges=no` 的独立用户 service 中运行，CLI 子进程则由测试隔离层使用临时 HOME/XDG、固定 CLI、断网 bwrap 和清空的环境启动。

原生输入只连接 `/sandbox/runtime` 下的 `nd-test-ime`。Python 与 C 输入器都拒绝其他 runtime/socket；私有 D-Bus、KWin、Fcitx 和 Rime 数据由测试创建。没有向 owner 的 `wayland-0` 注入事件、开窗或加载模块。私有服务和临时根由 `cleanup.json` 核对清理。

## 交付检查

日志为证据根下的 `checks.status`、`workspace.log`、`scenarios.log`、`clippy.log`、`fmt.log` 和 `schemas.log`。

| 检查 | 结果 |
|---|---|
| `git merge v1` | PASS，基线 `70ff2a8227e86f49f3e895f5474fc88045bfbd37`，已是最新，无冲突 |
| `cargo test --workspace --locked` | PASS：246 passed、0 failed、2 个既有 ignored |
| `scripts/test-scenarios.sh` | PASS：182 passed、0 failed、2 个既有 ignored；另含四组 Python 原生输入回归全部通过 |
| 全量产品窗回归 `full-product/` | PASS：331.7 ms 再按停止回合、331.5 ms 再按收起面板；长按与真双 Esc 均通过 |
| `full-lab/default/`，25 Hz/600 ms | PASS：331.5 ms 再按有效；运行中切换到 5 Hz/900 ms 的长按回归也通过 |
| `full-lab/fast/`，40 Hz/250 ms | PASS：151.6 ms 再按有效；长按与真双 Esc 通过 |
| `full-lab/slow/`，12 Hz/900 ms | PASS：331.5 ms 再按有效；长按与真双 Esc 通过 |
| `full-lab/disabled/`，0 Hz/600 ms | PASS：331.6 ms 再按有效；关闭自动重复后的真实按键均正常分派 |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `scripts/check-schemas.sh` | PASS：完整 Schema 文件集及生成的 mod 类型一致 |
| 上述五组原生目录的 `cleanup.json` | PASS：service/slice 无残留，临时根已删除 |

workspace 的既有忽略项为真实 CLI 分叉/两次压缩夹具验证，以及本地 CLI 提取/完整语料验证。scenarios 的既有忽略项为真实 OOM 杀进程和三个 60 秒样本的 release CPU 验证。#75 的回归均实际运行。

## owner_checklist

`OWNER_PENDING` 只适用于日常桌面和实体键盘确认：

- Rime、豆包各确认点按取消后约 0.3 秒再按可收起面板或停止回合。
- 确认取消组词后继续长按不误停回合、不反复开合回退菜单，松开后真实双 Esc 正常。
- 记录与系统首个重复时刻接近的真实再按是否仍出现节奏歧义。

私有 KWin 与真实 Rime 的自动回归已实际执行，不属于待 owner 执行项。
