import { fmtCost, fmtNum } from './fmt'

export type Metric = 'cost' | 'out' | 'tokens' | 'tools'
export const METRICS: [Metric, string][] = [['cost', 'cost'], ['out', 'out tok'], ['tokens', 'total tok'], ['tools', 'tool calls']]

export interface Cell { tools: number; input: number; cache: number; output: number; cost: number }
export const zero = (): Cell => ({ tools: 0, input: 0, cache: 0, output: 0, cost: 0 })
export const add = (a: Cell, b: Cell) => { a.tools += b.tools; a.input += b.input; a.cache += b.cache; a.output += b.output; a.cost += b.cost }
export const valOf: Record<Metric, (c: Cell) => number> = {
  cost: (c) => c.cost,
  out: (c) => c.output,
  tokens: (c) => c.input + c.cache + c.output,
  tools: (c) => c.tools,
}
export const fmtVal = (metric: Metric, v: number) => (metric === 'cost' ? fmtCost(v) : fmtNum(Math.round(v)))

export const PALETTE = ['#3987e5', '#d95926', '#199e70', '#c98500', '#d55181', '#9085e9', '#e66767']
export const OTHER = '#898781'
// shared by every view and kept for the page's lifetime, so a model has one color everywhere and never gets repainted
const modelColors = new Map<string, string>()
export function modelColor(m: string) {
  let c = modelColors.get(m)
  if (!c && modelColors.size < PALETTE.length) modelColors.set(m, (c = PALETTE[modelColors.size]))
  return c ?? OTHER
}
export const shortModel = (m: string) => m.replace(/^claude-/, '')
