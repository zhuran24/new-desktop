# #23 主题切换与主题文件验证

日期：2026-10-06。状态：实现、默认测试、完整场景与 Clippy 验收完成；真机输入法和性能为 OWNER_PENDING。主题格式和使用方法见 [THEMES.md](../../crates/nd-desktop/THEMES.md)。

## 验收映射

| 要求 | 自动验证与结果 |
|---|---|
| 文件格式包含完整外观变量 | `nd-view-model/tests/themes.rs::theme_file_defines_the_complete_appearance_without_gpui`：独立样例的颜色、字体、字号、间距、圆角、阴影按已知字面值解析；格式不依赖 GPUI |
| 文件坏了或缺项回默认并解释 | `malformed_incomplete_and_unsafe_theme_values_have_actionable_errors`、`selected_file_reloads_falls_back_with_filename_and_recovers_after_repair`：无效 JSON、缺字段、错误版本、空字体和非法尺寸被拒；真实文件加载返回默认主题和文件名/错误，删除后提示，修复恢复 |
| 选择持久化、内置明暗、旧状态兼容 | `device_selection_survives_reopen_and_only_system_choice_follows_appearance`：系统/浅色/深色/文件选择经真实 ViewStateFile 保存、重读；旧 theme 字段保留原来的手动明暗 |
| 不重启热切换、界面里的选择 | `nd-daemon/tests/sessions.rs::native_theme_files_selection_and_system_appearance`：真实 GPUI 鼠标命中选择器；原子替换文件后同一窗口改变字号与颜色；截图像素确认主背景、表面颜色；坏文件有可见回退，修复恢复；重开保留选择 |
| 系统明暗的实时变化 | 同一原生场景：私有总线上的真实 dconf、XDG portal 及 GTK Settings 后端，`gsettings` 写真实偏好，`Settings.Read` 确认真实回应；系统选择随 light/dark 信号变化，手动浅色不被覆盖。没有模拟 portal |
| 目录和冷启动错误 | 同一原生场景：启动时选择的文件不存在仍提示；文件补齐、目录删除和重建后自动恢复。观察了实际 Wayland buffer 提交，清理后测试单元与临时根均撤销 |
| 聊天、草稿和后端保持 | `native_theme_change_preserves_streaming_chat_and_draft`：真实 CLI 输出期间换文件；草稿由 ndctl 经 nd-wire 写入，显示在真实 Composer；后端 run 与 backend_session 不变；界面 SIGKILL/冷启动后完成，端点恰好一个请求 |
| 新合入的会话设置视图 | `settings_native_window_changes_model_effort_and_title_through_nd_wire` 追加主题文件热改，确认设置和标题保留；保存设置面板前后截图 |
| 历史阅读状态保持 | #20 的 `native_navigation_jumps_to_a_round_via_history_page` 与千轮场景增加换主题验证：导航目标页、anchor、首条正文和条目数量不变；保留换主题前后原生截图 |

导航悬停提示另做截图差分：悬停新增区域必须使用文件的 surface 色；保持提示框打开再改 surface，同一批像素必须重绘成新色。`red-09.log` 捕获旧 Kit 提示框新增目标色像素为 0，`tooltip-final.log` 验证完整提示样式与打开期间热加载。

测试只断言文件格式的公共结果、设备状态文件接口、同步副本/ndctl、模型端点请求和真实绘制结果。没有假 CLI、假守护进程或内部数据表断言。

## 实现与贯穿约定

- `nd-view-model/src/theme.rs` 定义版本 1、必填字段及校验；`theme_files.rs` 定义目录快照、按文件名选择和回退；`state.rs` 保持每设备唯一写入者与旧格式兼容。
- `nd-desktop/src/themes.rs` 在独立线程读目录和文件，通知有界并合并一次编辑的文件事件；GPUI 回调一次性应用完整 Theme。空闲不轮询目录；窗口任务释放时关闭 watcher。导航提示直接取当前主题的颜色、字体、字号、间距、边框、圆角和阴影，打开期间也会更新。监视错误可见，并提供手动「重新加载」。
- `Desktop::select_theme` 修改本设备视图偏好；文件/系统变化通过 `set_theme` 更新已存在的 Composer 与 Kit/Base，再重绘现有视图。设备偏好走既有 ViewStateFile，不是守护进程配置，不绕过 Config::update 写配置。
- 会话状态与操作仍只经 nd-wire。本单没有新增 CLI/Codex 行为依赖、CLI 存储写入例外、结构/派发操作或可选组件，因此没有新增 CLI 契约或引擎崩溃矩阵行。既有场景与矩阵仍参与回归。
- `Theme` 完整变量与 `mode`、字体可从 `Presentation.theme` / `Desktop::theme()` 获取；文件名不能用作内容缓存键。Mermaid 请求与明暗出图由 #59 接入和验收。

## verification

工作树 `ticket-23`，分支 `ticket/23`。实现提交 `74cc125`，集成提交 `8af1085` 合入 v1 `8e4e6ec`，`f6c38fa` 再合入最新 v1 `9f1a69d`。三个合并冲突位于桌面模块声明、scenarios 参数和 Python 场景计划，均保留主题与历史分页两边行为。

- `cargo test --workspace --locked`：193 passed，0 failed，1 ignored（既有显式真 CLI 现场项）。
- `scripts/test-scenarios.sh`：121 passed，0 failed，1 ignored（既有手动真 OOM 项），RUST_TEST_THREADS=3；包含主题、设置面板以及 1/1000 轮历史的原生主题检查。
- workspace/all-targets 的 Clippy（含 daemon/testkit/claude/desktop scenarios，`-D warnings`）、格式、Python 语法、diff 空白和文档链接检查均通过。
- `native_smoke.py` 额外验证带目录监听器的窗口正常退出，状态写入线程可结束，守护进程仍存活；重连与实际 Wayland 帧通过。
- `cargo build --release -p nd-desktop --bin nd-desktop --locked` 通过（`logs/release-merged21.log`）；release SHA-256 `768c6c2d278fa90fc7e5ddff5c86b79b37f79b5bf1fead672ad4043b3b2db735`。
- 定向原生截图已查看：文件主题、替换后的字号与背景、错误提示、选择器、浅色、系统深色、流式聊天与草稿均能实际呈现。

构建目录 `/mnt/wd_external/nd-build/target/ticket-23`；6 jobs；所有 Cargo build/test/clippy 运行于 MemoryMax=12G、MemorySwapMax=0 的独立 scope。场景使用临时 HOME/CLAUDE_CONFIG_DIR/XDG、bwrap 断网、独立 `nd-test-` 单元和限额 slice。原生窗口在私有 KWin/D-Bus 中；没有使用 owner 的桌面、配置、凭据、模型服务或正在运行的后端。

证据根 `/mnt/wd_external/nd-build/tmp/ticket-23/`：`logs/` 为红绿、构建和检查日志（最终复验带 `-final`）；`tooltip-final/rounds-1/` 为悬停区域差分及打开期间热加载；`native-merged/` 为主题选择、真实系统通知、截图像素及清理证据；`settings-merged/` 为设置面板热切换；`smoke-final/` 为正常关闭与恢复；`chat-merged/` 为真实 CLI 流式换主题与草稿证据；`history-merged/rounds-{1,1000}/` 为导航锚点在换主题前后的证据。

红绿边界：01/03/04 的红灯是新增公共 API 的编译缺口；02 捕获了错误接受不支持版本的行为；05 是旧窗口没有加载文件主题；06 是测试驱动参数缺口，随后发现并修复合成点击坐标偏差；07 首轮为隔离环境缺 dconf，07b 才是产品缺系统通知监听的行为红灯。08 为新增缺失文件/目录恢复覆盖，第一次即通过，没有声称它揭示了新缺陷。

完整复验曾捕获 #19 场景的测试连接竞态：`scenarios-subscribe-race.log` 中，一次性 peek 在注入崩溃期间订阅，被 WebSocket `ResetWithoutClosingHandshake` 打断后 unwrap。该场景的恢复等待改用产品 `ReplicaFeed`，要求新纪元中的原消息已落地，再建连接查持久收据；原 pid、恰好两次模型请求、不重发和故障实际触发断言保留。定向复验 `recovery-fixed.log` 通过。该修正位于测试驱动，生产后端未改动。

另一次运行的 `scenarios-build-overlap.log` 在故障文件未消耗断言失败：并行运行的默认套件覆盖了 scenarios 守护进程二进制。这是测试编排错误；最终验证严格串行执行默认套件、重建 scenarios、完整场景，保留原断言。

## 待验证项与退路

#23 没有规格编号的独立待验证项。INDEX #23 所列的热加载、现有视图更新、错误回退、系统与手动选择一致性、草稿与历史阅读状态保持已自动验证，使用 GPUI 主方案。监视失效时显示错误并可手动重新加载；坏文件不会阻止窗口和守护进程工作。

规格第 2 步的 Mermaid 明暗出图仍归 #59；本单交出主题数据，不把它记为 Mermaid 验收通过。日常机器上的真实输入法、候选框、输入到上屏延迟和空闲 CPU 仍为 OWNER_PENDING。私有 KWin 的截图等待与后台文件合并延时不是界面性能测量；真实服务和输入法验收沿 #10/#14 清单，本单不引入新的真实模型请求。

可执行的 owner 步骤在不入库实施记录 `research/impl/tickets/23.md` 的 `owner_checklist`。
