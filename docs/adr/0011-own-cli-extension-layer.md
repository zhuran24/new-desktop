# New Desktop 给 CLI 带自己的 mod，不当模型端点、不带预加载，也不改 CLI 本体

有几样能力 CLI 的宿主协议做不到：回退或换后端后让旧进程留着跑完任务、但不再起回合，并把这些任务转接给新的当前后端会话（ADR 0016）；在 Claude Code 会话（包括 Workflow）里派 Codex 子代理；界面直接给子代理发消息；Summarize from here 等只在终端界面里有的操作。New Desktop 拉起每个 Claude Code 后端进程时，用 `--plugin-dir` 带上两个自己的 mod（research/round9/BRIDGE.md）：
- 钩子 mod（`new-desktop`）：退役、Codex 子代理（`turn.step` 里代答、任务交守护进程的 app-server 执行，`session.send` 转插话）、Summarize 的压缩钩子，外加导出当前对话等只读命令。转接也在这里：截下模型和它的子代理对转接任务的 TaskStop、SendMessage、Workflow 续跑、CronDelete，CronList 的结果并入别处的定时事项，放行读这些任务的输出文件；在 `session.append` 按守护进程的裁决处理 CLI 生成的任务通知；退役时截下 `scheduled-trigger` 来源的提示。
- 动作 mod（`new-desktop-actions`）：只在 `session.start` 起控制循环，不挂别的钩子，替守护进程发起给子代理发消息、派子代理、跑命令、调工具、改设置这些调用；转接时在持有者里替当前模型调 TaskStop、CronDelete、CronList，就地续接已结束的子代理，在当前进程里以 `asUser` 投转投结果和定时触发。分成两个，是因为 mod 自己发起的调用引出的子代理步骤，不经过它自己的 `turn.step`（research/round9/VERIFY.md E7）。

两个 mod 都经守护进程的 unix socket 长轮询取命令、POST 回报。Claude 的请求仍由 CLI 自己原样发给 Anthropic。owner 现有的 mod，在这些后端进程里用 `--settings` 的 `enabledPlugins` 关掉 codex-direct、sendnow、cc-quota、ultracode-toggle，其余照常加载，旧 mod 的文件不改。不带预加载脚本：sol、astra 这类在 Claude Code 里跑的 GPT 子代理不要了，GPT 子代理一律派给 Codex（owner 2026-10-05 定；GPT 在 Codex 里发挥更好，Workflow 里的 sol、astra 还会照字面去回答原话）。所以 New Desktop 对 CLI 只依赖协议、启动参数和公开的 mod 接口。终端和官方桌面端里的 GPT 分流照旧。

## Considered Options

- 守护进程当 CLI 的模型端点（research/round6/SUBAGENTS.md）：不用注入，但所有 Claude 请求要经本机转发，Anthropic 看到的连接特征会变，对账号的影响未知；还依赖半内部开关 `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL`。owner 选了不冒这个险。
- 预加载截下 Codex 请求、仿 Messages API 应答：要做 SSE 仿真、按请求头认 agent、同步工具回合，比在 `turn.step` 里代答复杂。
- 用 SDK MCP 或 CLI 的 UDS 收件箱当守护进程和 mod 之间的通道：前者模型看得见、拦下又会挡住 mod 自己；后者是公共信箱。
- 纯官方 CLI，回退用补救办法（软中断加逐个停任务、截断加回放）：缺口最多。
- 给 CLI 本体打补丁：最干净，但每个版本都要重打。

## Consequences

- 依赖 CLI 还在早期阶段（early access）的 mod 接口；2.1.286 → 2.1.289 只增不改，以后没有保证。每次升级 CLI 都按 ADR 0006 的关卡先分析、再跑 research/round9 的回归套件（退役、Codex 子代理等）。
- 后端进程声明 `perTaskStopAffordance`，Esc 用官方的 `interrupt`（ADR 0008）。
- 不用 `--await-initialize`：等两个 mod 的 hello 都到了再发 initialize；钩子 mod 没装上时不声明 codex 类型，免得约定的模型名被发给 Anthropic。
- 在 New Desktop 的后端进程里，sol、astra 这两个子代理名字改指 Codex（R10-E2 验证后），owner 的老习惯照用。
- 钩子 mod 另挂 `classic.Stop`、`classic.SubagentStop`，作为退役收尾的数据来源；动作 mod 按操作 id 留结果，守护进程重启后能对账；mod 和守护进程之间的协议只加不改，类型只在 Rust 里定义一次，导出 JSON Schema 再生成 TypeScript 类型（research/round12/DESIGN.md §1.4.11）。
- 写 CLI 自己存储的例外只有下面几处，都由 Claude 适配按 CLI 版本分开写，带独占登记的凭据，进升级关卡的格式检查（research/round13/DESIGN.md §2.5、§4.6，research/round15/decisions.md D4、D8、D9）：
  - 只限已验证的记录种类：普通子代理、两层嵌套、经典串行 Workflow（research/round13/DESIGN.md §2.5）。forked skill 的旁文件、storageV5 布局按不过版本门槛处理（子代理在记录所在就地续接）；并行或 v2 的 Workflow 不移动，回工具错误。
  - 子代理记录：只复制被续接的那一个子代理自己的文件，从没有进程的后端会话复制到当前后端会话，目标文件要么不存在、要么对应的子代理在目标进程里没在跑；往钉在旧版本上的活进程写要过兼容登记（ADR 0006）。会话分叉时复制已结束的，来源可以还活着，正在跑的不复制。
  - Workflow 运行目录：已结束的 run 续跑时整个移进当前 Claude 后端会话（复制后把源目录挪进 New Desktop 自己的目录），源后端会话有没有进程都行。
  - 旧副本：后端会话被拉起之前、还没有进程时，把它目录里已复制走的子代理记录移进 New Desktop 自己的目录。
  - 外部导入的会话有同文件旁支时，在记录末尾追加 `last-prompt` 钉叶子。
  - consentGated 设置行直接写设置键。
  - 接管时认领 CLI 作业目录里的交接单（改名、移走），只认领一次。
