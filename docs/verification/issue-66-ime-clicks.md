# #66 组词中点击发送与会话导航

日期：2026-10-09。状态：**部分修复，工单严格验收尚未关闭**。产品已阻止同一次点击误发或误导航；KWin 6.7.5 在交付按钮事件之前提交拼音并重置 Rime，候选保留要求仍未满足。

工作树为 `/home/zhuran24/mytools/new-desktop/.worktrees/bug-66`，分支 `bug/66`。集成基线为 `v1` 的 `c4632017fdce432fad4bdefd85a03402cc7b1a73`。构建使用 `/mnt/wd_external/nd-build/target/bug-66`、6 jobs、12 GiB 内存上限、零 swap。

## 当前行为

| 检查 | 结果 | 边界 |
|---|---|---|
| 组词中点击发送 | PASS | 公开 nd-wire 中没有新提示；界面提示先完成组词 |
| 组词中点击另一会话、新建会话 | PASS | 保持原会话，没有进入新建表单 |
| 原文、选区、编辑焦点 | PASS | `ni hao` 留在输入框，编辑焦点保留 |
| 重新经 Rime 确认“你好”后发送、导航 | PASS | 发送恰好一次；切换会话、新建表单正常 |
| 同一次点击后候选、预编辑仍在 | FAIL / UPSTREAM_LIMIT | KWin 已提交拼音并要求输入法 reset，`composing=false` |
| 日常桌面与豆包 | OWNER_PENDING | 本次未操作日常桌面，未启动豆包 |

这不是 `not-a-product-bug`：虽然原始提交由合成器触发，产品在随后执行发送或导航，确实违反组词中的动作保护。

## 原因与保护位置

KWin 6.7.5 的 [`InputMethodEventFilter::pointerButton`](https://github.com/KDE/kwin/blob/v6.7.5/src/input.cpp#L1949) 在把按下交给客户端之前调用 `commitPendingText()`；[`InputMethod::commitPendingText`](https://github.com/KDE/kwin/blob/v6.7.5/src/inputmethod.cpp#L200) 提交 pending text 并向输入法发送 reset。它对真实指针与私有 fake-input 走同一条输入过滤路径。

新回归的原始协议日志同样显示 `preedit_string(nil)`、`commit_string("ni hao")`、`done`，随后才是 `wl_pointer.button(..., 272, 1)`。因此只在鼠标按下或松开时读取实时 marked range 都太晚。GPUI 固定版本自身也有按下时 reset/unmark 的路径，但在此次 KWin 日志里，拼音提交已经先于该路径。

`crates/nd-desktop/src/composer.rs` 保存最近一次 Composer 渲染中的组词状态，按下时把它与现场状态合并后锁存；松开后的重画不能重新放行这次点击。发送、侧栏会话、新建会话使用同一个成对守卫。键盘与程序命令继续使用原来的现场组词检查。界面收到 `CompositionClickBlocked` 后显示“请先完成组词，再发送或切换会话”。没有修改 KWin、Fcitx、Rime、GPUI 依赖或 owner 配置。

这个保护解决动作误执行；它不声称恢复已被 KWin 重置的候选。严格保留候选需要输入法接入层或合成器支持，尚未选定方案，不作为已完成项。ADR 0001 的中文输入验收门槛仍有效。

## 回归证据（执行记录，截至本报告日期）

证据根目录：`/mnt/wd_external/nd-build/tmp/bug-66/evidence/`。

| 执行 | 结果与文件 |
|---|---|
| 原工具独立定向复现 | `reproduce-2/result.json`；实验窗与产品窗误发，产品新建导航失败 |
| 仓库回归，产品修改前 | `red-3/result.json`；三个控件都 FAIL：发送 `ni hao`、切走会话、进入新建表单 |
| 同一仓库回归，保护生效后 | `green-final/result.json`；三个控件都 PASS；`*-before.png`、`*-blocked.png` 为合成器截图 |
| 输入与协议 | 各次的 `input.jsonl`、`*.wayland.log`、`*.jsonl`；记录真按键、指针、nd-wire 快照和只读界面呈现 |
| 清理与日常进程 | 各次的 `cleanup.json`、`owner-processes.json`；私有单元和临时根目录清理完成 |

前两次新驱动运行 `red/`、`red-2/` 的 KWin 查询失败，是驱动调通记录，不能作为产品失败证明；红阶段产品证据只取 `red-3/`。`green/` 最初的进程快照只列出可读环境的 Fcitx，未把带能力、环境不可读的 KWin 列入；`green-final/owner-processes.json` 按公开 `--socket wayland-0` 参数另核 KWin，确认 PID 2487 与 Fcitx PID 2562 及各自启动时间前后相同。

`native_ime_clicks.py` 用系统 Luna Pinyin 数据部署私有 Rime，不复制个人词库。产品窗口只连接真守护进程、钉住的 Claude 2.1.289、真看守与 mod、离线模型端点。输入经过私有 KWin 与真输入法；没有调用产品输入处理函数，没有直接写守护进程内部表。`scenarios` feature 的控件矩形、Composer 呈现和导航观测只读，不产生输入事件。普通产品构建不包含这些观测画布。

可复验命令（输出目录必须不存在）：

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-66
export CARGO_BUILD_JOBS=6
export ND66_IME_OUTPUT=/mnt/wd_external/nd-build/tmp/bug-66/another-run
scripts/test-scenarios.sh composing_clicks_do_not_send_or_navigate_with_real_rime -- --exact --nocapture
```

## 交付检查

- `git merge v1`：基线已是最新 `v1`，返回 Already up to date。
- `cargo test --workspace --locked`：PASS，`workspace.log`。
- Clippy workspace/all-targets 的默认 feature 与 all-features 两种构建均 PASS（`-D warnings`），`clippy-default.log`、`clippy.log`。
- `cargo fmt --all -- --check`：PASS，`fmt.log`。
- `scripts/check-schemas.sh`：PASS，`schemas.log`，完整生成集无差异。
- `scripts/test-scenarios.sh`：PASS，`scenarios-fresh-service.log`；81 个会话场景通过，包含新增真 Rime 回归。看守套件 16 通过，按既有约定忽略会产生真实 OOM 桌面通知的手动测试。

直接从 agent 进程跑场景时，既有异 UID 检查被继承的 `NoNewPrivs=1` 阻断，报 `newuidmap: Could not set caps`（`scenarios.log`）。独立 systemd 用户服务在私有 namespace 中的 UID 映射探针成功；完整场景套件使用这一启动环境复跑，没有跳过检查或改变测试断言。

## 未关闭项

候选保留属于自动复现已证实的上游限制，不能转成 owner 已验或以通过动作保护代替。当前实现没有达到 #66 引用的完整 A1 候选保持要求；工单保持未关闭，范围决定待 owner 确认。

## owner_checklist

- `OWNER_PENDING`：在安排好的日常桌面验收时段，用 Rime 和豆包确认三处点击不误发、不导航，确认后操作正常。必须先修复来源报告列出的工具安全问题，再安排该工具接管真实输入。
- `OWNER_PENDING`：亲自体验中文编辑与按钮提示。此次私有虚拟输出回归不能代替打字手感、真实输出延迟或 CPU 验收。
