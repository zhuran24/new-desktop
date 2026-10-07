# 主题文件与切换

日期：2026-10-06。状态：主题文件、目录热加载、界面选择与系统明暗跟随已实现。

点击窗口右上角「主题」，选择「跟随系统」「默认浅色」「默认深色」或目录中的 JSON 文件。选中项立即应用到现有窗口，不重建会话、输入框或同步副本。主题面板显示主题目录与文件名；重名主题按文件名区分。Esc 收起主题面板，保留当前选择。

## 文件位置与选择规则

默认目录是 `$XDG_CONFIG_HOME/new-desktop/themes/`；未设 XDG_CONFIG_HOME 时为 `$HOME/.config/new-desktop/themes/`。启动时可以用 `--themes DIRECTORY` 指定其他目录。目录不存在时在后台创建；加载和解析也在后台执行。

目录中的普通 `.json` 文件都会列在选择器中。支持新建、就地保存、编辑器的原子替换、删除和目录重建；无需重新启动。每个文件最多 64 KiB，须为 UTF-8；不读取子目录和符号链接。

- 「跟随系统」使用 GPUI 的窗口外观通知；Linux 上来自 XDG Settings portal。没有明暗偏好时使用平台默认外观。手动选择默认浅色/深色或文件主题后，系统变化不覆盖该选择。
- 选中文件损坏、缺项、超出取值范围或被删除时，应用系统明暗对应的内置默认主题，并在窗口底部显示文件名和错误。原选择保留，文件修复后自动恢复。未选中的坏文件只在选择器中列出错误。
- 目录或监视出错时显示原因。自动监视不可用时，可在主题面板点击「重新加载」。
- 选择属于每设备的视图偏好，保存在 `--state` 对应的 `ui.json` 中，不写守护进程配置。旧版仅有 `theme` 明暗字段的状态文件继续保留原选择。新设备默认跟随系统。

## 文件格式 1

完整示例：[海风主题](../nd-view-model/tests/fixtures/ocean.json)。复制该文件到主题目录后可直接选择。所有列出的字段都必填；未知字段、未知版本和错误类型会报错。颜色必须为 `#RRGGBBAA`，包括 alpha，例如 `#123456ff`；尺寸单位为逻辑像素。

顶层包含 `version: 1`、`name` 和 `theme`。`name` 是显示名；文件名是选择的稳定身份，重命名等同于删除旧主题并新增另一个主题。

| `theme` 字段 | 内容或范围 |
|---|---|
| `mode` | `light` 或 `dark`，供控件、代码高亮和后续 Mermaid 使用 |
| `colors` | `background`、`surface`、`foreground`、`muted`、`border`、`accent`、`diff_added`、`diff_removed`；全部为颜色字符串 |
| `typography.family`、`typography.mono_family` | 界面与等宽字体名；须已安装，字体缺失由字体系统回退，不下载字体 |
| `typography.body`、`typography.small` | 8–72 |
| `typography.title` | 8–96 |
| `spacing.small`、`spacing.medium`、`spacing.large` | 0–128 |
| `radius` | 0–64 |
| `border_width` | 0–8 |
| `shadow.inset` | 布尔值 |
| `shadow.color` | 颜色字符串 |
| `shadow.offset_x`、`shadow.offset_y`、`shadow.spread` | -64–64 |
| `shadow.blur` | 0–128 |

名称和字体名须为 1–256 字节的非空文本，不得含控制字符。所有数值必须有限。主题格式属于 New Desktop，不采用 GPUI Kit 的可选字段补默认规则；缺项会明确回退。

## 接入其他视图

`nd-view-model` 的 `ThemeDocument::parse` 校验文件契约；`ThemeCatalog::read/resolve` 提供目录快照和选择结果；这些类型不依赖 GPUI。`ThemeSelection` 与 `ViewState::theme_selection()` 表示设备选择。原始 JSON 由解析边界校验后才能应用。

桌面视图通过 `Presentation.theme` 或 `Desktop::theme()` 取得完整变量。`Desktop::select_theme` 是设备选择入口；目录和系统变化解析后沿 `set_theme` 更新已有 Composer 与 Kit 全局主题，再通知窗口重绘。自有控件继续从 Theme 读取布局、颜色、字体、圆角和阴影；Kit 适配只在桌面层。

会话事实和操作仍经 nd-wire；主题选择不产生会话命令，也不读写 CLI 存储。后续 #59 取解析后的 `Theme.mode`、字体和主题变量作为 Mermaid 请求及缓存键输入，不能仅用文件名判断缓存是否有效。Mermaid 渲染与 nd-wire 请求形状由 #59 实现和验收；nd-wire 不能反向依赖 nd-view-model，需映射渲染参数或提取中立共享值类型。

## 自动验证

```sh
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-23
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test -p nd-view-model --test themes --locked
bash scripts/test-scenarios.sh native_theme
```

原生场景使用私有 KWin、D-Bus、dconf 与真实 XDG portal，启动真实守护进程；以鼠标命中产品选择器，修改真实文件，检查实际帧、错误提示、选择恢复及系统信号。没有连接日常桌面、凭据或模型服务。完整结果及真机验收边界见 [#23 验证](../../docs/verification/ticket-23.md)。
