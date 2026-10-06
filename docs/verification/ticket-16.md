# #16 守护进程草稿验证

日期：2026-10-06。状态：实现与自动验收完成；真实输入法的交互复核保留在 owner_checklist。草稿持久化、并发落败另存、恢复、双窗口同步和发送事务内清稿已实现。协议和后续回填约定见[会话说明](../../crates/nd-session/README.md#持久草稿)，操作方式见[桌面说明](../../crates/nd-desktop/README.md)。

## 验收映射

| 要求 | 证据 |
|---|---|
| 界面重开仍有草稿 | 主接缝 `draft_survives_ui_close_and_daemon_crash`：中文、多行、emoji 保存后关闭同步副本，再 SIGKILL 守护进程；重开快照相同，原命令回原收据，没有新增模型请求 |
| 两台界面各改一次 | `draft_two_devices_preserve_the_loser_and_publish_it_to_both`：两份真实同步副本同时从版本 0 编辑，当前稿只有一个胜者，另一份保留命令 id、设备、原版本、原文；同 id 重试不重复，改内容回 conflict；守护进程重启后仍在，取回后通过事件同步 |
| 发送不会清掉后来编辑 | `draft_send_consumes_only_the_matching_version_atomically`：匹配版本及正文才清空；旧命令重试、过期版本发送、非法编辑、非法发送均不清后来草稿 |
| 原生输入框自动保存和恢复 | `draft_native_windows_save_reopen_follow_and_recover`：真实 GPUI 输入，SIGKILL 后原生冷启动恢复；第二窗口修改同步给第一窗口；载入另存稿；输入后立即提交先等保存，再使两窗口清稿。实际模型只收到创建提示和明确发送的提示 |
| 异步界面保护 | `nd-view-model/tests/chat.rs`：旧保存回执不覆盖新编辑，远端更新不打断组词，落败采用当前稿，发送回执保留后续输入；发送未确认时后续快照不覆盖窗口正文，只有用户编辑或明确重试保存才解除保护 |
| 收据恢复 | 主接缝经 `CommandClient::receipt` 查回已保存草稿收据，查不存在的命令返回 Missing，草稿版本不变；桌面保存结果不明后重连/重试只查原收据，不把查询 unavailable 当作命令未受理 |
| 每个提交点可恢复 | `nd-session/tests/matrix.rs::draft_conflict_and_send_consumption_recover_at_every_commit_point`：正常运行及三种崩溃时点逐提交重开，落败稿只有一份、原生发送生效一次、后来编辑保留 |
| 贯穿约定 | 全部操作走 nd-wire，新增类型由 Rust 导出 Schema；无新 CLI 依赖或 CLI 原生存储写入，无新增可选组件；新发送场景已加入引擎矩阵；桌面样式来自主题变量 |

草稿是会话核心事实，显示缓存也随事务提交。冷启动从核心投影草稿，旧核心缺少字段时默认空稿版本 0。版本冲突另存不改变当前稿的版本，另存结果属于持久化成功的 Done。新建表单尚无会话身份，首条消息受理前仍是窗口内文字；附件结构由 #17 接入。

## 自动检查与环境

基线 `v1=6d77bed7538d142202fd0291d24d4379d0fcfcd0`，工作树 `.worktrees/ticket-16`，分支 `ticket/16`。交付前 `git merge v1` 为 Already up to date。构建位于 `/mnt/wd_external/nd-build/target/ticket-16`，6 jobs，全部 build/test/clippy 经独立 12 GiB、零 swap scope，没有使用 /tmp 构建退路。Cargo.lock 未改变。

| 检查 | 结果 |
|---|---|
| `cargo test --workspace --locked` | 165 passed，0 failed，1 ignored（既有的 #8 真 CLI 现场测试） |
| `scripts/test-scenarios.sh` | 88 passed，0 failed，1 ignored（既有的手动真 OOM 测试） |
| 最终界面保护和收据查询修正后的 `--test sessions` | 19 passed，0 failed；含本单 4 项与 #14 原生创建/流式恢复 |
| Clippy | workspace/all-targets，含 daemon/testkit/claude/desktop scenarios，`-D warnings` 通过 |
| 格式及协议 | cargo fmt、git diff --check、Python AST、脚本语法通过；四份 draft Schema 可重复生成 |
| release | 桌面、守护进程、看守构建通过；二进制散列在不入库实施记录及现场 hashes 文件 |

日志目录 `/mnt/wd_external/nd-build/tmp/ticket-16/logs/`。完整场景日志 `scenarios.log`，最终会话场景 `sessions-final.log`，最终全量、Clippy、release 分别为 `workspace-final.log`、`clippy-final.log`、`release-final.log`。性质测试 `PROPTEST_CASES=512`、`PROPTEST_RNG_SEED=20261006`。

TDD 红绿记录：01 持久化/重开；02 两设备落败稿；03 发送清稿；04 旧回执与新编辑；05 组词/发送后的新编辑；06 原生保存/重开/恢复；07 原生立即发送；08 未确认发送保留正文；09 跨 executor 收据查询（绿灯并入最终会话场景）。新增 API 的红灯为编译缺口；06/07 的初始失败是缺少测试驱动的对应入口，不冒称捕获了产品恢复算法缺陷。

原生复验还捕获了保存提示早于 Change 通知的问题：编辑器已有新文字时，旧编辑状态不能继续显示“已保存”。提示与测试观测现在同时核对真实编辑缓冲和已确认版本，故障证据在 `native-indicator-red.log` 及 `native-indicator-red/`。立即发送场景只要求已受理后的稳定空稿与实际模型请求，不要求保存前正文单独占一帧；最终 19 项会话场景通过。

原生证据目录 `/mnt/wd_external/nd-build/tmp/ticket-16/native-final/`，包含 `result.json`、各窗口的观察日志、`draft-two-windows.png`、`cleanup.json`。截图已查看，草稿保存状态、另存稿、取回入口和输入正文均可见；清理记录中剩余单元为空、临时目录已删除。#14 回归截图在同级 `native-chat/`。

测试使用真实守护进程、CLI 2.1.289、mod、看守、systemd、SQLite，只替换模型端点；每场景使用临时 HOME/CLAUDE_CONFIG_DIR/XDG、bwrap 断网和限额 slice。原生窗口在私有 KWin/D-Bus 中运行，未接入 owner 的显示会话、登录、真实会话、mod、Chrome 或代理。

## 待验证项与 owner_checklist

#16 没有专属编号待验证项。INDEX #16 列出的两份同步副本竞争、落败稿可找回、杀界面重开均通过自动实测，采用守护进程版本比较及落败另存的主方案，无需退路。#22 的总结后回填、#18 的撤回、#35 的回退由对应操作调用同一事务内回填入口验收，不把这些尚未实现的后端动作算作本单通过。

真实豆包/Rime 的输入链路仍为 OWNER_PENDING。复核时在同一个独立测试守护进程上打开两份 `nd-desktop --state` 不同的窗口，选中同一测试会话：

- [ ] A 用豆包、再用 Rime 保持组词，B 修改草稿；A 的组词、候选框和选区不被覆盖，组词中 Enter 不发送。完成组词后若基准过时，A 的全文出现在另存稿中。
- [ ] A 保持组词时点侧栏切换或「载入这份草稿」，应提示先完成组词，当前文字留在原会话；完成后操作成功。
- [ ] 发送保存好的草稿后马上继续输入或组词，受理只清原稿，新文字保留并随后保存。

可执行的独立实例准备、release 路径、双窗口命令和清理步骤见不入库的 `/home/zhuran24/mytools/new-desktop/research/impl/tickets/16.md` 的 owner_checklist。界面 p95、静止 CPU 和真实模型对话沿用 #14 的真机验收，不以本次虚拟 KWin 或伪模型端点声称通过。输入法验收失败时按 ADR 0001 采用 gtk4-rs 退路。
