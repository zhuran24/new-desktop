# ime-lab 手测清单：豆包、真实缩放与打字手感

状态：2026-10-04 编写，尚未执行。必做部分约 8 分钟，两项可选各约 1 分钟。只测 `ime-lab-main`。

## 这份清单测什么

自动测试只能在隔离的嵌套 KWin 里用 Rime 跑。下面三类只能在 owner 自己的桌面上手测：

- 豆包输入法：插件背后是 Wine 里的引擎，自动测试没有加载；
- 真实桌面的缩放：主屏 DP-4 为 1.5 倍，副屏 HDMI-A-2 为 1.9 倍；
- 真人操作：正常打字速度、长按、鼠标点击、语音输入。

已经有结论、这里不重复的内容：

- Rime（万象）在 1 倍缩放下的预编辑、提交、第二行候选框、组词中的 Enter / 右 Shift+Enter / Ctrl+Enter、符号、Backspace / Esc、emoji、两窗口切换，main 全部通过，见 [AUTOTEST.md](AUTOTEST.md)。组词中按下的 Enter、Esc、Backspace 全部被 Rime 吃掉，没有一个到达程序；豆包会不会同样吃掉，是本清单要回答的头号问题。
- 同一套 Rime 场景在嵌套 KWin 的 1.5 倍分数缩放下复跑，main 七项全部通过，候选框落在程序上报的矩形处。判定在 [review-verdict.json](autotest/review-scale15/20261004T192707Z-main-guard0-suite-scale15/review-verdict.json)，运行方法是 [run-scale15.diff](autotest/review-scale15/run-scale15.diff) 对 `autotest/run.py` 的改动。
- v070（Kit 0.7.0 发布版）在折到第二行后组词时，每次按键都会先把候选框提交到输入框左上角，不能用，不再手测。

## 0. 启动（1 分钟）

在自己的终端里运行，fish 和 bash 都适用：

```
cd /home/zhuran24/mytools/claude-gpui/ime-lab
env -u IME_LAB_SECONDS -u IME_LAB_SCENARIO \
  WAYLAND_DISPLAY=wayland-0 XDG_RUNTIME_DIR=/run/user/1000 \
  IME_LAB_GUARD=0 IME_LAB_LOG=/home/zhuran24/mytools/claude-gpui/ime-lab/logs/manual-main-guard0.jsonl \
  WAYLAND_DEBUG=client ./bin/ime-lab-main 2>> logs/manual-main-guard0-wayland.log
```

窗口标题 `ime-lab main`：左上是已发送的消息，左下是输入框，右侧是最近 50 条事件（新的在上）。Enter 发送并清空输入框，Shift+Enter 换行，Ctrl+Enter 只记录不发送；消息只进左上列表，不连接任何服务。日志会记下打出的所有文字，测试时只打测试内容。

窗口放在 1.5 倍的主屏上，点进输入框。输入法按程序记状态、新程序默认停在英文档，先按 Ctrl+空格激活，再用平时的方式切到豆包（光标旁出现「豆」）。

## 1. 豆包：候选框跟随（2 分钟）

操作：

1. 输入 `nihao`，停 1 秒，用数字键选一个候选（有 emoji 候选就选它）。
2. 正常速度打一句话，重复几遍，直到换到第 2、3 行；在第 3 行再打一个词，边打边盯着候选框。
3. 从别处复制十几行中文，Ctrl+V 粘进去，让输入框出现滚动；在末尾打一个长词，打到拼音折到下一行。
4. 组词时按几次 ←，只观察。
5. 打出一个词的拼音后停手 10 秒。

通过：

- 候选框始终在当前拼音（带下划线）起点的正下方，打字过程中不闪到输入框左上角或上一行。步骤 3 拼音折行引起滚动的那一下允许错开一行，下一次按键后回到正确位置。
- 步骤 5 右侧事件面板不再滚动，候选框不抖。持续滚动说明输入法和程序在互相触发。
- 步骤 4 光标停在拼音末尾不动是已知限制（gpui-pre 0.3.7 不处理预编辑内的光标位置），记下即可，不算失败。

出问题时：用 Spectacle 截图，记下当时在做什么。事件日志里的 `cursor_bounds` 是程序算出的矩形（窗口内逻辑像素）；协议日志里的 `set_cursor_rectangle` 加上紧随其后的 `commit` 才是真正交给 KWin 的位置。两者与截图里候选框的实际位置对照，就能分清是程序算错还是没有及时提交。

## 2. 豆包：组词中的回车类按键（2 分钟）

聊天输入的核心风险是：确认候选的回车把消息发了出去。1、3、4、5 都从输入 `nihao`、不选词开始：

1. 按 Enter。
2. 再按一次 Enter，此时没有拼音。
3. 输入 `nihao`，按左 Shift+Enter（平时的按法）。
4. 输入 `nihao`，按 Ctrl+Enter。
5. 输入 `nihao`，按 Esc。

通过：

- 1：左上消息列表不新增消息，输入框不多出空行。记下豆包上屏的内容（字母原文、首选词或什么都没有）。
- 2：恰好发出一条消息，内容与输入框一致，输入框随后清空。
- 3、4：不多出空行，不丢字。Shift+Enter 和 Ctrl+Enter 在 ime-lab 里本来就不发送；它们是否在组词中到达了程序，看收尾的汇总输出。
- 5：拼音和候选框都消失，不留下划线。正式客户端里 Esc 会打断正在运行的任务，所以它也不能在组词中到达程序（同样看汇总输出）。

步骤 1 发出了消息时：说明豆包把这次回车转给了程序。关窗，换应用层防护版重做步骤 1，区分两种情况：

```
cd /home/zhuran24/mytools/claude-gpui/ime-lab
env -u IME_LAB_SECONDS -u IME_LAB_SCENARIO \
  WAYLAND_DISPLAY=wayland-0 XDG_RUNTIME_DIR=/run/user/1000 \
  IME_LAB_GUARD=1 IME_LAB_LOG=/home/zhuran24/mytools/claude-gpui/ime-lab/logs/manual-main-guard1.jsonl \
  WAYLAND_DEBUG=client ./bin/ime-lab-main 2>> logs/manual-main-guard1-wayland.log
```

- 右侧出现 `send_blocked`、消息列表不增加：回车到达时组词还在，应用层防护或 Kit [#3255](https://github.com/longbridge/gpui-kit/pull/3255) 足以解决。
- 仍然发出：豆包先清掉拼音、再把回车转给程序，程序侧已看不到组词状态，要改豆包插件。插件在 `~/Documents/Codex/2026-09-12/li/outputs/doubao-original-linux/src/fcitx_keyboard.cpp`，`keyEvent` 按引擎返回的 handled 决定是否吃掉按键。

## 3. 豆包：编辑与焦点（1 分钟）

1. 没有拼音时，在已有文字上按住 Backspace 约 1 秒再松开。
2. 输入 `nihao` 不选词，用鼠标点一下输入框里另一处文字，再按空格。
3. 输入 `nihao` 不选词，Alt+Tab 切到别的窗口打两个字，再切回 ime-lab，直接输入 `nihao` 并按空格。

通过：

- 1：连续删除，松手立即停。
- 2：不出现重复的拼音、乱码或残留下划线。记下「你好」落在原处还是点击处。
- 3：切回后没有残留下划线或悬空候选框；第一个字母不丢，候选框在新拼音下方，空格后得到「你好」。记下原来的拼音是以字母上屏还是消失。

## 4. 豆包：打字手感（1 分钟）

准备一句三十字左右、带标点的话，按平时速度在 ime-lab 里打一遍，再在平时常用的程序里打一遍。

通过：延迟、丢字或重字、候选框跟随、光标位置都没有能察觉的差别。

顺带看一眼按左 Shift 切中英时「豆 / abc」提示的位置。它可能出现在上一个词的起点而不是光标处，因为 Kit 只在组词开始时上报光标位置；属于小毛病，记下即可。

## 5. 豆包语音（日常用语音才做，1 分钟）

按 Alt+A，说一句二十来字的话，再按 Alt+A 结束。

通过：识别中的文字带下划线出现在光标处，过长时自动换行；结束后变成正文；没有发出消息；窗口不失焦。

出问题时另看 `~/Documents/Codex/2026-09-12/li/work/doubao-runtime/bridge-events.log`。

## 6. Rime：左 Shift+Enter（30 秒）

切到 Rime，输入 `nihao`，按左 Shift+Enter。自动测试只按过右 Shift（万象把右 Shift 设成了 noop），左 Shift 是中英切换键，需要单独确认。

通过：不多出空行，不丢字；汇总输出里这一次 `shift-enter` 若出现，`preedit=` 应为 `None`。

## 7. 1.9 倍副屏（副屏接着时才做，1 分钟）

把窗口拖到 1.9 倍的副屏，点进输入框，重做第 1 节的步骤 1 和 2。

通过：同第 1 节。

## 收尾：汇总日志（30 秒）

关掉 ime-lab 窗口，程序随之退出。然后运行：

```
python3 -c "import json,sys
for l in open(sys.argv[1]):
    e=json.loads(l); k=e['event']; s=e.get('state') or {}
    if k in ('start','send','send_blocked') or (k=='key_down' and any(x in e.get('keystroke','') for x in ('enter','escape'))):
        print(e['seq'], k, e.get('keystroke') or e.get('text') or e.get('commit',''), 'preedit=%r' % s.get('preedit_text'))" /home/zhuran24/mytools/claude-gpui/ime-lab/logs/manual-main-guard0.jsonl
```

每行是一次启动、一次发送、一次被拦下的发送，或一次到达程序的 Enter / Esc 类按键。`key_down` 行的 `preedit=` 后面不是 `None`，就说明该键在组词中到达了程序。看防护版的结果时，把文件名换成 `manual-main-guard1.jsonl`。

| 文件 | 内容 |
|---|---|
| `logs/manual-main-guard0.jsonl`（防护版为 `guard1`） | 结构化事件：按键、预编辑、提交、候选框矩形、发送、焦点；每次启动以一条 `start` 分段 |
| `logs/manual-main-guard0-wayland.log`（同上） | 原始协议：`preedit_string`、`commit_string`、`set_cursor_rectangle`、键盘事件 |
| Spectacle 截图 | 候选框位置异常时的画面 |

字段含义见 [README.md](README.md) 的「日志格式和观测边界」。也可以直接把这几个文件交给 Claude 核对。

## 结果记录与判读

| 项目 | 结果 | 记录 |
|---|---|---|
| 1 豆包候选框跟随（1.5 倍） | | |
| 2 豆包回车类按键 | | 步骤 1 上屏内容： |
| 3 豆包编辑与焦点 | | 点击后的落点；切走后拼音的去向： |
| 4 豆包打字手感 | | |
| 5 豆包语音 | | |
| 6 Rime 左 Shift+Enter | | |
| 7 1.9 倍副屏 | | |

- 做了的项目全部通过：用 main 固定的 commit，或含 [#3297](https://github.com/longbridge/gpui-kit/pull/3297) 的 Kit 发布版即可，豆包和 Rime 都不依赖 #3255。正式客户端仍建议保留「有组词时不发送」的应用层防护，成本很低。
- 只有第 2 节不通过：按第 2 节的分支处理，加 #3255 或应用层防护，或改豆包插件。
- 第 1 或第 7 节不通过：按第 1 节「出问题时」的对照方法分清是 Kit / GPUI 算错位置还是输入法一侧的问题，带上截图和两份日志。
- 第 3 节步骤 2 出现重复或乱码：组词中的鼠标点击没有正确结束组词（Kit 点击时保留组词范围，也不通知输入法），需要给 Kit 打补丁。
