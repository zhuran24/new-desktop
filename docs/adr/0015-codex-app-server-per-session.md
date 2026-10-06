# Codex 运行时每个会话一个 app-server，Codex 二进制自管版本

一个会话的 Codex 后端会话，和它派出的 Codex 子代理，共用一个 app-server（research/round3/CODEX.md §3.2）；会话分叉出的线程留在来源的运行时里；闲置回收。Codex 二进制复制进版本库、钉版本，走和 CLI 同一个升级关卡（research/round10/COMPONENTS.md §11、§12.1）。

## Considered Options

- 全守护进程共用一个 app-server：最省内存，但一个崩了所有 GPT 会话一起断，升级要做没测过的换代交接。改用条件：实测 app-server 的常驻开销主要是每个进程的固定成本、每个线程开销很小，而且实际使用里 Codex 进程很少崩溃；到时只改 Codex 适配。
- 每个 Codex 后端会话一个 app-server，网页渲染和终端附着也各成独立进程：隔离最强，但内存最大，有两条没测过的进程间交接，卡片输入法还要多绕一个进程。实际用起来某类故障反复拖垮整个会话时，再按需引入对应部分。
