# New Desktop

owner 自写的 Claude Code 与 Codex 桌面端：Rust 守护进程加 GPUI 界面，后端是官方 Claude Code CLI 和 Codex app-server。领域用语见 `GLOSSARY.md`，已定的决定见 `docs/adr/`。

## Agent skills

### Issue tracker

工单和规格都是 GitHub 仓库 zhuran24/new-desktop 的 issue，用 gh 命令操作。见 `docs/agents/issue-tracker.md`。

### Triage labels

用默认的五个标签：needs-triage、needs-info、ready-for-agent、ready-for-human、wontfix。见 `docs/agents/triage-labels.md`。

### Domain docs

单一语境：根目录的 GLOSSARY.md 加 docs/adr/。见 `docs/agents/domain.md`。
