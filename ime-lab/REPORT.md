# ime-lab 交付验证报告

状态：2026-10-04 18:47 UTC，两个 release 变体交付，纯冒烟与 guard 合成对照通过，测试进程已清理。真实 Rime / 豆包中文输入体验尚未验收。使用与重建方法见 [README.md](README.md)。

## 构建产物

| 变体 | 二进制路径（相对本目录） | Kit commit | release 构建 | 最终增量构建耗时 | 二进制大小 |
|---|---|---|---|---|---|
| v070 | [bin/ime-lab-v070](bin/ime-lab-v070) | `0c830f4d257e69fdd17200650533ab4ca9a40cc0` | PASS / exit 0 | 1.217 s | 56,702,664 B / 54.08 MiB |
| main | [bin/ime-lab-main](bin/ime-lab-main) | `4c7f1350331562436df868c55ac33bebc4c6406c` | PASS / exit 0 | 1.201 s | 56,832,552 B / 54.20 MiB |

所有最终构建均使用 `--release --locked`，Rust 1.99.0，8 jobs，`systemd-run --user --scope -p MemoryMax=20G`，target 位于 `/tmp/gpui-research-20261004/target-clean`。主线编译期间读取 scope 的 `MemoryMax=21474836480`，确认 20 GiB 限额生效。没有 sudo、系统包安装或根分区 target。

表内耗时是已有缓存下对最终源码的增量构建，不是全量 release 时间。本次最早成功的 v070 / main 构建分别为 22.419 s / 27.784 s，也复用了上次留下的依赖产物；上次中断的构建没有完整耗时或成功结论。所有构建尝试保留于 `logs/build-*`。主线残留的 Cargo.lock 最初与 Git 依赖不符，重新解析后已固定并完成最终 `--locked` 构建。

可核对的构建元数据：[v070](evidence/build-v070.json)、[main](evidence/build-main.json)。其中包括精确二进制 SHA-256、源码 SHA-256 和对应 Cargo.lock SHA-256；[独立交付检查](evidence/check-evidence.json) 已核对文件与记录一致。

## 来源与补丁

v070 是 crates.io 的 Kit 0.7.0，main 固定为表中 commit；两者都使用 gpui-pre 0.3.7。为记录非公开状态，两者的 gpui-base 私有副本应用只读观测补丁，原始 Enter、preedit 与矩形计算逻辑均保留。逆向去除观测补丁后，v070 对应发布包、main 对应网络下载的固定 commit 源码逐文件比较无差异；主线其余 vendored 文件也与原始 archive 一致。证据：[源码审计](evidence/source-audit.json)、[来源](evidence/provenance.json)。

[#3297](https://github.com/longbridge/gpui-kit/pull/3297) 已于 2026-10-02 合并。GitHub compare 显示所选 main 比 merge `73ef866b50e16f2ff6bf5f6da1aded0aca27a5f2` ahead 6 / behind 0，包含该修复。证据：[当前 API](evidence/pr-3297-current.json)、[祖先比较](evidence/pr3297-ancestry.json)。

[#3255](https://github.com/longbridge/gpui-kit/pull/3255) 在 2026-10-04 实时查询中仍为 Open / Draft，未合并，head `a57b5b72e97ec74aefbe73b97352ad814e8a712e`。其 diff 添加 `is_composing()`、组合态 Enter 提前返回和测试。对固定 main 的干净 archive 执行 `git apply --check` 返回 1：`crates/base/src/input/base/state.rs:6701` 测试插入上下文不匹配。**没有生成 main+3255**，没有手动移植或解决冲突。证据：[API](evidence/pr-3255-current.json)、[原始 diff](evidence/pr-3255-current.diff)、[应用检查](evidence/pr-3255-apply-current.txt)。残留的旧三方应用日志虽 exit 0，但正文明确 `with conflicts`，不能作为干净应用的证据。

## 最终纯冒烟

每个变体使用独立 `kwin_wayland --virtual`、私有 D-Bus、私有 XDG 目录和独立 Wayland socket；未启动输入法实例。程序设置 6 秒自退出，以覆盖首次绘制后至少 5 秒。两者均有协议标题、surface buffer attach、可见窗口截图和非空 JSONL，无程序 panic；截图已逐张检查。

| 变体 | 进程持续时间 | 首次绘制至定时退出 | 退出码 | JSONL | 证据 |
|---|---|---|---|---|---|
| v070 | 6.229 s | 6.006 s | 程序 0 / KWin 0 | 9 条 | [截图](logs/smoke-v070-guard0-none-20261004T184343411668Z/window.png) / [JSONL](logs/smoke-v070-guard0-none-20261004T184343411668Z/events.jsonl) / [完整结果](evidence/smoke-v070-guard0-none.json) |
| main | 6.229 s | 6.009 s | 程序 0 / KWin 0 | 9 条 | [截图](logs/smoke-main-guard0-none-20261004T184350115907Z/window.png) / [JSONL](logs/smoke-main-guard0-none-20261004T184350115907Z/events.jsonl) / [完整结果](evidence/smoke-main-guard0-none.json) |

纯冒烟记录到启动、窗口创建、首次绘制、窗口/输入框焦点、修饰键状态、光标矩形、自结束和退出。没有真实输入，因此没有真实 preedit 或 commit。

## 合成组件检查

在同样的嵌套 KWin 中，程序调用 EntityInputHandler 并向自己的 GPUI Window 分发按键事件；没有向操作系统注入键盘输入。场景覆盖中文/emoji、Shift+Enter 换行、Ctrl+Enter 不发送、Enter 发送并清空、preedit 中 Enter、提交后的下一次 Enter、unmark、失焦和重新聚焦。

| 变体 | guard | JSONL 条数 | preedit 期间发送 | 阻止发送 | 场景检查 |
|---|---|---|---|---|---|
| v070 | 0 | 71 | 1 | 0 | PASS，复现基线误发 |
| v070 | 1 | 70 | 0 | 1 | PASS，应用 guard 生效 |
| main | 0 | 71 | 1 | 0 | PASS，复现基线误发 |
| main | 1 | 70 | 0 | 1 | PASS，应用 guard 生效 |

证据：[v070 默认](evidence/smoke-v070-guard0-check.json)、[v070 guard](evidence/smoke-v070-guard1-check.json)、[main 默认](evidence/smoke-main-guard0-check.json)、[main guard](evidence/smoke-main-guard1-check.json)。每项均正常退出并保留截图和完整事件。另由 `scripts/check_evidence.py` 校验序号、毫秒时间戳、各按键、preedit 选区、中文提交、错误发送/阻止发送的数量以及无残留进程。

这些结果证明组件路径和测试程序的行为，不证明 fcitx5/Rime/豆包一定会把同样的 Enter 转发给 GPUI，也不证明候选框真实定位正确。#3255 在实际 Wayland 会话中的影响仍需日常输入法手测。

## 已实现的日志与未取得的数据

已记录：动作前按键及修饰键和 preedit 状态、到达元素的 KeyUp、preedit 文本与 Kit 选区、提交/替换请求及编辑后内容、unmark、输入改变、Kit Enter 入口与 PressEnter 事件、实际发送/阻止发送、输入焦点/窗口激活，以及 `bounds_for_range` 参数和返回的光标矩形。日志逐行 JSON，带 Unix 毫秒时间戳与事件序号。字段、单位和事件顺序详见 README。

接口边界：被输入法吞掉的物理键不可见；Wayland 的原始 preedit cursor_begin/cursor_end 被当前后端忽略，Kit 只能给出自身选区；候选面板的实际位置、候选词列表和输入法内部状态不可见；`text_commit` 也用于普通编辑，不能自动判定输入来源；控件的矩形返回值不等于 compositor 实际采用的矩形。开启 `WAYLAND_DEBUG=client` 可以单独取得协议文本日志，不属于结构化 JSON。

应用 guard 只阻止有 marked range 时的发送。确认事件先清除 preedit 再转发 Enter 的情况、修饰键组词行为、长文本、缩放、多窗口和长时间稳定性尚未做真实输入法验收。

## 进程与约束核对

交付时遗留的本任务进程列表：`[]`。检查涵盖 10 次开发/最终运行的进程组与 XDG runtime 环境；临时 runtime 目录也无残留。嵌套 KWin、私有 D-Bus、测试程序与截图进程均已结束。没有启动私有 fcitx5 实例；来源审计的临时解包目录已删除，Cargo 缓存和 target 按需保留。

owner 的 KWin PID 2699、fcitx5 PID 2773、豆包 bridge PID 3171 仍在运行，启动时间与任务开始检查一致。未向 wayland-0 注入输入、未在其上打开测试窗口、未移动鼠标或请求焦点；未重启/杀死/重载 owner 输入法，未修改其 fcitx5 配置/数据、全局环境或 KDE 设置。证据：[最终进程清点](evidence/process-cleanup.json)。

## 开发记录的判读

`logs/` 保留旧的中断构建、锁文件不匹配、一次新增检查代码的编译错误、两次早期合成场景失败与早期无截图的纯冒烟记录，避免覆盖历史。早期合成场景使用 GPUI `dispatch_keystroke`，它额外模拟 Enter 的字符输入，造成测试驱动重复插入换行；最终驱动改为单独 KeyDown/KeyUp，提交接口由场景明确调用。当前结论只以 `evidence/build-*.json`、`evidence/smoke-*.json` 和独立交付检查引用的最终记录为准。
