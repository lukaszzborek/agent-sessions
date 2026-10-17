<script lang="ts">
  import { type Cell, type Metric, add, zero, valOf, fmtVal, modelColor, shortModel } from './metric'

  // rows: usage slices (one session on one model); n counts distinct sessions per key
  let { rows, metric, label }: { rows: (Cell & { key: string; id: string; model: string })[]; metric: Metric; label: string } = $props()

  const val = (c?: Cell) => (c ? valOf[metric](c) : 0)
  const show = (v: number) => (v ? fmtVal(metric, v) : '')
  const pct = (v: number, of: number) => (of ? `${Math.round((100 * v) / of)}%` : '')
  const data = $derived.by(() => {
    const groups = new Map<string, { key: string; ids: Set<string>; byModel: Map<string, Cell>; total: Cell }>()
    const models = new Map<string, Cell>(), total = zero()
    for (const r of rows) {
      let g = groups.get(r.key)
      if (!g) groups.set(r.key, (g = { key: r.key, ids: new Set(), byModel: new Map(), total: zero() }))
      g.ids.add(r.id)
      let c = g.byModel.get(r.model)
      if (!c) g.byModel.set(r.model, (c = zero()))
      let m = models.get(r.model)
      if (!m) models.set(r.model, (m = zero()))
      add(c, r); add(g.total, r); add(m, r); add(total, r)
    }
    return {
      groups: [...groups.values()].sort((a, b) => val(b.total) - val(a.total)),
      models: [...models].map(([key, c]) => ({ key, c })).sort((a, b) => val(b.c) - val(a.c)),
      total,
      n: new Set(rows.map((r) => r.id)).size,
    }
  })
  const max = $derived(Math.max(1e-9, ...data.groups.map((g) => val(g.total))))
</script>

<div class="scroll">
  <table>
    <thead><tr>
      <th>{label}</th><th class="n" title="sessions">n</th>
      {#each data.models as m (m.key)}<th class="n" title={m.key}><i class="dot" style="background:{modelColor(m.key)}"></i>{shortModel(m.key)}</th>{/each}
      <th class="n">total</th><th class="n">%</th><th></th>
    </tr></thead>
    <tbody>
      {#each data.groups as g (g.key)}
        <tr>
          <td class="mono">{g.key}</td><td class="n dim">{g.ids.size}</td>
          {#each data.models as m (m.key)}<td class="n">{show(val(g.byModel.get(m.key)))}</td>{/each}
          <td class="n"><b>{show(val(g.total))}</b></td><td class="n dim">{pct(val(g.total), val(data.total))}</td>
          <td class="share">
            <div class="stack" title={data.models.filter((m) => val(g.byModel.get(m.key))).map((m) => `${shortModel(m.key)}: ${show(val(g.byModel.get(m.key)))}`).join('\n')}>
              {#each data.models as m (m.key)}
                {@const v = val(g.byModel.get(m.key))}
                {#if v}<i style="width:{(100 * v) / max}%;background:{modelColor(m.key)}"></i>{/if}
              {/each}
            </div>
          </td>
        </tr>
      {/each}
    </tbody>
    {#if data.groups.length > 1}
      <tfoot><tr>
        <td>total</td><td class="n dim">{data.n}</td>
        {#each data.models as m (m.key)}<td class="n">{show(val(m.c))}</td>{/each}
        <td class="n"><b>{show(val(data.total))}</b></td><td></td><td></td>
      </tr></tfoot>
    {/if}
  </table>
</div>

<style>
  .scroll { overflow-x: auto; }
  table { border-collapse: collapse; width: 100%; font-size: 12px; }
  th { text-align: left; color: var(--fg2); font-weight: 500; padding: 4px 8px; border-bottom: 1px solid var(--line); white-space: nowrap; }
  td { padding: 4px 8px; border-bottom: 1px solid var(--line); white-space: nowrap; font-variant-numeric: tabular-nums; }
  tfoot td { border-bottom: 0; border-top: 1px solid var(--fg3); color: var(--fg2); }
  tbody tr:hover td { background: color-mix(in srgb, var(--fg) 4%, transparent); }
  td b { font-weight: 600; }
  .n { text-align: right; }
  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 2px; margin-right: 6px; }
  .share { width: 25%; min-width: 120px; }
  .stack { display: flex; height: 8px; }
  .stack i { min-width: 1px; }
  .stack i:last-child { border-radius: 0 3px 3px 0; }
</style>
