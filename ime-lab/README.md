# ime-lab

状态：2026-10-04，v070 / main 的 release 二进制、嵌套 Wayland 冒烟和组件合成检查已完成；Rime / 豆包真实输入法验收待执行。详细结果见 [REPORT.md](REPORT.md)。

面向聊天输入的 GPUI 中文输入法测试程序：左侧上方显示已发送消息，下方为 Kit 的多行 `Textarea`，右侧显示最近 50 条 JSON 事件，最新在上。窗口标题固定为 `ime-lab v070` 或 `ime-lab main`。当前 Kit 将单行 `Input` 与多行 `Textarea` 分为独立控件；这里使用 `TextareaState`，底层仍为 Kit 的 `InputBaseState`。

## 变体

| 变体 | Kit 来源 / commit | #3297 候选定位修复 | #3255 组合态 Enter 修复 |
|---|---|---|---|
| v070 | crates.io `gpui-kit =0.7.0`；发布源码 `0c830f4d257e69fdd17200650533ab4ca9a40cc0` | 不含 | 不含 |
| main | Git 固定 `4c7f1350331562436df868c55ac33bebc4c6406c` | 包含，merge `73ef866b50e16f2ff6bf5f6da1aded0aca27a5f2` 是其祖先 | 不含 |
| main+3255 | 未生成 | — | PR 原始 diff 无法干净应用到该 main；没有手动解决冲突 |

两个可用变体都固定到 `gpui-pre 0.3.7`。各自的 [v070 Cargo.lock](variants/v070/Cargo.lock)、[main Cargo.lock](variants/main/Cargo.lock) 独立保存，主线的其他依赖也可能不同，因此两者比较不限于单独一个修复。

2026-10-04 GitHub API：[#3297](https://github.com/longbridge/gpui-kit/pull/3297) 已于 2026-10-02 合并；[#3255](https://github.com/longbridge/gpui-kit/pull/3255) 仍为 Open / Draft，head `a57b5b72e97ec74aefbe73b97352ad814e8a712e`。#3255 增加 `is_composing()`、Enter 提前返回和回归测试；原始 diff 的测试插入 hunk 在 `crates/base/src/input/base/state.rs:6701` 不匹配当前上下文。见 [原始 diff](evidence/pr-3255-current.diff)、[干净应用失败记录](evidence/pr-3255-apply-current.txt)、[来源记录](evidence/provenance.json)。可在该固定 commit 的**干净临时副本**根目录复核：

```bash
git apply --check /home/zhuran24/mytools/claude-gpui/ime-lab/evidence/pr-3255-current.diff
```

## 构建

在项目根目录运行，Python 需支持 `tomllib`（3.11+）：

```bash
cd /home/zhuran24/mytools/claude-gpui/ime-lab
python3 scripts/build.py all
# 或单独构建
python3 scripts/build.py v070
python3 scripts/build.py main
```

脚本对每次 `cargo build --release --locked` 使用 `systemd-run --user --scope -p MemoryMax=20G`，默认 8 jobs。默认缓存为 `/tmp/gpui-research-20261004/cargo-home`，target 为 `/tmp/gpui-research-20261004/target-clean`。可通过 `IME_LAB_CARGO_HOME`、`IME_LAB_TARGET_DIR`、`IME_LAB_JOBS` 覆盖；target 必须在 `/tmp`。`/tmp` 清空后 Cargo 会重新下载与编译。

二进制复制到 `bin/ime-lab-v070`、`bin/ime-lab-main`；构建日志、耗时和失败记录在 `logs/build-*`，最近成功构建的 commit、源码 SHA-256、Cargo.lock SHA-256、二进制大小与 SHA-256 在 `evidence/build-*.json`。当前交付的锁文件可直接 `--locked` 构建；`--resolve` 仅用于主动重新解析依赖，不是日常命令。项目采用两个独立 package，避免 workspace 将不同来源的观测补丁合并。

本机 Rust/Cargo 1.99.0、既有 Wayland/Vulkan/字体与开发库可完成构建运行，没有额外安装系统包。其他机器缺少系统库时应记录 Cargo 的具体错误；脚本不安装包。

## owner 手测：直接在当前桌面运行

以下命令供操作者主动在自己的 KDE Wayland 桌面执行；没有自动按键。窗口一直运行到手动关闭。

```bash
cd /home/zhuran24/mytools/claude-gpui/ime-lab
env -u IME_LAB_SECONDS -u IME_LAB_SCENARIO -u IME_LAB_LOG \
  WAYLAND_DISPLAY=wayland-0 XDG_RUNTIME_DIR=/run/user/1000 \
  IME_LAB_GUARD=0 ./bin/ime-lab-v070

env -u IME_LAB_SECONDS -u IME_LAB_SCENARIO -u IME_LAB_LOG \
  WAYLAND_DISPLAY=wayland-0 XDG_RUNTIME_DIR=/run/user/1000 \
  IME_LAB_GUARD=0 ./bin/ime-lab-main

# 对照：同一二进制启用应用层发送防护
env -u IME_LAB_SECONDS -u IME_LAB_SCENARIO \
  WAYLAND_DISPLAY=wayland-0 XDG_RUNTIME_DIR=/run/user/1000 \
  IME_LAB_GUARD=1 IME_LAB_LOG="$PWD/logs/manual-main-guard.jsonl" \
  ./bin/ime-lab-main
```

Enter 通过 Kit 的 `submit_on_enter(true)` / `InputEvent::PressEnter` 发送当前内容并清空；空内容也记录发送。Shift+Enter 走 Kit 默认换行；Ctrl+Enter 记录按键和 `PressEnter`，不发送。发送只追加到窗口内的消息列表，不连接任何模型或服务。

默认 `IME_LAB_GUARD=0`，不添加组合态防护。`IME_LAB_GUARD=1` 在收到 `PressEnter` 时检查 Kit 的 marked range，存在时记录 `send_blocked` 并保留内容；不修改 Kit 的 Enter 处理、不抑制 Shift+Enter 换行。这个防护只能看到**当时仍存在的 marked range**；如果输入法已清除 preedit 后又转发同一次 Enter，它不能反推按键归属，需要真实输入法验证。

建议每个变体 × guard 开关 × 日常输入法分别记录结果：

1. 输入 `nihao` 暂不选词，观察 preedit、下划线与候选框；按 Enter 确认候选，检查是否误发。确认后再按一次 Enter 应发送完整中文。
2. 测试空格/数字选词、Esc 取消、Backspace、中文标点、`中文🙂abc`，再检查 Shift+Enter 换行和 Ctrl+Enter 记录。
3. 已有长中文前缀时继续组词，覆盖行末自动换行、输入框滚动、窗口移动与缩放，检查候选框是否回到左上角或漂移。
4. 组合中切换焦点、切回输入框，检查正文、候选和 preedit 残留；保持组词静置并持续输入，观察抖动与异常增长。

## 自动冒烟与组件检查

```bash
cd /home/zhuran24/mytools/claude-gpui/ime-lab
python3 scripts/smoke.py all
python3 scripts/smoke.py all --scenario check
python3 scripts/smoke.py all --scenario check --guard 1
python3 scripts/check_evidence.py
```

每次测试使用私有 D-Bus、临时 XDG runtime/config/data/cache/state 和 `kwin_wayland --virtual`，socket 为 `ime-lab-<variant>-<pid>`，1400×900；不连接 `wayland-0`。私有 D-Bus 禁止服务自动激活，不启动 fcitx5 或豆包实例。程序自运行 6 秒，以保证首次绘制后至少 5 秒。每次留下 `events.jsonl`、Wayland 协议 `client.log`、嵌套窗口 `window.png`、`result.json`；`evidence/smoke-*.json` 指向最近对应结果。

通过条件包括：正常退出、首帧后存活至少 5 秒、协议中的正确标题和 surface buffer attach、JSON 日志非空、测试进程组无残留。截图由私有会话内的 Spectacle 获取；`screenshot_exists` 单独标明结果，最终交付检查要求截图存在。

`--scenario check` 仅在上述私有会话里，直接调用 `EntityInputHandler` 制造 preedit/commit，向**程序自己的 GPUI Window** 分发 KeyDown/KeyUp 事件。它检查中文/emoji、换行、Ctrl+Enter、发送清空、guard 对照、提交后再发送、unmark 与焦点；没有系统级键盘注入。日志明确标为 `scenario=check`，不能把它视为真实 text-input-v3 / fcitx5 / 候选面板测试。两条基线的 guard=0 检查预期包含一次 preedit 误发，检查 PASS 表示成功复现基线并验证程序，**不表示输入法验收通过**。

退出时脚本关闭自己的程序、KWin 和私有 D-Bus，必要时只清理本次创建的进程组，删除临时 XDG 目录。不要用进程名进行全局 `kill`。

## 日志格式和观测边界

`IME_LAB_LOG` 指定 JSONL 路径，默认是编译时项目根目录下的 `logs/<变体>-<Unix毫秒>.jsonl`；当前根目录为 `/home/zhuran24/mytools/claude-gpui/ime-lab`。文件采用追加模式，每条事件立即写入并 flush；同一个文件跨运行时用 `start` 和 `pid` 分段，`seq` 每次启动从 1 开始。正文和 preedit 都会进入日志。

共有字段：`schema=1`、`event`、`ts_ms`（Unix epoch 毫秒）、`seq`（本进程事件序号）、`pid`、`variant`。`state` / `before` / `after` 包含 `has_preedit`、`preedit_text`、`marked_range_utf16`、`selection_utf16`；range 为 `{start,end}` 半开区间，单位是 UTF-16 code unit，emoji 常占两个单位。

| 事件 | 特有字段 / 含义 |
|---|---|
| `start` | `commit`、`gpui_pre`、`guard`、`scenario`、日志路径、Wayland display 与 runtime |
| `key_down` | `keystroke`、`key`、`key_char`、`modifiers`、`state`；GPUI `intercept_keystrokes` 在快捷键动作前只读记录，不停止传播 |
| `key_down_unhandled` | 动作处理后仍到达元素的 KeyDown；额外记录 `is_held`、`prefer_character_input`；不是第二次物理按键 |
| `key_up` / `modifiers_changed` | GPUI 传到元素的抬键与修饰键状态；`control/alt/shift/platform/function` 为布尔值 |
| `kit_enter` | Kit `enter()` 入口的 `secondary`、`shift` 和组合状态，先于默认动作 |
| `press_enter` | Kit 发出的 `InputEvent::PressEnter`，含收到事件时的状态；它是异步事件订阅，不能替代动作前快照 |
| `preedit_update` | `replace_and_mark_text_in_range` 的 `text`、`replacement_range_utf16`、`requested_selection_utf16`、前后状态和 `value_after` |
| `text_commit` | `replace_text_in_range` 的请求文本、替换范围、前后状态和 `value_after`；也会包含普通字符、换行、清空等内部编辑，**不能仅凭名称认定来自 IME** |
| `unmark` | 取消 marked 标记的前后状态；取消标记不等同于删除文本 |
| `input_change` | Kit 的 Change 事件与当前 `value`；并非每次内部编辑都发出 Change |
| `cursor_bounds` | `bounds_for_range` 的 `range_utf16`、`input_bounds`、返回 `rect`（可为 null）、当时状态；矩形为窗口内逻辑像素，字段 `x/y/width/height` |
| `send` / `send_blocked` | 实际加入消息列表的文本 / 因 preedit 阻止发送；含状态和相关 guard 信息 |
| `focus` | `target=input/window` 与 `focused`；输入框焦点和窗口激活分开记录 |
| `first_render` / `window_opened` / `timed_close` / `exit` | 渲染、窗口创建、自结束与事件循环退出 |
| `scenario_step` / `scenario_pass` | 仅合成检查产生的步骤和断言结果 |

Kit 的公开 `InputEvent` 只有 Change / PressEnter / Focus / Blur，不能直接暴露上述全部输入细节。因此两个变体均对私有 `gpui-base` 副本添加相同的**只读观测补丁**：[v070 补丁](patches/observe-v070.patch)、[main 补丁](patches/observe-main.patch)。变体的 Cargo manifest 通过 `[patch]` 选择对应副本；GPUI 平台后端未修改。去掉观测补丁后，两份源码与原发布包 / 固定 commit 一致，见 [源码校验](evidence/source-audit.json)。`scripts/instrument.py` 是初次生成器，已插桩副本不应再次运行。

以下数据没有从当前接口取得：

- 被输入法截获而没有送到 GPUI 的物理按键、硬件扫描码、设备 ID 和原始按键时间戳。动作前观察也可能包含 GPUI 合成的修饰键事件。
- Wayland 原始 `preedit_string` 的 `cursor_begin/cursor_end`：gpui-pre 0.3.7 忽略这两项并传 `None` 给选区参数。JSON 中记录的是 Kit 收到的选区参数和 Kit 的实际选区，不是原始协议选区。
- 候选列表、候选面板的实际屏幕位置、fcitx5 / Rime / 豆包内部状态。`cursor_bounds` 是控件返回的矩形，不是候选框实测位置，也不保证后端每次都向 compositor 发送同一个矩形。
- `text_commit` 的来源分类与唯一物理确认键关联；需结合前后状态与协议日志解释。

需要原始协议时，手测命令可额外设置 `WAYLAND_DEBUG=client` 并将 stderr 重定向到单独文件；其中能看到 `preedit_string`、`commit_string` 和 `set_cursor_rectangle`，这部分是文本日志而非结构化 JSON。纯冒烟没有输入法实例，所以不会凭空产生真实 preedit/commit。

右侧仅保留 50 条，磁盘日志保存全部。矩形查询本身不会触发新重绘，避免观察面板制造查询—重绘循环；下次其他事件或绘制时会刷新面板。
