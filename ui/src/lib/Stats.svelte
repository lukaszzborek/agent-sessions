<script lang="ts">
  import type { Session } from './types'
  import { fmtNum, fmtCost, project, shortHook } from './fmt'
  import { range } from './range.svelte'
  import RangePicker from './RangePicker.svelte'

  // scoped: single session (+ its subagents) - no date range
  let { sessions, scoped = false }: { sessions: Session[]; scoped?: boolean } = $props()

  type Dim = 'model' | 'agent' | 'subagent' | 'project'
  const DIMS: Dim[] = ['model', 'agent', 'subagent', 'project']
  type Metric = 'cost' | 'out' | 'tokens' | 'tools'
  const METRICS: [Metric, string][] = [['cost', 'cost'], ['out', 'out tok'], ['tokens', 'total tok'], ['tools', 'tool calls']]

  let dim = $state<Dim>('model')
  let metric = $state<Metric>('cost')
  let selDay = $state<string | null>(null)
  let hoverDay = $state<string | null>(null)

  // one row per session × local day × model
  interface Row { day: string; s: Session; model: string; tools: number; input: number; cache: number; output: number; cost: number }
  const iso = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
  const dayCache = new Map<string, string>()
  function localDay(hour: string) {
    let d = dayCache.get(hour)
    if (!d) dayCache.set(hour, (d = iso(new Date(hour + ':00:00Z'))))
    return d
  }

  const rows = $derived.by(() => {
    const out: Row[] = []
    for (const s of sessions) for (const b of s.buckets ?? []) {
      const day = localDay(b.hour)
      if (!scoped && ((range.from && day < range.from) || (range.to && day > range.to))) continue
      out.push({ day, s, model: b.model, tools: b.tool_calls, input: b.usage.input, cache: b.usage.cache_read + b.usage.cache_write, output: b.usage.output, cost: b.cost })
    }
    return out
  })

  const keyOf: Record<Dim, (r: Row) => string> = {
    model: (r) => r.model,
    agent: (r) => r.s.agent,
    subagent: (r) => r.s.agent_type ?? (r.s.parent_id ? '?' : '(main)'),
    project: (r) => project(r.s.cwd),
  }
  const valOf: Record<Metric, (r: { tools: number; input: number; cache: number; output: number; cost: number }) => number> = {
    cost: (r) => r.cost,
    out: (r) => r.output,
    tokens: (r) => r.input + r.cache + r.output,
    tools: (r) => r.tools,
  }
  const fmtVal = (v: number) => (metric === 'cost' ? fmtCost(v) : fmtNum(Math.round(v)))

  interface Agg { key: string; ids: Set<string>; subIds: Set<string>; tools: number; input: number; cache: number; output: number; cost: number }
  function agg(list: Row[], key: (r: Row) => string): Agg[] {
    const m = new Map<string, Agg>()
    for (const r of list) {
      const k = key(r)
      let a = m.get(k)
      if (!a) m.set(k, (a = { key: k, ids: new Set(), subIds: new Set(), tools: 0, input: 0, cache: 0, output: 0, cost: 0 }))
      ;(r.s.parent_id ? a.subIds : a.ids).add(r.s.id)
      a.tools += r.tools; a.input += r.input; a.cache += r.cache; a.output += r.output; a.cost += r.cost
    }
    return [...m.values()]
  }

  // a key keeps the first color it got, so filters and metric switches never repaint it
  const PALETTE = ['#3987e5', '#d95926', '#199e70', '#c98500', '#d55181', '#9085e9', '#e66767']
  const OTHER = '#898781'
  const assigned: Partial<Record<Dim, Map<string, string>>> = {}
  const series = $derived(agg(rows, keyOf[dim]).sort((a, b) => valOf[metric](b) - valOf[metric](a)))
  const colorOf = $derived.by(() => {
    const m = (assigned[dim] ??= new Map())
    for (const a of series) if (!m.has(a.key) && m.size < PALETTE.length) m.set(a.key, PALETTE[m.size])
    return new Map(series.map((a) => [a.key, m.get(a.key) ?? OTHER]))
  })
  const legend = $derived.by(() => {
    const named = series.filter((a) => colorOf.get(a.key) !== OTHER).map((a) => a.key)
    return named.length < series.length ? [...named, 'other'] : named
  })

  // every calendar day between first and last, so idle days show as gaps
  const days = $derived.by(() => {
    const byDay = new Map<string, Row[]>()
    for (const r of rows) {
      let list = byDay.get(r.day)
      if (!list) byDay.set(r.day, (list = []))
      list.push(r)
    }
    const keys = [...byDay.keys()].sort()
    if (!keys.length) return []
    const out: { day: string; total: number; segs: { key: string; v: number }[]; agg?: Agg }[] = []
    for (let d = new Date(keys[0] + 'T00:00'); iso(d) <= keys[keys.length - 1]; d.setDate(d.getDate() + 1)) {
      const day = iso(d), list = byDay.get(day) ?? []
      const segs = agg(list, keyOf[dim]).map((a) => ({ key: a.key, v: valOf[metric](a) })).filter((x) => x.v > 0)
        .sort((a, b) => series.findIndex((x) => x.key === a.key) - series.findIndex((x) => x.key === b.key))
      out.push({ day, total: segs.reduce((a, x) => a + x.v, 0), segs, agg: agg(list, () => day)[0] })
    }
    return out
  })
  const maxDay = $derived(Math.max(1e-9, ...days.map((d) => d.total)))
  const tip = $derived(days.find((d) => d.day === hoverDay))
  const tipPos = $derived((100 * (days.findIndex((d) => d.day === hoverDay) + 0.5)) / Math.max(1, days.length))

  $effect(() => { if (selDay && !days.some((d) => d.day === selDay)) selDay = null })
  const focus = $derived(selDay ? rows.filter((r) => r.day === selDay) : rows)
  const table = $derived(agg(focus, keyOf[dim]).sort((a, b) => valOf[metric](b) - valOf[metric](a)))
  const maxRow = $derived(Math.max(1e-9, ...table.map(valOf[metric])))
  const total = $derived(agg(focus, () => 'total')[0])
  const dayRows = $derived(days.filter((d) => d.agg).reverse())
  const fmtDay = (d: string) => new Date(d + 'T00:00').toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' })

  const focusSessions = $derived([...new Set(focus.map((r) => r.s))])
  // name -> [calls, sessions]
  function usage(pick: (s: Session) => [string, number][]) {
    const m = new Map<string, [number, number]>()
    for (const s of focusSessions) for (const [n, c] of pick(s)) {
      const e = m.get(n) ?? [0, 0]
      e[0] += c; e[1]++
      m.set(n, e)
    }
    return [...m].sort((a, b) => b[1][0] - a[1][0])
  }
  const lists = $derived<[string, string, [string, [number, number]][]][]>([
    ['Tool calls', '', usage((s) => s.tools).slice(0, 30)],
    ['Subagents', 'subagent types spawned (Agent / Task / spawn_agent calls)', usage((s) => s.subagents ?? [])],
    ['Shell commands', 'programs invoked inside Bash / exec tool calls (git commit, cargo build, grep, …)', usage((s) => s.cmds ?? []).slice(0, 40)],
    ['Skills', 'Skill tool calls + slash commands (Claude only)', usage((s) => s.skills)],
    ['Hooks', 'hook executions by command (Claude only)', usage((s) => s.hooks)],
  ])
</script>

{#snippet cols(r: Agg, max: number)}
  <td>{r.ids.size || ''}</td><td>{r.subIds.size || ''}</td>
  <td>{r.tools || ''}</td>
  <td>{fmtNum(r.input)}</td><td>{fmtNum(r.cache)}</td><td>{fmtNum(r.output)}</td>
  <td>{r.cost ? fmtCost(r.cost) : ''}</td>
  <td class="share"><div style="width:{(100 * valOf[metric](r)) / max}%;background:{colorOf.get(r.key) ?? 'var(--fg3)'}"></div></td>
{/snippet}

{#snippet head(label: string)}
  <thead><tr>
    <th>{label}</th>
    <th title="top-level sessions active">sessions</th>
    <th title="subagent sessions active">subagents</th>
    <th title="tool calls">tools</th>
    <th title="input tokens, uncached (billed full price)">in</th>
    <th title="cache read + cache write tokens">cache</th>
    <th title="output tokens (incl. thinking)">out</th>
    <th title="API list price equivalent (subscription plans bill differently)">cost</th>
    <th>{METRICS.find((m) => m[0] === metric)?.[1]}</th>
  </tr></thead>
{/snippet}

<div class="wrap" class:scoped>
  <div class="row">
    {#if !scoped}<h2>Stats</h2><RangePicker />{/if}
    <select bind:value={metric}>{#each METRICS as [m, l]}<option value={m}>{l}</option>{/each}</select>
    <span class="dim">by</span>
    <select bind:value={dim}>{#each DIMS as d}<option value={d}>{d}</option>{/each}</select>
    {#if selDay}<button class="chip on" onclick={() => (selDay = null)}>{fmtDay(selDay)} ✕</button>{/if}
  </div>

  {#if total}
    <div class="tiles">
      <div><b>{fmtCost(total.cost)}</b><span>cost</span></div>
      <div><b>{total.ids.size}</b><span>sessions</span></div>
      <div><b>{total.subIds.size}</b><span>subagents</span></div>
      <div><b>{fmtNum(total.tools)}</b><span>tool calls</span></div>
      <div><b>{fmtNum(total.output)}</b><span>out tok</span></div>
      <div><b>{fmtNum(total.input + total.cache + total.output)}</b><span>total tok</span></div>
    </div>
  {:else}
    <p class="dim">no usage in this range</p>
  {/if}

  {#if days.length > 1}
    <div class="legend">
      {#each legend as k}<span><i style="background:{colorOf.get(k) ?? OTHER}"></i>{k}</span>{/each}
    </div>
    <div class="chart" role="img" aria-label="{metric} per day by {dim}">
      <div class="grid">
        {#each [1, 0.5] as f}<div style="bottom:{f * 100}%"><span>{fmtVal(maxDay * f)}</span></div>{/each}
      </div>
      <div class="bars" onmouseleave={() => (hoverDay = null)} role="presentation">
        {#each days as d (d.day)}
          <button class="col" class:sel={selDay === d.day} class:fade={selDay && selDay !== d.day} aria-label={d.day}
            onmouseenter={() => (hoverDay = d.day)} onclick={() => (selDay = selDay === d.day ? null : d.day)}>
            {#each d.segs as g}<i style="height:{(100 * g.v) / maxDay}%;background:{colorOf.get(g.key) ?? OTHER}"></i>{/each}
          </button>
        {/each}
      </div>
      {#if tip}
        <div class="tip" style="left:clamp(90px, {tipPos}%, calc(100% - 90px))">
          <b>{fmtDay(tip.day)}</b><b class="r">{fmtVal(tip.total)}</b>
          {#each tip.segs as g}
            <span><i style="background:{colorOf.get(g.key) ?? OTHER}"></i>{g.key}</span><span class="r">{fmtVal(g.v)}</span>
          {/each}
        </div>
      {/if}
      <div class="axis"><span>{fmtDay(days[0].day)}</span><span>{fmtDay(days[days.length - 1].day)}</span></div>
    </div>
  {/if}

  {#if total}
    <h3>By {dim}{selDay ? ` · ${fmtDay(selDay)}` : ''}</h3>
    <table>
      {@render head(dim)}
      <tbody>
        {#each table as r}<tr><td class="mono"><i class="dot" style="background:{colorOf.get(r.key) ?? OTHER}"></i>{r.key}</td>{@render cols(r, maxRow)}</tr>{/each}
      </tbody>
    </table>
    {#if dim === 'model'}<div class="note dim">tokens and cost are attributed per API call, so a session that switched models is split between them.</div>{/if}
  {/if}

  {#if dayRows.length > 1}
    <h3>By day</h3>
    <table>
      {@render head('day')}
      <tbody>
        {#each dayRows as d}
          <tr class="clickable" class:selrow={selDay === d.day} onclick={() => (selDay = selDay === d.day ? null : d.day)}>
            <td class="mono">{fmtDay(d.day)}</td>{@render cols(d.agg!, maxDay)}
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#each lists as [title, hint, items]}
    <h3 title={hint}>{title}{selDay ? ` · sessions active ${fmtDay(selDay)}` : ''}</h3>
    <div class="tools">
      {#each items as [n, [c, ns]]}
        <div class="trow"><span class="mono" title={n}>{title === 'Hooks' ? shortHook(n) : n}</span><div class="bar" style="width:{(100 * c) / items[0][1][0]}%"></div><span class="dim">{c} <span class="dim2">in {ns} sessions</span></span></div>
      {/each}
      {#if !items.length}<span class="dim">none</span>{/if}
    </div>
  {/each}
</div>

<style>
  .wrap { padding: 16px; }
  .wrap.scoped { padding: 12px 16px 40px; }
  h2 { margin: 0; font-size: 15px; }
  h3 { font-size: 13px; margin: 24px 0 8px; color: var(--fg2); }
  .tiles { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 12px; }
  .tiles div { background: var(--bg2); border: 1px solid var(--line); border-radius: 6px; padding: 8px 14px; min-width: 96px; display: flex; flex-direction: column; }
  .tiles b { font-size: 18px; font-variant-numeric: tabular-nums; }
  .tiles span { font-size: 11px; color: var(--fg2); }

  .legend { display: flex; flex-wrap: wrap; gap: 4px 14px; margin-top: 16px; font-size: 11px; color: var(--fg2); font-family: var(--mono); }
  .legend i, .tip i, .dot { display: inline-block; width: 8px; height: 8px; border-radius: 2px; margin-right: 6px; }
  .chart { position: relative; margin: 8px 0 0 52px; }
  .bars { display: flex; gap: 2px; height: 180px; border-bottom: 1px solid var(--line); position: relative; }
  .col { flex: 1; min-width: 0; max-width: 40px; display: flex; flex-direction: column-reverse; gap: 1px; padding: 0; border: 0; background: transparent; cursor: pointer; }
  .col:hover, .col.sel { background: color-mix(in srgb, var(--fg) 7%, transparent); }
  .col.fade i { opacity: .35; }
  .col i { display: block; width: 100%; min-height: 1px; }
  .col i:last-child { border-radius: 3px 3px 0 0; }
  .grid div { position: absolute; left: 0; right: 0; border-top: 1px dashed var(--line); }
  .grid span { position: absolute; right: 100%; top: -7px; margin-right: 6px; font-size: 10px; color: var(--fg3); white-space: nowrap; }
  .axis { display: flex; justify-content: space-between; font-size: 10px; color: var(--fg3); margin-top: 3px; }
  .tip { position: absolute; bottom: calc(100% - 24px); transform: translateX(-50%); z-index: 2; pointer-events: none; display: grid; grid-template-columns: auto auto; gap: 2px 14px;
    background: var(--bg3); border: 1px solid var(--line); border-radius: 6px; padding: 6px 10px; font-size: 11px; white-space: nowrap; box-shadow: 0 4px 14px #0008; }
  .tip span:not(.r) { font-family: var(--mono); color: var(--fg2); }
  .r { text-align: right; font-variant-numeric: tabular-nums; }

  table { border-collapse: collapse; width: 100%; font-size: 12px; }
  th { text-align: left; color: var(--fg2); font-weight: 500; padding: 4px 8px; border-bottom: 1px solid var(--line); }
  td { padding: 4px 8px; border-bottom: 1px solid var(--line); white-space: nowrap; font-variant-numeric: tabular-nums; }
  td:nth-child(n + 2):nth-child(-n + 8), th:nth-child(n + 2):nth-child(-n + 8) { text-align: right; }
  .share { width: 30%; min-width: 120px; }
  .share div { height: 8px; border-radius: 0 3px 3px 0; min-width: 1px; }
  tr.clickable { cursor: pointer; }
  tr.clickable:hover td, tr.selrow td { background: color-mix(in srgb, var(--fg) 5%, transparent); }
  .note { font-size: 11px; margin-top: 6px; }
  .chip { font: inherit; font-size: 11px; padding: 2px 8px; border: 1px solid var(--user); border-radius: 10px; background: color-mix(in srgb, var(--user) 25%, transparent); color: var(--fg); cursor: pointer; }
  .tools { display: grid; grid-template-columns: max-content 1fr max-content; gap: 3px 10px; align-items: center; max-width: 700px; font-size: 12px; }
  .trow { display: contents; }
  .tools .bar { height: 10px; background: var(--tool); opacity: .6; border-radius: 2px; }
  .dim2 { color: var(--fg3); font-size: 11px; }
</style>
