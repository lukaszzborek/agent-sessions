<script lang="ts">
  import type { Session } from './types'
  import { fmtDate, fmtSecs, fmtNum, fmtCost, totalTokens } from './fmt'

  // tag: preselected from the route (#/compare/<tag>); groups = sessions split by another tag family (arm:x by default)
  let { sessions, tag = '' }: { sessions: Session[]; tag?: string } = $props()

  let sel = $state('')
  let groupBy = $state('arm')
  $effect(() => { sel = tag })

  const allTags = $derived(
    [...new Set(sessions.flatMap((s) => s.tags))].filter((t) => !t.startsWith('trial:')).sort((a, b) => (b.startsWith('task:') ? 1 : 0) - (a.startsWith('task:') ? 1 : 0) || a.localeCompare(b)),
  )
  const roots = $derived(sessions.filter((s) => !s.parent_id && (!sel || s.tags.includes(sel))))
  // tag families present on the selected sessions ("arm", "rep", "grade", …), for the group-by select
  const families = $derived([...new Set(roots.flatMap((s) => s.tags.filter((t) => t.includes(':') && !t.startsWith('trial:')).map((t) => t.split(':')[0])))].sort())
  // `arm` is only a preferred default: fall back to a family the selection actually has
  $effect(() => { if (families.length && !families.includes(groupBy)) groupBy = families[0] })
  const groupOf = (s: Session) => s.tags.find((t) => t.startsWith(groupBy + ':'))?.slice(groupBy.length + 1) ?? '(none)'
  const tagVal = (s: Session, fam: string) => s.tags.find((t) => t.startsWith(fam + ':'))?.slice(fam.length + 1) ?? ''

  // own + all descendant subagents (claude lists every descendant in root.children, so dedupe by seen set)
  const byKey = $derived(new Map(sessions.map((s) => [s.agent + s.id, s])))
  interface Row { s: Session; group: string; rep: string; grade: string; n: number; tools: number; input: number; cacheR: number; cacheW: number; output: number; tok: number; cost: number; msgs: number; dur: number }
  const rows = $derived.by<Row[]>(() =>
    roots
      .map((root) => {
        const seen = new Set([root.agent + root.id])
        const q = [root]
        const r: Row = { s: root, group: groupOf(root), rep: tagVal(root, 'rep'), grade: tagVal(root, 'grade'), n: -1, tools: 0, input: 0, cacheR: 0, cacheW: 0, output: 0, tok: 0, cost: 0, msgs: root.assistant_msgs, dur: 0 }
        while (q.length) {
          const s = q.pop()!
          r.n++; r.tools += s.tool_calls; r.input += s.usage.input; r.cacheR += s.usage.cache_read; r.cacheW += s.usage.cache_write
          r.output += s.usage.output; r.tok += totalTokens(s.usage); r.cost += s.cost ?? 0
          for (const cid of s.children) {
            const c = byKey.get(s.agent + cid)
            if (c && !seen.has(c.agent + c.id)) { seen.add(c.agent + c.id); q.push(c) }
          }
        }
        if (root.started && root.ended) r.dur = (new Date(root.ended).getTime() - new Date(root.started).getTime()) / 1000
        return r
      })
      .sort((a, b) => a.group.localeCompare(b.group) || a.rep.localeCompare(b.rep, undefined, { numeric: true }) || (a.s.started ?? '').localeCompare(b.s.started ?? '')),
  )

  const median = (xs: number[]) => {
    if (!xs.length) return 0
    const v = [...xs].sort((a, b) => a - b)
    const m = v.length >> 1
    return v.length % 2 ? v[m] : (v[m - 1] + v[m]) / 2
  }
  type Num = 'cost' | 'tok' | 'input' | 'cacheR' | 'cacheW' | 'output' | 'tools' | 'msgs' | 'dur'
  const COLS: [Num, string, (v: number) => string][] = [
    ['cost', 'cost', fmtCost], ['tok', 'Σ tok', fmtNum], ['input', 'in', fmtNum], ['cacheR', 'cache↓', fmtNum], ['cacheW', 'cache↑', fmtNum],
    ['output', 'out', fmtNum], ['tools', 'tools', String], ['msgs', 'turns', String], ['dur', 'wall', fmtSecs],
  ]
  // per group: n, pass count, median of every numeric column
  const groups = $derived.by(() => {
    const m = new Map<string, Row[]>()
    for (const r of rows) m.set(r.group, [...(m.get(r.group) ?? []), r])
    return [...m].map(([g, rs]) => ({
      g, n: rs.length, pass: rs.filter((r) => r.grade === 'pass').length, graded: rs.filter((r) => r.grade).length,
      med: Object.fromEntries(COLS.map(([k]) => [k, median(rs.map((r) => r[k]))])) as Record<Num, number>,
    }))
  })
  // delta vs the first group (baseline, e.g. control)
  const delta = (v: number, base: number) => (!base || v === base ? '' : `${v > base ? '+' : ''}${Math.round(((v - base) / base) * 100)}%`)
</script>

<div class="wrap">
  <div class="row">
    <h2>Compare</h2>
    <select bind:value={sel}>
      <option value="">all sessions</option>
      {#each allTags as t}<option value={t}>{t}</option>{/each}
    </select>
    <span class="dim">group by</span>
    <select bind:value={groupBy}>
      {#each families as f}<option value={f}>{f}</option>{/each}
      {#if !families.includes(groupBy)}<option value={groupBy}>{groupBy}</option>{/if}
    </select>
    <span class="dim">{rows.length} sessions · subagents rolled in</span>
  </div>
  {#if !rows.length}
    <div class="dim note">no sessions with this tag. Tags come from a source's <span class="mono">meta</span> file (<span class="mono">{'{"tags": ["arm:x", "grade:pass"]}'}</span>) or the session header.</div>
  {:else}
    <h3>Medians per {groupBy}{#if groups.length > 1} <span class="dim2">(Δ vs {groups[0].g})</span>{/if}</h3>
    <table>
      <thead><tr><th>{groupBy}</th><th>n</th><th>pass</th>{#each COLS as [, l]}<th>{l}</th>{/each}</tr></thead>
      <tbody>
        {#each groups as g, i}
          <tr>
            <td class="mono">{g.g}</td><td>{g.n}</td><td>{g.graded ? `${g.pass}/${g.graded}` : ''}</td>
            {#each COLS as [k, , f]}
              <td>{f(g.med[k])}{#if i}<span class="d" class:up={g.med[k] > groups[0].med[k]} class:down={g.med[k] < groups[0].med[k]}>{delta(g.med[k], groups[0].med[k])}</span>{/if}</td>
            {/each}
          </tr>
        {/each}
      </tbody>
    </table>

    <h3>Sessions</h3>
    <table>
      <thead><tr><th>{groupBy}</th><th>rep</th><th>grade</th><th>started</th><th>sub</th>{#each COLS as [, l]}<th>{l}</th>{/each}<th>title</th></tr></thead>
      <tbody>
        {#each rows as r (r.s.agent + r.s.id)}
          <tr>
            <td class="mono">{r.group}</td><td>{r.rep}</td>
            <td class:pass={r.grade === 'pass'} class:fail={r.grade === 'fail'}>{r.grade}</td>
            <td class="dim">{fmtDate(r.s.started)}</td><td>{r.n || ''}</td>
            {#each COLS as [k, , f]}<td>{f(r[k])}</td>{/each}
            <td class="small"><a href="#/s/{r.s.agent}/{encodeURIComponent(r.s.id)}">{r.s.title}</a></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .wrap { padding: 16px; }
  h2 { margin: 0; font-size: 15px; }
  h3 { font-size: 13px; margin: 24px 0 8px; color: var(--fg2); }
  .dim2 { color: var(--fg3); font-weight: normal; }
  .note { margin-top: 16px; }
  table { border-collapse: collapse; margin-top: 8px; font-size: 12px; white-space: nowrap; }
  th { text-align: right; color: var(--fg2); font-weight: 500; padding: 4px 8px; border-bottom: 1px solid var(--line); }
  td { text-align: right; padding: 4px 8px; border-bottom: 1px solid var(--line); font-variant-numeric: tabular-nums; }
  th:first-child, td:first-child, td.small, th:last-child { text-align: left; }
  .small { font-size: 11px; max-width: 420px; overflow: hidden; text-overflow: ellipsis; }
  .small a { text-decoration: none; color: var(--fg2); }
  .small a:hover { color: var(--fg); }
  .pass { color: var(--result); } .fail { color: var(--err); }
  .d { margin-left: 4px; font-size: 10px; color: var(--fg3); }
  .d.up { color: var(--err); } .d.down { color: var(--result); }
</style>
