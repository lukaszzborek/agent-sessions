<script lang="ts">
  import type { Session } from './types'
  import { shortHook } from './fmt'

  // suffix: appended to every heading, e.g. the focused day
  let { sessions, suffix = '' }: { sessions: Session[]; suffix?: string } = $props()

  // name -> [calls, sessions]
  function usage(pick: (s: Session) => [string, number][]) {
    const m = new Map<string, [number, number]>()
    for (const s of sessions) for (const [n, c] of pick(s)) {
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

{#each lists as [title, hint, items]}
  <h3 title={hint}>{title}{suffix}</h3>
  <div class="tools">
    {#each items as [n, [c, ns]]}
      <div class="trow"><span class="mono" title={n}>{title === 'Hooks' ? shortHook(n) : n}</span><div class="bar" style="width:{(100 * c) / items[0][1][0]}%"></div><span class="dim">{c} <span class="dim2">in {ns} sessions</span></span></div>
    {/each}
    {#if !items.length}<span class="dim">none</span>{/if}
  </div>
{/each}

<style>
  h3 { font-size: 13px; margin: 24px 0 8px; color: var(--fg2); }
  .tools { display: grid; grid-template-columns: max-content 1fr max-content; gap: 3px 10px; align-items: center; max-width: 700px; font-size: 12px; }
  .trow { display: contents; }
  .tools .bar { height: 10px; background: var(--tool); opacity: .6; border-radius: 2px; }
  .dim2 { color: var(--fg3); font-size: 11px; }
</style>
