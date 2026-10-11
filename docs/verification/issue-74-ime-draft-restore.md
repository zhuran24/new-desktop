# #74 组词中载回另存稿

2026-10-11 的实施与验证记录。分支 `bug/74`；红绿阶段基线为 `v1` 的 `70ff2a8227e86f49f3e895f5474fc88045bfbd37`，交付基线为包含 #75 的 `791c3aa3d4174050fab68bfe05ade444396597fb`。全部自动检查通过；日常桌面验收为 `OWNER_PENDING`。

## 原因与修法

载回按钮原来只在点击完成时由 `restore_saved_draft` 检查现场组词状态。真实协议记录显示，KWin 先交付 `preedit_string(nil)`、`commit_string("ni hao")`，随后才交付鼠标按下；此时现场 `composing` 已为 false，载回把正在输入的文字替换成另存稿。

载回按钮与发送、会话导航共用 Composer 的成对鼠标守卫：按下时锁存最近一次渲染及现场的组词状态，点击完成时消费锁存结果。被拦截的点击产生 `CompositionClickBlocked`，界面显示「请先完成组词，再继续操作」；完成组词后的独立点击正常载回。程序入口原有的现场组词检查仍生效。

产品代码在 `crates/nd-desktop/src/drafts.rs`、`chat.rs`；共用守卫在 `composer.rs`。没有更改输入法、合成器或依赖版本。KWin 提交拼音并重置候选的行为遵循 [#66 已接受的范围](https://github.com/zhuran24/new-desktop/issues/66#issuecomment-6096963586)，候选保留继续由 #71 跟踪。

## 回归接缝

`crates/nd-daemon/tests/sessions.rs` 的 `composing_click_does_not_restore_saved_draft_with_real_rime` 接入 `scripts/test-scenarios.sh`。它经公开 nd-wire 的草稿版本冲突生成「另存草稿🙂」，然后运行 `crates/nd-desktop/tests/native_ime_clicks.py --saved-draft <id>`。

输入进入私有虚拟 KWin、真实 Fcitx/Rime 和产品 Composer，不调用产品输入处理函数。每种主题单独打开产品窗口，以私有剪贴板与真实按键建立行中组词 `leftni haoright`。控件矩形及 GPUI 实际裁切区只读；被裁切的载回按钮经私有 EIS 滚轮展现后再点击。点击前再次记录 `composing=true`、选区及焦点，避免把未命中按钮的点击当作动作保护证据。

每个手势都要求点击前无警告，点击后出现「请先完成组词」，同时核对正文、选区、焦点、会话保持不变，公开 nd-wire 中没有新增人类提示。继续观察 0.7 秒，排除异步保存或同步迟到覆盖正文。KWin 重置 Rime 后，重新经真实输入法确认「你好」，再独立点击载回；界面与 nd-wire 必须都读到另存稿正文，另存稿列表保持不变。

## 执行证据

证据根目录：`/mnt/wd_external/nd-build/tmp/bug-74/`。环境为 KWin 6.7.5、Fcitx 5.1.23、fcitx5-rime 5.1.16；Rime 使用系统 Luna Pinyin 数据部署，没有复制个人词库。守护进程、看守、钉住的 Claude 2.1.289 与 mod 为真实进程，模型端点为断网沙箱中的离线端点。

| 执行 | 结果 | 证据 |
|---|---|---|
| 产品修复前 | 明暗两种主题均 FAIL：正文被换为「另存草稿🙂」，选区改变，警告为空 | `red-final/result.json`、`red-final/input.jsonl`；修复前测试差异为 `red-test.patch` |
| 产品修复后 | 明暗两种主题均 PASS：正文、选区与焦点保持；点击前警告为空，点击后出现新提示；完成组词后载回及持久化成功 | `green/result.json`、`green/input.jsonl`、`green/restore-*-blocked.png`、`green/restore-*-restored.png` |
| 拦截生效但删去提示的变异体 | 明暗两种主题均 FAIL，失败发生在缺少本次提示的断言 | `warning-mutant/result.json`、`warning-mutant/input.jsonl`、`warning-mutant.diff`；检查脚本在 finally 中恢复产品源文件 |

协议顺序的直接证据在 `red-final/restore-light.wayland.log` 第 4806–4809 行；各次真实输入、nd-wire 读数和点击前后的画面状态在对应 `input.jsonl`。截图与 JSON 都确认明暗主题。

`red/`、`red-reproduced/`、`red-click/`、`red-visible/` 为驱动调通记录：分别遇到输入法影响前置正文、按钮被裁切、缺少有效滚轮身份，均不作为产品覆盖正文的失败证据。有效红阶段仅使用 `red-final/`。

#74 的原生运行使用临时 HOME、CLAUDE_CONFIG_DIR、XDG、私有 D-Bus 和 `nd-test-native` socket，bwrap 断网；helper 拒绝其他 socket，也不创建 `/dev/uinput`。各目录的 `cleanup.json` 验证私有单元和 slice 无残留、临时根已删除；`owner-processes.json` 验证 owner 的 KWin/Fcitx PID 与启动时间前后相同。没有在 owner 的 `wayland-0` 注入事件、弹窗或加载模块。

构建与 Cargo 检查串行使用 `/mnt/wd_external/nd-build/target/bug-74`，6 jobs、独立 systemd cgroup、12 GiB 内存上限、零 swap。场景检查由独立 `NoNewPrivileges=no` 用户服务启动，避免 agent 继承的 `NoNewPrivs=1` 阻断既有异 UID 场景；运行时隔离策略不变。

## 交付检查

| 检查 | 结果 | 证据根目录下的文件 |
|---|---|---|
| `git merge v1` | 自动合入包含 #75 的最新 v1，无冲突；合并后重跑全部检查 | `merge.log` |
| `cargo test --workspace --locked` | 246 passed、0 failed、2 项既有 ignored | `workspace.log` |
| `scripts/test-scenarios.sh` | 183 passed、0 failed、2 项既有 ignored；另含默认、快速、慢速和关闭重复四组真实 Esc 回归，全部 PASS | `scenarios.log`、`merged-escape/*/result.json` |
| Clippy workspace/all-targets，默认及 all-features | 两种配置均通过 `-D warnings` | `clippy-default.log`、`clippy-all-features.log` |
| `cargo fmt --all -- --check` | PASS | `fmt.log` |
| Schema 文件集与生成 mod 类型比对 | PASS，完整生成集无差异 | `schemas.log` |

完整场景套件在合并后再次运行载回、组词中发送及导航、Esc 产品分派、候选定位、文件拖入和主题切换。载回与相邻输入法回归的呈现及清理证据分别在 `merged-ime-restore/`、`merged-ime-clicks/`、`merged-ime-position/`；各命令退出状态与用时记录在 `checks.json`。合并前的完整检查日志保留在 `pre-merge-checks/`，没有新增忽略项。

## 复验

输出目录必须不存在。按 `research/impl/BUILD.md` 在独立限额 systemd 用户服务中执行：

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-74
export CARGO_BUILD_JOBS=6
export ND74_IME_OUTPUT=/mnt/wd_external/nd-build/tmp/bug-74/another-run
scripts/test-scenarios.sh composing_click_does_not_restore_saved_draft_with_real_rime -- --exact --nocapture
```

## owner_checklist

- `OWNER_PENDING`：在安排好的日常桌面验收时段，由 `tool/ime-bench` 按 owner 的屏幕排列分别用 Rime、豆包和明暗主题复核组词中载回。检查本次点击的新提示、正文、选区和焦点，以及确认文字后的正常载回。私有虚拟输出回归不代表这项已完成。
