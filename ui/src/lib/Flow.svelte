<script lang="ts">
  import type { Agent, Detail, Event, Session } from './types'
  import { fmtCost, fmtDur, fmtNum, fmtTime } from './fmt'
  import { untrack } from 'svelte'

  let { detail, byKey }: { detail: Detail; byKey: Map<string, Session> } = $props()

  const agent = $derived<Agent>(detail.summary.agent)

  // graph: start -> turn -> turn -> ... ; each turn hangs the subagents it spawned (as a tree) below it
  interface Node {
    kind: 'start' | 'turn' | 'agent'; id: string; title: string; sub: string; tip: string; tag?: string
    s?: Session; model?: string; spawnTs?: string; agents: Node[]; next?: Node
    /// own cost (this session / this turn) and cost incl. every subagent below
    cost: number; roll: number
    col: number; row: number; x: number; y: number
  }
  let start = $state<Node | null>(null)

  const mk = (kind: Node['kind'], id: string): Node => ({ kind, id, title: '', sub: '', tip: '', agents: [], cost: 0, roll: 0, col: 0, row: 0, x: 0, y: 0 })
  const stats = (s: Session) => `${s.tool_calls}t · ${fmtNum(s.usage.output)}out · ${fmtDur(s.started, s.ended)}`
  // own cost, or the subtree total (own value stays in the tooltip) when subagents add to it
  const costLabel = (n: Node) => (n.roll > n.cost ? `Σ${fmtCost(n.roll)}` : n.cost ? fmtCost(n.cost) : '')
  const usage = (s: Session) => `in ${fmtNum(s.usage.input)} · cache↓ ${fmtNum(s.usage.cache_read)} · out ${fmtNum(s.usage.output)}`
  // first meaningful line: skip injected boilerplate (skill preamble, tags, caveats)
  const junk = /^(<|Base directory for this skill|Caveat:|# AGENTS\.md|\[Request interrupted|\[Image)/
  const firstLine = (t?: string) => {
    const ls = t?.split('\n').map((l) => l.trim()).filter(Boolean) ?? []
    return ls.find((l) => !junk.test(l)) ?? ls[0] ?? ''
  }

  // all descendants share parent_id = root (claude stores them in one dir), so link by spawn events;
  // parent_id only as fallback for ids nobody spawned.
  const related = $derived.by(() => {
    const all = [...byKey.values()].filter((s) => s.agent === agent && s.parent_id === detail.summary.id)
    const ids = new Set(all.map((s) => s.id))
    for (const s of all) for (const c of s.children) ids.add(c)
    return [...ids].sort()
  })
  // rebuild only when this session's detail or its subagent set/state changes - not on every list push.
  // byKey read inside is untracked; identity of unchanged sessions is preserved by App so this key is stable.
  const buildKey = $derived(related.map((id) => { const s = byKey.get(agent + '/' + id); return `${id}:${s?.ended ?? ''}:${s?.tool_calls ?? ''}` }).join('|'))
  let gen = 0
  $effect(() => {
    detail; buildKey
    untrack(build)
  })
  function build() {
    const rs = detail.summary
    const ids = related
    const my = ++gen
    Promise.all(
      [...ids].map((id) => fetch(`/api/sessions/${agent}/${encodeURIComponent(id)}`).then((x) => (x.ok ? (x.json() as Promise<Detail>) : null)).catch(() => null)),
    ).then((ds) => {
      if (my !== gen) return // superseded by a newer build
      const events = new Map<string, Event[]>([[rs.id, detail.events]])
      for (const d of ds) if (d) events.set(d.summary.id, d.events)
      const spawn = new Map<string, { by: string; e: Event }>()
      for (const [pid, evs] of events)
        for (const e of evs) if (e.kind === 'subagent' && e.child_session_id && !spawn.has(e.child_session_id)) spawn.set(e.child_session_id, { by: pid, e })

      const agentNode = (id: string, e?: Event): Node => {
        const s = byKey.get(agent + '/' + id)
        const n = mk('agent', id)
        n.s = s
        n.spawnTs = e?.ts ?? s?.started
        n.tag = e?.meta?.subagent_type ?? e?.meta?.agent_type
        n.model = (e?.meta?.resolvedModel ?? e?.meta?.model ?? s?.models[0])?.replace(/^claude-/, '')
        n.title = (s?.title ?? e?.meta?.description ?? id).replace(/^\[[^\]]*\]\s*/, '')
        n.sub = s ? stats(s) : 'not indexed'
        n.cost = s?.cost ?? 0
        n.tip = [s?.title ?? id, e?.meta?.description, s && `${fmtTime(s.started)} · ${fmtDur(s.started, s.ended)} · ${s.tool_calls} tools · ${usage(s)}`].filter(Boolean).join('\n')
        n.agents = agentsOf(id, s)
        return n
      }
      const agentsOf = (pid: string, s?: Session): Node[] => {
        const out: Node[] = []
        for (const [id, sp] of spawn) if (sp.by === pid) out.push(agentNode(id, sp.e))
        for (const id of s?.children ?? []) if (!spawn.has(id)) out.push(agentNode(id))
        return out.sort((a, b) => (a.spawnTs ?? '').localeCompare(b.spawnTs ?? ''))
      }

      const st = mk('start', rs.id)
      st.s = rs
      st.title = rs.title
      st.model = rs.models[0]?.replace(/^claude-/, '')
      st.sub = stats(rs)
      st.cost = rs.cost ?? 0
      st.tip = `${rs.title}\n${fmtTime(rs.started)} · ${fmtDur(rs.started, rs.ended)} · ${rs.tool_calls} tools · ${usage(rs)}`

      // split root events into turns at user messages (consecutive user events = one turn, e.g. text + image)
      const rootAgents = agentsOf(rs.id, rs)
      let last: Node = st, cur: Node | null = null, prevUser = false, i = 0
      let tools = 0, out = 0, cst = 0, t0 = '', t1 = ''
      const flush = () => {
        if (!cur) return
        cur.cost = cst
        cur.sub = `${tools} tools · ${fmtNum(out)}out · ${fmtDur(t0, t1)}`
        cur.tip += `\n${fmtTime(t0)} · ${cur.sub}`
        cur.agents = rootAgents.filter((a) => a.spawnTs && a.spawnTs >= t0 && a.spawnTs <= t1)
        for (const a of cur.agents) rootAgents.splice(rootAgents.indexOf(a), 1)
        last.next = cur
        last = cur
      }
      for (const e of detail.events) {
        if (e.kind === 'user') {
          if (!(prevUser && cur)) {
            flush()
            cur = mk('turn', String(e.id))
            cur.tag = `turn ${++i}`
            cur.title = firstLine(e.text) || '(empty)'
            cur.tip = `${cur.tag}: ${cur.title}`
            tools = 0; out = 0; cst = 0; t0 = e.ts ?? ''; t1 = e.ts ?? ''
          } else if (cur.title === '(empty)') cur.title = firstLine(e.text) || cur.title
          prevUser = true
          continue
        }
        prevUser = false
        if (!cur) continue
        if (e.kind === 'tool_call' || e.kind === 'subagent') tools++
        if (e.usage) out += e.usage.output
        cst += e.cost ?? 0
        if (e.ts && e.ts > t1) t1 = e.ts
      }
      flush()
      last.agents.push(...rootAgents) // spawns not matched to a turn -> last node
      // roll = own cost + every subagent below; the start node also owns all its turns' agents
      const roll = (n: Node): number => (n.roll = n.cost + n.agents.reduce((a, c) => a + roll(c), 0))
      let subs = 0
      for (let n: Node | undefined = st; n; n = n.next) {
        roll(n)
        if (n !== st) subs += n.roll - n.cost
      }
      st.roll += subs
      for (let n: Node | undefined = st; n; n = n.next)
        for (const a of nodesOf(n))
          if (a.cost) a.tip += `\ncost ${fmtCost(a.cost)}${a.roll > a.cost ? ` · incl. subagents ${fmtCost(a.roll)}` : ''}`
      layout(st)
      if (!start) reset() // first render only; live rebuilds keep pan/zoom
      start = st
    })
  }

  // --- layout: start + turns on row 0 left->right; each turn's agent tree hangs below-right of it ---
  const W = 340, H = 62, GX = 70, GY = 16, PAD = 40
  const depthOf = (ns: Node[]): number => (ns.length ? 1 + Math.max(...ns.map((n) => depthOf(n.agents))) : 0)
  function layout(st: Node) {
    let col = 0
    for (let n: Node | undefined = st; n; n = n.next) {
      n.col = col
      n.row = 0
      let row = 1
      const place = (a: Node, c: number) => {
        a.col = c
        if (!a.agents.length) { a.row = row++; return }
        for (const k of a.agents) place(k, c + 1)
        a.row = (a.agents[0].row + a.agents[a.agents.length - 1].row) / 2
      }
      for (const a of n.agents) place(a, col + 1)
      col += 1 + depthOf(n.agents)
    }
    for (const n of nodesOf(st)) { n.x = PAD + n.col * (W + GX); n.y = PAD + n.row * (H + GY) }
  }
  function nodesOf(st: Node) {
    const out: Node[] = []
    const walk = (n: Node) => { out.push(n); n.agents.forEach(walk) }
    for (let n: Node | undefined = st; n; n = n.next) walk(n)
    return out
  }
  const nodes = $derived(start ? nodesOf(start) : [])
  const size = $derived({ w: Math.max(...nodes.map((n) => n.x + W), 0) + PAD, h: Math.max(...nodes.map((n) => n.y + H), 0) + PAD })
  const bez = (x0: number, y0: number, x1: number, y1: number) => `M${x0},${y0} C${(x0 + x1) / 2},${y0} ${(x0 + x1) / 2},${y1} ${x1},${y1}`
  const edgeNext = (a: Node, b: Node) => bez(a.x + W, a.y + H / 2, b.x, b.y + H / 2)
  // agent -> agent: right port to left port; turn -> agent: bottom port curving down-right into left port
  const edgeAgent = (a: Node, b: Node) =>
    a.kind === 'agent' ? bez(a.x + W, a.y + H / 2, b.x, b.y + H / 2) : `M${a.x + W / 2},${a.y + H} C${a.x + W / 2},${b.y + H / 2} ${b.x - 30},${b.y + H / 2} ${b.x},${b.y + H / 2}`
  const clip = (t: string, n: number) => (t.length > n ? t.slice(0, n - 1) + '…' : t)
  const color = (n: Node) => (n.kind === 'start' ? `var(--${agent})` : n.kind === 'turn' ? 'var(--user)' : n.s ? 'var(--sub)' : 'var(--fg3)')
  const icon = (n: Node) => (n.kind === 'start' ? '▶' : n.kind === 'turn' ? '✎' : '⚙')
  const href = (n: Node) => (n.kind === 'agent' && n.s ? `#/s/${agent}/${encodeURIComponent(n.id)}` : undefined)

  // --- pan / zoom ---
  let zoom = $state(1), px = $state(0), py = $state(0)
  let drag: { x: number; y: number; px: number; py: number } | null = null
  function down(e: MouseEvent) { if ((e.target as Element).closest('a[href]')) return; drag = { x: e.clientX, y: e.clientY, px, py } }
  function move(e: MouseEvent) { if (drag) { px = drag.px + e.clientX - drag.x; py = drag.py + e.clientY - drag.y } }
  function up() { drag = null }
  function wheel(e: WheelEvent) {
    e.preventDefault()
    const z = Math.min(2.5, Math.max(0.2, zoom * (e.deltaY < 0 ? 1.1 : 0.9)))
    const r = (e.currentTarget as Element).getBoundingClientRect()
    const mx = e.clientX - r.left, my = e.clientY - r.top
    px = mx - ((mx - px) * z) / zoom
    py = my - ((my - py) * z) / zoom
    zoom = z
  }
  function reset() { zoom = 1; px = 0; py = 0 }
</script>

<div class="flow">
  <div class="row bar">
    <span class="dim">{nodes.length} nodes · {fmtDur(detail.summary.started, detail.summary.ended)}</span>
    <span class="legend">
      <span><i style="background:var(--{agent})"></i>session</span>
      <span><i style="background:var(--user)"></i>turn</span>
      <span><i style="background:var(--sub)"></i>subagent</span>
    </span>
    <span style="flex:1"></span>
    <span class="dim">drag to pan · wheel to zoom</span>
    <button class="sm" onclick={reset}>{Math.round(zoom * 100)}%</button>
  </div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="canvas" onmousedown={down} onmousemove={move} onmouseup={up} onmouseleave={up} onwheel={wheel}>
    {#if !start}
      <div class="dim pad">loading…</div>
    {:else}
      <svg width={size.w * zoom} height={size.h * zoom} viewBox="0 0 {size.w} {size.h}" style="transform:translate({px}px,{py}px)" font-family="var(--sans)" font-size="11">
        {#each nodes as n}
          {#if n.next}
            <path d={edgeNext(n, n.next)} fill="none" stroke="var(--user)" stroke-opacity=".7" stroke-width="1.5" />
            <circle cx={n.x + W} cy={n.y + H / 2} r="3" fill="var(--user)" />
            <circle cx={n.next.x} cy={n.next.y + H / 2} r="3" fill="var(--bg)" stroke="var(--user)" stroke-width="1.5" />
          {/if}
          {#each n.agents as c}
            <path d={edgeAgent(n, c)} fill="none" stroke="var(--sub)" stroke-opacity={c.s ? 0.8 : 0.5} stroke-width="1.5" stroke-dasharray={c.s ? undefined : '5 4'} />
            <circle cx={c.x} cy={c.y + H / 2} r="3" fill="var(--bg)" stroke="var(--sub)" stroke-width="1.5" />
          {/each}
          {#if n.agents.length}
            {#if n.kind === 'agent'}<circle cx={n.x + W} cy={n.y + H / 2} r="3" fill="var(--sub)" />
            {:else}<circle cx={n.x + W / 2} cy={n.y + H} r="3" fill="var(--sub)" />{/if}
          {/if}
        {/each}
        {#each nodes as n}
          {@const c = color(n)}
          <a href={href(n)}>
            <g class="node" class:ghost={n.kind === 'agent' && !n.s} class:link={!!href(n)} transform="translate({n.x},{n.y})">
              <title>{n.tip}</title>
              <rect width={W} height={H} rx="10" fill="var(--bg2)" stroke={c} stroke-opacity=".7" stroke-width="1.2" stroke-dasharray={n.kind === 'agent' && !n.s ? '4 3' : undefined} />
              <rect x="0" y="0" width="5" height={H} rx="2.5" fill={c} />
              <rect x="14" y="14" width="34" height="34" rx="8" fill={c} fill-opacity=".18" />
              <text x="31" y="36" text-anchor="middle" font-size="16" fill={c}>{icon(n)}</text>
              <text x="58" y="24" fill="var(--fg)" font-weight="600" font-size="12">{clip(n.title, 34)}</text>
              <g transform="translate(58,32)" font-family="var(--mono)" font-size="10">
                {#if n.model}
                  <rect width={n.model.length * 6.2 + 12} height="18" rx="5" fill="var(--bg3)" stroke="var(--line)" />
                  <text x="6" y="12.5" fill="var(--fg)">{n.model}</text>
                {/if}
                <text x={(n.model?.length ?? -3) * 6.2 + 20} y="12.5" fill="var(--fg3)">{clip(n.sub, 28)}</text>
              </g>
              {#if costLabel(n)}
                <text x={W - 8} y="45" text-anchor="end" font-size="10" font-family="var(--mono)" fill={n.roll > n.cost ? c : 'var(--fg2)'}>{costLabel(n)}</text>
              {/if}
              {#if n.tag}<text x={W - 8} y="12" text-anchor="end" font-size="9" fill="var(--fg3)" font-family="var(--mono)">{n.tag}</text>{/if}
            </g>
          </a>
        {/each}
      </svg>
    {/if}
  </div>
</div>

<style>
  .flow { display: flex; flex-direction: column; height: calc(100vh - 150px); min-height: 400px; }
  .bar { font-size: 11px; padding: 6px 16px; }
  .bar button.sm { font-size: 11px; padding: 0 6px; font-family: var(--mono); }
  .legend { display: inline-flex; gap: 10px; font-size: 10px; color: var(--fg3); font-family: var(--mono); margin-left: 8px; }
  .legend i { display: inline-block; width: 8px; height: 8px; border-radius: 2px; margin-right: 4px; }
  .canvas {
    flex: 1; overflow: hidden; cursor: grab; position: relative;
    background-color: var(--bg);
    background-image: radial-gradient(var(--line) 1px, transparent 1px);
    background-size: 18px 18px;
  }
  .canvas:active { cursor: grabbing; }
  svg { display: block; transform-origin: 0 0; user-select: none; }
  a { text-decoration: none; }
  .node.link { cursor: pointer; }
  .node.link:hover > rect:first-of-type { fill: var(--bg3); stroke-opacity: 1; }
  .ghost { opacity: .7; }
  .pad { padding: 20px; }
</style>
