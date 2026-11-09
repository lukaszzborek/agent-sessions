<script lang="ts">
  import { range, limits, applyPreset, type Preset } from './range.svelte'

  const fmt = (ms: number) => new Date(ms).toLocaleString(undefined, { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' })
</script>

<select value={range.preset} onchange={(e) => applyPreset(e.currentTarget.value as Preset)}>
  <option value="today">today</option><option value="7d">last 7 days</option><option value="30d">last 30 days</option><option value="90d">last 90 days</option>
  <option value="mtd">month to date</option><option value="prev">previous month</option><option value="all">all</option>
  {#if limits.claudeWeek}<option value="claude-wk">claude limit week</option><option value="claude-prev-wk">claude prev limit week</option>{/if}
  {#if limits.codexResets.length}<option value="codex-wk">codex limit week</option><option value="codex-prev-wk">codex prev limit week</option>{/if}
  <option value="custom">custom</option>
</select>
<input type="date" bind:value={range.from} oninput={() => applyPreset('custom')} title="from (inclusive)" />
<span class="dim">–</span>
<input type="date" bind:value={range.to} oninput={() => applyPreset('custom')} title="to (inclusive)" />
{#if range.start}<span class="dim" title="{range.agent} sessions only">{fmt(range.start)} → {fmt(range.end)}</span>{/if}
