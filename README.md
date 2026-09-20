# agent-sessions

Local web UI over coding-agent session logs (Claude Code, Codex, Pi). Sessions are archived into SQLite, so they outlive the agents' own log cleanup.

- [Run](#run)
- [What it shows](#what-it-shows)
- [Storage and live updates](#storage-and-live-updates)
- [Config](#config): [sources](#sources), [tags](#tags), [prices](#prices), [telemetry](#telemetry), [rtk](#rtk)
- [Development](#development)
- [License](#license)

## Run

```bash
git clone https://github.com/lukaszzborek/agent-sessions.git && cd agent-sessions
just run                                          # builds ui + release binary, serves http://127.0.0.1:7777
PORT=8000 ./server/target/release/agent-sessions  # other port
./server/target/release/agent-sessions --open     # also opens the browser (Windows browser on WSL)
```

Needs `just`, `npm` and `cargo`. The UI is embedded into the binary, so the release binary is all you need afterwards.

Everything stays local: the server binds to `127.0.0.1` only and has no auth, and nothing is sent anywhere
(the optional [telemetry](#telemetry) integration only reads from your own Loki / Prometheus).
Session logs contain prompts, file contents and command output, so do not expose the port beyond localhost.

With no config it reads only the current user's logs (source `user`):

| agent | location |
|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` (+ `subagents/agent-*.jsonl`) |
| Codex | `~/.codex/sessions/**/*.jsonl` (titles from `~/.codex/session_index.jsonl`) |
| Pi | `~/.pi/agent/sessions/**/*.jsonl` |

Anything else is added as a [source](#sources).

## What it shows

**List** - all sessions, filterable by source, tag, skill, hook and free text. Sessions active in the last 2 min get a green dot, archived ones 🗄.

**Session** - three modes:

- `events`: user / assistant / thinking, tool calls + results, models used (with switch markers), compactions, token usage and cost per event. Subagents expand inline and link to their child sessions.
- `flow`: turn-by-turn graph with the subagents each turn spawned hanging below it.
- `stats`: the stats view scoped to this session + its subagents.

**Stats** (`#/stats`) - date range, a metric (cost / out tok / total tok / tool calls) and a dimension (model / agent / subagent / project):
totals, stacked per-day chart, table by the chosen dimension, table by day (click a day to focus it), then usage lists:
tool calls, subagent types, shell commands, skills and hooks (last two Claude only).

**Compare** (`#/compare`) - sessions sharing a tag side by side, grouped by a tag family. See [tags](#tags).

Cost: Pi logs carry their own cost; for Claude and Codex it is computed from token usage and the [price table](#prices).
Tokens and cost are attributed per API call, so a session that switched models is split between them.

## Storage and live updates

DB: `~/.local/share/agent-sessions/sessions.db`. Each source file is stored zstd-compressed with its parsed summary, keyed by path + mtime + size.
Files deleted on disk stay in the DB, marked archived; the detail view falls back to the stored copy. `↻` rescans.

Data dir was renamed from `ai-sessions` to `agent-sessions`; on first run after upgrading, an old `~/.local/share/ai-sessions` is
moved automatically. The config file moved too (`ai-sessions/config.json` -> `agent-sessions/config.toml`) but the format changed,
so it is not auto-converted - a startup message flags an old config.json left behind.

The server polls the session dirs every 2 s. On change it rebuilds the index and pushes the affected session keys over SSE;
the UI refreshes the list and the open session in place, following the tail when scrolled to the bottom.
Every 10 min a background pass stores telemetry and rtk history for the sessions that have them, so those survive too.

## Config

`~/.config/agent-sessions/config.toml`, all keys optional. Read once at startup; on a parse error the error is logged and defaults are used.

```toml
[telemetry]
enabled = true                           # false: OTel telemetry not fetched, stored or shown
loki_url = "https://loki.example"        # env LOKI_URL overrides
prom_url = "https://prometheus.example"  # env PROM_URL overrides

[rtk]
enabled = true                           # false: rtk history not read, stored or shown

[[sources]]
name = "windows"
path = "/mnt/c/Users/me"

[[sources]]
name = "bench"
path = "/home/me/bench/results"
claude = "**/claude-projects"
codex = "**/codex-sessions"
meta = "meta.json"

[[sources]]
name = "ci"
path = "/srv/ci"
claude = "logs"
enabled = false
```

### Sources

Every session carries a `source`: `user` for the built-in home dirs, otherwise the `name` of the `sources` entry it was found under.
The list has one toggle per source (only `user` on by default, remembered per browser); stats and compare follow the same selection.

| key | |
|---|---|
| `name` | required, unique, `user` is reserved; invalid entries are logged and skipped |
| `path` | required, base dir |
| `claude` / `codex` / `pi` | glob relative to `path` matching dirs that hold that agent's `*.jsonl` logs (each walked recursively; `"."` = `path` itself). None of the three set = `path` is a home dir: `.claude/projects`, `.codex/sessions`, `.pi/agent/sessions` |
| `meta` | name of a JSON file carrying `tags` for the sessions below it, see [tags](#tags) |
| `enabled` | default `true`; `false` keeps the entry but skips indexing |

Nothing is auto-detected: on WSL the Windows-side CLIs' logs need an entry like `windows` above.

### Tags

Every session has `tags`: list filter, search, pills, editable in the session header. They are stored in the DB.

Automatic tags: whatever produces the sessions drops a JSON file next to them and the source names it in `meta`.

```jsonc
// <path>/run-123/1/meta.json  ->  applies to every session file below run-123/1/
{ "tags": ["task:fix-utf8", "arm:rtk", "rep:3", "grade:pass"] }
```

Only `tags` (array of strings) is read, so the file may be the producer's own metadata with that one key added.
It is looked up in the ancestor dirs of each session file up to the source `path`, nearest wins. The source name is added as a tag too.
Tags are persisted with the session, so they survive the files being cleaned up, and are refreshed when the meta file changes or appears late.

`key:value` tags form families. Compare shows the sessions with a tag side by side, grouped by any family (`arm` when present),
with per-group medians and Δ vs the first group. Conventions it understands:
`grade:pass` / `grade:fail` (pass rate), `rep:<n>` (repetition column); `task:` / `trial:` are kept off the list pills.

### Prices

Costs use a built-in USD-per-1M-token table (`server/src/pricing.rs`). Add models or override prices in
`~/.config/agent-sessions/prices.toml` - kept apart from `config.toml` so it can be shared as is:

```toml
# DeepSeek list prices, 2026-09
[deepseek-chat]
input = 0.27
output = 1.10
cache_read = 0.07        # optional, defaults to input
# cache_write_5m, cache_write_1h: optional, default to input

["gpt-5.6-sol"]          # ids containing a dot must be quoted
input = 4.0
output = 20.0
```

The table name is a model id prefix; the longest matching prefix wins and a provider prefix (`deepseek/…`) is ignored.
Read once at startup; changing any price recomputes the stored cost of all sessions on the next start.

### Telemetry

Optional, Claude only. Point `loki_url` / `prom_url` at the Loki / Prometheus that receive Claude Code's OTel export
(`CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_METRICS_INCLUDE_SESSION_ID=true`; full setup in [docs/telemetry-brief.md](docs/telemetry-brief.md)).

The session detail then shows:

- per request: `ttft`, duration, effort (joined by `request_id`)
- per tool call: execution time, result size, permission decision (joined by `tool_use_id`)
- an `otel` row: reported cost, active time, lines added / removed, commits, PRs, hook overhead

It is fetched separately from the session and any failure yields nothing, so other agents' sessions and pre-telemetry Claude sessions are unaffected.

### rtk

For bench runs whose trial dir (the dir holding the source's `meta` file) contains `rtk-data/history.db`:
every shell tool call is annotated with the rtk rows it produced (`rtk −N tok`), or `no rtk` when rtk left the command alone.
The history is copied into the DB per session, so it survives the trial dir being deleted.

## Development

```bash
cd server && cargo run   # API on :7777; needs ui/dist to exist (can be empty)
just dev-ui              # vite dev server, proxies /api to :7777
```

| path | |
|---|---|
| `server/src/main.rs` | axum API + embedded static UI, watch loop, background enrichment |
| `server/src/index.rs` | source discovery, parallel parse, DB sync, parent/child linking, meta tags |
| `server/src/parsers/{claude,codex,pi}.rs` | log format -> normalized `Event` list |
| `server/src/model.rs` | `SessionSummary`, `Event`, `Usage` |
| `server/src/db.rs` | SQLite store (raw blob + summary per file, tags, telemetry, rtk) |
| `server/src/config.rs` | config file + `Source` definition |
| `server/src/pricing.rs` | price table + `prices.toml` |
| `server/src/extensions/{telemetry,rtk}.rs` | optional per-session enrichment |
| `ui/src/App.svelte` | hash routing: session / `#/stats` / `#/compare` |
| `ui/src/lib/*.svelte` | `SessionList`, `SessionView`, `EventCard`, `Flow`, `Stats` (aggregate or scoped), `Compare`, `RangePicker` |
| `ui/src/lib/*.svelte.ts` | shared state: live updates, date range, sources, tags |

### API

| | |
|---|---|
| `GET /api/sessions` | all session summaries |
| `GET /api/sessions/:agent/:id` | session detail (long event bodies truncated) |
| `GET /api/sessions/:agent/:id/events/:eid` | one event, untruncated |
| `GET /api/sessions/:agent/:id/telemetry` | OTel enrichment; `null` when unset, not Claude, or nothing recorded |
| `POST /api/sessions/:agent/:id/tags` | `{"add":[…],"remove":[…]}` |
| `POST /api/refresh` | rescan |
| `GET /api/watch` | SSE, `["agent/id", …]` per change |

## License

[Apache-2.0](LICENSE)
