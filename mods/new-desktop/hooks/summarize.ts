// 钩子 mod 的总结能力：「从这里总结」「总结到这里」的压缩钩子。
// 只处理参数以 ND_SUM 开头、主对话（没有 agentId）的那次压缩；别的压缩原样交下游。
// 按所选提示文字的散列和次序定位，只把所选范围交给摘要器，范围外的原行（带着引擎的 handle）原样拼回。
// 定位不到就不压缩（skip），原因以 nd-anchor-gone: 开头；钩子出错也 skip，不落回整段压缩。
import { ANCHOR_GONE, SUMMARIZE_FAILED, SUMMARIZE_PREFIX } from './proto.ts'
import type { SummarizeSpec } from './proto.ts'

async function sha256(text: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text))
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('')
}

/** 所选提示在对话里的下标；定位不到时回原因。 */
async function locate(rows: any[], spec: SummarizeSpec): Promise<number | string> {
  const hits: number[] = []
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i]
    if (row?.role === 'user' && typeof row.text === 'string' && (await sha256(row.text)) === spec.sha256) {
      hits.push(i)
    }
  }
  if (hits.length === 0) return '对话里找不到这条提示（可能已被总结）'
  if (hits.length !== spec.of || spec.nth < 1 || spec.nth > hits.length) {
    return `同一原文在对话里出现 ${hits.length} 次，与预期的 ${spec.of} 次对不上`
  }
  return hits[spec.nth - 1] as number
}

export async function onSessionCompact($: any, e: any, next: any) {
  const instructions = typeof e?.instructions === 'string' ? e.instructions : ''
  if (!instructions.startsWith(SUMMARIZE_PREFIX) || e.agentId !== undefined) return next(e)
  let spec: SummarizeSpec
  try {
    spec = JSON.parse(instructions.slice(SUMMARIZE_PREFIX.length))
  } catch (error) {
    return { skip: `${ANCHOR_GONE} 总结参数无法解析` }
  }
  const rows: any[] = Array.isArray(e.messages) ? e.messages : []
  const at = await locate(rows, spec)
  if (typeof at === 'string') return { skip: `${ANCHOR_GONE} ${at}` }
  const part = spec.scope === 'from' ? rows.slice(at) : rows.slice(0, at)
  if (part.length === 0) return { skip: `${ANCHOR_GONE} 所选范围里没有可总结的内容` }
  const result = await next({ ...e, instructions: undefined, messages: part })
  if (!result?.messages) return result
  const messages =
    spec.scope === 'from'
      ? [...rows.slice(0, at), ...result.messages]
      : [...result.messages, ...rows.slice(at)]
  return { messages }
}

/** 钩子出错或超时：不压缩。不挂这个处理器的话，引擎会落回整段压缩。 */
export async function onSessionCompactError($: any, e: any, next: any) {
  return { skip: `${SUMMARIZE_FAILED} ${String(next?.error?.message ?? '压缩钩子出错')}` }
}
