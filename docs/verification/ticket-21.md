# #21 会话设置与标题验证

日期：2026-10-06。状态：实现与全部自动验收完成。真模型和真机输入法仍为 OWNER_PENDING。

## 基线与隔离

- 分支 `ticket/21`，工作树 `.worktrees/ticket-21`；已合入 v1 `8e4e6ec`（#19/#20）。#18 尚未包含在此基线，不把其 Esc 接线算作本单交付。
- CLI：`/mnt/wd_external/nd-build/cli/claude-2.1.289`；SHA-256 `a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348`。
- Cargo.lock SHA-256 `9c0b546d361109bc36e581453de57e5dc8229c75f7ea0e3447976a164d6e818b`。本单未新增第三方依赖。
- 产物 `/mnt/wd_external/nd-build/target/ticket-21`，6 jobs；build/test/clippy 均为 12 GiB、零 swap 的独立 scope。
- 真守护进程、CLI、两个 mod、看守、systemd、SQLite；仅模型端点离线替换。每场景私有 HOME、Claude/XDG 目录、断网 bwrap、限额 `nd-test-` 单元。原生窗口使用私有 KWin/D-Bus，不使用 owner 的显示会话、凭据或服务。

## 待验证项结论

**ultracode：CONFIRMED，采用主方案。** 在固定 2.1.289 经产品 `session.configure` 实测 `apply_flag_settings{settings:{ultracode}}`。Opus requested/available/applied 可开、可关；开关不改变 high effort。改 effort 档位后 requested 和 applied 均为 false，遵从 CLI 原生规则。切到 Haiku 时可出现 requested=true、available=false、applied=false，此时能力表关闭、界面隐藏入口。缺少能力报告也隐藏；Codex 在后端端口强制关闭。

**effort max：CONFIRMED。** 旧资料未核实的 max 在此版本实际接受；回读为 max，下一次正式模型请求的 output_config.effort=max。不能用旧 schema 的 low/medium/high/xhigh 静态枚举否定实际入口。暂切不支持 effort 的模型后，进程退出、续接、再切回支持模型，仍恢复用户选择的 max。

**模型下一回合：CONFIRMED。** 扣住第二回合的离线响应，提交改模型并发第三条提示，旧回合结束前没有新模型请求；结束后先控制、回读，再放第三条。自定义模型产生 max_tokens=1 的校验请求，测试把它与真正携带第三条提示的请求分开断言。

**标题：离线产品契约 CONFIRMED，真模型质量 OWNER_PENDING。** 原生生成接口使用当前会话模型、要求 JSON title；返回值更新两处投影，CLI 自己持久写 ai-title。手动 rename 发自定义标题事件；关守护进程、闲置回收再续接后标题保持。生成中重启不增加模型请求；生成途中手动命名不会被迟到结果覆盖。短提示不生成；首条合格提示才触发；空生成返回、后端退出均保留摘要。

**Codex 展示边界：纯计算验收 CONFIRMED。** 后端端口拒绝 Codex ultracode，即使报告误带 true；公共视图在 caps=false 或缺失时不构造开关。当前基线尚无可运行的 Codex 会话，真 app-server 场景由 #27 接入时复验。

## 自动测试

主接缝测试位于 `crates/nd-daemon/tests/sessions.rs`，11 个 `settings_*` 场景覆盖：

1. 忙时改模型、代持下一提示、正式请求与重启回读。
2. ultracode on/off、effort 保持、能力值。
3. 初始回读、Haiku 禁用、权限和 effort 回收后恢复。
4. 手动标题同步、重启及原生续接。
5. AI 标题只生成一次、返回值与原生持久条目。
6. AI 生成中重启、手动标题优先、模型请求不重复。
7. 真实 GPUI 设置界面经 nd-wire 修改模型、effort 和标题。
8. 修订号冲突、同 id 收据不变、CLI 拒绝后保持原值。
9. 后端退出时结清标题操作、保留摘要。
10. 改 effort 关闭 ultracode、max 的后续请求、暂不支持的模型和进程退出后的恢复。
11. 后来的首条合格提示触发、CLI 返回空标题时使用摘要。

窄接缝一：`crates/nd-session/tests/matrix.rs` 的 3 个测试覆盖 4 行场景，每个提交的 BeforeCommit/AfterCommit/AfterHandoff 全扫，开纯度双跑。能力派生和视图计算各一个直接测试。录制回归 `crates/nd-claude/tests/settings_recording.rs` 使用真控制流水；夹具 SHA-256 `5cbc057543dd72937fe1297147ebd76a91904f2cca7d2142d932de820eb3b594`。

复验命令：

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-21
export CARGO_BUILD_JOBS=6
cargo fmt --all -- --check
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- cargo test --workspace --locked
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- cargo clippy --workspace --all-targets --locked -- -D warnings
RUST_TEST_THREADS=3 ND_NATIVE_SETTINGS_OUTPUT=/mnt/wd_external/nd-build/tmp/ticket-21-evidence/native scripts/test-scenarios.sh -- --nocapture
```

最终结果：workspace **189 passed / 0 failed / 1 ignored**（既有 #8 真 CLI 现场项）；完整隔离场景 **119 passed / 0 failed / 1 ignored**（既有手动 OOM 项）。本单 11 个场景全过；手动标题原生 custom-title 条目另加断言并复验通过。默认及 scenarios 特性下的 clippy 均全过，13 份 nd-wire Schema 重新生成逐文件一致。专项 TDD 的失败和通过日志保存在 `/mnt/wd_external/nd-build/tmp/ticket-21-*.log`。原生截图与清理报告：`/mnt/wd_external/nd-build/tmp/ticket-21-evidence/native/`。

## owner_checklist

1. 在获准连接真模型后，选 Haiku，新建会话输入至少 10 个字的正常提示；确认侧栏出现能概括内容的 AI 标题。手动改名，再发送消息、关闭重开界面，确认手动标题保留。生成标题会多一次当前模型请求；可先用 `[sessions] auto_title=false` 关闭。
2. 真服务获准后，在支持的 Claude 模型开启 ultracode，观察真实 Workflow 行为；改变 effort、关闭开关、改成不支持的模型，核对 CLI 的实际服务端行为。离线端点已验证控制、请求和回读，不能替代真实模型/资格验证。
3. 原生设置面板的标题输入框分别用豆包、Rime 测组词、上屏、候选框、编辑已有中文标题和点击保存；确认不误发送聊天提示。若需要重新确认本机输入延迟/静止 CPU，按 research/impl/tickets/14.md 的 owner_checklist 执行真机测量；隔离截图不构成这些性能验收。
