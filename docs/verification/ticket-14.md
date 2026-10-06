# #14 第一个能聊的界面验证

日期：2026-10-06。状态：实施和离线自动验收完成；真机输入法、实际性能和真模型对话待 owner 验收。产品接口见[桌面说明](../../crates/nd-desktop/README.md)，CLI 行为依赖见[契约清单](../cli-contracts.md)。

## 验收映射

| 要求 | 测试与结果 |
|---|---|
| 新建前获取后端模型列表 | `nd-daemon/tests/sessions.rs::new_session_models_come_from_the_backend_before_any_conversation`：真实 CLI 返回可选模型，无产品会话、无模型请求；不存在的目录和后端报错 |
| 侧栏准备中、部分完成、创建失败 | `nd-view-model/tests/chat.rs::sidebar_keeps_preparing_partial_and_withdrawn_notices_visible`；实际创建成功/两种失败的主接缝沿用同文件中的 #13 场景 |
| 流式正文及最终块替换、第二轮对话 | `desktop_client_cold_reopens_during_a_delta_and_continues_the_conversation`：产品 `CommandClient`/`ReplicaFeed` 驱动真实守护进程与 CLI，累积正文递增，完整块同 id，第二轮请求带第一轮历史 |
| 增量期间杀界面重开 | `native_chat_window_creates_and_recovers_during_streaming_markdown`：真实 GPUI 表单选目录和 CLI 返回的模型，Composer 提交；未闭合代码块期间 SIGKILL GPUI，冷启动取累计快照，最终同 id 只有一块，不新增模型请求 |
| Markdown/代码块最简版 | 同一原生场景绘制中文标题、粗体、Rust 代码及未闭合围栏；明暗两套实际 Wayland 帧与截图均生成并人工查看。渲染使用固定 Kit，正文整体投影，无另一个 CLI 解码器 |
| 排序、未知条目、失败解释和草稿清空 | `nd-view-model/tests/chat.rs` 其余 3 项：按 seq 排序，未知条目保留后备文字，完成的创建不挤占聊天区，部分失败保留原因；旧修订的受理不能清掉新文字，交付不明不视为受理 |
| 贯穿约定 | `models` 是新增公共 nd-wire 请求及 Rust 生成的 Schema；创建/发送沿用现有操作，未新增引擎矩阵行或 CLI 存储写入例外；常驻聊天和可卸载槽位的边界保持；颜色、字体、间距取主题变量 |

## 自动检查

交付基线：实现提交 `04b72aa`，集成提交 `0633efc` 合入 v1 `74c4f02`（#15 谱系）；CLI 契约文档保留两单内容。测试在工作树 `ticket/14`，构建产物为 `/mnt/wd_external/nd-build/target/ticket-14`，6 jobs，独立 12 GiB/零 swap scope。性质测试使用 512 个样本、固定 seed 20261006。

- `cargo test --workspace --locked`：161 passed，0 failed，1 ignored（#8 既有的显式真 CLI 现场测试）。
- `scripts/test-scenarios.sh`：84 passed，0 failed，1 ignored（#6 会触发桌面通知的手动 OOM 项）。本单新增 3 项主接缝/原生场景；无真实模型端点。
- `cargo clippy --workspace --all-targets --features nd-daemon/scenarios,nd-testkit/scenarios,nd-claude/scenarios,nd-desktop/scenarios --locked -- -D warnings`：通过。
- 格式、diff 空白、Python 语法和 shell 语法检查通过；nd-wire/mod Schema 重生成后与提交内容无差异。
- release 桌面、守护进程和看守构建完成（11.21s）；桌面 SHA-256 `63879ae2145b6eb94f2c71c75caedac05b751c6ff6af58d1bb14cfeaed4617f5`，构建日志 `release-merged.log`。

现场日志目录 `/mnt/wd_external/nd-build/tmp/ticket-14/logs/`，原生最终证据 `/mnt/wd_external/nd-build/tmp/ticket-14/native-presented/`（`result.json`、`streaming.png`、`dark.png`、`light.png`、`cleanup.json`、`hashes.txt`）。`result.json` 保存故障前/冷启动/完成三个快照；`cleanup.json` 记录专用单元和临时根已清理。截图在副本日志后等待呈现再采样，脚本改动后定向原生场景复验通过（`native-presented.log`）；截图等待不用于性能测量。场景使用临时 HOME/CLAUDE_CONFIG_DIR/XDG、bwrap 断网、真实 systemd/SQLite/CLI/mod/看守；原生窗口使用私有 KWin/D-Bus，没有向日常桌面注入输入。

红绿证据 `red-01..07.log`、`green-01..07.log`：模型目录、侧栏、消息投影、跨 executor 命令与恢复、草稿修订、原生场景驱动、会话状态呈现。新增 API 的红灯为编译缺口；原生场景首轮红灯是缺少测试驱动脚本，不冒称捕获了恢复算法缺陷。真实 CLI 的 `haiku` 解析到带日期的模型 ID，场景路由按实际请求安排，产品没有写死此映射。

## 待验证项与证明边界

#14 无单独编号的待验证项。INDEX #14 所列新建、模型来源、状态投影、流式 Markdown、原生进程冷启动累积一致性均已自动验证，采用 GPUI 主方案。模型查询失败时禁用创建并显示原因，不猜模型。未闭合围栏有原生窗口证据。

以下保持 OWNER_PENDING，不使用历史 ime-lab 结果、合成输入或虚拟 KWin 代替：豆包/Rime 的真实输入链路、候选框、上屏 p95 ≤50 ms、静止 CPU <1%、真实模型服务。中文输入法验收失败时按 ADR 0001 改 gtk4-rs。辅助进程本次只做短命只读模型目录，未关闭第 7 步 R12-X3/X6 的通用辅助进程问题。

真实验收步骤保存在不入库的实施记录 `research/impl/tickets/14.md` 的 `owner_checklist`；输入法和 CPU 的离线入口也见 [COMPOSER.md](../../crates/nd-desktop/COMPOSER.md)。三种发送意图/Esc（#18）、持久草稿（#16）、分页（#20）、设置与降级组件（#21/#22）、主题文件（#23）和完整账号/额度（#30）保持各自工单范围。
