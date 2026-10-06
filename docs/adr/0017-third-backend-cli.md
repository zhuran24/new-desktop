# 接第三家 CLI：第一版只做两家时就用得上的四件小事，通用接入等真接时再做

ADR 0009 给 Gemini 留了位置。第 14 轮查清：持续驱动原版 Gemini 只能经 ACP，接它要新写一个适配器（协议之外还有交付结论靠读记录、收尾靠记录加 `/proc`、转接清单并进下一条 prompt、带历史新建靠写原生记录再加载）、记录解析、一对编解码、一个只给后台派发的 nd MCP 中转进程和 npm 版本来源（research/round12/DESIGN.md §7）。取舍以总工作量和稳定性为准，复用只是手段（owner）。所以第一版只把两家时就各有两个实现在用的四处写成同一个形状：对话转换按「源后端解码成中间条目、再编码成目标后端」写；带历史新建是一步 `Open{Seeded}`；动作与身份不带后端名（`Managed{child}`、`Native`、`Decision::Offered`）；跨后端的设置换算经中立的 `Profile`。Gemini 适配器、nd MCP 中转、ACP 通用接入档都等真接时再做。

## Consequences

- 这四件让对话转换、带历史新建、跨后端子代理、设置换算随后端数线性增长，不按后端两两配对；两家时只多一组中间条目的往返测试。第三家来时省多少，用实际改动检验（research/round12/DESIGN.md §5）。
- 真接 Gemini 时另记一条 ADR：经 ACP，每个承载位一个 `gemini --acp` 进程；不做退役、原地回退、追加导入和 mod 对应物，走已有的降级路径；带历史新建靠写原生记录再加载，又是一处写 CLI 存储的例外；只给后台派发，经 nd MCP 中转；定下原生历史的保留策略。接口预计加三个中立变体（`Landing::NextPrompt`、`Unobservable::BackgroundShells`、`Unobservable::SubagentDetail`）。
- 「引擎、fold、独占登记的代码不改」只是工程预期：第二种记录格式进续接前的叶子检查、MCP 中转进程的通道归属与身份核对、它自己的钉版，都要另算（research/round12/review-2.md M07）。
- ACP 通用接入档到第二个 ACP 后端要接时再做：那时有两个适配器，接缝才是真的。Gemini 的 ACP 部分届时写成适配器里一个独立的内部模块，不立 trait。
- 重核的触发：Gemini 的 ACP 路径接入它新的 AgentProtocol，或 ACP v2 稳定且 Gemini 跟进。

## Considered Options

- 现在就做 ACP 通用接入档：只有一个使用者，看不出哪些该通用；各家覆盖不齐（Gemini 没有 list、resume，Kimi 的 fork 未实现，OpenCode 不支持 undo），回合中再发 prompt 三家语义不同；ACP v2 草案删了 `session/load`、`set_mode`、`fs/*`，现在搭的通用层多半要跟着改（research/round14/acp.md）。
- Claude、Codex 也改走 ACP，三家一套接法：会丢掉插话意图与撤回、有效对话导出与回放、`thread/inject_items`、mod 通道、目标暂停；ADR 0004、0009 的直接协议不动。
- 对话转换、子代理派发、设置换算按后端两两写：两家时一样省事，三家起变成平方项。
- 用 Gemini 的无头 `-p` 或 SDK 接：前者一次读完 stdin 就退出；后者是进程内的库，关了钩子、MCP 和扩展；A2A 服务明写实验性（research/round14/gemini-cli.md §1）。
