# #19：回合中守护进程重启与交付不明

日期：2026-10-06。实现、全工作区测试与完整隔离场景验收通过。真模型验证由 owner 决定是否执行。第 4 步的转接表、退役重申和通知持久确认不在本单结论内。

## 实现契约

- 守护进程装载会话、递增写入代次，端口按检查点追平流水、对账未结票和交付不明票，逐承载位报 `Recovered`；全部来源完成后，独占登记进入身份已知并完成第一次注册表扫描。闸门内只做事实折叠和检查点提交，不续操作、不派发输入。
- 恢复期 `session.create/send/resend` 的新命令回 `unavailable`，没有副作用或收据；已有收据、冲突、墓碑仍按原结果返回。同步副本以原命令 id、原正文退避重试，间隔上限 1.6 秒；传输中断仍只查收据，查不到就是 `DeliveryUnknown`。
- Claude 检查点保存未回显输入的原 uuid 与看守输入序号。看守确认只在检查点提交之后；检查点越过流式增量时，累积正文同事务入缓存，重开继续累积同一条目。
- `Unknown` 的原票、签发者和授予保留，恢复对账不重新签票。`Clarified` 只改原消息的当前结论，不修改原命令收据，不复活已收场的操作。
- 只有检查点前缀可解释、剩余流水连续完整且覆盖接回高水位，才能证明未写出。旧检查点缺少输入记账、流水缺行、读失败或看守消失，均不能当成 `Withheld`。没有确定结论时保留 `unknown`，不自动发送。
- 消息确认未送达后显示 `not_delivered` 和重发按钮。`session.resend` 从原消息取正文和意图，原消息变为 `resent`，新消息以本次命令 id 进入发送台；这三件事和收据同事务。重复命令 id 回原收据；第二个重发命令因原消息资格已经消费而被拒绝。

## 自动验收与接缝

主接缝位于 `crates/nd-daemon/tests/sessions.rs`：同步副本对真守护进程、固定 CLI 2.1.289、两个 mod、看守、systemd 和 SQLite；只使用离线模型端点与 scenarios 构建故障点。

| 测试 | 观察与结论 |
|---|---|
| `restart_keeps_a_written_message_pending_until_its_original_echo` | CLI 暂停读取，输入已记账且流水可回收；restart 后恢复同一原 uuid，CLI 原生记录只有一条输入，后端 pid 不变 |
| `recovering_commands_have_no_receipt_and_the_replica_retries_the_same_id` | CLI 暂停导致 hello 未完成；新建和发送都没有收据，发送的自动重试超过原来的五次上限，恢复后同 id 只受理一次；取消等待后不再发送未受理请求 |
| `streaming_survives_kill_and_service_restart_with_a_checkpoint_mid_block` | 分别 SIGKILL 与 systemctl restart；流式中另一输入触发检查点，纪元改变后副本重取快照，前缀保留，最终条目、回合与模型请求不重不漏，pid 不变 |
| `ambiguous_write_and_crash_before_accounting_never_resend_the_native_input` | 原生输入写出后、记结果前 abort；以及人为制造不明结果后重启。后续原 uuid 回显澄清为送达，零重复输入；不明时拒绝重发 |
| `missing_input_journal_is_unknown_not_permission_to_write_again` | 测试在写后暂停守护进程、暂移真实输入流水，再杀守护进程。缺失证据回 unknown；恢复流水后原输入送达，没有另投 |
| `recovery_confirms_an_unwritten_unknown_and_nd_wire_resends_only_on_request` | 注入写入结果不明且实际未写出的故障；重启由完整真实流水证明未写出，显示未送达但不自动发。经 nd-wire 点击等价命令重发一次，原收据不变 |

窄接缝一：`nd-session/tests/engine.rs` 验 Unknown → 重启 → Lost → 重启 → 用户重发，包括原收据不变、两个设备的重发资格互斥；`tests/matrix.rs` 新增 `(发送/用户重发，证实未送达)`，在提交前、提交后交出前、交出后结果入账前的每一次提交上崩溃重开，断言最终状态和原生步骤至多一次。既有四行矩阵继续通过。

纯视图计算：`nd-view-model/tests/chat.rs` 的 `delivery_unknown_is_explained_and_only_confirmed_non_delivery_offers_resend` 验中文状态、证据说明及按钮资格。GPUI 按相同视图计算渲染，按钮只调用公共 `CommandClient → nd-wire`，颜色、字号和间距取主题变量。

## 待验证项与退路

INDEX #19 没有新增编号项。N7 的独立 transient 服务方案在本单两种守护进程重启场景中再次成立，使用主方案。写后未记账、输入流水被确认回收、纪元变化、恢复期无收据、同 id 重试和迟到回显均有产品主接缝结果。流水不能证明完整时采用保守退路 `Unknown`，禁止自动重投；不是把“没找到输入”当成“证实没发”。

E18、完整 S5、退役状态/转接表的 `desired` 重申和 R12-H01 留给第 4 步。后续适配器必须在这些屏障全部满足之后才报 `Recovered`。本单只有 Claude 当前承载位，未实现的退役/转接能力不宣称已验证。

没有应用写 CLI 原生存储的例外；CLI 行为见 [契约清单](../cli-contracts.md)。`Cargo.lock` 未改变，固定 CLI SHA-256 为 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`。

## 运行与隔离

工作树 `ticket-19`、分支 `ticket/19`；构建产物在 `/mnt/wd_external/nd-build/target/ticket-19`。所有 Cargo build/test 使用 6 jobs 和独立 systemd scope（12 GiB、零 swap）。场景使用临时 HOME/CLAUDE_CONFIG_DIR/XDG、bwrap 断网、`nd-test-` 单元和专用 slice；不访问真实模型端点、owner 的配置、凭据或活动进程。OOM 默认忽略。

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-19 CARGO_BUILD_JOBS=6 RUST_TEST_THREADS=3
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- cargo test --workspace --locked
scripts/test-scenarios.sh
```

日志位于 `/mnt/wd_external/nd-build/tmp/ticket-19/logs/`。`red-01..11.log` 与 `green-01..11.log` 记录逐项 TDD；新增 API 的初次红灯是编译缺口，写入证据、闸门和流式前缀的红灯是运行时外部行为失败。自动结果不替代真输入法、真模型或界面性能验收。

## 集成检查

- `git merge v1`：最新 v1 `6d77bed` 已包含，无冲突。
- `cargo test --workspace --locked`：164 passed、0 failed、1 ignored（既有显式真 CLI 现场项）；`workspace-final.log`。
- `scripts/test-scenarios.sh -- --nocapture`：90 passed、0 failed、1 ignored（既有手动 OOM 项）；`scenarios-final.log`。含本单六项主接缝、原生 GPUI、N7 的 50+50 次重启及真 Workflow 回归。
- `cargo clippy --workspace --all-targets`（含 daemon/testkit/claude/desktop 的 scenarios，`-D warnings`）通过；`clippy-final.log`。
- nd-wire 与 mod Schema 重生成无差异；cargo fmt、git diff --check 通过。
- 最终 `systemctl --user list-units 'nd-test-*'` 为空；没有遗留测试单元。没有真 OOM kill。
- 全部检查使用 BUILD.md 规定的工作树、构建目录和资源隔离；无新增第三方依赖。
