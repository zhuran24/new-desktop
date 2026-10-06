# #17 附件与 diff 验证

日期：2026-10-06。状态：实现与自动验收已完成；真实模型、日常输入法与性能保留为 OWNER_PENDING。自动场景仅连接隔离伪模型端点。

## 产品行为

- 桌面粘贴图片、粘贴文件或拖入文件后，经同步副本的鉴权 HTTP PUT 按 SHA-256 上传。草稿可移除附件；上传失败显示原因，上传完成前禁发。只有对应草稿修订的明确受理才清除正文和附件。纯附件消息可发送，组词时仍禁止发送。
- 支持 PNG、JPEG、GIF、WebP、PDF 和 UTF-8 文本；按字节识别，不靠扩展名。每个附件 1 字节至 5 MiB，每条最多 8 个、总计最多 16 MiB。不支持的二进制文件须先转换为上述格式。
- 消息只携带 `Attachment{blob,name,media_type,size}`。同一事务核对 blob 大小与可引用性、保留引用、落消息和收据；整条无效时不留部分引用。首条提示尚未进入发送步骤的新建失败释放引用；已有历史提示的失败或交付不明仍保留附件。
- Claude 在事务外读取并核散列，以 image/document base64 内容块或标注文件名的 UTF-8 文本块发送。用户正文保持原样，既有 UUID、发送意图和对账语义保持。消息事件不带附件正文。只读 CLI 自己的回显和记录，不写 CLI 原生存储。
- Markdown 中的 diff/patch 围栏和 Edit 工具的替换片段显示为 diff。行前缀、旧/新行号、中文、空行、无末尾换行标记保留；不完整或损坏 hunk 不猜行号。Edit 的行号从片段起算，不能当作文件绝对行号。`unified_diff`、`replacement_diff`、`message_blocks` 保留为完整重算的简单差分基准。
- 图片预览走 HTTP GET；最近 8 张自动加载，其他图片点击加载。缓存最多 64 项。diff 增删颜色使用 `Theme.colors.diff_added/diff_removed`，其余配色、字号、字体和间距均来自主题。

## 测试接缝与覆盖

| 接缝 | 场景或测试 | 观察结果 |
|---|---|---|
| 主接缝 | `attachments_reach_the_model_and_remain_in_the_conversation` | 相同内容得到相同散列；真 CLI 发出的模型请求包含精确 PNG 字节；快照保存引用；GET 读回 |
| 主接缝 | `jpeg_gif_and_webp_attachments_reach_the_model_with_their_original_bytes` | 三种格式经真 CLI 后 MIME 和解码字节均等于原件 |
| 主接缝 | `files_without_caption_reach_the_model_and_survive_restart_and_collection` | UTF-8 正文和 PDF 原字节进模型请求；无正文创建和发送；重启后消息引用、文件仍在；未引用上传被清理 |
| 主接缝 | `attachment_validation_rejects_a_whole_message_without_holding_partial_uploads` | 缺失、大小不符、不支持类型整条拒绝，未引用内容可清理 |
| 主接缝 | `withdrawn_creation_releases_unused_attachments_and_keeps_its_receipt` | 没有发出首条提示的创建补偿释放引用；清理后相同命令仍回原收据 |
| 主接缝 | `a_multi_megabyte_attachment_is_not_lost_at_the_watchdog_frame_boundary` | 2,400,000 字节文本经过看守和 CLI，模型收到完整正文 |
| 主接缝 | `desktop_upload_reads_file_bytes_and_rejects_unsupported_or_missing_files` | 公共桌面命令入口读取真实文件；拒绝目录、缺失文件和不支持二进制 |
| 主接缝 | `real_edit_tool_exposes_the_replaced_text_for_diff_display` | 真 CLI Read/ Edit 工具往返后，文件实际改变，快照提供完整替换片段 |
| 原生窗口加主接缝 | `native_attachment_paste_drop_and_diff_rendering` | 私有 KWin/剪贴板；图片粘贴、文件粘贴和 GPUI 拖放事件产生三个真实上传；模型收到三份内容；流式期间杀界面，重开不重发；明暗截图 |
| 引擎崩溃矩阵 | `attachment_create_and_send_recover_at_every_commit_point`、`attachment_creation_compensation_releases_unused_uploads_at_every_commit_point` | 新建与发送带附件；三种故障点遍历全部提交；快照、端口动作保留引用；原生步骤至多一次 |
| 纯计算 | `nd-view-model/tests/diff.rs`、`chat.rs`，`nd-composer/tests/input.rs` | 多 hunk/行号/Unicode/无末尾换行、围栏与工具替换、草稿修订、纯附件的组词守卫 |

运行入口：`cargo test --workspace --locked` 和 `scripts/test-scenarios.sh`。所有 build/test/Clippy 使用 6 jobs、独立 12 GiB/零 swap scope，产物位于 E 盘 `target/ticket-17`。CLI 固定 `/mnt/wd_external/nd-build/cli/claude-2.1.289`；临时 HOME、CLAUDE_CONFIG_DIR、XDG、断网 bwrap 和 `nd-test-` 单元由场景运行器管理。默认套件不触发真实 OOM。

本单扩充了既有创建/发送的崩溃矩阵场景，没有另建派发动作或可选组件。HTTP 鉴权沿用 UDS 对端 uid 与私有 runtime 目录，既有 `attachment_roundtrip_uses_authenticated_http_and_survives_restart` 场景随全套运行。

## verification：待验证项与退路

#17 无专属编号待验项；INDEX #17 的产品链路边界已按上表实测。

1. **图片与普通文件**：固定 CLI 接受上述图片和 PDF 内容块，文本以明确文件名加正文发送，主方案成立。不支持类型明确拒绝，不假装模型能看到本机路径。真服务的内容理解不由伪端点证明。
2. **文件粘贴**：固定 GPUI 的 Linux Wayland `Clipboard::read` 只尝试文本与图片；文件 MIME 不会生成 `ExternalPaths`。已采用退路：桌面在 Paste 捕获阶段，用 `/usr/bin/timeout 2 /usr/bin/wl-paste --type text/uri-list --no-newline` 读取文件 URI，只传 XDG_RUNTIME_DIR/WAYLAND_DISPLAY；后台限时、最多 64 KiB。无文件 MIME 时继续 Kit 原生粘贴，异步期间焦点/正文/选区改变或进入组词即取消。Wayland 文件粘贴需要 `wl-clipboard`；拖放不依赖它。原生场景以私有 `wl-copy` 实测。X11 的文件剪贴板兼容不在本机 Wayland 证明范围内。
3. **大附件**：旧看守单行上限约 2 MiB，实际测试失败。看守帧上限扩为 128 MiB，单行仍限制为帧的四分之一（32 MiB）；适配器在写出前拒绝超过编码上限的消息，避免误标交付不明。伪端点也显式设 32 MiB 请求上限，防止 Axum 默认 2 MiB 上限制造假的模型拒绝。运行中的旧看守不会获得新界限；大附件要求同版本组合中的新看守。
4. **diff 与主题**：简单投影和原生明暗截图通过。尚未优化 diff 算法或虚拟化，不声称长历史性能达标。

## 后续工单接口

- #16 草稿：复用 `nd_wire::Attachment` 和 Draft 修订语义，草稿持久化/移除时在同一事务 `hold/release`。当前未发送附件只有上传宽限期（默认一天），重启界面不恢复内存草稿。过期引用发送会被拒绝，需重新上传。
- #35 回退、#50/#51 转换：字节由 Blobs 持有，引用随提示投影保存。不要把 base64 放进 nd-wire 事件或让转换依赖 GPUI 图片对象。保留或删除历史时，以 `message/<会话 id>/<命令 id>` 为引用所有者；初次创建使用创建命令 id，不使用提示显示 id。
- #20：保留简单 diff 全量投影作差分基准；分页时按显示范围安排图片下载。#23：主题文件需要覆盖两个 diff 颜色变量。
- 清理只能释放被删除业务对象自己的 owner 引用，不能按某个 UI 窗口消失来释放已发消息。

## owner_checklist

独立实例的完整启动、登录、清理命令见不入库实施记录 `research/impl/tickets/17.md`。以下尚未执行，不影响自动实施闭环。

- [ ] 真实模型：在独立测试配置登录后创建 Haiku 会话；粘贴一张四象限彩色截图和一份含唯一标记的文本/PDF，让模型描述颜色并逐字读出标记；确认回复符合真实内容。真服务调用由 owner 自行决定。
- [ ] 日常桌面：分别从截图工具粘贴图片、Dolphin 复制文件后粘贴、拖入含中文/空格名称的文件；检查缩略图、移除、纯附件发送和错误提示。两种主题核对 diff 的增删文字、行号、水平滚动；失败上传不清空草稿。
- [ ] 豆包/Rime：带附件时组词按 Enter 不发送，候选位置正确；非组词 Shift/Alt+Enter 换行；点击发送和 Enter 行为一致。真实候选框和输入延迟必须由 owner 在日常桌面验收。
- [ ] release 上屏 p95 ≤50 ms、静止 CPU <1%：分别在空草稿、带图草稿和流式输出时采样；使用 #14 清单中的相机/呈现帧方案和 `crates/nd-desktop/scripts/composer-cpu.py`，保留原始样本。合成事件、虚拟 KWin 和默认测试不能替代这些测量。
