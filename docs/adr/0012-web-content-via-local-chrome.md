# 网页内容用本机 Chrome 渲染：Mermaid 借日常 Chrome，MCP App 卡片用自起的无窗口实例

Mermaid 图和 MCP App 卡片要网页引擎。Wayland 下别的进程的窗口嵌不进 GPUI，GPUI 在 Linux 上也没有外部纹理，所以只能让 Chrome 在屏外渲染：截屏帧画进 GPUI，鼠标、键盘、输入法经 CDP 送回（research/round5/chrome-embed.md）。owner 要求尽量复用他平常开着的 Chrome（磁盘、内存）。按这个要求：
- Mermaid：在日常 Chrome（CDP 端口 38947）里建一个隔离的浏览器上下文，开看不见的页面算出 SVG，由 GPUI 自己画。不开窗口，不开标签，比另起实例省约 100 MiB。
- MCP App 卡片：用同一个 Chrome 程序另起一个 New Desktop 自己的无窗口实例，只在有卡片时运行。放进日常 Chrome 只省 25–35 MiB，却要多一个最小化窗口，卡片随浏览器生灭，还会被别的调试工具看到。
- 卡片的无窗口实例由桌面界面拉起：每台装了桌面界面的电脑用它自己的 Chrome。
- 日常 Chrome 没开时，Mermaid 用守护进程拉起的短命无窗口实例，闲置约 2 分钟就关，因为没有桌面界面时（比如只有手机连着）也要能出图。两个实例同时在的情况只出现在这几分钟里，多占约 70–145 MiB。不替 owner 拉起他的日常浏览器。

## Considered Options

- CEF：另占约 300 MiB 磁盘，Chromium 的安全更新要自己跟，Linux 上也没有和 GPUI 集成的先例，只作后备。
- `--app` 窗口、wry 和 GPUI Kit 的 WebView：Wayland 下嵌不进来。
