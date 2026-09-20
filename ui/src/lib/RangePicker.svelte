<script lang="ts">
  import { range, applyPreset, type Preset } from './range.svelte'
</script>

<select value={range.preset} onchange={(e) => applyPreset(e.currentTarget.value as Preset)}>
  <option value="7d">last 7 days</option><option value="30d">last 30 days</option><option value="90d">last 90 days</option>
  <option value="mtd">month to date</option><option value="prev">previous month</option><option value="all">all</option>
  <option value="custom">custom</option>
</select>
<input type="date" bind:value={range.from} oninput={() => (range.preset = 'custom')} title="from (inclusive)" />
<span class="dim">–</span>
<input type="date" bind:value={range.to} oninput={() => (range.preset = 'custom')} title="to (inclusive)" />
