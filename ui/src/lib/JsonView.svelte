<script lang="ts">
  import JsonView from './JsonView.svelte'
  let { v }: { v: unknown } = $props()
  const entries = $derived(Array.isArray(v) ? v.map((x, i) => [String(i), x] as const) : Object.entries(v as object))
  const short = (s: string) => !s.includes('\n') && s.length <= 100
</script>

{#if entries.length === 0}
  <span class="lit">{Array.isArray(v) ? '[]' : '{}'}</span>
{:else}
  <div class="obj">
    {#each entries as [k, x]}
      <div class="row">
        <span class="key">{k}</span>
        <div class="val">
          {#if x !== null && typeof x === 'object'}
            <JsonView v={x} />
          {:else if typeof x === 'string'}
            {#if short(x)}<span class="str">{x}</span>{:else}<pre class="str">{x}</pre>{/if}
          {:else}
            <span class="lit">{JSON.stringify(x)}</span>
          {/if}
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .obj { display: flex; flex-direction: column; gap: 3px; }
  .row { display: flex; gap: 8px; align-items: baseline; }
  .key { color: var(--fg2); font: 11px/1.5 var(--mono); flex: 0 0 auto; min-width: 70px; }
  .val { flex: 1; min-width: 0; }
  .str { color: var(--fg); font: 12px/1.4 var(--mono); white-space: pre-wrap; word-break: break-word; }
  pre.str { background: var(--bg); border: 1px solid var(--line); border-radius: 3px; padding: 6px 8px; margin: 1px 0; }
  .lit { color: var(--pi); font: 12px/1.4 var(--mono); }
  .val :global(.obj) { border-left: 1px solid var(--line); padding-left: 8px; margin: 2px 0; }
</style>
