// 动作 mod 的状态：身份、绑定和按操作 id 留下的结果。这里不碰 `$`。
import { ACTION_MOD } from './proto.ts'
import type { Command, ModName, OpState, Outcome, PluginOptions, Rejection } from './proto.ts'

/** 本模块的身份与绑定。重载（含热重载、worker 重生）时模块变量清零，代次重新生成。 */
export const self = {
  name: ACTION_MOD as ModName,
  options: { sock: '', run: '' } as PluginOptions,
  gen: crypto.randomUUID(),
  /** 最近一次 hello 成功时报的后端会话 id；空表示还没报到。 */
  sid: '',
  epoch: 0,
  polling: false,
}

/** 本代次已结束的操作，按操作 id 留着，供 query 和重连后补报。 */
const results = new Map<string, Outcome>()
/** 正在执行的操作。 */
const running = new Set<string>()
/** 结果 POST 失败、等通道恢复后补报的操作。 */
export const unsent = new Map<string, Outcome>()
const KEEP = 256

export function configure(options: Partial<PluginOptions> | undefined) {
  self.options = { sock: String(options?.sock ?? ''), run: String(options?.run ?? '') }
}

export function begin(opId: string) {
  running.add(opId)
}

export function finish(opId: string, outcome: Outcome) {
  running.delete(opId)
  results.delete(opId)
  results.set(opId, outcome)
  while (results.size > KEEP) {
    const oldest = results.keys().next().value
    if (oldest === undefined) break
    results.delete(oldest)
  }
}

export function answer(opIds: string[]): OpState[] {
  return opIds.map((op_id) => {
    const outcome = results.get(op_id)
    if (outcome) return { op_id, phase: 'done', outcome }
    return { op_id, phase: running.has(op_id) ? 'running' : 'unknown' }
  })
}

/** 先核后端会话 id，再核 mod 代次；对不上就不执行。 */
export function stale(command: Command, current: string): Rejection | undefined {
  if (command.expected_backend_session_id !== current) return { code: 'stale_session', current }
  if (command.expected_mod_gen !== self.gen) return { code: 'stale_gen', current: self.gen }
  return undefined
}

export function nextUrl(sid: string): string {
  const query = new URLSearchParams({
    run: self.options.run,
    mod: self.name,
    mod_gen: self.gen,
    backend_session_id: sid,
  })
  return 'http://nd/next?' + query.toString()
}
