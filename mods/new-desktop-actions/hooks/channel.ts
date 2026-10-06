// 动作 mod 的控制循环：报到（hello）、长轮询取命令、按操作 id 回结果、/clear 后重绑。
// 刻意保持薄：命令原样变成 `$` 调用，结果按操作 id 留着。
// `$` 只在本文件的顶层函数之间传递（CLI 的静态检查不允许跨文件传 `$`）。
import { MOD_VERSION, PROTO_VERSION } from './proto.ts'
import type { Command, HelloCause, HelloReply, Next, Outcome } from './proto.ts'
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

async function execute($: any, command: Command): Promise<Outcome> {
  const current = await $.session.id()
  const reason = stale(command, current)
  if (reason) return { status: 'rejected', reason }
  switch (command.action.type) {
    case 'ping':
      return { status: 'done', value: { backend_session_id: current, mod_gen: self.gen } }
    case 'query':
      return { status: 'done', value: { ops: answer(command.action.op_ids) } }
    default:
      return { status: 'rejected', reason: { code: 'unsupported' } }
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
        begin(command.op_id)
        let outcome: Outcome
        try {
          outcome = await execute($, command)
        } catch (error) {
          outcome = { status: 'failed', error: String(error) }
        }
        finish(command.op_id, outcome)
        unsent.set(command.op_id, outcome)
        await report($, command.op_id, outcome)
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
