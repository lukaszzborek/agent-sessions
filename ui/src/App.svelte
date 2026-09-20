<script lang="ts">
  import type { Session } from './lib/types'
  import SessionList from './lib/SessionList.svelte'
  import SessionView from './lib/SessionView.svelte'
  import Stats from './lib/Stats.svelte'
  import Compare from './lib/Compare.svelte'
  import { live } from './lib/live.svelte'
  import { sourceFilter } from './lib/sources.svelte'

  let sessions = $state<Session[]>([])
  let loading = $state(true)
  let route = $state(location.hash)
  let listWidth = $state(380)
  let mode = $state<'events' | 'flow'>('events')

  const visible = $derived(sessions.filter((s) => sourceFilter.on.includes(s.source)))
  // `user` first
  const sources = $derived([...new Set(sessions.map((s) => s.source))].sort((a, b) => +(b === 'user') - +(a === 'user') || a.localeCompare(b)))
  const byKey = $derived(new Map(sessions.map((s) => [s.agent + '/' + s.id, s])))

  // keep object identity for unchanged sessions so downstream $derived/props don't re-run on every push
  async function load() {
    loading = true
    const fresh: Session[] = await (await fetch('/api/sessions')).json()
    const old = new Map(sessions.map((s) => [s.agent + '/' + s.id, s]))
    sessions = fresh.map((s) => {
      const o = old.get(s.agent + '/' + s.id)
      return o && JSON.stringify(o) === JSON.stringify(s) ? o : s
    })
    loading = false
  }
  async function refresh() {
    loading = true
    await fetch('/api/refresh', { method: 'POST' })
    await load()
  }
  load()
  window.addEventListener('hashchange', () => (route = location.hash))

  // live: server pushes keys of sessions whose files changed; refresh list + bump per-session tick
  // coalesce: one list fetch in flight at a time, changes arriving meanwhile are batched into the next one
  const es = new EventSource('/api/watch')
  let pending: string[] = []
  let pulling = false
  es.onmessage = (m) => {
    pending.push(...(JSON.parse(m.data) as string[]))
    if (!pulling) pull()
  }
  async function pull() {
    pulling = true
    while (pending.length) {
      const keys = pending
      pending = []
      await load()
      for (const k of keys) live.tick[k] = (live.tick[k] ?? 0) + 1
    }
    pulling = false
  }

  const sel = $derived.by(() => {
    const m = route.match(/^#\/s\/(\w+)\/([^/]+)/)
    return m ? { agent: m[1] as Session['agent'], id: decodeURIComponent(m[2]) } : null
  })
  const view = $derived(route.startsWith('#/stats') ? 'stats' : route.startsWith('#/compare') ? 'compare' : 'session')
  const compareTag = $derived(decodeURIComponent(route.match(/^#\/compare\/([^/]+)/)?.[1] ?? ''))

  // drag resize
  function startDrag(e: MouseEvent) {
    const x0 = e.clientX, w0 = listWidth
    const mv = (e: MouseEvent) => (listWidth = Math.max(240, Math.min(800, w0 + e.clientX - x0)))
    const up = () => { window.removeEventListener('mousemove', mv); window.removeEventListener('mouseup', up) }
    window.addEventListener('mousemove', mv); window.addEventListener('mouseup', up)
  }
</script>

<div class="layout">
  <aside style="width:{listWidth}px">
    <header class="row">
      <strong>Agent Sessions</strong>
      <span class="dim">{visible.length}</span>
      <span style="flex:1"></span>
      <a href="#/stats" class:on={view === 'stats'} class="tab">stats</a>
      <a href="#/compare" class:on={view === 'compare'} class="tab" title="compare tagged sessions side by side">compare</a>
      <button onclick={refresh} disabled={loading} title="rescan session files">{loading ? '…' : '↻'}</button>
    </header>
    <SessionList sessions={visible} {sources} selected={sel} />
  </aside>
  <button class="gutter" onmousedown={startDrag} aria-label="resize sidebar"></button>
  <main>
    {#if view === 'stats'}
      <Stats sessions={visible} />
    {:else if view === 'compare'}
      <Compare sessions={visible} tag={compareTag} />
    {:else if sel}
      {#key route}
        <SessionView agent={sel.agent} id={sel.id} {byKey} depth={0} bind:mode />
      {/key}
    {:else}
      <div class="empty dim">select session</div>
    {/if}
  </main>
</div>

<style>
  .layout { display: flex; height: 100%; }
  aside { display: flex; flex-direction: column; border-right: 1px solid var(--line); background: var(--bg2); flex-shrink: 0; }
  header { padding: 8px 10px; border-bottom: 1px solid var(--line); }
  .tab { text-decoration: none; padding: 2px 8px; border-radius: 4px; color: var(--fg2); }
  .tab.on { background: var(--bg3); color: var(--fg); }
  .gutter { width: 4px; padding: 0; border: 0; border-radius: 0; cursor: col-resize; background: transparent; }
  .gutter:hover { background: var(--line); }
  main { flex: 1; overflow: auto; min-width: 0; }
  .empty { padding: 40px; text-align: center; }
</style>
