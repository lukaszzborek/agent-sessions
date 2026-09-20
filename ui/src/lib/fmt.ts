export const fmtNum = (n: number) =>
  n >= 1e6 ? (n / 1e6).toFixed(1) + 'M' : n >= 1e3 ? (n / 1e3).toFixed(n >= 1e4 ? 0 : 1) + 'k' : String(n)
export const fmtCost = (c?: number) => (c == null ? '' : '$' + c.toFixed(c >= 1 ? 2 : 3))
export const fmtDate = (s?: string) => (s ? new Date(s).toLocaleString(undefined, { dateStyle: 'short', timeStyle: 'short' }) : '')
export const fmtTime = (s?: string) => (s ? new Date(s).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' }) : '')
export function fmtDur(a?: string, b?: string) {
  if (!a || !b) return ''
  let s = Math.max(0, (new Date(b).getTime() - new Date(a).getTime()) / 1000)
  if (s < 60) return Math.round(s) + 's'
  if (s < 3600) return Math.round(s / 60) + 'm'
  return (s / 3600).toFixed(1) + 'h'
}
export const project = (cwd: string) => cwd.split(/[/\\]/).filter(Boolean).slice(-1)[0] ?? cwd
export const totalTokens = (u: { input: number; output: number; cache_read: number; cache_write: number }) =>
  u.input + u.output + u.cache_read + u.cache_write
// shorten hook command for display: drop dir prefix of first token, collapse quotes
export const shortHook = (h: string) => h.replace(/^(?:\S*\/)?([^\s/]+)/, '$1').replace(/["']/g, '').slice(0, 60)
// list pills: source/task/trial tags are long and shared by every trial of a task - filter/search on them instead
export const shortTags = (s: { tags: string[]; source: string }) => s.tags.filter((x) => x !== s.source && !x.startsWith('task:') && !x.startsWith('trial:'))
export const fmtMs = (ms: number) => (ms < 1000 ? `${Math.round(ms)}ms` : ms < 60000 ? `${(ms / 1000).toFixed(1)}s` : `${Math.round(ms / 60000)}m`)
export const fmtSecs = (s: number) => fmtMs(s * 1000)
export const fmtBytes = (b: number) => (b < 1024 ? `${b}B` : b < 1048576 ? `${(b / 1024).toFixed(1)}kB` : `${(b / 1048576).toFixed(1)}MB`)
