// 动作 mod 入口：只挂 session.start，在那里起控制循环。
import { onSessionStart } from './channel.ts'
import { configure } from './state.ts'

export function register(on: any, options: any) {
  configure(options)
  on('session.start', onSessionStart)
}
