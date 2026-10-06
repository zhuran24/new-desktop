// 由 nd-mod-schema 从 crates/nd-mod-proto 生成，不要手改。
// 重新生成：cargo run -p nd-mod-proto --bin nd-mod-schema -- .

export const PROTO_VERSION = 1;
export const MOD_VERSION = "0.1.0";
export const HOOK_MOD: ModName = "new-desktop";
export const ACTION_MOD: ModName = "new-desktop-actions";

/** 命令的动作。可重发类别见 [`Action::resend`]。 */
export type Action =
  /** 核对通道与身份，结果是 [`Pong`]。 */
  | { type: "ping" }
  /** 按操作 id 查本代次留下的状态和结果，结果是 [`QueryAnswer`]。 */
  | { type: "query"; op_ids: string[] };

/** 守护进程发给 mod 的一条命令。mod 执行前核对身份，对不上就拒绝。 */
export type Command = {
  action: Action;
  expected_backend_session_id: string;
  expected_mod_gen: string;
  op_id: string;
};

/** 非 200 回应的正文。 */
export type ErrorReply = {
  code: string;
  message: string;
};

/** `POST /hello`：mod 报到并绑定到（后端进程，后端会话）。 */
export type Hello = {
  /** `$.session.id()`：CLI 当前的后端会话 id。 */
  backend_session_id: string;
  cause: HelloCause;
  /** `$.session.version().version`。 */
  cli_version: string;
  mod: ModName;
  /** 每次模块装载新生成；重载后旧代次的结果查不到。 */
  mod_gen: string;
  mod_version: string;
  proto: number;
  run: string;
};

/** 发 hello 的原因。 */
export type HelloCause =
  /** 模块装载（含热重载、worker 重生）后的 `session.start`。 */
  | "start"
  /** `/clear` 之后 CLI 换了后端会话 id。 */
  | "clear"
  /** 守护进程要求重报（例如守护进程重启后不认得这个绑定）。 */
  | "rehello"
  /** 轮询前重读后端会话 id，发现变了。 */
  | "id_changed";

export type HelloReply = {
  /** 绑定代次：这个后端进程每换一次后端会话 id 加一。 */
  binding_epoch: number;
};

/** 两个 mod 的插件名（plugin.json 的 name，也是 pluginConfigs 的键）。 */
export type ModName = "new-desktop" | "new-desktop-actions";

/** 把全部消息类型挂在一个根上，供 JSON Schema 与 TypeScript 一次导出。 */
export type ModProtocol = {
  error: ErrorReply;
  hello: Hello;
  hello_reply: HelloReply;
  next: Next;
  next_query: NextQuery;
  options: PluginOptions;
  pong: Pong;
  query_answer: QueryAnswer;
  report: Report;
  report_ack: ReportAck;
  result: ResultPost;
};

/** `GET /next` 的回应：长轮询到时限、有命令或要求重报 hello 时返回。 */
export type Next = {
  commands: Command[];
  /** 守护进程不认得这个绑定（或后端会话 id 刚换），mod 应重读 id 并重报 hello。 */
  rehello: boolean;
};

/** `GET /next` 的查询参数。 */
export type NextQuery = {
  backend_session_id: string;
  mod: ModName;
  mod_gen: string;
  run: string;
};

export type OpPhase =
  | "running" | "done"
  /** 本代次没见过这个操作 id。 */
  | "unknown";

export type OpState = {
  op_id: string;
  outcome?: Outcome | null;
  phase: OpPhase;
};

export type Outcome =
  | { status: "done"; value: unknown }
  | { status: "failed"; error: string }
  /** mod 没有执行这条命令。 */
  | { status: "rejected"; reason: Rejection };

/** `--settings` 的 `pluginConfigs.<mod>.options`；字段在 plugin.json 的 userConfig 里声明。 */
export type PluginOptions = {
  /** 后端进程编号。 */
  run: string;
  /** 守护进程 mod 通道的 unix socket 路径（约 100 字节以内）。 */
  sock: string;
};

/** ping 的结果。 */
export type Pong = {
  backend_session_id: string;
  mod_gen: string;
};

/** query 的结果。 */
export type QueryAnswer = {
  ops: OpState[];
};

export type Rejection =
  /** 命令带的后端会话 id 不是当前的（例如 `/clear` 之后）。 */
  | { code: "stale_session"; current: string }
  /** 命令带的 mod 代次不是当前装载的。 */
  | { code: "stale_gen"; current: string }
  /** 这个 mod 不认识或不执行这种动作。 */
  | { code: "unsupported" };

/** `POST /report`：mod 主动上报的事实。 */
export type Report = {
  backend_session_id: string;
  body: ReportBody;
  dispatch_id?: string | null;
  mod: ModName;
  mod_gen: string;
  /** 由 mod 生成，重发时不变，守护进程据此去重。 */
  report_id: string;
  run: string;
};

export type ReportAck = {
  /** 只有报告已落库才为 true；false 表示守护进程只收在内存里。 */
  durable: boolean;
};

export type ReportBody = { kind: "session_end"; ended_session_id: string; reason: string };

/** `POST /result/<op_id>` 的正文。 */
export type ResultPost = {
  backend_session_id: string;
  mod: ModName;
  mod_gen: string;
  outcome: Outcome;
  run: string;
};
