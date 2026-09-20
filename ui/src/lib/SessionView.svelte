<script lang="ts">
  import type { Agent, Detail, Event, Kind, Session, Telemetry } from './types'
  import { fmtDate, fmtDur, fmtNum, fmtCost, fmtMs, fmtSecs, project, totalTokens } from './fmt'
  import EventCard from './EventCard.svelte'
  import Flow from './Flow.svelte'
  import Stats from './Stats.svelte'
  import { live } from './live.svelte'
  import { tick, untrack } from 'svelte'

  let { agent, id, byKey, depth, mode = $bindable('events') }: { agent: Agent; id: string; byKey: Map<string, Session>; depth: number; mode?: 'events' | 'flow' | 'stats' } = $props()

  let detail = $state<Detail | null>(null)
  let tele = $state<Telemetry | null>(null)
  const s = $derived(detail?.summary)
  let error = $state('')
  let show = $state<Record<Kind, boolean>>({
    user: true, assistant: true, thinking: true, tool_call: true, tool_result: true, subagent: true, model_change: true, compaction: true, system: true,
  })
  let toolFilter = $state('')
  let newTag = $state('')

  async function editTags(edit: { add?: string[]; remove?: string[] }) {
    const r = await fetch(`/api/sessions/${agent}/${encodeURIComponent(id)}/tags`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(edit) })
    if (r.ok && detail) detail.summary.tags = await r.json()
  }

  let root = $state<HTMLElement>()

  $effect(() => {
    detail = null
    tele = null
    error = ''
    untrack(load)
  })
  // live update: refetch in place (keeps scroll/expanded state), follow tail if already at bottom
  $effect(() => {
    live.tick[agent + '/' + id]
    untrack(() => detail && load())
  })
  // one fetch in flight; ticks arriving meanwhile trigger a single trailing reload
  let busy = false, dirty = false
  async function load() {
    if (busy) { dirty = true; return }
    busy = true
    try {
      const r = await fetch(`/api/sessions/${agent}/${encodeURIComponent(id)}`)
      if (!r.ok) { error = String(r.status); return }
      const d: Detail = await r.json()
      // reuse unchanged event objects so keyed EventCards don't re-render (no flicker of JSON views / nested sessions)
      if (detail) {
        const old = new Map(detail.events.map((e) => [e.id, e]))
        d.events = d.events.map((e) => { const o = old.get(e.id); return o && JSON.stringify(o) === JSON.stringify(e) ? o : e })
      }
      const sc = root?.closest('main')
      const atBottom = sc && sc.scrollHeight - sc.scrollTop - sc.clientHeight < 80
      detail = d
      if (atBottom && depth === 0) { await tick(); sc.scrollTop = sc.scrollHeight }
      if (d.summary.agent === 'claude') loadTele()
    } finally {
      busy = false
      if (dirty) { dirty = false; load() }
    }
  }
  // separate request: optional, slow (external Loki/Prom) and never blocks the session itself
  async function loadTele() {
    try {
      const r = await fetch(`/api/sessions/${agent}/${encodeURIComponent(id)}/telemetry`)
      if (r.ok) tele = (await r.json()) as Telemetry | null
    } catch { /* telemetry is best-effort */ }
  }
  // one api_request per request_id, but thinking + assistant events share it: badge only the first
  const firstOfReq = $derived.by(() => {
    const seen = new Set<string>(), ids = new Set<number>()
    for (const e of detail?.events ?? []) if (e.request_id && !seen.has(e.request_id)) { seen.add(e.request_id); ids.add(e.id) }
    return ids
  })
  const rtk = $derived.by(() => {
    const calls = (detail?.events ?? []).filter((e) => e.meta?.rtk)
    if (!calls.length) return null
    const rows = calls.flatMap((e) => e.meta!.rtk as { input: number; saved: number }[])
    return { calls: calls.length, hit: calls.filter((e) => e.meta!.rtk.length).length, input: rows.reduce((a, r) => a + r.input, 0), saved: rows.reduce((a, r) => a + r.saved, 0) }
  })
  const hookMs = $derived(Object.values(tele?.hooks ?? {}).reduce((a, h) => a + h.total_ms, 0))
  const hookTitle = $derived(Object.entries(tele?.hooks ?? {}).map(([n, h]) => `${n}: ${h.count}× ${fmtMs(h.total_ms)}${h.blocking ? ` · ${h.blocking} blocking` : ''}${h.errors ? ` · ${h.errors} errors` : ''}`).join('\n'))
  const isLive = $derived(s?.ended && Date.now() - new Date(s.ended).getTime() < 120_000)

  const kinds: { k: Kind; label: string; color: string }[] = [
    { k: 'user', label: 'user', color: 'var(--user)' },
    { k: 'assistant', label: 'assistant', color: 'var(--assistant)' },
    { k: 'thinking', label: 'thinking', color: 'var(--thinking)' },
    { k: 'tool_call', label: 'tool call', color: 'var(--tool)' },
    { k: 'tool_result', label: 'result', color: 'var(--result)' },
    { k: 'subagent', label: 'subagent', color: 'var(--sub)' },
    { k: 'system', label: 'system', color: 'var(--sys)' },
  ]
  const counts = $derived.by(() => {
    const c: Partial<Record<Kind, number>> = {}
    for (const e of detail?.events ?? []) c[e.kind] = (c[e.kind] ?? 0) + 1
    return c
  })
  // tool_call_id -> call event, so result cards can show tool name + duration
  const calls = $derived(new Map((detail?.events ?? []).filter((e) => e.tool_call_id && e.kind !== 'tool_result').map((e) => [e.tool_call_id!, e])))
  const visible = $derived.by(() => {
    if (!detail) return []
    const ev = detail.events
    // hide tool_result when its tool_call is hidden by tool filter
    // toolFilter is a tool name, or "cmd:<prog>" for a shell program inside Bash/exec calls
    const match = (e: Event) => !toolFilter || (toolFilter.startsWith('cmd:') ? !!e.meta?.cmds?.includes(toolFilter.slice(4)) : e.tool_name === toolFilter)
    const callIds = new Set(ev.filter((e) => (e.kind === 'tool_call' || e.kind === 'subagent') && match(e)).map((e) => e.tool_call_id))
    return ev.filter((e) => {
      if (!show[e.kind]) return false
      if (toolFilter) {
        if (e.kind === 'tool_call' || e.kind === 'subagent') return match(e)
        if (e.kind === 'tool_result') return callIds.has(e.tool_call_id)
      }
      return true
    })
  })
  const parent = $derived(s?.parent_id ? byKey.get(s.agent + '/' + s.parent_id) : undefined)
  // this session plus its subagents (recursively), for the stats tab
  const scope = $derived.by(() => {
    if (!s) return []
    const out: Session[] = []
    const walk = (x: Session) => {
      out.push(x)
      for (const cid of x.children) {
        const c = byKey.get(x.agent + '/' + cid)
        if (c) walk(c)
      }
    }
    walk(s)
    return out
  })
</script>

{#if error}
  <div class="pad muted">failed to load: {error}</div>
{:else if !detail || !s}
  <div class="pad dim">loading…</div>
{:else}
  <div class="head" class:nested={depth > 0} bind:this={root}>
    <div class="row">
      <span class="agent {s.agent}">{s.agent}</span>
      {#if isLive}<span class="livedot" title="active in last 2 min"></span>{/if}
      <span class="muted">{project(s.cwd)}</span>
      {#if s.source !== 'user'}<span class="src" title={s.path}>{s.source}</span>{/if}
      {#if s.git_branch}<span class="dim mono">⎇ {s.git_branch}</span>{/if}
      {#if s.version}<span class="dim mono">v{s.version}</span>{/if}
      <span class="dim">{fmtDate(s.started)} · {fmtDur(s.started, s.ended)}</span>
      {#if parent}<a class="dim" href="#/s/{parent.agent}/{encodeURIComponent(parent.id)}">↑ parent: {parent.title}</a>{/if}
      <span class="dim mono" style="margin-left:auto" title={s.path}>{s.id}</span>
    </div>
    <h2>{s.title}</h2>
    <div class="row stats">
      {#each s.models as m}<span class="badge model">{m}</span>{/each}
      <span title="input (uncached)">in {fmtNum(s.usage.input)}</span>
      <span title="cache read">cache↓ {fmtNum(s.usage.cache_read)}</span>
      <span title="cache write">cache↑ {fmtNum(s.usage.cache_write)}</span>
      <span title="output">out {fmtNum(s.usage.output)}</span>
      {#if s.usage.reasoning}<span title="reasoning tokens">think {fmtNum(s.usage.reasoning)}</span>{/if}
      <span class="muted">Σ {fmtNum(totalTokens(s.usage))}</span>
      {#if s.max_context}<span title="peak context: prompt tokens of the largest single API call{s.context_window ? ` (window ${fmtNum(s.context_window)})` : ''}">ctx {fmtNum(s.max_context)}{#if s.context_window} / {fmtNum(s.context_window)} ({Math.round((100 * s.max_context) / s.context_window)}%){/if}</span>{/if}
      {#if s.cost != null}<span class="muted" title="API list price equivalent (subscription plans bill differently)">{fmtCost(s.cost)}</span>{/if}
      {#if rtk}<span title="rtk history: rewritten shell calls / all shell calls · tokens rtk claims it filtered out of {fmtNum(rtk.input)} raw">rtk {rtk.hit}/{rtk.calls} calls · −{fmtNum(rtk.saved)} tok</span>{/if}
    </div>
    <div class="row tags">
      {#each s.tags as t}
        <span class="tag">{t}<button title="remove tag (meta-file tags come back on rescan)" onclick={() => editTags({ remove: [t] })}>×</button></span>
      {/each}
      <form onsubmit={(e) => { e.preventDefault(); if (newTag.trim()) { editTags({ add: [newTag] }); newTag = '' } }}>
        <input placeholder="+ tag" bind:value={newTag} />
      </form>
    </div>
    {#if tele}
      <div class="row stats tele" title="from Claude Code OTel telemetry (Loki / Prometheus)">
        <span class="dim">otel</span>
        {#if tele.cost_usd != null}<span title="cost reported by Claude Code ({tele.api_requests} api requests)">${tele.cost_usd.toFixed(tele.cost_usd >= 1 ? 2 : 3)}</span>{/if}
        {#if tele.active_cli_s != null}<span title="active time: cli / user">active {fmtSecs(tele.active_cli_s)}{#if tele.active_user_s != null} / {fmtSecs(tele.active_user_s)}{/if}</span>{/if}
        {#if tele.loc_added != null || tele.loc_removed != null}<span title="lines of code added / removed">+{tele.loc_added ?? 0} −{tele.loc_removed ?? 0}</span>{/if}
        {#if tele.commits}<span>{tele.commits} commit{tele.commits === 1 ? '' : 's'}</span>{/if}
        {#if tele.prs}<span>{tele.prs} PR{tele.prs === 1 ? '' : 's'}</span>{/if}
        {#if hookMs}<span title={hookTitle}>hooks {fmtMs(hookMs)}</span>{/if}
        {#if tele.start_type}<span class="dim">{tele.start_type}</span>{/if}
      </div>
    {/if}
    <div class="row filters">
      {#if depth === 0}
        <span class="seg">
          <button class:on={mode === 'events'} onclick={() => (mode = 'events')}>events</button>
          <button class:on={mode === 'flow'} onclick={() => (mode = 'flow')}>flow</button>
          <button class:on={mode === 'stats'} onclick={() => (mode = 'stats')} title="stats for this session + its subagents">stats</button>
        </span>
      {/if}
      {#each mode === 'stats' ? [] : kinds as k}
        {#if counts[k.k]}
          <button class:on={show[k.k]} style="--c:{k.color}" onclick={() => (show[k.k] = !show[k.k])}>
            <i style="background:{k.color}"></i>{k.label} {counts[k.k]}
          </button>
        {/if}
      {/each}
      {#if s.tools.length && mode !== 'stats'}
        <select bind:value={toolFilter}>
          <option value="">all tools</option>
          {#each s.tools as [n, c]}<option value={n}>{n} ({c})</option>{/each}
          {#if s.cmds?.length}
            <optgroup label="shell commands">
              {#each s.cmds as [n, c]}<option value={'cmd:' + n}>{n} ({c})</option>{/each}
            </optgroup>
          {/if}
        </select>
      {/if}
      {#if s.children.length}<span class="dim">{s.children.length} subagents</span>{/if}
    </div>
  </div>
  {#if mode === 'stats'}
    <Stats sessions={scope} scoped />
  {:else if mode === 'flow'}
    <Flow {detail} {byKey} />
  {:else}
    <div class="events">
      {#each visible as e (e.id)}
        <EventCard {e} call={e.kind === 'tool_result' ? calls.get(e.tool_call_id ?? '') : undefined} tele={e.request_id ? (firstOfReq.has(e.id) ? tele?.requests[e.request_id] : undefined) : e.kind === 'tool_result' && e.tool_call_id ? tele?.tools[e.tool_call_id] : undefined} {agent} sessionId={id} {byKey} {depth} />
      {/each}
    </div>
  {/if}
{/if}

<style>
  .pad { padding: 20px; }
  .livedot { width: 8px; height: 8px; border-radius: 50%; background: #3fb950; }
  .head { padding: 10px 16px; border-bottom: 1px solid var(--line); position: sticky; top: 0; background: var(--bg); z-index: 2; }
  .head.nested { position: static; background: var(--bg2); }
  h2 { margin: 4px 0 6px; font-size: 15px; font-weight: 600; }
  .stats { font-family: var(--mono); font-size: 11px; color: var(--fg2); gap: 12px; }
  .badge.model { color: var(--fg); }
  .tele { margin-top: 4px; }
  .tags { margin-top: 6px; gap: 4px; font-size: 11px; }
  .tag { display: inline-flex; align-items: center; gap: 2px; padding: 0 2px 0 6px; border-radius: 3px; font-family: var(--mono); background: color-mix(in srgb, var(--user) 18%, transparent); border: 1px solid color-mix(in srgb, var(--user) 50%, transparent); }
  .tag button { border: 0; background: transparent; padding: 0 3px; color: var(--fg3); font-size: 12px; line-height: 1; }
  .tag button:hover { color: var(--fg); }
  .tags input { width: 80px; padding: 1px 6px; font-size: 11px; }
  .filters { margin-top: 8px; gap: 6px; }
  .filters button { display: inline-flex; align-items: center; gap: 5px; font-size: 11px; }
  .filters button i { width: 8px; height: 8px; border-radius: 2px; display: inline-block; opacity: .5; }
  .filters button.on { background: var(--bg3); color: var(--fg); border-color: var(--line); }
  .filters button.on i { opacity: 1; }
  .filters button:not(.on) { color: var(--fg3); }
  .events { padding: 8px 16px 40px; }
  .seg { display: inline-flex; }
  .seg button { border-radius: 0; font-size: 11px; }
  .seg button:first-child { border-radius: 4px 0 0 4px; }
  .seg button:last-child { border-radius: 0 4px 4px 0; }
  .src { font-size: 10px; padding: 0 4px; border: 1px solid var(--line); border-radius: 3px; color: var(--fg2); }
</style>
