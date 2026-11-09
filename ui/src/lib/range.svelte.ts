import type { Agent } from './types'

export type Preset = 'today' | '7d' | '30d' | '90d' | 'mtd' | 'prev' | 'all' | 'claude-wk' | 'claude-prev-wk' | 'codex-wk' | 'codex-prev-wk' | 'custom'

// shared by Stats + SessionStats views; yyyy-mm-dd local time, inclusive, '' = unbounded.
// Limit-window presets also narrow to one agent and [start, end) in ms (0 = unset).
export const range = $state({ preset: '30d' as Preset, from: '', to: '', start: 0, end: 0, agent: '' as Agent | '' })

export interface WeekReset { weekday: number; hour: number; minute: number }
// Claude's weekly reset is fixed per account but not logged, so it comes from config.
// A Codex window starts on first use after the previous one expired; `codexResets` (ms, newest first)
// are the ends its logs reported.
export const limits = $state({ claudeWeek: null as WeekReset | null, codexResets: [] as number[] })

const WEEK = 7 * 864e5
const iso = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
// UTC hour key as in `Bucket.hour`; rounded since Codex resets land at arbitrary minutes
export const hourKey = (ms: number) => new Date(Math.round(ms / 36e5) * 36e5).toISOString().slice(0, 13)

function claudeWindow(prev: boolean): [number, number] | null {
  const w = limits.claudeWeek
  if (!w) return null
  // local-date arithmetic keeps the reset at its wall-clock time across DST
  const end = new Date()
  end.setHours(w.hour, w.minute, 0, 0)
  end.setDate(end.getDate() + ((w.weekday - end.getDay() + 7) % 7))
  if (end.getTime() <= Date.now()) end.setDate(end.getDate() + 7)
  if (prev) end.setDate(end.getDate() - 7)
  const start = new Date(end)
  start.setDate(start.getDate() - 7)
  return [start.getTime(), end.getTime()]
}

function codexWindow(prev: boolean): [number, number] | null {
  const [last] = limits.codexResets
  if (last === undefined) return null
  const now = Date.now()
  // expired: nothing runs until the next use starts a window
  if (last <= now) return prev ? [last - WEEK, last] : [last, now]
  if (!prev) return [last - WEEK, last]
  // resets reported within one window drift by minutes
  const end = limits.codexResets.find((r) => r <= last - WEEK + 36e5) ?? last - WEEK
  return [end - WEEK, end]
}

export function applyPreset(p: Preset) {
  range.preset = p
  const agent = p.startsWith('claude') ? 'claude' : p.startsWith('codex') ? 'codex' : ''
  const w = agent === 'claude' ? claudeWindow(p === 'claude-prev-wk') : agent === 'codex' ? codexWindow(p === 'codex-prev-wk') : null
  // assigned once each: a transient reset would re-run every view on the minute re-resolve
  range.agent = agent
  range.start = w?.[0] ?? 0
  range.end = w?.[1] ?? 0
  const now = new Date()
  const y = now.getFullYear(), m = now.getMonth()
  const ago = (n: number) => iso(new Date(y, m, now.getDate() - n + 1))
  if (p === 'all') { range.from = ''; range.to = '' }
  else if (p === 'today') { range.from = iso(now); range.to = iso(now) }
  else if (p === '7d') { range.from = ago(7); range.to = iso(now) }
  else if (p === '30d') { range.from = ago(30); range.to = iso(now) }
  else if (p === '90d') { range.from = ago(90); range.to = iso(now) }
  else if (p === 'mtd') { range.from = iso(new Date(y, m, 1)); range.to = iso(now) }
  else if (p === 'prev') { range.from = iso(new Date(y, m - 1, 1)); range.to = iso(new Date(y, m, 0)) }
  else if (w) { range.from = iso(new Date(w[0])); range.to = iso(new Date(w[1] - 1)) }
}
applyPreset(range.preset)

export function setLimits(l: Partial<typeof limits>) {
  Object.assign(limits, l)
  if (range.agent) applyPreset(range.preset)
}

// presets are relative to now: re-resolve them once the date rolls over, limit windows every tick
// since one can end at any minute (unchanged values don't trigger updates)
let day = iso(new Date())
setInterval(() => {
  const d = iso(new Date())
  if (d === day && !range.agent) return
  day = d
  if (range.preset !== 'custom') applyPreset(range.preset)
}, 60_000)
