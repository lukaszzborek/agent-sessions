<script lang="ts">
  import type { Session, Agent } from './types'
  import { fmtDate, fmtNum, fmtCost, project, totalTokens, shortHook, shortTags } from './fmt'
  import { sourceFilter, toggleSource } from './sources.svelte'
  import { tagFilter } from './tags.svelte'

  let { sessions, sources, selected }: { sessions: Session[]; sources: string[]; selected: { agent: Agent; id: string } | null } = $props()

  let agents = $state<Record<Agent, boolean>>({ claude: true, codex: true, pi: true })
  let q = $state('')
  let proj = $state('')
  let model = $state('')
  let skill = $state('')
  let hook = $state('')
  let showSub = $state(true)
  let limit = $state(200)
  let collapsed = $state(new Set<string>())
  function toggle(e: Event, k: string) {
    e.preventDefault()
    e.stopPropagation()
    const n = new Set(collapsed)
    n.has(k) ? n.delete(k) : n.add(k)
    collapsed = n
  }

  const projects = $derived([...new Set(sessions.map((s) => project(s.cwd)))].sort())
  const models = $derived([...new Set(sessions.flatMap((s) => s.models))].sort())
  const skills = $derived([...new Set(sessions.flatMap((s) => s.skills.map((x) => x[0])))].sort())
  const hooks = $derived([...new Set(sessions.flatMap((s) => s.hooks.map((x) => x[0])))].sort())
  const tags = $derived([...new Set(sessions.flatMap((s) => s.tags))].sort())
  const has = (xs: [string, number][], k: string) => xs.some((x) => x[0] === k)

  const filtered = $derived.by(() => {
    const ql = q.toLowerCase()
    const match = (s: Session) =>
      agents[s.agent] &&
      (showSub || !s.parent_id) &&
      (!proj || project(s.cwd) === proj) &&
      (!model || s.models.includes(model)) &&
      (!skill || has(s.skills, skill)) &&
      (!hook || has(s.hooks, hook)) &&
      (!tagFilter.tag || s.tags.includes(tagFilter.tag)) &&
      (!ql || s.title.toLowerCase().includes(ql) || s.id.includes(ql) || s.git_branch?.toLowerCase().includes(ql) || s.tags.some((t) => t.toLowerCase().includes(ql)))
    const ok = sessions.filter(match)
    if (!showSub) return ok.map((s) => ({ s, depth: 0, nkids: 0 }))
    // group: parent first, then its subagents (oldest first) nested below
    const key = (s: Session) => s.agent + s.id
    const present = new Set(ok.map(key))
    const kids = new Map<string, Session[]>()
    for (const s of ok) {
      if (s.parent_id && present.has(s.agent + s.parent_id)) {
        const k = s.agent + s.parent_id
        let list = kids.get(k)
        if (!list) kids.set(k, (list = []))
        list.push(s)
      }
    }
    const out: { s: Session; depth: number; nkids: number }[] = []
    const emit = (s: Session, depth: number) => {
      const c = kids.get(key(s)) ?? []
      out.push({ s, depth, nkids: c.length })
      if (collapsed.has(key(s))) return
      c.sort((a, b) => (a.started ?? '').localeCompare(b.started ?? ''))
      for (const x of c) emit(x, depth + 1)
    }
    for (const s of ok) if (!s.parent_id || !present.has(s.agent + s.parent_id)) emit(s, 0)
    return out
  })

  // per-session rollup: own usage + all descendant subagents (children can be filtered out of the list)
  const totals = $derived.by(() => {
    const by = new Map(sessions.map((s) => [s.agent + s.id, s]))
    // claude flattens the tree: the root lists every descendant in `children` and intermediate
    // agents list theirs again - so walk the subtree with a seen set instead of summing per parent
    const sum = (root: Session) => {
      const seen = new Set([root.agent + root.id])
      const q = [root]
      const acc = { tok: 0, cost: 0, n: -1, ctx: 0 }
      while (q.length) {
        const s = q.pop()!
        acc.tok += totalTokens(s.usage)
        acc.cost += s.cost ?? 0
        // subagents have their own window, so the tree peak is a max, not a sum
        acc.ctx = Math.max(acc.ctx, s.max_context ?? 0)
        acc.n++
        for (const cid of s.children) {
          const c = by.get(s.agent + cid)
          if (c && !seen.has(c.agent + c.id)) {
            seen.add(c.agent + c.id)
            q.push(c)
          }
        }
      }
      return acc
    }
    return new Map(sessions.map((s) => [s.agent + s.id, sum(s)]))
  })

  let now = $state(Date.now())
  setInterval(() => (now = Date.now()), 30_000)
  const isLive = (s: Session) => !!s.ended && now - new Date(s.ended).getTime() < 120_000

  function dayOf(s?: string) {
    return s ? new Date(s).toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' }) : 'unknown'
  }
</script>

<div class="filters">
  <div class="row">
    {#each ['claude', 'codex', 'pi'] as const as a}
      <button class:on={agents[a]} class="agent {a}" onclick={() => (agents[a] = !agents[a])}>{a}</button>
    {/each}
    <label class="dim" style="margin-left:auto"><input type="checkbox" bind:checked={showSub} /> subagents</label>
  </div>
  <!-- also shown for a single source that the stored filter hides, else there is no way back -->
  {#if sources.length > 1 || sources.some((n) => !sourceFilter.on.includes(n))}
    <div class="row">
      {#each sources as n}
        <button class:on={sourceFilter.on.includes(n)} class="source" onclick={() => toggleSource(n)} title="sessions indexed from source '{n}'">{n}</button>
      {/each}
    </div>
  {/if}
  <input placeholder="search title / id / branch" bind:value={q} />
  <div class="row">
    <select bind:value={proj} style="flex:1;min-width:0">
      <option value="">all projects</option>
      {#each projects as p}<option value={p}>{p}</option>{/each}
    </select>
    <select bind:value={model} style="flex:1;min-width:0">
      <option value="">all models</option>
      {#each models as m}<option value={m}>{m}</option>{/each}
    </select>
  </div>
  <div class="row">
    <select bind:value={skill} style="flex:1;min-width:0">
      <option value="">all skills</option>
      {#each skills as k}<option value={k}>{k}</option>{/each}
    </select>
    <select bind:value={hook} style="flex:1;min-width:0">
      <option value="">all hooks</option>
      {#each hooks as h}<option value={h}>{shortHook(h)}</option>{/each}
    </select>
  </div>
  {#if tags.length}
    <div class="row">
      <select bind:value={tagFilter.tag} style="flex:1;min-width:0">
        <option value="">all tags</option>
        {#each tags as t}<option value={t}>{t}</option>{/each}
      </select>
      {#if tagFilter.tag}<a class="dim" href="#/compare/{encodeURIComponent(tagFilter.tag)}" title="side-by-side table of sessions with this tag">compare</a>{/if}
    </div>
  {/if}
  <div class="dim">{filtered.length} sessions</div>
</div>

<ul>
  {#each filtered.slice(0, limit) as { s, depth, nkids }, i (s.agent + s.id)}
    {@const prevDay = i > 0 ? dayOf(filtered[i - 1].s.started) : null}
    {@const day = depth ? prevDay : dayOf(s.started)}
    {@const k = s.agent + s.id}
    {@const tot = totals.get(k)}
    {#if day !== prevDay}<li class="day dim">{day}</li>{/if}
    <li class:sel={selected?.agent === s.agent && selected?.id === s.id} class:sub={depth > 0}>
      <a href="#/s/{s.agent}/{encodeURIComponent(s.id)}" style:padding-left={depth ? `${12 + 14 * depth}px` : null}>
        <div class="row top">
          <span class="agent {s.agent}">{s.agent}</span>
          {#if isLive(s)}<span class="livedot" title="active in last 2 min"></span>{/if}
          <span class="proj">{project(s.cwd)}</span>
          {#if s.source !== 'user'}<span class="src" title={s.path}>{s.source}</span>{/if}
          {#if s.git_branch}<span class="dim mono branch">⎇ {s.git_branch}</span>{/if}
          {#if s.archived}<span class="dim" title="source file deleted; served from db">🗄</span>{/if}
          <span class="dim time">{fmtDate(s.started)}</span>
        </div>
        <div class="title">{s.title}</div>
        <div class="row meta">
          {#if nkids}
            <button class="tog" class:open={!collapsed.has(k)} onclick={(e) => toggle(e, k)} title={collapsed.has(k) ? 'show subagents' : 'hide subagents'}>
              <span class="chev">▸</span>{nkids} sub
            </button>
          {:else if s.children.length}
            <span class="pill" style="color:var(--sub)" title="subagents spawned (filtered out)">{s.children.length} sub</span>
          {/if}
          {#each s.models as m}<span class="pill">{m}</span>{/each}
          {#each shortTags(s) as t}<button class="pill tag" onclick={(e) => { e.preventDefault(); tagFilter.tag = t }} title="filter by tag">{t}</button>{/each}
          <span class="stats">
          <span class="dim mono" title="user / assistant messages">{s.user_msgs}u·{s.assistant_msgs}a</span>
          <span class="dim mono" title="tool calls">{s.tool_calls}t</span>
          {#if s.skills.length}<span class="dim mono" title={s.skills.map(([n, c]) => `${n} ×${c}`).join('\n')}>{s.skills.reduce((a, x) => a + x[1], 0)}sk</span>{/if}
          {#if tot?.ctx}<span class="dim mono" title="peak context (prompt tokens of one API call){tot.ctx > (s.max_context ?? 0) ? `\nown: ${fmtNum(s.max_context ?? 0)} · subagent peak: ${fmtNum(tot.ctx)}` : ''}">{fmtNum(tot.ctx)}ctx</span>{/if}
          {#if s.hooks.length}<span class="dim mono" title={s.hooks.map(([n, c]) => `${shortHook(n)} ×${c}`).join('\n')}>{s.hooks.reduce((a, x) => a + x[1], 0)}hk</span>{/if}
          </span>
          {#if tot && tot.n}
            <span
              class="mono right roll"
              title="total incl. {tot.n} subagent{tot.n > 1 ? 's' : ''} (all levels)&#10;own: {fmtNum(totalTokens(s.usage))}{s.cost != null ? ' · ' + fmtCost(s.cost) : ''}"
            >Σ {fmtNum(tot.tok)}{#if tot.cost} · {fmtCost(tot.cost)}{/if}</span>
          {:else}
            <span class="dim mono right" title="total tokens (input + cache + output)">{fmtNum(totalTokens(s.usage))}{#if s.cost != null} · {fmtCost(s.cost)}{/if}</span>
          {/if}
        </div>
      </a>
    </li>
  {/each}
  {#if filtered.length > limit}
    <li><button style="width:100%;margin:6px 10px;width:calc(100% - 20px)" onclick={() => (limit += 200)}>more ({filtered.length - limit})</button></li>
  {/if}
</ul>

<style>
  .filters { padding: 8px 10px; display: flex; flex-direction: column; gap: 6px; border-bottom: 1px solid var(--line); }
  .filters input:not([type='checkbox']) { width: 100%; }
  ul { list-style: none; margin: 0; padding: 0; overflow: auto; flex: 1; }
  li.day { padding: 8px 12px 3px; font-size: 10px; text-transform: uppercase; letter-spacing: .08em; position: sticky; top: 0; background: var(--bg2); z-index: 1; }
  li a { display: block; padding: 8px 12px; text-decoration: none; border-bottom: 1px solid var(--line); position: relative; }
  li a:hover { background: var(--bg3); }
  li.sel a { background: var(--bg3); box-shadow: inset 3px 0 0 var(--fg); }
  li.sub a { background: color-mix(in srgb, var(--bg2) 60%, var(--bg)); }
  li.sub a::before { content: ''; position: absolute; left: 14px; top: 0; bottom: 0; border-left: 1px solid var(--sub); opacity: .4; }
  li.sub.sel a { box-shadow: inset 3px 0 0 var(--sub); }
  .top { font-size: 11px; flex-wrap: nowrap; overflow: hidden; gap: 6px; }
  .top span { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .livedot { width: 7px; height: 7px; border-radius: 50%; background: #3fb950; flex-shrink: 0; }
  .proj { color: var(--fg2); font-weight: 500; }
  .roll { color: var(--sub); flex-shrink: 0; }
  .src { font-size: 10px; padding: 0 4px; border: 1px solid var(--line); border-radius: 3px; color: var(--fg2); }
  .branch { flex-shrink: 1; min-width: 0; }
  .time { margin-left: auto; flex-shrink: 0; }
  .title { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; margin: 3px 0 4px; font-size: 13px; }
  .meta { font-size: 11px; gap: 6px; flex-wrap: nowrap; overflow: hidden; }
  .meta > span { white-space: nowrap; }
  .right { margin-left: auto; flex-shrink: 0; }
  .pill { padding: 0 5px; border-radius: 3px; border: 1px solid var(--line); font-family: var(--mono); font-size: 10px; color: var(--fg2); flex-shrink: 0; }
  .pill.tag { background: color-mix(in srgb, var(--user) 18%, transparent); border-color: color-mix(in srgb, var(--user) 50%, transparent); color: var(--fg); cursor: pointer; line-height: 1.4; }
  .pill.tag:hover { background: color-mix(in srgb, var(--user) 35%, transparent); }
  .stats { display: flex; gap: 6px; overflow: hidden; min-width: 0; flex-shrink: 1; }
  .stats > span { white-space: nowrap; }
  .tog { display: inline-flex; align-items: center; gap: 4px; padding: 2px 8px 2px 5px; font-size: 11px; font-weight: 600; line-height: 1.3; border-radius: 10px; border: 1px solid var(--sub); background: transparent; color: var(--sub); cursor: pointer; flex-shrink: 0; }
  .tog:hover { background: var(--sub); color: #fff; }
  .tog .chev { display: inline-block; font-size: 10px; transition: transform .12s; }
  .tog.open .chev { transform: rotate(90deg); }
  button.agent { text-transform: uppercase; }
  button.agent.on.claude { background: var(--claude); border-color: var(--claude); color: #fff; }
  button.agent.on.codex { background: var(--codex); border-color: var(--codex); color: #fff; }
  button.agent.on.pi { background: var(--pi); border-color: var(--pi); color: #fff; }
  button.agent:not(.on) { color: var(--fg3); }
  button.source.on { background: var(--fg3); border-color: var(--fg3); color: var(--bg); }
  button.source:not(.on) { color: var(--fg3); }
</style>
