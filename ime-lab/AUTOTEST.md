# ime-lab：隔离 KWin / Rime 自动验收

状态：2026-10-04，真实输入路径的 4 轮正式矩阵、4 轮直接回焦补测已完成；12 个测试会话（含环境探针和预跑）全部退出，遗留进程 `[]`。结果只覆盖本报告固定二进制、万象 Rime、100% 缩放环境。

**main 优于 v070：v070 在第二行组词时实际候选窗反复回到第一行原点，main 未复现该回退。** 两个 guard 模式结果相同。真实 Rime 的 Enter、Shift+Enter、Ctrl+Enter 均没有误发送或额外换行；这不等于排除了其他输入法的 Enter 风险。

## 结果矩阵

PASS 表示本节定义的可观测行为通过；FAIL 表示有实际失败证据；无法测表示没有可运行产物。每格分别链接日志证据和原始截图。S2 的动态判定使用 KWin 窗口几何记录，停稳后的截图不能单独排除跳动。

| 变体 | guard | S1 预编辑/空格提交 | S2 第二行定位 | S3 三种 Enter | S4 符号插入 | S5 删除/取消 | S6 混排/emoji | S7 两窗口焦点 |
|---|---:|---|---|---|---|---|---|---|
| v070 | 0 | [PASS](autotest/results/20261004T190007Z-v070-guard0-suite/s1-events.json) / [图](autotest/results/20261004T190007Z-v070-guard0-suite/s1-preedit.png) | [FAIL](autotest/results/20261004T190007Z-v070-guard0-suite/analysis.json) / [图](autotest/results/20261004T190007Z-v070-guard0-suite/s2-preedit.png) | [PASS](autotest/results/20261004T190007Z-v070-guard0-suite/s3-events.json) / [图](autotest/results/20261004T190007Z-v070-guard0-suite/s3-enter-after.png) | [PASS](autotest/results/20261004T190007Z-v070-guard0-suite/s4-events.json) / [图](autotest/results/20261004T190007Z-v070-guard0-suite/s4-ascii-question.png) | [PASS](autotest/results/20261004T190007Z-v070-guard0-suite/s5-events.json) / [图](autotest/results/20261004T190007Z-v070-guard0-suite/s5-backspace.png) | [PASS](autotest/results/20261004T190007Z-v070-guard0-suite/s6-events.json) / [图](autotest/results/20261004T190007Z-v070-guard0-suite/s6-mixed.png) | [PASS](autotest/results/20261004T190448Z-v070-guard0-focus/s7-events.json) / [图](autotest/results/20261004T190448Z-v070-guard0-focus/s7-next-preedit.png) |
| v070 | 1 | [PASS](autotest/results/20261004T190109Z-v070-guard1-suite/s1-events.json) / [图](autotest/results/20261004T190109Z-v070-guard1-suite/s1-preedit.png) | [FAIL](autotest/results/20261004T190109Z-v070-guard1-suite/analysis.json) / [图](autotest/results/20261004T190109Z-v070-guard1-suite/s2-preedit.png) | [PASS](autotest/results/20261004T190109Z-v070-guard1-suite/s3-events.json) / [图](autotest/results/20261004T190109Z-v070-guard1-suite/s3-enter-after.png) | [PASS](autotest/results/20261004T190109Z-v070-guard1-suite/s4-events.json) / [图](autotest/results/20261004T190109Z-v070-guard1-suite/s4-ascii-question.png) | [PASS](autotest/results/20261004T190109Z-v070-guard1-suite/s5-events.json) / [图](autotest/results/20261004T190109Z-v070-guard1-suite/s5-backspace.png) | [PASS](autotest/results/20261004T190109Z-v070-guard1-suite/s6-events.json) / [图](autotest/results/20261004T190109Z-v070-guard1-suite/s6-mixed.png) | [PASS](autotest/results/20261004T190500Z-v070-guard1-focus/s7-events.json) / [图](autotest/results/20261004T190500Z-v070-guard1-focus/s7-next-preedit.png) |
| main | 0 | [PASS](autotest/results/20261004T190211Z-main-guard0-suite/s1-events.json) / [图](autotest/results/20261004T190211Z-main-guard0-suite/s1-preedit.png) | [PASS](autotest/results/20261004T190211Z-main-guard0-suite/analysis.json) / [图](autotest/results/20261004T190211Z-main-guard0-suite/s2-preedit.png) | [PASS](autotest/results/20261004T190211Z-main-guard0-suite/s3-events.json) / [图](autotest/results/20261004T190211Z-main-guard0-suite/s3-enter-after.png) | [PASS](autotest/results/20261004T190211Z-main-guard0-suite/s4-events.json) / [图](autotest/results/20261004T190211Z-main-guard0-suite/s4-ascii-question.png) | [PASS](autotest/results/20261004T190211Z-main-guard0-suite/s5-events.json) / [图](autotest/results/20261004T190211Z-main-guard0-suite/s5-backspace.png) | [PASS](autotest/results/20261004T190211Z-main-guard0-suite/s6-events.json) / [图](autotest/results/20261004T190211Z-main-guard0-suite/s6-mixed.png) | [PASS](autotest/results/20261004T190512Z-main-guard0-focus/s7-events.json) / [图](autotest/results/20261004T190512Z-main-guard0-focus/s7-next-preedit.png) |
| main | 1 | [PASS](autotest/results/20261004T190312Z-main-guard1-suite/s1-events.json) / [图](autotest/results/20261004T190312Z-main-guard1-suite/s1-preedit.png) | [PASS](autotest/results/20261004T190312Z-main-guard1-suite/analysis.json) / [图](autotest/results/20261004T190312Z-main-guard1-suite/s2-preedit.png) | [PASS](autotest/results/20261004T190312Z-main-guard1-suite/s3-events.json) / [图](autotest/results/20261004T190312Z-main-guard1-suite/s3-enter-after.png) | [PASS](autotest/results/20261004T190312Z-main-guard1-suite/s4-events.json) / [图](autotest/results/20261004T190312Z-main-guard1-suite/s4-ascii-question.png) | [PASS](autotest/results/20261004T190312Z-main-guard1-suite/s5-events.json) / [图](autotest/results/20261004T190312Z-main-guard1-suite/s5-backspace.png) | [PASS](autotest/results/20261004T190312Z-main-guard1-suite/s6-events.json) / [图](autotest/results/20261004T190312Z-main-guard1-suite/s6-mixed.png) | [PASS](autotest/results/20261004T190525Z-main-guard1-focus/s7-events.json) / [图](autotest/results/20261004T190525Z-main-guard1-focus/s7-next-preedit.png) |
| main+3255 | 0 | 无法测 | 无法测 | 无法测 | 无法测 | 无法测 | 无法测 | 无法测 |
| main+3255 | 1 | 无法测 | 无法测 | 无法测 | 无法测 | 无法测 | 无法测 | 无法测 |

S4 的 PASS 指最终字符正确插入；单字节提交**确实被派发为按键**，详见下文。S7 的 PASS 指候选框属于活动窗口、切回后直接输入正常；失焦时原组词由输入法提交为拼音，**不表示保留并恢复未完成的组词**。

`main+3255` 没有二进制，本轮没有移植补丁或重编译。原始补丁应用失败证据见 [已有记录](evidence/pr-3255-apply-current.txt)。

## 测试环境与来源

| 项目 | 实测值 |
|---|---|
| 合成器 / 输出 | KWin 6.7.5，`--virtual`，1400×900，scale=1；单独 runtime 和 `ime-lab-auto-*` socket |
| 输入法 | Fcitx 5.1.23、fcitx5-rime 5.1.16、librime 1.17.0；独立进程、私有 D-Bus |
| Rime 方案 | 从用户目录复制的 `wanxiang`（万象拼音）；复制用户词库和 build，启动后只读写副本 |
| 输入路径 | KWin fake-input 键盘 → 嵌套 KWin → `zwp_input_method_v1` → Fcitx/Rime → `zwp_text_input_v3` → ime-lab |
| 键盘注入 | evdev 键码；每次按下/抬起后间隔 35 ms；注入设备整个会话保持连接；修饰 Shift 使用右 Shift |
| 截图 / 几何 | 同一私有总线中的 Spectacle ScreenShot2；KWin 脚本读取 `frameGeometry` / `clientGeometry`；动态几何定时器名义间隔 1 ms |
| 程序 | [REPORT.md](REPORT.md) 记录的 release 二进制，GPUI 平台后端及 Kit 均未在本轮修改；`IME_LAB_SCENARIO` 不设置 |

依赖完整版本见 [packages.txt](autotest/packages.txt)。KWin 启动 Fcitx 并传递专用连接的机制与 [Fcitx 官方 Wayland / KDE 文档](https://fcitx-im.org/wiki/Using_Fcitx_5_on_Wayland#KDE_Plasma)一致；实际连接以本次协议日志为准。

| 变体 | Kit commit | 二进制 SHA-256 |
|---|---|---|
| v070 | `0c830f4d257e69fdd17200650533ab4ca9a40cc0` | `1d1517da36cf20215d23a5e48b37db908d5b89ad0dc141f63b8bb0db5e9c1601` |
| main | `4c7f1350331562436df868c55ac33bebc4c6406c` | `bc2b7b5ebe1207e460ed0fe14387be0c492848ff963eb6c0e812dd20c1956e42` |

main 包含 #3297 的固定来源证据见 [既有祖先比较](evidence/pr3297-ancestry.json)。两条基线还包含其他版本差异，因此这里证明这两个产物的行为差异，不把整个差异实验当作单一提交的严格因果隔离。

## 各场景观察与判据

### S1：预编辑与空格提交

四轮均在输入框内显示带下划线的 `ni hao`，首个候选为「你好」。候选框全局几何为 `(77,687,279,33)`。协议出现 `preedit_string("ni hao",0,6)`，按空格后出现 `commit_string("你好")`，JSONL 的 `text_commit.text` 和最终输入框均为「你好」。

每轮还在组词结束后额外按一次 Enter 做正向检查：**恰好发送一条「你好」并清空输入框**。后续测试没有额外 `send`，证明没有把“发送功能没有工作”误判为防误发送成功。对应 `control-send.png` 和 `control-send` 阶段日志。

### S2：第二行候选框定位——v070 FAIL

输入 `nihao ` 32 次得到 64 个汉字，自然换到第二行，再输入 `nihao`。窗口内容区全局原点为 `(50,64)`；日志中的正确锚点矩形是窗口内 `(209,623,0,20)`，加上内容区偏移及矩形高度后，对应候选框左上角 `(259,707)`。这里锚定的是 Rime 整段预编辑选区的起点，不要求与拼音尾部插入光标的 x 坐标相同。

| 变体 / guard | 第二行正确矩形 | 错误矩形 | 候选窗正确 → 错误位置 | 本次五字母组词回退次数 | 每次记录持续时间 |
|---|---|---|---|---:|---|
| v070 / 0 | `(209,623,0,20)` | `(27,603,0,20)` | `(259,707)` → `(77,687)` | 5 | 34、34、33、34、34 ms |
| v070 / 1 | 同上 | 同上 | 同上 | 5 | 34、34、33、34、34 ms |
| main / 0 | `(209,623,0,20)` | 无原点回退 | 保持在第二行 | 0 | — |
| main / 1 | 同上 | 无原点回退 | 保持在第二行 | 0 | — |

候选框宽度随候选词变化，最终为 279，高度为 33。main 在新面板初现时也采样到短暂 `(231,707)`，随后到 `(259,707)`，始终在第二行；“PASS”不表示每一个内部几何采样都完全静止。

![四轮候选窗水平位置](autotest/candidate-position.png)

该图来自 KWin 的实际面板几何采样，不是从应用矩形推算出的面板轨迹。采样不是逐帧录屏，不能用其时间精度替代实际显示器的视觉延迟测量。

v070 / guard=0 的原始几何片段（Unix 毫秒）：

```text
1791140428373  candidate x=77,  y=687, width=240, height=33
1791140428407  candidate x=259, y=707, width=240, height=33
1791140428443  candidate x=77,  y=687, width=240, height=33
1791140428477  candidate x=259, y=707, width=240, height=33
```

原文见 [KWin 日志](autotest/results/20261004T190007Z-v070-guard0-suite/kwin.log) 中 `IME_LAB_CANDIDATE`；应用矩形、原始 `set_cursor_rectangle` 行号和时间在 [analysis.json](autotest/results/20261004T190007Z-v070-guard0-suite/analysis.json)。[停稳截图](autotest/results/20261004T190007Z-v070-guard0-suite/s2-preedit.png) 能显示最终恢复的位置，不能否定期间的回跳。

### S3：组词中的三种 Enter

| 按键 | 四轮最终文本 | 误发送 | 额外换行 | 观察 |
|---|---|---:|---:|---|
| Enter | `nihao` | 0 | 0 | Rime 提交原始字母 |
| 右 Shift+Enter | `nǐ hǎo` | 0 | 0 | 当前万象方案提交带声调拼音，并短暂显示“原编码”提示 |
| Ctrl+Enter | `ni hao` | 0 | 0 | 提交含空格的预编辑文本 |

Fcitx key_trace 记录真实 Enter/修饰键，应用协议记录对应提交；这些组词确认键没有作为 Enter 动作传给 Kit，故每轮 `send_blocked=0`。应用层 guard 开关在这一输入法/方案下结果相同。[组件合成检查](REPORT.md)复现的“组合态 Enter 误发”不因此被推翻；本次 Rime 没有产生同样的按键转发路径。

每轮目录保存 `s3-enter-before/after.png`、`s3-shift-enter-before/after.png`、`s3-ctrl-enter-before/after.png`；三种操作的事件合并在矩阵链接的 `s3-events.json` 中。例如 [main 三种 Enter 日志](autotest/results/20261004T190211Z-main-guard0-suite/s3-events.json)。

### S4：半角符号与全角问号

四轮最终均能得到 `/`、`@`、`?`、`？`，未发送消息、未多插字符。

- 万象方案输入 `/` 时先进入符号预编辑，再按 Enter 提交 `/`；`s4-ascii-slash.png` 是组词状态，`s4-slash-commit.png` 是提交状态。
- 中文标点状态的 `?` 键提交 `？`；用方案已有的 Ctrl+右 Shift+3 切换英文标点后，同一键提交半角 `?`，随后切回。`@` 直接提交 `@`。
- `/`、`@`、`?` 的协议 `commit_string` 后，GPUI 的 `key_down` 中分别出现 `/`、`@`、`?`，之后才记录字符插入。全角 `？` 走多字节文本提交。**“插入成功”和“按键派发”在这里同时成立。**

main / guard=0 的链路证据见 [S4 事件](autotest/results/20261004T190211Z-main-guard0-suite/s4-events.json)、[原始协议](autotest/results/20261004T190211Z-main-guard0-suite/primary-wayland.log)。当前 ime-lab 没有绑定这些符号的应用快捷键，因此本测试不能保证未来添加快捷键后仍不拦截字符。

### S5：Backspace 与 Esc

四轮均从 `ni hao` 更新为 `ni ha`，候选同步更新；Esc 后正文为空、marked range 清空、候选消失。没有预编辑残留。截图为 `s5-before.png`、`s5-backspace.png`、`s5-esc.png`。

### S6：中文、英文、emoji 混排

四轮最终均为 `你好abc🙂`；截图中字形正常，日志 Unicode 内容一致，UTF-16 选择位置为 7，未崩溃。中文由 Rime 的 `nihao+空格` 产生，英文由 `abc+Enter` 产生；emoji 使用 Fcitx Unicode 插件的 Ctrl+右 Shift+U、`1f642`、Enter，所有内容都经由真实键盘事件进入输入法。没有调用应用的文本插入接口、设置剪贴板或粘贴。

十六进制模式快捷键与 [Fcitx Unicode 插件定义](https://github.com/fcitx/fcitx5/blob/master/src/modules/unicode/unicode.h)一致，实际结果见 [main 混排截图](autotest/results/20261004T190211Z-main-guard0-suite/s6-mixed.png) 和 [S6 日志](autotest/results/20261004T190211Z-main-guard0-suite/s6-events.json)。此项验证 Unicode 插件提交的 emoji，不代表已测 Rime 的 emoji 候选词选择功能。

### S7：组词中切到另一实例，再切回

四轮正式测试都启动第二个同变体 ime-lab，并仅通过私有 KWin 脚本按 PID 激活窗口。第二实例可以组词、显示自己的候选；切回后能够继续输入。

额外四轮补测省去回焦后的 Esc，**切回后的第一键就是 `n`**。四组结果一致：新 `nihao` 正常显示候选，空格提交「你好」，没有吞键、错误窗口接收或消息发送。详见 [focus-verdicts.json](autotest/focus-verdicts.json)，矩阵的 S7 格使用这些直接输入补测证据。

共同的失焦行为是：输入法在 `text_input_v3.leave` 之前明确发送 `commit_string("ni hao")`，应用保留该拼音并清除组合标记。切回时原候选不恢复；直接新组词后正文为 `ni haoni hao`，空格提交后为 `ni hao你好`。这是收到明确文本提交后的结果，不能称为应用凭空残留 preedit。若产品要求切换窗口后继续原来的未完成组词，这套配置不满足该更强要求。

证据：[直接回焦的协议](autotest/results/20261004T190512Z-main-guard0-focus/primary-wayland.log)、[事件](autotest/results/20261004T190512Z-main-guard0-focus/s7-events.json)、[恢复后新候选](autotest/results/20261004T190512Z-main-guard0-focus/s7-next-preedit.png)。

## 可复现操作

全部脚本在 [autotest/](autotest/)。需要已有系统工具：`kwin_wayland`、`dbus-run-session`、`fcitx5`、`fcitx5-remote`、`qdbus6`、`spectacle`、`wayland-scanner`、GCC、pkg-config、Wayland 客户端开发文件；证据验证用 Python/Pillow，轨迹图额外用 Matplotlib。本机均已具备，没有安装系统包或使用 sudo。

```bash
cd /home/zhuran24/mytools/claude-gpui/ime-lab

# 独立环境门槛：Rime 激活、预编辑和“你好”提交必须成功
python3 autotest/run.py main --mode probe

# 两变体 × 两种防护开关，依次运行
python3 autotest/run.py all --matrix --mode suite
python3 autotest/verify.py

# 专门核对：回焦后不先按 Esc，直接开始下一次组词
python3 autotest/run.py all --matrix --mode focus
python3 autotest/verify-focus.py

# 图像核查素材、几何轨迹和最终清理检查
python3 autotest/visual-review.py
python3 autotest/plot-position.py
python3 autotest/audit-cleanup.py
```

单轮可用 `python3 autotest/run.py v070 --guard 1 --mode suite`。每次保留独立时间戳目录；`matrix.json` / `matrix-focus.json` 指向最近一次对应完整矩阵。首次四轮完整矩阵使用的脚本快照保存在 [matrix-source/](autotest/matrix-source/)；当前 `run.py` 的 S7 使用经补测验证的直接回焦输入序列。二进制不变。

脚本执行以下隔离步骤：

1. 在 `/tmp/ime-lab-autotest-*` 建立 0700 的 HOME、runtime、config、data、cache、state。先从原用户目录复制 Fcitx 配置和 Rime 数据，再把**副本** profile 改为 `keyboard-us` / `rime`、默认 Rime、默认激活。复制时解除软链接，避免写回来源。没有复用原 Rime 锁文件的 inode。
2. 子进程环境删除继承的 DISPLAY、WAYLAND_DISPLAY、WAYLAND_SOCKET、D-Bus 地址和 IM 模块变量。通过自定义 `dbus.conf` 启动不含服务自动激活目录的私有总线，避免激活 owner 会话服务。
3. 使用下列核心参数启动 KWin；具体临时路径和完整命令在每轮 `result.json`：

   ```text
   dbus-run-session --config-file <private>/dbus.conf --
     kwin_wayland --virtual --socket ime-lab-auto-<pid>
     --width 1400 --height 900 --scale 1
     --no-lockscreen --no-global-shortcuts --no-kactivities
     --inputmethod <private>/fcitx.sh
     --exit-with-session <private>/session.sh
   ```

4. 仅在该子进程环境设置 `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1`。KWin 启动的输入法脚本执行：

   ```text
   fcitx5 -D --disable all
     --enable wayland,waylandim,keyboard,rime,classicui,dbus,dbusfrontend,unicode
     --verbose key_trace=5,waylandim=5
   ```

   豆包、X11 前端、通知和其他插件不加载。`fcitx5-remote` 只使用私有总线；`fcitx-status.json` 必须为 `{"im":"rime","state":"2"}`。

5. [inject.c](autotest/inject.c) 编译在 `/tmp/ime-lab-autotest-tools`，强制检查私有 runtime 前缀、0700 权限、UID、socket 文件类型和精确 socket 名，拒绝继承 `WAYLAND_SOCKET`。没有鼠标注入代码。对 owner 地址的拒绝检查在连接前返回 2，见 [inject-refusal.txt](autotest/inject-refusal.txt)。
6. Spectacle 在同一私有总线和显示 socket 中执行 `-b -n -f -o <png>`。KWin 脚本只在该总线激活测试 PID、读取窗口几何；原始截图均为 1400×900。
7. 每轮结束时终止测试应用，KWin 随 session 退出，Fcitx 和私有总线随之退出；兜底只清理本轮新建的进程组。删除本轮临时配置和词库，保留截图与日志。程序受控停止记录退出码 -15，属于 SIGTERM 收尾；正式矩阵与回焦补测的 KWin 会话退出码均为 0，测试阶段没有崩溃证据。

没有 Rust 编译；原有 Cargo 缓存和 `/tmp/gpui-research-20261004/target-clean` 未用于本轮构建。C 注入工具的编译产物只在 `/tmp`。

## 证据目录与校验

每轮 `actions.jsonl` 记录注入键码、操作阶段、截图和焦点操作时间；`primary.jsonl` / `secondary.jsonl` 是应用原始事件；`*-wayland.log` 是原始协议；`fcitx.log` 是真实按键和输入法生命周期；`kwin.log` 包含面板几何。`analysis.json` 关联阶段、事件序号、最后状态和截图几何；`s1-events.json` 至 `s7-events.json` 是便于查看的片段。

| 变体 / guard | 正式目录 | 原始日志 | 主应用事件数 / 原始截图数 |
|---|---|---|---:|
| v070 / 0 | [20261004T190007Z-v070-guard0-suite](autotest/results/20261004T190007Z-v070-guard0-suite/) | [JSONL](autotest/results/20261004T190007Z-v070-guard0-suite/primary.jsonl) / [Wayland](autotest/results/20261004T190007Z-v070-guard0-suite/primary-wayland.log) / [Fcitx](autotest/results/20261004T190007Z-v070-guard0-suite/fcitx.log) / [KWin](autotest/results/20261004T190007Z-v070-guard0-suite/kwin.log) | 3632 / 28 |
| v070 / 1 | [20261004T190109Z-v070-guard1-suite](autotest/results/20261004T190109Z-v070-guard1-suite/) | [JSONL](autotest/results/20261004T190109Z-v070-guard1-suite/primary.jsonl) / [Wayland](autotest/results/20261004T190109Z-v070-guard1-suite/primary-wayland.log) / [Fcitx](autotest/results/20261004T190109Z-v070-guard1-suite/fcitx.log) / [KWin](autotest/results/20261004T190109Z-v070-guard1-suite/kwin.log) | 3635 / 28 |
| main / 0 | [20261004T190211Z-main-guard0-suite](autotest/results/20261004T190211Z-main-guard0-suite/) | [JSONL](autotest/results/20261004T190211Z-main-guard0-suite/primary.jsonl) / [Wayland](autotest/results/20261004T190211Z-main-guard0-suite/primary-wayland.log) / [Fcitx](autotest/results/20261004T190211Z-main-guard0-suite/fcitx.log) / [KWin](autotest/results/20261004T190211Z-main-guard0-suite/kwin.log) | 3285 / 28 |
| main / 1 | [20261004T190312Z-main-guard1-suite](autotest/results/20261004T190312Z-main-guard1-suite/) | [JSONL](autotest/results/20261004T190312Z-main-guard1-suite/primary.jsonl) / [Wayland](autotest/results/20261004T190312Z-main-guard1-suite/primary-wayland.log) / [Fcitx](autotest/results/20261004T190312Z-main-guard1-suite/fcitx.log) / [KWin](autotest/results/20261004T190312Z-main-guard1-suite/kwin.log) | 3285 / 28 |

正式矩阵共 112 张原始截图。四张 `review-contact-sheet.png` 为便于审查而裁取输入区域的索引图，原图保留；已逐项核对正文、候选和取消状态。S3 的“原编码”小窗是万象状态提示，不能误认作未清理的词候选。

| 变体 / guard | 直接回焦补测目录 | 证据 |
|---|---|---|
| v070 / 0 | [20261004T190448Z-v070-guard0-focus](autotest/results/20261004T190448Z-v070-guard0-focus/) | [JSONL](autotest/results/20261004T190448Z-v070-guard0-focus/primary.jsonl) / [Wayland](autotest/results/20261004T190448Z-v070-guard0-focus/primary-wayland.log) / [截图](autotest/results/20261004T190448Z-v070-guard0-focus/s7-next-preedit.png) |
| v070 / 1 | [20261004T190500Z-v070-guard1-focus](autotest/results/20261004T190500Z-v070-guard1-focus/) | [JSONL](autotest/results/20261004T190500Z-v070-guard1-focus/primary.jsonl) / [Wayland](autotest/results/20261004T190500Z-v070-guard1-focus/primary-wayland.log) / [截图](autotest/results/20261004T190500Z-v070-guard1-focus/s7-next-preedit.png) |
| main / 0 | [20261004T190512Z-main-guard0-focus](autotest/results/20261004T190512Z-main-guard0-focus/) | [JSONL](autotest/results/20261004T190512Z-main-guard0-focus/primary.jsonl) / [Wayland](autotest/results/20261004T190512Z-main-guard0-focus/primary-wayland.log) / [截图](autotest/results/20261004T190512Z-main-guard0-focus/s7-next-preedit.png) |
| main / 1 | [20261004T190525Z-main-guard1-focus](autotest/results/20261004T190525Z-main-guard1-focus/) | [JSONL](autotest/results/20261004T190525Z-main-guard1-focus/primary.jsonl) / [Wayland](autotest/results/20261004T190525Z-main-guard1-focus/primary-wayland.log) / [截图](autotest/results/20261004T190525Z-main-guard1-focus/s7-next-preedit.png) |

机器校验结果见 [verdicts.json](autotest/verdicts.json)、[focus-verdicts.json](autotest/focus-verdicts.json)。校验涵盖二进制哈希、日志连续序号、原生测试模式、截图可解码和尺寸、Rime 提交、发送正向检查、无误发送、符号按键派发、候选几何、双窗口焦点以及清理记录。产品 FAIL 会保留在结果里，不会被解释为测试工具失败。脚本哈希见 [source-sha256.json](autotest/source-sha256.json)。

### 环境探针与未采用路径

执行记录单独保留，不计入正式矩阵：

- [首个环境探针](autotest/results/20261004T185400Z-main-guard0-probe/result.json)未提交「你好」：每次注入后销毁 fake-input 设备，引起 KWin seat 能力变化及 Fcitx deactivate/activate，首字母和组合状态丢失。改为整个会话保持设备后，[独立门槛探针](autotest/results/20261004T185509Z-main-guard0-probe/result.json)通过。
- [预跑](autotest/results/20261004T185635Z-main-guard0-suite/analysis.json)尝试 fake-input v6 的 Unicode keysym，没有得到 emoji；应用记录成 `xf86rfkill`，仅发生于禁用全局快捷键的私有会话。正式测试使用已验证的 Fcitx Unicode 十六进制模式，不依赖该 keysym 路径。
- 所有正式场景所需环境链路已经打通。没有 wtype 编译或系统包缺口；ScreenShot2 截图可用。虚拟环境的 PipeWire 警告不影响本次静态截图，未做视频录制。

## 清理与测试边界

最终检查覆盖 12 个新建进程组，并扫描 `/proc` 中私有 runtime 环境。**本任务遗留进程列表：`[]`；临时 runtime/config/data 目录：`[]`。** 注入工具编译目录 `/tmp/ime-lab-autotest-tools` 仅含文件，不是运行中的会话。完整列表见 [cleanup.json](autotest/cleanup.json)。

每轮前后 owner 的 Fcitx PID 2773、豆包 bridge PID 3171 的 PID/启动时间均一致，Fcitx 配置文件 SHA-256 一致。KWin 当前 PID 2699 与开测前进程清单一致；每轮自动快照未读到 owner KWin 的环境，因此不对它声称逐轮启动时间校验。最终身份记录见 [owner-final.json](autotest/owner-final.json)。没有向 owner 的显示 socket 连接注入器，没有移动其鼠标或切换其窗口，没有重启、杀死、重载其输入法，也没有修改其 Fcitx 配置、Rime 原目录、全局环境或 KDE 设置。输入法词库副本在使用后删除；配置与词库的当前内容会影响未来重跑的候选排序，本报告没有归档整份私人词库。

本次不能代表：

- 150% / 200% 缩放、双屏、不同字体或窗口尺寸、窗口移动/缩放、多行滚动及行中编辑的完整定位验收。
- 豆包、其他 Rime 方案或用户真实物理键盘手感、输入延迟、候选选择偏好。
- 长时间内存/CPU 稳定性、15 分钟持续输入、后台流式内容更新。
- 有 `/`、`@`、`?` 快捷键的真实聊天应用，或其他输入法把组合确认 Enter 转交应用后的发送安全性。
- 保留未完成组词的跨窗口恢复；当前输入法会在失焦时明确提交原始拼音。

在已测条件下选择 **main**。guard=0/1 没有实测差异；不能用本次 Rime 结果证明 guard 对所有输入法有效，也不能用其掩盖 v070 的定位失败。
