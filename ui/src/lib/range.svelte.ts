import type { Session } from './types'

export type Preset = '7d' | '30d' | '90d' | 'mtd' | 'prev' | 'all' | 'custom'

// shared by Stats + SessionStats views; yyyy-mm-dd local time, inclusive, '' = unbounded
export const range = $state({ preset: '30d' as Preset, from: '', to: '' })

const iso = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`

export function applyPreset(p: Preset) {
  range.preset = p
  const now = new Date()
  const y = now.getFullYear(), m = now.getMonth()
  const ago = (n: number) => iso(new Date(y, m, now.getDate() - n + 1))
  if (p === 'all') { range.from = ''; range.to = '' }
  else if (p === '7d') { range.from = ago(7); range.to = iso(now) }
  else if (p === '30d') { range.from = ago(30); range.to = iso(now) }
  else if (p === '90d') { range.from = ago(90); range.to = iso(now) }
  else if (p === 'mtd') { range.from = iso(new Date(y, m, 1)); range.to = iso(now) }
  else if (p === 'prev') { range.from = iso(new Date(y, m - 1, 1)); range.to = iso(new Date(y, m, 0)) }
}
applyPreset(range.preset)

export function inRange(sessions: Session[]): Session[] {
  const lo = range.from ? new Date(range.from + 'T00:00').getTime() : -Infinity
  const hi = range.to ? new Date(range.to + 'T00:00').getTime() + 864e5 : Infinity
  if (lo === -Infinity && hi === Infinity) return sessions
  return sessions.filter((s) => {
    if (!s.started) return false
    const t = new Date(s.started).getTime()
    return t >= lo && t < hi
  })
}
