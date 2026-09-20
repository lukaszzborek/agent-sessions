<script lang="ts">
  import type { Agent, Event, Session, TeleReq, TeleTool } from './types'
  import { fmtTime, fmtNum, fmtDur, fmtMs, fmtBytes } from './fmt'
  import SessionView from './SessionView.svelte'
  import JsonView from './JsonView.svelte'
  import { untrack } from 'svelte'

  let { e, call, tele, agent, sessionId, byKey, depth }: { e: Event; call?: Event; tele?: TeleReq | TeleTool; agent: Agent; sessionId: string; byKey: Map<string, Session>; depth: number } = $props()

  const COLLAPSE = 600
  // initial value only: card is keyed by event id, so `e` never changes for this instance
  let open = $state(untrack(() => e.kind === 'assistant' || (e.text?.length ?? 0) <= COLLAPSE))
  let full = $state<string | null>(null)
  let showChild = $state(false)
  let raw = $state(false)

  const text = $derived(full ?? e.text ?? '')
  const child = $derived(e.child_session_id ? byKey.get(agent + '/' + e.child_session_id) : undefined)
  const color: Record<string, string> = {
    user: 'var(--user)', assistant: 'var(--assistant)', thinking: 'var(--thinking)', tool_call: 'var(--tool)',
    tool_result: 'var(--result)', subagent: 'var(--sub)', model_change: 'var(--fg2)', compaction: 'var(--fg2)', system: 'var(--sys)',
  }
  const label = $derived.by(() => {
    switch (e.kind) {
      case 'tool_call': return e.tool_name ?? 'tool'
      case 'tool_result': { const n = e.tool_name ?? call?.tool_name; return 'result' + (n ? ' ← ' + n : '') }
      case 'subagent': return (e.tool_name ?? 'subagent') + (e.meta?.subagent_type || e.meta?.agent_type ? ` · ${e.meta?.subagent_type ?? e.meta?.agent_type}` : '')
      case 'model_change': return 'model'
      default: return e.kind
    }
  })

  async function loadFull() {
    const r = await fetch(`/api/sessions/${agent}/${encodeURIComponent(sessionId)}/events/${e.id}`)
    full = ((await r.json()) as Event).text ?? ''
    open = true
  }
  function expand() {
    open = true
    if (e.truncated && full == null) loadFull()
  }
  const oneLine = $derived(e.kind === 'model_change' || e.kind === 'compaction')
  const dur = $derived.by(() => {
    if (e.kind !== 'tool_result' || !call?.ts || !e.ts) return ''
    const ms = new Date(e.ts).getTime() - new Date(call.ts).getTime()
    return ms < 1000 ? `${Math.max(0, ms)}ms` : ms < 60000 ? `${(ms / 1000).toFixed(1)}s` : fmtDur(call.ts, e.ts)
  })
  // shell tool call: show command as code, description in header (claude Bash, codex exec/shell, pi bash)
  const shellName = (n?: string) => !!n && /^(bash|shell|exec_command|shell_command|local_shell)$/i.test(n)
  const shell = $derived.by(() => {
    if (e.kind !== 'tool_call' || !shellName(e.tool_name) || json == null || typeof json !== 'object') return undefined
    const j = json as Record<string, unknown>
    const cmd = j.command ?? j.cmd
    const command = Array.isArray(cmd) ? cmd.join(' ') : typeof cmd === 'string' ? cmd : undefined
    if (!command) return undefined
    const rest = Object.fromEntries(Object.entries(j).filter(([k]) => k !== 'command' && k !== 'cmd' && k !== 'description'))
    return { command, description: typeof j.description === 'string' ? j.description : undefined, rest }
  })
  // claude already appends stderr to the result text; only show separately when it isn't there
  const stderr = $derived.by(() => { const s = (e.meta?.stderr as string | undefined)?.trim(); return s && !text.includes(s) ? s : undefined })
  // rows rtk logged for this shell call; empty = the hook left the command alone
  const rtk = $derived.by(() => {
    const rows = e.meta?.rtk as { cmd: string; rtk_cmd: string; input: number; output: number; saved: number }[] | undefined
    if (!rows) return undefined
    if (!rows.length) return { rows: 0, t: 'no rtk', title: 'rtk did not rewrite this command' }
    const saved = rows.reduce((a, r) => a + r.saved, 0)
    const title = rows.map((r) => `${r.rtk_cmd}: ${r.input} → ${r.output} tok  (${r.cmd})`).join('\n')
    return { rows: rows.length, t: `rtk −${fmtNum(saved)} tok`, title }
  })
  const flags = $derived.by(() => {
    const m = e.meta ?? {}
    const f: string[] = []
    if (m.interrupted) f.push('interrupted')
    if (m.timedOutAfterMs) f.push(`timeout ${Math.round(m.timedOutAfterMs / 1000)}s`)
    if (m.backgroundTaskId) f.push('background')
    if (m.dangerouslyDisableSandbox) f.push('no sandbox')
    if (m.exit_code != null) f.push(`exit ${m.exit_code}`)
    if (m.returnCodeInterpretation) f.push(String(m.returnCodeInterpretation))
    return f
  })
  // OTel overlay (claude only): api latency on assistant/thinking, tool timing + decision on results
  const otel = $derived.by(() => {
    if (!tele) return []
    const out: { t: string; title: string; err?: boolean }[] = []
    if ('ttft_ms' in tele || 'cost_usd' in tele) {
      const r = tele as TeleReq
      if (r.ttft_ms != null) out.push({ t: `ttft ${fmtMs(r.ttft_ms)}`, title: 'time to first token' })
      if (r.duration_ms != null) out.push({ t: fmtMs(r.duration_ms), title: 'api request duration' })
      if (r.effort) out.push({ t: r.effort, title: 'effort' })
      if (r.speed && r.speed !== 'normal') out.push({ t: r.speed, title: 'speed' })
    } else {
      const r = tele as TeleTool
      if (r.duration_ms != null) out.push({ t: fmtMs(r.duration_ms), title: 'tool execution (otel)' })
      if (r.result_bytes != null) out.push({ t: fmtBytes(r.result_bytes), title: 'tool result size' })
      if (r.success === false) out.push({ t: 'failed', title: 'tool reported failure', err: true })
      if (r.decision && r.decision !== 'accept') out.push({ t: r.decision, title: `decision (${r.decision_source ?? ''})`, err: true })
      else if (r.decision_source === 'user') out.push({ t: 'asked', title: 'permission prompt shown to user' })
    }
    return out
  })
  // structured view for tool inputs/outputs that are JSON objects/arrays (truncated text won't parse -> raw)
  const json = $derived.by(() => {
    if (e.kind !== 'tool_call' && e.kind !== 'tool_result' && e.kind !== 'subagent') return undefined
    const t = text.trim()
    if (!(t.startsWith('{') || t.startsWith('['))) return undefined
    try { return JSON.parse(t) as unknown } catch { return undefined }
  })
</script>

{#if oneLine}
  <div class="marker" style="--c:{color[e.kind]}"><span class="dim">{fmtTime(e.ts)}</span> <span class="badge">{e.text}</span></div>
{:else}
  <div class="card" style="--c:{color[e.kind]}" class:err={e.is_error}>
    <div class="hdr row">
      <span class="k">{label}</span>
      <span class="dim">{fmtTime(e.ts)}</span>
      {#if (e.text?.length ?? 0) > COLLAPSE}<button class="sm" onclick={() => (open = !open)}>{open ? '▴ less' : '▾ more'}</button>{/if}
      {#if json !== undefined}<button class="sm" class:on={raw} onclick={() => (raw = !raw)} title="toggle raw JSON">raw</button>{/if}
      {#if e.kind === 'subagent' && e.meta?.description}<span class="muted">{e.meta.description}</span>{/if}
      {#if e.model && (e.kind === 'assistant' || e.kind === 'thinking')}<span class="dim mono">{e.model}</span>{/if}
      {#if e.kind === 'subagent' && (e.meta?.resolvedModel || e.meta?.model)}<span class="dim mono" title="subagent model">{e.meta.resolvedModel ?? e.meta.model}</span>{/if}
      {#if e.is_error}<span style="color:var(--err)">error</span>{/if}
      {#if dur}<span class="dim mono" title="call → result">{dur}</span>{/if}
      {#each otel as o}<span class="otel" class:oerr={o.err} title={o.title}>{o.t}</span>{/each}
      {#each flags as f}<span class="flag">{f}</span>{/each}
      {#if e.meta?.cmds}<span class="cmds">{e.meta.cmds.join(' · ')}</span>{/if}
      {#if rtk}<span class="otel" class:oerr={!rtk.rows} title={rtk.title}>{rtk.t}</span>{/if}
      {#if shell?.description}<span class="muted">{shell.description}</span>{/if}
      {#if e.usage}
        <span class="dim mono" title="in / cache read / out">
          {fmtNum(e.usage.input)} / {fmtNum(e.usage.cache_read)}↓ / {fmtNum(e.usage.output)}
        </span>
      {/if}
      {#if e.context}<span class="dim mono" title="context: prompt tokens of this API call">{fmtNum(e.context)}ctx</span>{/if}
      <span style="flex:1"></span>
      {#if child}
        <button class="sm" onclick={() => (showChild = !showChild)} style="color:var(--sub)">
          {showChild ? '▾' : '▸'} {child.title} · {child.tool_calls} tools · {fmtNum(child.usage.output)} out
        </button>
        <a class="dim" href="#/s/{agent}/{encodeURIComponent(child.id)}" title="open subagent session">open</a>
      {:else if e.kind === 'subagent' && e.child_session_id}
        <span class="dim">child {e.child_session_id} (not indexed)</span>
      {/if}
    </div>
    {#if text}
      <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
      <div class="body" class:clamp={!open} role={open ? undefined : 'button'} onclick={open ? undefined : expand} title={open ? undefined : 'click to expand'}>
        {#if shell && !raw}
          <pre class="cmd">{shell.command}</pre>
          {#if Object.keys(shell.rest).length}<div class="json"><JsonView v={shell.rest} /></div>{/if}
        {:else if json !== undefined && !raw}
          <div class="json"><JsonView v={json} /></div>
        {:else}
          <pre>{text}</pre>
        {/if}
        {#if open && stderr}<pre class="stderr">{stderr}</pre>{/if}
        {#if !open}<div class="more">▾ show all{e.truncated ? ' (load full)' : ''}</div>{/if}
      </div>
      {#if e.truncated && full == null && open}
        <button class="sm" onclick={loadFull}>load full ({e.kind === 'tool_result' ? 'output' : 'text'} truncated)</button>
      {/if}
    {/if}
    {#if showChild && child}
      <div class="child">
        <SessionView agent={child.agent} id={child.id} {byKey} depth={depth + 1} />
      </div>
    {/if}
  </div>
{/if}

<style>
  .card { border-left: 3px solid var(--c); background: var(--bg2); margin: 6px 0; border-radius: 0 4px 4px 0; }
  .card.err { background: #2a1616; }
  .cmds { color: var(--tool); font-family: var(--mono); }
  .flag { color: var(--err); font-family: var(--mono); border: 1px solid var(--err); border-radius: 3px; padding: 0 4px; }
  .otel { color: var(--fg3); font-family: var(--mono); font-size: 10px; border: 1px solid var(--line); border-radius: 3px; padding: 0 4px; }
  .otel.oerr { color: var(--err); border-color: var(--err); }
  pre.cmd { background: var(--bg); border: 1px solid var(--line); border-radius: 3px; margin: 8px 10px; padding: 6px 8px; }
  pre.stderr { color: var(--err); border-top: 1px dashed var(--line); }
  .hdr { padding: 4px 10px; font-size: 11px; border-bottom: 1px solid var(--line); }
  .k { color: var(--c); font-weight: 600; font-family: var(--mono); }
  pre { padding: 8px 10px; max-height: none; }
  .json { padding: 8px 10px; }
  .body { position: relative; }
  .body.clamp { max-height: 140px; overflow: hidden; cursor: pointer; }
  .body.clamp > :first-child { mask-image: linear-gradient(#000 50%, transparent 90%); }
  .body.clamp:hover { background: var(--bg3); }
  .more { position: absolute; left: 0; right: 0; bottom: 0; padding: 3px 10px; font-size: 11px; color: var(--fg2); font-family: var(--mono); background: linear-gradient(transparent, var(--bg2) 40%); }
  .body.clamp:hover .more { color: var(--fg); }
  .marker { margin: 10px 0; font-size: 11px; display: flex; gap: 8px; align-items: center; }
  .marker::after { content: ''; flex: 1; border-top: 1px dashed var(--line); }
  button.sm { font-size: 11px; padding: 0 6px; margin: 4px 10px 6px; }
  .hdr button.sm { margin: 0; }
  .child { margin: 6px; border: 1px solid var(--sub); border-radius: 4px; background: var(--bg); }
</style>
