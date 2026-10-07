# 独占登记骨架

`nd-claims` 是 #12 的常驻登记模块，保存后端会话租约、自有进程身份、未命名预留和最后自有叶子，持续检测 Claude 外部写入者。SQLite 与 `nd-store` 共用；直接调用 `Exclusivity` 是规格确认的窄接缝二。

## 装配与恢复

1. 用守护进程的 `Arc<Store>` 和实际 `CLAUDE_CONFIG_DIR` 创建一个 `Exclusivity::open`。每个数据库只装一个实例；会话执行器共享它。模块不能按界面开关卸下。
2. 看守枚举的 `nd_runs::Found` 经 `observe_watchdog` 报入。参数 `generation` 是控制连接代次；进程编号跨重连不变，重新拉起必须用新编号。辅助进程也报 `Up`，没有持有记录就不报假 `Holding`。
3. 适配器从自己的流水/恢复快照报 `Holding`。流水来的观察用 `observe_in(tx, ...)`，与批次、游标及会话事实同事务。`Held::last_leaf` 只收自有流水确认的 UUID，`None` 表示本批没有新的叶子证据。尚未确认的预留不因空 `Holding` 释放，须用明确的 `NeverOpened`/`Gone` 证据。
4. 看守枚举完成、各适配器的身份/持有集合恢复完成后，协调者报 `Recovered`。不认识版本或身份不明的进程保留责任，不能为了完成恢复伪造空集合。
5. 等 `recovery() == Ready` 再恢复操作。`Recovered` 只到 `IdentitiesKnown`；随后完整注册表扫描才到 `Ready`。可在事务外调用 `refresh()` 立即扫描，后台也会按默认 2 秒补扫。`watch()` 给出提交后失效通知，消费者醒来后重读公开查询；通知可以合并，不携带唯一的业务事实。

创建实例会把持久租约标成身份待核，重置扫描阶段；不会按未报告、Drop、超时释放租约。退出登记实例只停止自己的扫描线程，不发任何进程停止命令。

## 调用契约

| 接口 | 用法 |
|---|---|
| `admit(tx, cause, Act::Open)` | 与操作阶段/发件同事务。`Known` 立即把租约预留到 `via` 后端进程编号；`Fresh` 保存未命名预留，之后 `bind` |
| `admit(tx, cause, Act::Write)` | 每次重查外部障碍与持有者。返回 `Live(run)` 才能写现有进程；`Fresh` 表示当前无持有者，调用方先通过一次 `Open` 预留/拉起，不能直接发送 |
| `readmit(tx, cause, act)` | 仅用于适配器证实 `Withheld` 的未写出票；重算当前资格，被撤销的授予以后重问仍须重算。已写出或不明的票用原授予继续对账 |
| `bind(tx, cause, bs)` | 后端返回新 id 后，与落定同事务绑定。冲突返回 `BindResult::Conflict`，暂停双方并保留原持有者；调用方必须提交该结果，随后处理原操作，不能把冲突变成事务回滚 |
| `observe_in` / `observe` | 前者供流水批次使用；后者自开小事务供控制通道使用。后者与所有只读查询、`refresh` 都不要在 `Store::write` 闭包中调用 |
| `lease` / `peek` / `owned_leaf` | 只读，不触发外部 I/O，不授予。未收到绑定证据的预留不能作为可写活进程 |
| `check_record(bs, path)` | 事务外完整读文件，直接用 `nd-claude-records` 求 CLI 选中的叶子；返回自有基线与文件叶子，不修改基线，也不等于续接授权 |
| `externals` | 按进程身份排除自己后，再按自己持租约的后端会话 id 过滤展示。冲突裁决只使用常开的注册表扫描；CLI 列表仅用于展示，失败只使列表查询报错 |
| `want_list` | RAII 列表兴趣。持有时补扫调用 `CliCommands::agents`；Drop 只停止列表命令，不关闭目录检测 |

同一 `cause` 不能换动作，包括等待中的原因。已经授予的 `Open` 重放返回历史授予，不代表可以重复执行原生创建；创建执行去重由后端票与引擎负责。`Write` 则始终重判障碍。

租约只因已核实的 `Gone`、较新 `Holding` 不再持有已确认的绑定、或适配器核实的 `NeverOpened` 离开。`Holding` 的控制代次必须匹配，流水位置严格递增。`IdentityMismatch` 只暂停写入；重复 PID、错误启动 ticks、错误 boot id 不把原身份换掉。`observe_watchdog` 对 `Gone` 额外核对 `/proc`，活着或观察失败时不能释放。直接 `observe_in(Gone)` 的调用方须已持有同等强度的身份/退出证据。

## 外部文件与命令

扫描 `sessions/*.json`、`jobs/*/state.json`，未知字段忽略。真实 2.1.289 的 `procStart` 是字符串；`pidDomain` 由 machine-id 和 PID 命名空间构成。只在命名空间可核对、`startedAt` 属于本次启动、PID 和启动 ticks 对上且进程存活时形成身份；异命名空间、无 PID 或缺启动字段都不猜。自有身份再带当前 boot id 比较。坏文件使写入等待 `Checking`，完整重扫成功才恢复。

inotify 的变化、溢出和错误触发完整重扫，周期补扫覆盖丢事件、目录新建和没有文件变化的进程退出。监听和周期扫描不依赖列表订阅。完整扫描与事务提交之间、准入与实际写出之间仍有外部竞争窗口；本模块负责后续发现和暂停，不宣称提供 OS 文件锁。

`open_with_commands` 注入固定版本的 `PinnedCli`，环境从完整白名单构造并去掉 `BUN_OPTIONS`。窄接缝仅替换这一个接口。`stop` 提供 8 位十六进制 id 的命令边界，本单没有接管入口，也不把 stop 成功当作退出证据。`open` 不配置短命 CLI，文件检测仍正常；若要展示列表，应由版本库装配 `PinnedCli`。

## 范围与后续交界

- #13/#19 装配登记并把 `watch` 变化投进会话执行器/恢复协调者；本单不新增 nd-wire/UI 私有接口，也不替代会话命令账本。
- #26/#27 完成 Codex 运行时消费者、游标下限和排空；本骨架支持多条 Codex 租约、并发未命名预留与晚绑定。终端 Codex TUI 同线程并发续接没有注册表，不能检测。
- #43 实现目录活动、恢复代码占用、`hold_valid`、`direct_write`、`settle`。当前 `Act` 只开放本阶段的 `Open/Write`，控制输入不经过它。
- #49 将完整续接前叶子核对、`continued_elsewhere` 谱系事实和 `PinLeaf` 整合进操作事务；当前只保留自有基线和只读比较。
- #62/#64 扩展外部列表、版本/续接参数资格、接管确认与停止状态机。本单的后台无 pid 夹具来自真实 CLI，但没有证明接管或完整后台元数据契约 R12-X2。

存储是登记模块独占写入的版本内 JSON 状态行，调用方不得访问内部表。该骨架没有承诺常数时间准入；后续规模优化可在模块内部拆表/加提交后索引，不改变公共接缝。生产代码不写 CLI 存储，无新增结构/派发操作，所以引擎崩溃矩阵由实际操作工单接入；本单覆盖调用方事务回滚和从同一数据库重建。

## 验证

```bash
export CARGO_TARGET_DIR=/mnt/wd_external/nd-build/target/ticket-12
export CARGO_BUILD_JOBS=6
systemd-run --user --scope --quiet -p MemoryMax=12G -p MemorySwapMax=0 -- cargo test -p nd-claims --locked
scripts/test-claims-live.sh
```

默认测试用真 SQLite、真短命进程以及真实 CLI 生成的注册表/记录副本，只脚本化 `CliCommands`。现场脚本在 bwrap 断网命名空间里运行两条钉版 CLI，验证自有排除、同 id 冲突、真实 agents 输出和清理；使用临时 HOME/config/XDG 与独立受限 slice，不加载 owner 配置或凭据。原始夹具的来源清单在 `tests/fixtures/`；执行证据保留在 E 盘。验证结论见 [ticket-12](../../docs/verification/ticket-12.md)。

稳态 `Write` 每次重新裁决，不保存历史 cause 或 grant。`Open` 预留按 cause 建索引、按 run 清理，已退出 run 的身份单独建索引；旧 v1 的整块 JSON 在打开时事务迁移。退出身份保留用于拒绝迟到的 Up/Holding，不参与每次状态 JSON 的解码与重写。
