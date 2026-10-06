# #6 看守进程与保活验证

日期：2026-10-06。status=complete。N7、V6 成立，采用独立 transient service；默认测试不制造 OOM，真实 OOM 仅保留为 ignored 手动测试。owner_checklist 为空。

## 基线和复验

- 实现提交：`0424085`；合入 v1 后的代码提交：`edffa04d43227d3a51817000408e08530c808984`。
- 合入的 v1：`2fd1623c3069993b5937f2c4612125bd7a42e467`（#10 输入框）；仅 Cargo.toml 成员/依赖列表冲突，双方成员均保留，GPUI 不在 default-members。
- CLI：固定 `/mnt/wd_external/nd-build/cli/claude-2.1.289`，SHA-256 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`。
- 父规格本地副本 SHA-256：`18d503436e36402296c8e29f3da3c9c59e7b738fddff61ef936cec88c7526f98`；开工时远端 #1 body 与本地相同、无评论。#6 远端 body 与本地要点相同、无评论。
- 构建目录 `/mnt/wd_external/nd-build/target/ticket-6`，6 jobs；所有 build/test/clippy 均在独立 12 GiB、零 swap scope。限额探针实读 `memory.max=12884901888`、`memory.swap.max=0`。未使用 `/tmp` 构建退路。
- 完整日志根目录：`/mnt/wd_external/nd-build/tmp/ticket-6/`。

默认复验：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-6
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test --workspace --locked
scripts/test-scenarios.sh -- --nocapture
```

| 检查 | 结果 | 日志 |
|---|---|---|
| `cargo test --workspace --locked` | 103 passed、0 failed、1 ignored。唯一忽略项为 #8 已有的显式真 CLI 测试 | `workspace-final.log` |
| `scripts/test-scenarios.sh -- --nocapture` | 51 passed、0 failed、1 ignored；其中 #6 为 13 passed，真实 OOM 项 ignored | `scenarios-final.log` |
| `cargo clippy --workspace --all-targets --features nd-daemon/scenarios,nd-testkit/scenarios --locked -- -D warnings` | PASS | `clippy-final.log` |
| 格式、空白 | `cargo fmt --all -- --check`、`git diff --check` PASS | 交付检查 |
| 资源清理 | 本单单元/slice 查询为空，E 盘 `nd-test-disk-*` 临时目录为空；正常场景显式 close，失败路径 Drop 清理 | `cleanup-final.log` |

内核性质测试使用 `PROPTEST_CASES=512`、`PROPTEST_RNG_SEED=20261006`。测试始终使用临时 HOME/CLAUDE_CONFIG_DIR/XDG、bwrap 断网与伪模型端点；未读取 owner 会话/凭据，未连接真实模型服务，未安装生产服务。

## N7：服务保活与内存限制

**成立，保留 transient service 主方案，不启用 scope 退路。**

`n7_thousand_lines_per_second_survive_fifty_kills_and_fifty_restarts` 使用真实 Python stdout 生产器按单调时钟每秒输出 1000 行。真实 nd-daemon 被 SIGKILL 50 次、systemctl restart 50 次；每次都用同步副本重新取得全局快照和 runs 诊断，检查纪元改变、后端身份不变；适配器侧控制连接重新附着并逐条核对流水序号和生产器计数。

最终场景核对 **31,700 行**，无丢失、无重复。后端 PID `792292`、start_ticks `2835634`、boot_id `ccb7d888-b500-48fc-960f-8714669f07e7` 在全部 100 次故障后不变。真实 `/proc/<pid>/cgroup` 属于看守单元；看守与 daemon 是同一专用 slice 的不同 cgroup；单元 `Restart=no`，`PartOf`/`BindsTo` 为空。完整 cgroup 路径见 `scenarios-final.log`。

默认内存验收 `n7_disk_page_cache_reaches_slice_limit_without_any_oom_kill` 在 `/mnt/wd_external/nd-build/tmp/nd-test-disk-*/page-cache.bin` 写 **512 MiB 普通磁盘文件**。每 8 MiB fsync，保留可回收页缓存、匿名内存保持很小；文件不在 tmpfs。专用 slice 上限 128 MiB、零 swap，最终实读：

| 指标 | 值 |
|---|---:|
| memory.max | 134,217,728 B |
| memory.current | 134,053,888 B |
| memory.events.max | 2141 |
| memory.events.oom | 0 |
| memory.events.oom_kill | 0 |

后端仍活着，文件大小正确，清理后临时目录消失。默认验证依靠内核限额命中和回收证据，不以杀进程证明限制生效。

### 仅手动的 OOM 验证

`manual_oom_kill_triggers_desktop_notification_only_run_manually` 标有 `#[ignore = "会触发 KDE Memory Shortage Avoided 桌面通知，只手动跑"]`。全仓库 Rust/Python/shell 检查未发现其他刻意制造 OOM 的测试；默认 workspace 和场景入口均不运行此项。

按 owner 补充要求，在标记 ignored 后**单独手动运行一次**：128 MiB slice 中对 512 MiB 匿名内存逐页写入。结果 PASS，`memory.events.max=61`、`oom=2`、`oom_kill=1`；超额后端退出，另一个独立场景的同步副本继续可用。证据 `n7-oom-manual.log`；这次会产生桌面通知，不属于默认回归。

手动复验会再次产生通知，命令仅选中这一项：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-6
export CARGO_BUILD_JOBS=6
export ND_TEST_DAEMON="$CARGO_TARGET_DIR/debug/nd-daemon"
export ND_TEST_WATCHDOG="$CARGO_TARGET_DIR/debug/nd-watchdog"
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test -p nd-testkit --features scenarios --test watchdogs \
  manual_oom_kill_triggers_desktop_notification_only_run_manually \
  -- --ignored --exact --nocapture
```

## V6：真实 Workflow 的流水大小与增量占比

**已实测，保留 64 MiB 软限、256 MiB 硬限，不改变溢出策略。** `v6_real_workflow_records_versioned_fixture_and_measures_stream_share` 让真 CLI 执行六个顺序 Workflow 子代理，每个由本地端点发送 2048 个 SSE 文本增量，间隔配置为 5 ms。模型路由按本版本实际 `sonnet → claude-sonnet-5-5`，观察到六个真实 agent id 的 HTTP 请求和 Workflow 成功完成通知。

最终运行耗时 **75.31 秒**，stdout **35 行 / 50,789 B**；其中 stream_event **12 行 / 3,379 B**，占行数 **34.29%**、原始 stdout 字节 **6.65%**。带看守信封和两条输入后的保留流水 **57,964 B**，溢出 0 B、LostLines=false。CLI 的子代理模型 SSE 并不全部出现在主 stdout；测量以真实看守读到的行为为准，不能用伪模型发出的 12,288 个增量冒充流水行数。

这是一条约 75 秒、六个子代理的离线长 Workflow 样本，不是数小时真服务压测。样本远低于默认软限，当前阈值无需降低；未来版本或实际任务体积增长仍按相同的软限丢增量、硬限转存、真损失 Unknown 规则处理。另有明确的低阈值场景覆盖所有溢出分支。

当前仓库夹具是同一脚本另一次约 75 秒运行的完整录制范围（从 initialize 到 Workflow 完成，CLI 此时仍活着），含 36 条看守记录。文件：`crates/nd-watchdog-proto/tests/fixtures/watchdog/claude/2.1.289/long-workflow.jsonl`；SHA-256 `8045853deb421e3a4685143150bbc401d1166fd75056bbb89289b148f629d09c`。首行的 count/first_seq/last_seq 可检出末尾整行缺失，连续性检查可检出中间漏行；默认纯回归验证六个子代理完成。最终场景生成的另一个原始录制和统计在日志根目录的 `v6-workflow.jsonl`、`v6-stats.json`。

## 其余行为与证明边界

| 行为 | 自动测试 |
|---|---|
| 幂等拉起、输入序号去重、重启前后流水一致 | `daemon_restart_preserves_backend_identity_and_numbered_io` |
| 并发 launch 返回同一身份，不顶掉正在使用的适配器连接 | `concurrent_idempotent_launch_does_not_replace_the_adapter_controller` |
| 软限仅丢 stream_event、保留 Gap；硬限转存事实；ack 回收 | `soft_limit_marks_only_deltas_and_hard_limit_preserves_facts_in_overflow` |
| 溢出目录真实不可写时 LostLines；nd-wire 中 tail=Unknown | `storage_failure_reports_lost_lines_and_never_claims_a_clean_tail` |
| daemon 离线时后端退出，仍及时清后代、记录尾部退出码 | `backend_exit_cleans_descendants_even_without_daemon_and_watchdog_never_restarts` |
| 看守被杀不自动重启后端 | `watchdog_death_cleans_backend_and_cannot_replay_the_launch` |
| 预期单元不在而进程活着，报 IdentityMismatch | `live_process_without_its_expected_unit_is_identity_mismatch_not_gone` |
| 后端 exec 失败，报 Gone/NeverLaunched | `failed_backend_spawn_reports_never_launched_with_no_live_identity` |
| 阻塞 stdin 时仍能结束后端；断开的在途大输入完成一次 | `new_controller_can_end_a_backend_while_old_input_pipe_is_blocked`、`cancelled_input_transfer_finishes_once_and_reconnect_deduplicates_its_sequence` |
| 真实录制解码；部分 JSON、中间漏行、完整末行被删均拒绝 | `nd-watchdog-proto/tests/recordings.rs` |

测试使用规格已确认的接缝：真 daemon/SyncReplica 的身份和 Unknown 观察、真实托管和看守协议、真实 systemd，以及实际模型 HTTP 请求；高吞吐用普通真实进程，未实现假 CLI/看守。TDD 红测日志为日志根目录的 `red-01/02/03/05/06/07/08/10/11.log`，另有对应 green 和最终场景日志；验证既有行为的增补场景直接通过，不冒称它们曾红测。首次身份检查、阻塞输入结束、并发幂等和完整末行截断回归均曾暴露真实失败并已修复。

本单暴露供 #12 使用的 Up/Gone/IdentityMismatch 观察，供 #11/#13/#19 使用的连接、序号、ack 和 LostLines 信息。任务账本、会话折叠、mod 就绪、退役和转接的最终语义尚属对应工单；不能把这里的 `tail=Unknown` 或录制通过当作那些业务已实现。没有新会话结构/派发命令，没有 CLI 原生存储写入例外，没有需要 owner 才能完成的本单验收。
