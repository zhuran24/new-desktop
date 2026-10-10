# #67：取消组词后的 Esc 长按

日期：2026-10-10。状态：**修复及全部自动交付检查通过；日常桌面/实体键盘验收 OWNER_PENDING**。分支 `bug/67`，工作树 `bug-67`。集成基线 `v1=f3b61ab4b529ea62f7b6a4fdceebda1db6cbb6ca`，包含 #66、#68、#69、#70 的修复。输入法接入层备选方案 #72 未实施。

## 识别规则与阈值依据

[Composer](../../crates/nd-desktop/src/composer.rs) 观察真实编辑器的 marked range。预编辑两侧正文按 UTF-16 范围提取；预编辑结束后，若正文恰好等于此前两侧正文，且应用没有收到正在按下的 Esc，就认为输入法消费了取消键。提交了候选文字时不会进入取消保护。

[系统重复设置读取](../../crates/nd-desktop/src/keyboard_repeat.rs) 在后台建立仅绑定 seat/keyboard 的独立 Wayland 连接，只读 `wl_keyboard.repeat_info`。它不创建 surface、不取得焦点、不请求输入法协议、不收集物理按键身份。连接持续监听设置更新，使用阻塞 poll；最后一个持有者释放后，唤醒 socket 关闭并结束线程，没有定时轮询。生产 GPUI、Kit 和共享 Cargo registry 没有修改。

设合成器报告重复延迟为 `D`，频率为 `R`，周期 `P=1/R`：

| 情况 | 判定 |
|---|---|
| 输入法刚清空未提交预编辑 | 第一个到达的 Esc 距清空不超过 `D+2P` 时吞掉 |
| 已识别的取消键重复 | 后续 Esc 距上一个吞掉的 Esc 不超过 `2P` 时继续吞掉 |
| 普通 Esc 已分派 | 首次重复需在 `[max(0,D−2P),D+2P]` 内，且上一个 KeyUp 与此 KeyDown 紧邻，间隔不超过 `P/2` |
| 普通重复链 | 后续间隔不超过 `2P`，且仍有上述紧邻松开/按下特征 |
| 明显停顿、其他键、重新组词、失焦 | 结束原重复链；下一次符合输入准入的 Esc 正常分派 |
| `R=0` 或未获得有效 repeat_info | 不启动时间猜测，保留已有按下/松开保护 |

`2P` 是一个正常重复周期加一个周期的调度迟到/漏帧余量；初次窗口在系统 `D` 上加同样余量。普通 Esc 的 `P/2` 条件识别 Fcitx 在一个重复 tick 内紧邻转发的松开/按下，避免把真正松开后、接近短重复延迟的第二次按下吞掉。全部窗口随系统设置缩放，没有固定毫秒数或额外的全局 Esc 防抖。

依据：[Wayland 1.26.0 的 repeat_info 定义](https://gitlab.freedesktop.org/wayland/wayland/-/blob/1.26.0/protocol/wayland.xml)规定延迟单位为毫秒、频率为每秒重复数、0 禁止重复，并允许运行中更新。[KWin v6.7.5 的配置读取](https://github.com/KDE/kwin/blob/ab7df7ccb7c6af20f4b279cd6220f7cd3d2267d7/src/keyboard_input.cpp#L194)把 KDE `kcminputrc` 的 Keyboard/RepeatDelay 和 RepeatRate 发送为 repeat_info。测试直接核对真实协议消息，不从测试期望值推导产品阈值。

例如 `25Hz/600ms` 时，首个取消重复窗口为 680ms，连续窗口为 80ms；`40Hz/250ms` 时分别为 300ms、50ms。普通短重复延迟曾误吞约 210ms 的真双按；对应真实回归先失败，再加入松开间隔条件后通过。

这是节奏启发式，没有恢复物理按键身份。取消后的快速真再按若与重复节奏重合，或 GUI 严重迟到/批量交付真实松开和再按，仍可能无法区分。取消后明确停顿的政策由上述窗口确定；不能承诺取消后任意速度的真连按都被识别。普通空闲双 Esc 的 500ms 规格由真实输入、不同间隔和不同系统设置覆盖。若真实日常输入仍有冲突，备选接入层方案归 #72。

## 真实输入链路与结果

接缝已由 #67 贯穿约定及实施指令指定：独立 EIS 键盘 → 私有 KWin → 真实 Fcitx 5.1.23 / Rime Luna Pinyin → 产品 Composer / 产品窗。没有调用程序内部输入处理函数。回合场景接真实守护进程、固定 Claude CLI 和离线模型端点；模型回复被明确挂起，测试中只有真实的后续 Esc 才停止回合。

测试文件：

- [native_escape.py](../../crates/nd-desktop/tests/native_escape.py)：Composer 和产品窗的输入操作、公开编辑器/面板状态断言、整个长按期间的状态检查。
- [private_keyboard.c](../../crates/nd-desktop/tests/private_keyboard.c)：限定 `/sandbox/runtime` 和 `nd-test-ime` 的 EIS 键盘；产品窗模式附带 EIS 指针以点击真实输入框。一次长按只注入一次 Esc 按下与一次松开。
- [sessions.rs](../../crates/nd-daemon/tests/sessions.rs)：`native_rime_escape_hold_preserves_turn_and_real_double_escape_opens_rewind`，运行真守护进程、CLI、产品窗，确认没有额外发送。
- [test-scenarios.sh](../../scripts/test-scenarios.sh)：接入产品窗回归及四组 Composer 设置回归。

| 验证 | 结果 |
|---|---|
| 原失败测试：行中组词后按住 Esc 0.9s | 修复前 0→8；修复后 0→0，正文仍是“左🙂右” |
| 普通非组词 Esc 长按 | 新增红灯：1→10；修复后只增加一次 |
| 取消组词后明显停顿再按 | 增加一次；产品窗真实后续 Esc 仍可停止活动回合 |
| 活动回合中取消组词并长按 | 整个期间回合保持运行，面板不打开，没有误发送 |
| 空闲取消组词并长按 | 全程不打开、关闭回退菜单 |
| 回退菜单已打开时长按 | 只关闭一次，后续重复不重新打开 |
| 真双 Esc，约 30/210/450ms 间隔 | 两次动作均有效；产品窗打开回退菜单 |
| 提交预编辑后立即 Esc | 正常分派，无取消保护 |
| `25Hz/600ms`、`40Hz/250ms`、`12Hz/900ms` | 真实协议确认配置；长按、双按、提交均通过 |
| `0Hz/600ms` | 无自动重复；真重新按下、双按仍通过 |
| 运行中 KDE 配置通知切换为 `5Hz/900ms` | 两个 keyboard 对象收到新 repeat_info；新长按节奏及后续真 Esc 通过 |
| 关闭节奏识别的产品对照构建 | 真实长按误停活动回合，回归明确失败；恢复产品代码后通过 |

对照构建只临时关闭分派前的节奏保护，仍使用真实 KWin/Fcitx/Rime、守护进程、CLI 和产品窗；关闭动作没有提交。先前原始红灯、普通长按红灯、短延迟双按红灯和各自绿灯均保存。

## 运行方法与隔离

构建目录固定为 `/mnt/wd_external/nd-build/target/bug-67`，6 jobs，每次构建/测试使用 12GiB、零 swap 的独立用户 scope 或 service。遵循 `/home/zhuran24/mytools/new-desktop/research/impl/BUILD.md`。默认场景套件在正常用户服务环境运行，避免实施 shell 的 NoNewPrivs 干扰异 UID 验证；场景内部仍沿用各自的独立 slice、私有 HOME/XDG、断网 bwrap。

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-67
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo build --locked -p nd-desktop --features scenarios
python -B crates/nd-desktop/tests/native_escape.py \
  --bin-dir "$CARGO_TARGET_DIR/debug" \
  --output /mnt/wd_external/nd-build/tmp/bug-67/recheck --reconfigure
```

可用 `--repeat-rate 40 --repeat-delay 250` 等覆盖其他系统设置。测试需要 libei、C 编译器、pkg-config、Python dbus/GLib、wl-clipboard、KWin、Fcitx/Rime、rime_deployer、kwriteconfig6、bwrap 和用户 systemd。产品窗场景由 `scripts/test-scenarios.sh` 自动建立会话并执行。

全部事件仅发往私有显示 `nd-test-ime`。没有在日常 `wayland-0` 注入事件、打开实验窗或弹窗。测试结束后的 `cleanup.json` 核对 service/slice 无残留、临时根已删除；没有读取登录凭据或访问真实模型端点。

## 交付检查

集成基线已合并，#66 的鼠标组词保护与 #67 的按键节奏保护共同保留。

| 检查 | 状态 | 证据 |
|---|---|---|
| cargo test --workspace --locked | PASS：246 passed，0 failed，2 ignored | implementation/workspace.log |
| scripts/test-scenarios.sh | PASS：181 passed，0 failed，2 ignored；另加 4 组 Python 原生输入回归全部通过 | implementation/scenarios.log、full-product/、full-lab/ |
| clippy 全 workspace / targets / features，-D warnings | PASS | implementation/clippy.log |
| cargo fmt --all -- --check | PASS | implementation/fmt.log |
| Schema 文件集比对 | PASS | implementation/schemas.log |

忽略项沿用已有套件声明：workspace 的真实 CLI 分叉/两次压缩夹具生成验证，以及本地 CLI 提取/完整语料验证；场景套件的真实 OOM 杀进程，以及三个 60s 样本的 release CPU 验证。#67 的输入回归均实际运行，没有新增忽略项。

## 上游草稿与源码核实

两份英文草稿均未发布：

- [Fcitx GitHub issue 草稿](../upstream/fcitx5-escape-repeat.md)：取消组词后的合成松开/按下及 repeat provenance，明确 V1 运行验证、V2 仅静态核对。
- [KWin bugs.kde.org 草稿](../upstream/kwin-input-method-key-timestamp.md)：InputMethod::key 丢弃 time，Seat/Keyboard 的时间戳传播链与影响。

2026-10-10 重新核实安装包版本与 Cargo.lock，GitHub tag object 解析后通过 contents API 读取准确提交的源码。Fcitx `5.1.23` 为 `27cca6e0239938b2d53937061d0980c087856b22`；KWin `v6.7.5` 为 `ab7df7ccb7c6af20f4b279cd6220f7cd3d2267d7`。草稿引用准确源码行和提交，不把产品 workaround 表述成上游修复。

## 证据位置

新证据根为 `/mnt/wd_external/nd-build/tmp/bug-67/implementation/`：

| 路径 | 内容 |
|---|---|
| red/、green/ | 同一原回归：0→8 红灯、0→0 绿灯 |
| red-idle-hold/、green-full/ | 普通长按红灯和修复结果 |
| red-double-gap/、green-double-gap/ | 短重复延迟下约 210ms 真双按红灯和修复结果 |
| product-mutant/、product-final/ | 关闭保护后误停回合的对照；集成最新 v1 后产品回归 |
| fast/、slow/、disabled/、live-settings2/ | 非默认配置、关闭重复、运行中更新的协议和结果 |
| full-product/、full-lab/ | 完整场景套件的最终输入回归 |
| checks.status、workspace.log、scenarios.log、clippy.log、fmt.log、schemas.log | 最终常规检查 |
| fcitx-v1.cpp、fcitx-v2.cpp、kwin-inputmethod.cpp、kwin-seat.cpp、kwin-keyboard.cpp、kwin-keyboard-input.cpp | 重新读取的精确上游提交源码 |

旧定位证据仍位于父目录的 `red/`、`interface-hold/`、`interface-repress/`、`kwin-key-disassembly.txt`：首次物理 Esc 被 Rime 消费；Fcitx 重复复用原时间；KWin 应用端转发时间全为 0；GPUI 把每个 Pressed 构造为 is_held=false。当前修复没有改变这条上游输入链。

## owner_checklist

`OWNER_PENDING`：仅保留日常桌面和实体键盘所需确认。

- Rime、豆包各确认取消组词后长按不计新 Esc，不误停回合、不反复开合菜单。
- 在日常重复设置下确认明显停顿后的 Esc、普通空闲双 Esc；记录取消后快速真再按的启发式边界。
- 确认实际输入法焦点、候选框和物理输入延迟，没有新增交互问题。

私有虚拟显示中的真实输入法回归已经自动完成，不能标成 owner 未执行项；实体键盘与日常桌面则不能由这些结果代签。
