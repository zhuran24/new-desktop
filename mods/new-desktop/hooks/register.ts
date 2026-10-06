// 钩子 mod 入口：只登记钩子，各能力在自己的文件里，共享状态在 state.ts。
import { onClassicSessionStart, onSessionStart } from './channel.ts'
import { onSessionEnd } from './lifecycle.ts'
import { configure } from './state.ts'

export function register(on: any, options: any) {
  configure(options)
  on('session.start', onSessionStart)
  on('classic.SessionStart', onClassicSessionStart)
  on('session.end', onSessionEnd)
}
