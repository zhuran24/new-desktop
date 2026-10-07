# #12 独占登记骨架验证

日期：2026-10-06。实现包括持久租约、自有身份、两层恢复、外部写入者检测和脚本化 CLI 命令接缝。验收接缝是父规格已确认的窄接缝二，接口与后续工作边界见 [nd-claims](../../crates/nd-claims/README.md)。

## 自动验收

默认行为测试 `crates/nd-claims/tests/claims.rs` 共 21 项，均直接驱动 `Exclusivity`。数据库是真 SQLite，身份来自临时真进程，注册表和记录基于固定 CLI 实写文件；仅 `CliCommands` 被脚本化。

| 验收 | 测试与结论 |
|---|---|
| 租约单写者、同事务、按原因幂等 | `opening_a_backend_session_reserves_it_for_one_run_and_rolls_back_with_the_caller`；回滚无租约，另一个会话不能夺取；等待中的原因也不能换动作 |
| 身份核不上不释放 | `identity_mismatch_and_daemon_restart_preserve_responsibility_until_verified_exit`、`watchdog_identity_reports_cannot_turn_mismatched_or_live_processes_into_gone`；错 ticks、错误 Gone、实例重建、晚到 bind 均保留责任，核实原身份退出才释放 |
| 换绑和旧流水 | `holding_observations_clear_only_confirmed_old_bindings_and_keep_the_owned_leaf`；旧控制代次/旧序号无效，批次回滚无影响，`/clear` 后释放旧绑定，基线保留 |
| 预留未决 | `an_empty_holding_snapshot_does_not_prove_an_unwritten_reservation_was_never_created`、`another_open_or_never_opened_reservation_cannot_erase_an_existing_thread_lease`；未确认预留不能被空表清掉，另一条新建失败不释放已有线程或别的原因创建的预留 |
| 晚绑定与冲突 | `fresh_reservations_bind_independently_and_conflicts_pause_both_runs_without_overwriting`；两个 Codex 未命名预留分别绑定，冲突暂停双方、不覆盖原租约 |
| 两层恢复 | `recovery_requires_identities_then_a_complete_scan_and_excludes_own_pid_before_holding`；身份阶段不报外部，第一扫前不放行，自己的身份即使尚未 Holding 也不误判为外部 |
| 同 id 外部进程 | `a_second_process_with_the_same_session_id_blocks_even_an_already_admitted_write`；展示隐藏该 id，冲突仍拦，外部退出后恢复 |
| 无 pid | `real_cli_background_session_without_a_pid_blocks_even_when_no_one_is_listing_external_sessions`；未经改动的真后台 `state.json` 触发 ExternalUnverified；脚本化列表同样保守 |
| 坏文件、旧 PID、异命名空间 | `unverified_entries_and_incomplete_registry_reads_never_mean_no_external_writer`、`stale_local_pids_are_ignored_but_foreign_pid_namespaces_remain_unverified`；半文件/缺 id 不放行，完整重扫可恢复；旧 ticks 不借用活 PID，旧启动时间/异 PID 域不猜身份 |
| 常开检测与卸载 | `detection_keeps_running_without_a_list_subscriber_and_stops_without_releasing_leases`；无列表兴趣仍检测，Drop 不停止后端、不释放租约 |
| 提交与恢复重算 | `subscribers_wake_only_for_committed_claim_changes`、`only_proven_unsent_grants_are_rechecked_after_restart_and_never_opened_is_observation`、`cause_identity_is_stable_while_waiting_and_readmit_revokes_an_unsent_grant`；回滚不通知，Withheld 重算，已交付不明的授予留存 |
| 叶子来源 | `record_checks_use_the_cli_selected_leaf_without_replacing_the_own_journal_baseline`；CLI 的附件尾部不当叶子，文件读取得到的不同叶子不覆盖自有流水基线，坏记录明确失败 |

测试按指定 TDD 技能做纵向红→绿。代表性实测失败为：事务接口尚未实现；同原因 Write 不重判外部冲突；坏注册表仍放行；无列表订阅时未检测；异命名空间 PID 被当成已退；旧启动时间借到当前身份；`NeverOpened` 错清同运行时其他租约；空 Holding 错清未决预留。以上失败对应保留的行为回归。

## 真实 CLI 现场

运行 `scripts/test-claims-live.sh`。固定 CLI 为 `/mnt/wd_external/nd-build/cli/claude-2.1.289`，SHA-256 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`。

- 真 CLI 经 stream-json 生成记录及 `sessions/<pid>.json`，没有手工伪造原始注册表或对话记录。
- Rust probe 在相同 bwrap 命名空间读真实 `/proc`：未登记时识别为外部；登记自有身份/绑定后允许写入，真 `agents --json --all` 不使自身误报外部。
- 第二条真实 CLI `--resume` 同一个完整 id，第一条仍活着：`peek(Write)` 返回 ExternalWriter，展示列表仍排除该自有 id。两条 CLI 正常结束，注册项删除。
- 真实 `--bg` 生成 `jobs/<short>/state.json`。该环境没有登录，条目状态为 blocked/login required 且无 pid；验证的是无 pid 格式及保守阻塞，不是后台对话、接管或元数据完整性。
- `probe.txt` 的 `own: PASS`、`duplicate: PASS` 是实际公开接口断言；模型请求仅来自本地替代端点，不涉及真模型质量或 API 兼容验收。

最终现场证据：`/mnt/wd_external/nd-build/tmp/ticket-12-delivery-final/`，包含原始注册表、记录、agents 输出、进程身份、两条 CLI 的流水、probe、CLI 后台会话状态、manifest、isolation。manifest 对每个文件列 SHA-256，并记录临时目录删除及服务/slice 不活跃。

默认夹具分别来自 `ticket-12-registry/` 与 `ticket-12-registry-bg3/`，清单在 `tests/fixtures/manifest.json`、`background-manifest.json`。副本变形测试明确只改变必要字段；实际 CLI 格式契约由现场脚本验证。

## 待验证项、退路与范围

INDEX 的 #12 没有单独编号待验证项。租约、自有身份、无 pid 与两层恢复的新增实现已通过上述本单测试，采用主方案。异常输入的退路已落地：外部身份不明返回 ExternalUnverified；扫描失败返回 Checking；自有身份冲突返回 HolderUnknown 并保留责任；完整恢复前返回 Recovering。

R12-X2 的后台版本/cwd/完整 id/续接参数、R10-E6 的 stop 后续接、R13-D20 的停进程组证明属于 #64。本次 blocked 后台夹具不能关闭这些项。Codex TUI 同线程外部续接不可发现的规格限制保持不变。目录活动与恢复代码占用属于 #43；完整续接/外部谱系与写 last-prompt 属于 #49。

没有新增会话结构或派发操作，也没有写 CLI 存储例外。会话引擎尚由 #13 实施，不能把窄接缝测试宣称为 nd-wire 主接缝整条对话通过；操作引擎接入时须将实际 Open/发送场景纳入它的崩溃矩阵。

## 交付检查

- `cargo test --workspace --locked`：通过。
- `cargo clippy -p nd-claims --all-targets --locked -- -D warnings`：通过。
- `scripts/test-claims-live.sh`：通过。
- `cargo fmt --all -- --check`、`git diff --check`：通过。
- 所有构建/测试使用 `/mnt/wd_external/nd-build/target/ticket-12`、6 jobs、独立 12 GiB/零 swap cgroup；没有启用 OOM 测试或磁盘退路。
- 现场 bwrap `--unshare-all`，临时 HOME/CLAUDE_CONFIG_DIR/XDG、空环境加白名单、没有 owner home/run 挂载；systemd 只使用独立测试服务和 slice，清理已经核对。
- 最终日志在 `/mnt/wd_external/nd-build/tmp/ticket-12-checks/`。Git 提交和合并基线见仓库外 `research/impl/tickets/12.md`。

## owner_checklist

空。本单没有需要 owner 才能做的输入法、界面性能、真模型对话或真服务步骤。后续工单的人工验收不由本单提前声明通过。
