# New Desktop

Rust 守护进程与原生桌面界面，后端使用官方 Claude Code CLI 和 Codex app-server。当前代码交付工作区与组件内核；守护进程、协议和桌面外壳由后续工单接入。

领域用语见 [GLOSSARY.md](GLOSSARY.md)，架构决定见 [docs/adr](docs/adr/)。

## 工作区

需要 Rust 1.99。统一依赖在根 `Cargo.toml` 声明，`Cargo.lock` 入库，日常构建使用 `--locked`。

| 路径 | 职责 |
|---|---|
| `crates/nd-kernel` | 组件登记、依赖协调、实例代次、两段式停止；生产依赖只有标准库 |
| `ime-lab/variants/*` | 独立实验工作区，不参与产品的 workspace 测试 |

工作区按实现逐步增加 crate。`default-members` 显式列出无需 GPUI 的成员；后续 `nd-desktop` 加入 `members`，保持在 `default-members` 之外。同步副本 `nd-ui-core`、视图模型 `nd-view-model`、输入框状态机 `nd-composer` 各自作为不依赖 GPUI 的库接入。

GPUI 版本集中预留为 `gpui-pre =0.3.7`、gpui-kit Git 提交 `4c7f1350331562436df868c55ac33bebc4c6406c`。当前没有界面 crate，Cargo 不把未使用的 workspace 依赖写进锁文件；接入 `nd-desktop` 时继承 Kit 依赖并更新锁文件，核对同一依赖树没有其他 GPUI 线。产品不使用 ime-lab 的本地观测补丁。

## 构建与检查

本机实施工作树的构建产物放外置盘。以工单 #2 为例：

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-2
export CARGO_BUILD_JOBS=6
cargo fmt --all -- --check
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo clippy --workspace --all-targets --locked -- -D warnings
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- \
  cargo test --workspace --locked
```

GitHub CI 在限额 12 GiB、禁用 swap 的 Rust 容器中执行同一组检查，构建产物位于容器 `/tmp/nd-target`。内核测试经公开接口运行，不启动 CLI、读取登录数据或连接模型端点。

## 组件接入

公开接口和驱动规则见 [组件内核](crates/nd-kernel/README.md)。可选组件按配置启停，常驻组件不允许被配置禁用。守卫只撤销登记和调用权；进程、租约与已派发的工作由各自业务模块负责。

## 许可

本项目代码可任选 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 许可使用。第三方依赖保留各自许可证。
