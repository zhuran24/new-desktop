# 功能边界跟着 CLI 走

New Desktop 是给 Claude Code CLI（以及其他后端）做的桌面端，不是 claude.ai 的客户端：官方桌面端里依赖 claude.ai 云服务的功能（Code Projects、云端会话、账号设置、连接器授权等），CLI 本身支持的才做，CLI 不支持的不做；远程控制改由我们自己的守护进程提供远程访问。官方的 mod 界面协议也不做，以后需要的定制直接写成组件。
