# #69 会话头固定布局验证

日期：2026-10-09。status=implemented；全部自动交付检查通过，日常桌面复核为 OWNER_PENDING。工单：[GitHub #69](https://github.com/zhuran24/new-desktop/issues/69)。

## 产品行为与原因

回合进行中且存在另存草稿时，会话头、会话标题和「会话设置」入口固定在对话区上方。对话和展开的设置正文在对话区滚动；打开设置定位到设置正文开头。底部输入区最多占窗口内容高度的一半，排队选项、附件和草稿信息在其上方独立滚动，输入框保持可用。

修复前，会话头与消息列表都在 `content` 的滚动内容中。跟随最新消息滚到末尾时，头部也被滚出视口。较小窗口中，底部草稿区的固有高度还会耗尽对话区域。该原因属于产品布局，不涉及测试工具的拖放源。

- `crates/nd-desktop/src/chat.rs`：固定的会话头与可滚动的消息内容分开渲染，头部使用当前会话快照。
- `crates/nd-desktop/src/settings.rs`：标题与设置开关固定，展开的设置正文进入对话滚动区域。
- `crates/nd-desktop/src/lib.rs`：中间区域允许收缩，底部信息区受高度约束并支持滚动；`scenarios` 构建只读绘制后的控件几何。

## 复现与回归证据

基线：`v1` 的 `c4632017fdce432fad4bdefd85a03402cc7b1a73`。工作树 `.worktrees/bug-69`，分支 `bug/69`。构建目录 `/mnt/wd_external/nd-build/target/bug-69`，6 jobs，独立 12 GiB／零 swap scope。

原生场景 `native_session_header_stays_visible_with_a_running_turn_and_saved_draft` 位于 `crates/nd-daemon/tests/sessions.rs`，驱动脚本为 `crates/nd-desktop/tests/native_header.py` 与 `native_input.c`。

场景用真守护进程、CLI 2.1.289、两个 mod、看守、systemd 和 SQLite；离线模型端点扣住第二回合回应。另一个界面身份通过公开 `session.draft.update` 的版本冲突产生一份另存草稿。窗口输入经过私有 KWin、真 fcitx5 和系统 Luna Pinyin Rime 数据；没有在程序内部注入键盘或鼠标事件。

| 证据 | 结果 |
| --- | --- |
| `red-rime-4/result.json` | 修复前，Rime 已上屏「你好」；1050×850 下会话头 `y=-172`，断言失败 |
| `green-2/result.json` | 修复后，两种窗口尺寸下会话头均为 `y=81`，设置入口均为 `y=112`；点击打开、关闭设置成功 |
| `red/result.json`、`green-long/result.json` | 首回合为 30 段落的长历史场景，修复前头部 `y=-1303`；修复后两种尺寸均通过 |
| 同一回归场景 | EIS 滚轮确实改变消息位置，会话头位置不变，滚动后设置入口仍可点 |
| 同一回归场景的公开快照 | Rime 上屏未增加提示数量，回合仍在进行，另存草稿仍存在 |

坐标为产品窗口内容中的逻辑像素；KWin 装饰窗分别为 1050×850、800×600，内容区分别为 1050×814、800×564。

证据目录：`/mnt/wd_external/nd-build/tmp/bug-69-evidence/`。每次原生运行保存截图、产品 stdout、私有 KWin/fcitx 日志、结果 JSON 和 `cleanup.json`。`green-2` 的六张截图分别记录两种尺寸的关闭设置、展开设置、滚动后状态。

仓库中的回归保留最小的短回答场景。`green-long` 使用同一窗口边界，将离线模型首条应答扩大为 30 段落；夹具差异见 `long-history-fixture.diff`。交付源文件的 SHA-256 与完整交付检查时一致。

## 复验命令

在本工作树中运行；构建及场景仍使用独立限额和离线环境：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-69
export CARGO_BUILD_JOBS=6
export ND69_NATIVE_OUTPUT=/mnt/wd_external/nd-build/tmp/bug-69-recheck
bash scripts/test-scenarios.sh native_session_header_stays_visible_with_a_running_turn_and_saved_draft -- --nocapture
```

场景需 KWin、Spectacle、fcitx5-rime、系统 Luna Pinyin 数据、bwrap、systemd 用户实例、C 编译器、wayland-scanner、libei 开发文件、Python 的 dbus／GObject／Pillow 模块和 `/dev/dri`。输出目录中旧文件会被本次运行覆盖，应为每次保留证据选择不同目录。

## 交付检查

检查日志位于证据目录的 `checks/`；最终退出码记录在 `checks/status.json`。

| 检查 | 结果 |
| --- | --- |
| `git merge v1` | 已是最新；基线为上述 `c463201` |
| `cargo test --workspace --locked` | PASS，退出码 0 |
| `bash scripts/test-scenarios.sh` | PASS，退出码 0；含 #69 原生回归及原有主题、设置、附件、历史、Esc 场景 |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | PASS，退出码 0 |
| `cargo fmt --all -- --check` | PASS，退出码 0 |
| `bash scripts/check-schemas.sh` | PASS，退出码 0 |

运行平台版本及最终源文件 SHA-256 见证据目录 `source-provenance.json`。`checks/native-header/result.json` 是完整套件中的 #69 回归结果；其清理记录确认专用单元／slice 无残留、临时目录已移除。

若执行入口继承 `NoNewPrivs=1`，完整场景中的异 UID 检查会因 `newuidmap: Could not set caps` 停止。该环境失败保留在 `checks/scenarios-inherited-nnp.log`。系统用户服务重新启动的检查进程为 `NoNewPrivs=0`（`checks/runner-status.txt`），异 UID 检查照原判据通过。需要从这种受限入口重跑完整场景时，使用临时服务，脚本中的 cargo build/test 仍各自受限：

```bash
systemd-run --user --wait --pipe --collect \
  --unit=nd-test-bug69-recheck \
  --working-directory="$PWD" \
  -p MemoryMax=12G -p MemorySwapMax=0 \
  --setenv=CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/bug-69 \
  --setenv=CARGO_BUILD_JOBS=6 \
  -- bash scripts/test-scenarios.sh
```

## 隔离与清理

原生场景使用临时 HOME、Claude/XDG 目录、私有 D-Bus 和独立 `nd-test-` 单元／slice。bwrap 断网，不暴露 owner 的 Wayland、X11、D-Bus、输入设备或凭据目录。输入助手在连接前强制检查私有运行目录与 `nd-test-header` socket；不创建 uinput 设备。私有 KWin 退出后，专用单元、slice 与临时目录由 `Sandbox` 清理，结果在各次 `cleanup.json` 中核对。

## owner_checklist

- `OWNER_PENDING`：在测试工具可安全用于日常桌面后，用其产品窗口场景复核 1050×850 和 800×600：回合进行中、有排队选项和另存草稿时，头部可见、设置可点、滚动不移动头部。私有 KWin 的自动复验不代替这项日常桌面确认。
