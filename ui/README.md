# agent-sessions ui

Svelte 5 + TypeScript + Vite single-page app. `npm run build` writes `dist/`, which the server embeds into its binary. See the [root README](../README.md) for what the app does.

```bash
npm install
npm run dev     # vite dev server; proxies /api to the server on 127.0.0.1:7777
npm run build   # -> dist/
npm run check   # svelte-check + tsc
```

Run the server (`cargo run` in `../server`) alongside `npm run dev`.

## Layout

| path | |
|---|---|
| `src/App.svelte` | shell + hash routing: session (default), `#/stats`, `#/compare` |
| `src/lib/SessionList.svelte` | list, filters (source, tag, skill, hook, text) |
| `src/lib/SessionView.svelte` | session detail: `events` / `flow` / `stats` modes, tag editing |
| `src/lib/EventCard.svelte` | one event; telemetry and rtk badges |
| `src/lib/Flow.svelte` | turn graph with spawned subagents |
| `src/lib/Stats.svelte` | aggregate stats, or `scoped` to one session + its subagents |
| `src/lib/Compare.svelte` | tagged sessions side by side, grouped by tag family |
| `src/lib/RangePicker.svelte`, `JsonView.svelte` | date range control, collapsible JSON |
| `src/lib/{live,range,sources,tags}.svelte.ts` | shared rune state: SSE updates, date range, source toggles, tags |
| `src/lib/types.ts`, `fmt.ts` | API types, formatters |

No router and no store library: routing is `location.hash`, shared state is module-level runes.
Per-browser preferences (source toggles) live in `localStorage`.
