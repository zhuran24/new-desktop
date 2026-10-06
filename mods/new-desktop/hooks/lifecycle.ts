// 钩子 mod 的会话生命周期能力：把 session.end 作为报告交给守护进程。
// session.end 整条链只有约 1.5 秒墙钟，这里只发一次本机 POST，不做重活。
import type { ReportAck } from './proto.ts'
import { self } from './state.ts'

async function report($: any, reason: string, ended: string): Promise<ReportAck> {
  const response = await $.http.fetch('http://nd/report', {
    method: 'POST',
    socketPath: self.options.sock,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      run: self.options.run,
      mod: self.name,
      mod_gen: self.gen,
      backend_session_id: self.sid,
      report_id: crypto.randomUUID(),
      body: { kind: 'session_end', reason, ended_session_id: ended },
    }),
  })
  if (response.status !== 200) throw new Error(`/report: HTTP ${response.status}`)
  return JSON.parse(response.text)
}

export async function onSessionEnd($: any, e: any, next: any) {
  if (self.sid !== '') {
    try {
      await report($, String(e?.reason ?? ''), String(e?.sessionId ?? ''))
    } catch (error) {
      // 守护进程不在：结束不等它。
    }
  }
  return next(e)
}
