// 动作 mod 的控制循环：报到（hello）、长轮询取命令、按操作 id 回结果、/clear 后重绑。
// 刻意保持薄：命令原样变成 `$` 调用，结果按操作 id 留着。
// `$` 只在本文件的顶层函数之间传递（CLI 的静态检查不允许跨文件传 `$`）。
import { MOD_VERSION, PROTO_VERSION, SUMMARIZE_PREFIX } from './proto.ts'
import type {
  Command,
  CompactDone,
  ForkDone,
  HelloCause,
  HelloReply,
  Next,
  Outcome,
  ShellDone,
} from './proto.ts'
import { answer, begin, finish, nextUrl, self, stale, unsent } from './state.ts'

const RETRY_MS = 1000

async function post($: any, path: string, body: unknown): Promise<any> {
  const response = await $.http.fetch('http://nd' + path, {
    method: 'POST',
    socketPath: self.options.sock,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (response.status !== 200) throw new Error(`${path}: HTTP ${response.status} ${response.text}`)
  return JSON.parse(response.text)
}

async function hello($: any, sid: string, cause: HelloCause) {
  const { version } = await $.session.version()
  const reply: HelloReply = await post($, '/hello', {
    proto: PROTO_VERSION,
    run: self.options.run,
    mod: self.name,
    mod_version: MOD_VERSION,
    mod_gen: self.gen,
    backend_session_id: sid,
    cli_version: version,
    cause,
  })
  self.sid = sid
  self.epoch = reply.binding_epoch
}

async function report($: any, opId: string, outcome: Outcome) {
  await post($, '/result/' + encodeURIComponent(opId), {
    run: self.options.run,
    mod: self.name,
    mod_gen: self.gen,
    backend_session_id: self.sid,
    outcome,
  })
  unsent.delete(opId)
}

/** `/compact ND_SUM …`：命令排到会话空闲才跑；钩子 mod 的压缩钩子按参数定位、只压缩所选范围。 */
async function compact($: any, spec: unknown): Promise<CompactDone> {
  const result = await $.command.run({ command: 'compact', args: SUMMARIZE_PREFIX + JSON.stringify(spec) })
  if (result?.deny !== undefined) return { compacted: false, skipped: `denied: ${String(result.deny)}` }
  if (typeof result?.text === 'string') return { compacted: false, skipped: result.text }
  return { compacted: true }
}

/** `!` 模式：调 Bash 跑这一条命令，再照 CLI 自己 `!` 模式的行格式把命令和输出追加进对话。 */
async function shell($: any, line: string, description: string): Promise<ShellDone> {
  const result = await $.tool.call({ tool: 'Bash', command: line, description })
  if (result?.deny !== undefined) {
    return { stdout: '', stderr: '', appended: false, denied: String(result.deny) }
  }
  let exit: number | undefined
  let stdout: string
  let stderr: string
  if (result?.isError) {
    const text = String(result.text ?? result.result ?? '')
    const code = /^(?:Error: )?Exit code (-?\d+)\n?/.exec(text)
    exit = code ? Number(code[1]) : undefined
    stdout = ''
    stderr = code ? text.slice(code[0].length) : text
  } else {
    const output = result?.result ?? {}
    exit = output.interrupted ? undefined : 0
    stdout = String(output.stdout ?? result?.text ?? '')
    stderr = String(output.stderr ?? '')
  }
  const appended = await $.session.append({
    message: {
      type: 'user',
      content: [
        { type: 'text', text: `<bash-input>${line}</bash-input>` },
        { type: 'text', text: `<bash-stdout>${stdout}</bash-stdout><bash-stderr>${stderr}</bash-stderr>` },
      ],
    },
  })
  return { exit, stdout, stderr, appended: !!appended && appended.deny === undefined }
}

/** 派 fork 型子代理：带着父对话，后台跑。 */
async function fork($: any, prompt: string, description: string): Promise<ForkDone> {
  const result = await $.agent.spawn({ subagentType: 'fork', prompt, description })
  if (result?.deny !== undefined) return { denied: String(result.deny) }
  return { agent_id: result?.agentId, model: result?.model }
}

async function execute($: any, command: Command): Promise<Outcome> {
  const current = await $.session.id()
  const reason = stale(command, current)
  if (reason) return { status: 'rejected', reason }
  const action = command.action
  switch (action.type) {
    case 'ping':
      return { status: 'done', value: { backend_session_id: current, mod_gen: self.gen } }
    case 'query':
      return { status: 'done', value: { ops: answer(action.op_ids) } }
    case 'compact':
      return { status: 'done', value: await compact($, action.spec) }
    case 'shell':
      return { status: 'done', value: await shell($, action.command, action.description) }
    case 'fork':
      return { status: 'done', value: await fork($, action.prompt, action.description) }
    default:
      return { status: 'rejected', reason: { code: 'unsupported' } }
  }
}

/** 执行一条命令并回报结果。长命令不挡住长轮询；`!` 命令按到达次序一条一条跑（工作目录要接得上）。 */
async function run($: any, command: Command) {
  begin(command.op_id)
  let outcome: Outcome
  try {
    outcome = await execute($, command)
  } catch (error) {
    outcome = { status: 'failed', error: String(error) }
  }
  finish(command.op_id, outcome)
  unsent.set(command.op_id, outcome)
  try {
    await report($, command.op_id, outcome)
  } catch (error) {
    // 留在 unsent 里，控制循环恢复后补报。
  }
}

async function runShell($: any, command: Command) {
  const previous = self.shells
  let release = () => {}
  self.shells = new Promise<void>((resolve) => {
    release = resolve
  })
  await previous
  try {
    await run($, command)
  } finally {
    release()
  }
}

async function loop($: any) {
  for (;;) {
    try {
      for (const [opId, outcome] of [...unsent]) await report($, opId, outcome)
      const sid = await $.session.id()
      if (sid !== self.sid) await hello($, sid, self.sid === '' ? 'start' : 'id_changed')
      const response = await $.http.fetch(nextUrl(self.sid), { socketPath: self.options.sock })
      if (response.status !== 200) throw new Error(`/next: HTTP ${response.status} ${response.text}`)
      const next: Next = JSON.parse(response.text)
      if (next.rehello) {
        await hello($, await $.session.id(), 'rehello')
        continue
      }
      for (const command of next.commands) {
        const type = command.action.type
        if (type === 'ping' || type === 'query') await run($, command)
        else if (type === 'shell') void runShell($, command)
        else void run($, command)
      }
    } catch (error) {
      // 守护进程不在或正重启：稍后重连；没报出去的结果留在 unsent 里补报。
      await $.clock.sleep(RETRY_MS)
    }
  }
}

/** 每次模块装载一次：同步报到（`$` 调用不占钩子预算），再起控制循环。 */
export async function onSessionStart($: any, e: any, next: any) {
  try {
    await hello($, await $.session.id(), 'start')
  } catch (error) {
    // 守护进程暂时不可达：控制循环会重试报到。
  }
  if (!self.polling) {
    self.polling = true
    $.clock.after(0, () => {
      void loop($)
    })
  }
  return next(e)
}
