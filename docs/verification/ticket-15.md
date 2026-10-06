# #15 谱系验证记录

日期：2026-10-06。状态：单项验证通过，等待最终集成检查。实现和接口见 [会话组件说明](../../crates/nd-session/README.md#谱系与轮导航)。本记录只证明纯函数及隔离真 CLI 的谱系行为，不证明回退、换后端等后续结构操作已经实现。

## 验收与证据

| 工单要求 | 自动验证与结论 |
|---|---|
| 谱系纯函数直接测 | `nd-session/tests/lineage.rs`：落地不等于开轮、多消息合轮、进行中到结束、轮与消息/票/原生位置分离、显式回退/清空/外部续写边、嵌套拓扑、当前段切换、共同前缀、会话分叉来源、换后端有效区间、导入及同步点作废、重复/非法事实拒绝；512 组随机截点及序列化往返 |
| 轮 id 与 Claude user uuid 对应 | `human_rounds_map_to_cli_uuids_and_survive_restart`：同步副本读真守护进程，轮位置与回显一致，CLI 自己落盘的 user/assistant 行分别含提示 UUID/最终锚点；重启后索引原样保留；SIGSTOP 时“已写出”不计新轮 |
| 多条 user 合一轮 | `coalesced_cli_prompts_share_one_navigation_round`：真 CLI 暂停后连续写两条提示，再恢复；一次模型请求同时含两条，导航恰好增加一轮，该轮含两条消息和两个 UUID |
| 流式期间的轮身份持久 | `a_running_round_keeps_its_identity_when_the_daemon_restarts`：进行中已有轮 ID，杀守护进程、自动重开、结束后同 ID 和原生位置，总模型请求仍为一次。本项不涵盖 #19 的全部流式内容恢复要求 |
| 后端已创建、首条提示未回显 | `a_create_whose_backend_died_after_the_first_message_was_written_is_partial`：根段和后端会话已记入，轮索引为空；退出后仍走既有“部分完成”处理 |
| 真实录制回归 | `recorded_human_rounds_keep_their_user_uuids_and_final_assistant_anchor`；`stream-tool-two-turns` 夹具通过隔离真 CLI 重新录制，含 Bash 工具往返；工具调用的多次模型请求仍归同一轮；原有完整事实与投影差分回归继续运行 |
| 迟到与重放 | `late_completion_and_repeated_native_positions_do_not_invent_new_human_rounds`、`replaying_an_older_turn_prefix_does_not_move_the_completed_fork_anchor`：不重复开轮、不倒退终态、不改写已结束轮的最终 assistant 锚点 |
| 导入先于落定 | `an_import_records_a_mirrors_sync_point_before_it_becomes_current`：镜像同步点成功/不明证据可以先记，旧承载区间只在落定事实后关闭；一个原生位置不能指向两个不同的轮 |

主接缝在 `crates/nd-daemon/tests/sessions.rs`，纯函数在 `crates/nd-session/tests/lineage.rs`，录制回归在 `crates/nd-claude/tests/conversation.rs`。公开结果从同步副本快照、实际模型请求及 CLI 自己生成的记录观察，不查询业务私有表作旁路断言。

## 待验证项与退路

INDEX #15 没有编号待验证项。Claude 的两项实际依赖 `LINEAGE-ROUND`、`LINEAGE-ANCHOR` 已在固定 2.1.289 上离线实测成立，采用主方案，见 [CLI 契约清单](../cli-contracts.md#谱系与轮索引15)。没有用 Codex 的真实 `clientId` 回显，本单的 Codex 数据只检验后端无关的谱系值；V3 仍归第 3 步。

缺回显或实际回合归属时不猜轮；缺最终原生锚点时不供后续分叉操作使用；压缩跨同步点、外部续写、版本不兼容、导入失败或不明由调用方交 `InvalidateSync`。未写 CLI 存储，无需写记录例外的往返测试。

## 隔离和复验

所有 cargo build/test/clippy 均使用 `CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-15`、`CARGO_BUILD_JOBS=6`，外套 `systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 --`。场景使用固定 `/mnt/wd_external/nd-build/cli/claude-2.1.289`，临时 HOME/CLAUDE_CONFIG_DIR/XDG、空环境白名单、bwrap 断网和测试独立 slice；模型仅为沙盒离线端点。没有读取 owner 的会话、配置、凭据或活动桌面，也没有真实服务请求。

复验命令（在 `ticket/15` 工作树，配置上述构建环境及 cgroup）：

```sh
cargo test --workspace --locked
scripts/test-scenarios.sh
cargo clippy --workspace --all-targets --features nd-daemon/scenarios,nd-testkit/scenarios,nd-claude/scenarios --locked -- -D warnings
cargo fmt --all -- --check
```

随机测试固定 `PROPTEST_CASES=512`、`PROPTEST_RNG_SEED=20261006`。日志在 `/mnt/wd_external/nd-build/tmp/ticket-15/logs/`。TDD 的 `tdd-01` 至 `tdd-12` 保留先红后绿的日志；编译期缺接口和运行期行为失败分别记录，清空/外部续写/随机截点及真实合轮、流式重启是对既有切片的扩展验证。

没有新增结构/派发操作或可选组件，不新增崩溃矩阵行；新建、闲置回收、按需拉起的既有矩阵全量回归。所有导航数据走 nd-wire，无界面私有通道。

## owner_checklist

本工单无必须由 owner 完成的验收。纯函数、真 CLI 断网协议和守护进程恢复均可自动复验。真机输入法、界面延迟/CPU、真模型对话及真实服务验证属于各自工单，本单不将其列为已通过。
