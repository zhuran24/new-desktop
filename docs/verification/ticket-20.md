# #20 导航条与历史分页验收

日期：2026-10-06。实现与自动验收完成；日常桌面上的呈现延迟与 CPU 为 OWNER_PENDING。

## 对外行为

导航消费 `lineage` 的稳定轮身份，每轮一条短横线。虚拟列表只构建可见的导航格；悬停显示轮次与最多 160 字的提示预览，点击通过 `SyncReplica::get` 按需读取历史。正文以 60 条为一个窗口，支持更早、较新与回到最新。阅读历史时流式输出保持阅读页；切会话、切段、快速连续跳转会撤销旧查询结果的使用资格。

`session/<id>/items` 的查询契约见 [会话库 README](../../crates/nd-session/README.md#历史页与导航)；`PageReq` 和 `Page` 是唯一 Rust 类型源，Schema 在 [request](../../protocol/request.schema.json) 和 [page](../../protocol/page.schema.json)。查询在独立连接上进行，不阻塞发送、审批等命令队列。

冷快照提供最近一页正文、进行中条目的完整累计内容和完整轮导航/谱系。历史正文仍持久保存在显示缓存中；事件会移除滑出快照窗口的条目。分页游标绑定会话与段，不依赖进程纪元，因此能跨守护进程重启续读。分页回应带事件流观察点；它不能覆盖查询期间已经收到的更新。

## 自动验收

| 验收 | 接缝与测试 |
|---|---|
| 千轮冷启动正文有界、1000 格导航、任意轮定位 | 真守护进程/CLI/mod/看守/systemd/SQLite，`a_thousand_real_rounds_open_bounded_and_jump_through_public_pages` 实际运行 1000 个模型端点回合；冷快照正文 ≤60，跳转第 1/500/1000 轮，模型请求总数仍为 1000 |
| 原生导航悬停、点击、实际帧 | 上述千轮场景及 `native_navigation_jumps_to_a_round_via_history_page` 在私有 KWin 中使用真实 GPUI；定位可见导航格后发送鼠标移动/点击事件，核对 tooltip、页锚点、首条正文、行数与实际 Wayland buffer，保存截图 |
| 历史分页、重启、只读 | `history_pages_are_read_only_ordered_and_resume_after_restart`；逐页升序、跨页不重、两轮提示齐全，重启仍可读，坏游标失败，无额外模型请求 |
| 与简单投影一致 | `walking_pages_matches_the_simple_projection_and_cursors_survive_append`；137 条输入逐页结果与既有 `projection::project` 完全相同；追加不挪旧边界；拒绝跨会话游标和非法 limit |
| 合轮、旁支、旧锚点 | `merged_prompts_share_one_mark_and_old_branch_cursors_cannot_jump_into_a_new_segment`；合轮一格、只排除明确非当前段、当前段既有提示仍可导航、切段使旧游标失效 |
| 长正文 | `a_thousand_rounds_open_as_one_bounded_page_with_all_navigation_marks`；1000 轮、每轮长回答，冷快照只带 30 个提示和 30 个回答，完整 1000 轮预览，第 500 轮边界正确 |
| 异步界面竞争 | `nd-view-model/tests/history.rs`；快速跳转、切会话、读取旧页时到来新输出、页回应晚于流式更新 |

日志：`/mnt/wd_external/nd-build/tmp/ticket-20/logs/`。最终测试数量、提交与原生证据在本机实施记录 `research/impl/tickets/20.md` 的 verification 中记录。

复验命令（在 ticket-20 工作树）：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-20
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- cargo test --workspace --locked
RUST_TEST_THREADS=3 scripts/test-scenarios.sh -- --nocapture
```

原生截图检查依赖系统 Python 的 Pillow，用于排除尚未呈现窗口的空帧；它不是延迟测量。

场景严格使用临时 HOME/CLAUDE_CONFIG_DIR/XDG、bwrap 断网、`nd-test-` 独立单元与限额 slice。仅替换模型端点；千轮场景将 mod 长轮询超时缩短到 20 ms。默认不触及真服务或 owner 的配置、凭据、会话。

## 待验证项与边界

本单没有编号待验证项。千轮传输规模、页边界和跳转走主方案；GPUI 实际导航与取页有自动窗口证据。规格未为“打开不卡”规定毫秒阈值，协议耗时不能代替屏幕呈现延迟；日常桌面的最终流畅度与空闲 CPU 按实施记录中的 owner_checklist 实测。

历史读取只依赖产品显示缓存，未新增 CLI 原生字段依赖或写 CLI 存储例外。CLI 压缩不删除已持久的显示正文；原生压缩/回退的操作验收仍由其工单负责。合轮和旁支在公开谱系/投影值接口验证，不把纯值测试写成真 CLI 回退已执行。外部历史导入由后续工单写入实际正文与谱系；缺正文时给 `history_unavailable`，不猜轮或锚点。

没有新增结构或派发操作；沿用既有引擎崩溃矩阵。导航属于常驻聊天外壳，不新增可选组件。所有视图的颜色、字体和间距消费应用主题变量。
