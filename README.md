# New Desktop

Rust 守护进程与原生桌面界面，后端使用官方 Claude Code CLI 和 Codex app-server。当前代码包含组件内核、本机守护进程、只读同步流、配置与存储底座、Claude 记录解析及对话转换库；后端会话和桌面外壳由后续工单接入。

领域用语见 [GLOSSARY.md](GLOSSARY.md)，架构决定见 [docs/adr](docs/adr/)。

## 工作区

需要 Rust 1.99。统一依赖在根 `Cargo.toml` 声明，`Cargo.lock` 入库，日常构建使用 `--locked`。

| 路径 | 职责 |
|---|---|
| `crates/nd-kernel` | 组件登记、依赖协调、实例代次、两段式停止；生产依赖只有标准库 |
| `crates/nd-wire` | 协议 Rust 类型与 JSON Schema 生成器 |
| `crates/nd-config` | 分节监视、修订冲突检查、原子配置写入 |
| `crates/nd-store` | SQLite 事务、只读池、SHA-256 附件及引用 |
| `crates/nd-ui-core` | 无 GPUI 依赖的连接、同步副本及读取接口 |
| `crates/nd-daemon` | 守护进程、UDS HTTP/WebSocket、可选组件和 ndctl |
| [`crates/nd-claude-records`](crates/nd-claude-records/README.md) | Claude 主记录选链、字节偏移索引、历史分页；只读纯库 |
| [`crates/nd-convert`](crates/nd-convert/README.md) | Claude/Codex 经中间条目互转、损失清单与中立设置换算；纯库 |
| `ime-lab/variants/*` | 独立实验工作区，不参与产品的 workspace 测试 |

工作区按实现逐步增加 crate。`default-members` 显式列出无需 GPUI 的成员；后续 `nd-desktop` 加入 `members`，保持在 `default-members` 之外。同步副本 `nd-ui-core` 已提供；视图模型 `nd-view-model`、输入框状态机 `nd-composer` 各自作为不依赖 GPUI 的库接入。

GPUI 版本集中预留为 `gpui-pre =0.3.7`、gpui-kit Git 提交 `4c7f1350331562436df868c55ac33bebc4c6406c`。当前没有界面 crate，Cargo 不把未使用的 workspace 依赖写进锁文件；接入 `nd-desktop` 时继承 Kit 依赖并更新锁文件，核对同一依赖树没有其他 GPUI 线。产品不使用 ime-lab 的本地观测补丁。

## 构建与检查

本机实施工作树的构建产物放外置盘。以工单 #3 为例：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-3
export CARGO_BUILD_JOBS=6
cargo fmt --all -- --check
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo clippy --workspace --all-targets --locked -- -D warnings
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test --workspace --locked
```

GitHub CI 在限额 12 GiB、禁用 swap 的 Rust 容器中执行同一组检查，构建产物位于容器 `/tmp/nd-target`。默认测试经公开接口运行，记录解析使用已录制的离线 CLI 夹具，不启动 CLI、读取登录数据或连接模型端点。记录解析库另有需明确运行的真 CLI 断网验收，见其说明。systemd 隔离场景需显式启用 `nd-daemon/scenarios`，运行条件及命令见[守护进程使用说明](crates/nd-daemon/README.md)。

## 守护进程

安装、配置、ndctl、协议与后续接口约定见[守护进程使用说明](crates/nd-daemon/README.md)。协议 Schema 位于 [protocol](protocol/)，systemd 用户服务位于 [packaging/systemd](packaging/systemd/)。

## 组件接入

公开接口和驱动规则见 [组件内核](crates/nd-kernel/README.md)。可选组件按配置启停，常驻组件不允许被配置禁用。守卫只撤销登记和调用权；进程、租约与已派发的工作由各自业务模块负责。

## 许可

本项目代码可任选 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 许可使用。第三方依赖保留各自许可证。
