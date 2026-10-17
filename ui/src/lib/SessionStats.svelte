<script lang="ts">
  import type { Session } from './types'
  import { fmtNum, fmtCost, fmtDur, fmtTime } from './fmt'
  import { type Cell, type Metric, METRICS, add, zero, valOf, fmtVal, modelColor, shortModel } from './metric'
  import ModelMatrix from './ModelMatrix.svelte'
  import UsageLists from './UsageLists.svelte'

  // sessions[0] is the viewed session, the rest its subagents
  let { sessions }: { sessions: Session[] } = $props()

  let metric = $state<Metric>('cost')
  let order = $state<'value' | 'time'>('value')

  const val = (c?: Cell) => (c ? valOf[metric](c) : 0)
  const show = (v: number) => (v ? fmtVal(metric, v) : '')
  const pct = (v: number, of: number) => (of ? `${Math.round((100 * v) / of)}%` : '')
  const typeOf = (s: Session) => (s.parent_id ? (s.agent_type ?? '?') : '(main)')

  // buckets already split each session's usage by model
  const slices = $derived(sessions.flatMap((s) => (s.buckets ?? []).map((b) => ({
    key: typeOf(s), id: s.id, model: b.model,
    tools: b.tool_calls, input: b.usage.input, cache: b.usage.cache_read + b.usage.cache_write, output: b.usage.output, cost: b.cost,
  }))))
  const rows = $derived(sessions.map((s) => {
    const byModel = new Map<string, Cell>(), total = zero()
    for (const x of slices) if (x.id === s.id) {
      let m = byModel.get(x.model)
      if (!m) byModel.set(x.model, (m = zero()))
      add(m, x); add(total, x)
    }
    return { s, key: typeOf(s), byModel, total }
  }))
  const sum = (cs: Cell[]) => cs.reduce((a, c) => (add(a, c), a), zero())
  const total = $derived(sum(rows.map((r) => r.total)))
  const main = $derived(rows[0]?.total ?? zero())
  const subs = $derived(rows.slice(1))
  const subTotal = $derived(sum(subs.map((r) => r.total)))
  const models = $derived.by(() => {
    const m = new Map<string, Cell>()
    for (const x of slices) {
      let c = m.get(x.model)
      if (!c) m.set(x.model, (c = zero()))
      add(c, x)
    }
    return [...m].map(([key, c]) => ({ key, c })).sort((a, b) => val(b.c) - val(a.c))
  })
  const instances = $derived(order === 'time' ? [...subs].sort((a, b) => (a.s.started ?? '').localeCompare(b.s.started ?? '')) : [...subs].sort((a, b) => val(b.total) - val(a.total)))
  const maxInst = $derived(Math.max(1e-9, ...subs.map((r) => val(r.total))))
  const tipOf = (byModel: Map<string, Cell>) => models.filter((m) => val(byModel.get(m.key))).map((m) => `${shortModel(m.key)}: ${show(val(byModel.get(m.key)))}`).join('\n')
  const label = $derived(METRICS.find((m) => m[0] === metric)?.[1] ?? '')
</script>

<div class="wrap">
  {#if !models.length}
    <p class="dim">no usage recorded</p>
  {:else}
    <div class="tiles">
      <div><b>{fmtCost(total.cost)}</b><span>total cost</span></div>
      {#if subs.length}
        <div><b>{fmtCost(main.cost)}</b><span>main · {pct(main.cost, total.cost)}</span></div>
        <div><b>{fmtCost(subTotal.cost)}</b><span>{subs.length} subagents · {pct(subTotal.cost, total.cost)}</span></div>
      {/if}
      <div><b>{fmtNum(total.tools)}</b><span>tool calls</span></div>
      <div><b>{fmtNum(total.output)}</b><span>out tok</span></div>
      <div><b>{fmtNum(total.input + total.cache + total.output)}</b><span>total tok</span></div>
    </div>

    <div class="row ctl">
      <select bind:value={metric}>{#each METRICS as [m, l]}<option value={m}>{l}</option>{/each}</select>
    </div>

    <h3>By model</h3>
    <div class="split">
      {#each models as m (m.key)}{#if val(m.c)}<i style="flex:{val(m.c)};background:{modelColor(m.key)}" title="{m.key}: {show(val(m.c))}"></i>{/if}{/each}
    </div>
    <div class="legend">
      {#each models as m (m.key)}
        <span><i style="background:{modelColor(m.key)}"></i>{shortModel(m.key)} <b>{show(val(m.c)) || '–'}</b> <span class="dim">{pct(val(m.c), val(total))}</span></span>
      {/each}
    </div>

    <h3 title="each cell: {label} of that agent type on that model">Agent × model · {label}</h3>
    <ModelMatrix rows={slices} {metric} label="agent" />

    {#if subs.length}
      <h3 class="row">
        Subagents · {label}
        <span class="seg">
          <button class:on={order === 'value'} onclick={() => (order = 'value')}>by {label}</button>
          <button class:on={order === 'time'} onclick={() => (order = 'time')}>by start</button>
        </span>
      </h3>
      <div class="scroll">
        <table>
          <thead><tr>
            <th>type</th><th>task</th><th>model</th><th class="n">start</th><th class="n">dur</th>
            <th class="n" title="tool calls">tools</th>
            <th class="n" title="input tokens, uncached (billed full price)">in</th>
            <th class="n" title="cache read + cache write tokens">cache</th>
            <th class="n" title="output tokens (incl. thinking)">out</th>
            <th class="n" title="API list price equivalent (subscription plans bill differently)">cost</th>
            <th>{label} by model</th>
          </tr></thead>
          <tbody>
            {#each instances as r (r.s.id)}
              <tr>
                <td class="mono">{r.key}</td>
                <td class="title"><a href="#/s/{r.s.agent}/{encodeURIComponent(r.s.id)}" title={r.s.title}>{r.s.title.replace(/^\[[^\]]*\]\s*/, '')}</a></td>
                <td class="mono">{#each models as m (m.key)}{#if r.byModel.has(m.key)}<span class="model"><i class="dot" style="background:{modelColor(m.key)}"></i>{shortModel(m.key)}</span>{/if}{/each}</td>
                <td class="n dim">{fmtTime(r.s.started)}</td><td class="n dim">{fmtDur(r.s.started, r.s.ended)}</td>
                <td class="n">{r.total.tools || ''}</td>
                <td class="n">{fmtNum(r.total.input)}</td><td class="n">{fmtNum(r.total.cache)}</td><td class="n">{fmtNum(r.total.output)}</td>
                <td class="n"><b>{fmtCost(r.total.cost)}</b></td>
                <td class="share">
                  <div class="stack" title={tipOf(r.byModel)}>
                    {#each models as m (m.key)}
                      {@const v = val(r.byModel.get(m.key))}
                      {#if v}<i style="width:{(100 * v) / maxInst}%;background:{modelColor(m.key)}"></i>{/if}
                    {/each}
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  {/if}

  <UsageLists {sessions} />
</div>

<style>
  .wrap { padding: 12px 16px 40px; }
  h3 { font-size: 13px; margin: 24px 0 8px; color: var(--fg2); font-weight: 600; }
  h3.row { gap: 12px; }
  .ctl { margin-top: 16px; }
  .tiles { display: flex; flex-wrap: wrap; gap: 8px; }
  .tiles div { background: var(--bg2); border: 1px solid var(--line); border-radius: 6px; padding: 8px 14px; min-width: 96px; display: flex; flex-direction: column; }
  .tiles b { font-size: 18px; font-variant-numeric: tabular-nums; }
  .tiles span { font-size: 11px; color: var(--fg2); }

  .split { display: flex; gap: 2px; height: 14px; border-radius: 3px; overflow: hidden; background: var(--bg2); }
  .split i { min-width: 2px; }
  .legend { display: flex; flex-wrap: wrap; gap: 4px 18px; margin-top: 8px; font-size: 12px; font-family: var(--mono); color: var(--fg2); }
  .legend b { color: var(--fg); font-weight: 500; font-variant-numeric: tabular-nums; }
  .legend i, .dot { display: inline-block; width: 8px; height: 8px; border-radius: 2px; margin-right: 6px; }

  .scroll { overflow-x: auto; }
  table { border-collapse: collapse; width: 100%; font-size: 12px; }
  th { text-align: left; color: var(--fg2); font-weight: 500; padding: 4px 8px; border-bottom: 1px solid var(--line); white-space: nowrap; }
  td { padding: 4px 8px; border-bottom: 1px solid var(--line); white-space: nowrap; font-variant-numeric: tabular-nums; }
  td b { font-weight: 600; }
  .n { text-align: right; }
  .title { max-width: 360px; overflow: hidden; text-overflow: ellipsis; }
  .title a { text-decoration: none; }
  .title a:hover { text-decoration: underline; }
  .model + .model { margin-left: 10px; }
  tbody tr:hover td { background: color-mix(in srgb, var(--fg) 4%, transparent); }
  .share { width: 25%; min-width: 120px; }
  .stack { display: flex; height: 8px; }
  .stack i { min-width: 1px; }
  .stack i:last-child { border-radius: 0 3px 3px 0; }
  .seg { display: inline-flex; font-weight: 400; }
  .seg button { border-radius: 0; font-size: 11px; }
  .seg button:first-child { border-radius: 4px 0 0 4px; }
  .seg button:last-child { border-radius: 0 4px 4px 0; }
</style>
