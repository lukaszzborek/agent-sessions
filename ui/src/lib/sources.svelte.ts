// source filter shared by the list, stats and compare; only `user` until other sources are switched on
const KEY = 'sources'
function load(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? 'null')
    if (Array.isArray(v)) return v
  } catch {}
  return ['user']
}
export const sourceFilter = $state({ on: load() })
export function toggleSource(name: string) {
  sourceFilter.on = sourceFilter.on.includes(name) ? sourceFilter.on.filter((x) => x !== name) : [...sourceFilter.on, name]
  try { localStorage.setItem(KEY, JSON.stringify(sourceFilter.on)) } catch {}
}
