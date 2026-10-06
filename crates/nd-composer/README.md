# 输入框状态机

`step(ComposerState, InputEvent) -> (ComposerState, Vec<ComposerAction>)` 是不依赖 GPUI、I/O 或后端的公开接缝。编辑器持有文本和选择区，每次决策前提供正文、marked range 是否存在、焦点和发送准入；`Edit`、`Focus`、`EnableSend` 更新这些状态。

- 普通 Enter 和 `Submit` 产生正文原样的提交意图，不清空草稿；空白、组词中或禁用发送时不产生提交。
- Shift/Alt+Enter 产生 `InsertNewline`，由编辑器在当前选择区执行。组词中不换行，Ctrl/Cmd+Enter 不提交。
- Esc 在组词中只产生 `CancelComposition`；否则只产生一个 `Escape`，由会话能力决定是否停回合。失焦和长按重复不产生键盘动作。
- `composing` 取 marked range 的存在性，不能按正文、拼音或范围长度推断。收到编辑器取消/提交结果后再以 `Edit` 更新；状态机不猜测输入法是否已消耗某个物理键。

直接测试：`cargo test -p nd-composer --locked`（构建资源按 `research/impl/BUILD.md` 包装）。这些测试证明输入意图，不证明实际输入法或界面性能。

GPUI 适配、后续接线和真机入口见 [桌面输入框](../nd-desktop/COMPOSER.md)。
