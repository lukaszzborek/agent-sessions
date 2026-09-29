export type Preset = 'today' | '7d' | '30d' | '90d' | 'mtd' | 'prev' | 'all' | 'custom'

// shared by Stats + SessionStats views; yyyy-mm-dd local time, inclusive, '' = unbounded
export const range = $state({ preset: '30d' as Preset, from: '', to: '' })

const iso = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`

export function applyPreset(p: Preset) {
  range.preset = p
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
}
applyPreset(range.preset)

// presets are relative to today: re-resolve them once the date rolls over
let day = iso(new Date())
setInterval(() => {
  const d = iso(new Date())
  if (d === day) return
  day = d
  if (range.preset !== 'custom') applyPreset(range.preset)
}, 60_000)
