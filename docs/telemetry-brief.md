# Claude Code telemetry setup

How to configure Claude Code's OTel export so that agent-sessions can enrich Claude sessions
(see [Telemetry](../README.md#telemetry)). Covers `~/.claude/settings.json` and the Prometheus / Loki side.

## Claude Code config

`~/.claude/settings.json` → `env` (replace the hosts with your Prometheus OTLP receiver and Loki):

```json
"CLAUDE_CODE_ENABLE_TELEMETRY": "1",
"OTEL_METRICS_EXPORTER": "otlp",
"OTEL_EXPORTER_OTLP_PROTOCOL": "http/protobuf",
"OTEL_EXPORTER_OTLP_ENDPOINT": "https://prometheus.example/api/v1/otlp",
"OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE": "cumulative",
"OTEL_METRIC_EXPORT_INTERVAL": "60000",
"OTEL_METRICS_INCLUDE_SESSION_ID": "true",
"OTEL_LOGS_EXPORTER": "otlp",
"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": "https://loki.example/otlp/v1/logs",
"OTEL_LOGS_EXPORT_INTERVAL": "5000"
```

Metrics use the generic endpoint, so the SDK appends `/v1/metrics` → effective URL `…/api/v1/otlp/v1/metrics`.
Logs use a signal-specific endpoint, which takes the **full** path with nothing appended.

Claude Code does not emit traces. Span series such as `otelcol_receiver_accepted_spans_total` come from the collector itself, if one is in the path.

## What agent-sessions reads

Both queries are keyed by the session id, taken from the session's jsonl.

**Loki** - `query_range` over the session's time span:

```logql
{service_name="claude-code"} | session_id="<id>" | event_name=~"api_request|tool_result|tool_decision|hook_execution_complete"
```

| event | joined to | shown as |
|---|---|---|
| `api_request` | assistant event by `request_id` (jsonl `requestId`) | ttft, duration, effort, reported cost |
| `tool_result`, `tool_decision` | tool call by `tool_use_id` | execution time, result size, permission decision + source |
| `hook_execution_complete` | session | hook overhead |

**Prometheus** - instant query, last value per series within 90 d:

```promql
sum by (__name__, type, start_type) (last_over_time({__name__=~"claude_code_(active_time_seconds|lines_of_code_count|commit_count|pull_request_count|session_count)_total", session_id="<id>"}[90d]))
```

Hence `OTEL_METRICS_INCLUDE_SESSION_ID=true` is required: without it the metrics carry no `session_id` and the `otel` row stays empty.
The cost is one series set per session; watch Prometheus cardinality if session counts grow large.

Results are stored in the agent-sessions DB (a background pass runs every 10 min), so they outlive Loki / Prometheus retention.

## What lands

### Metrics

8 counters: `claude_code_active_time_seconds_total`, `claude_code_code_edit_tool_decision_total`,
`claude_code_commit_count_total`, `claude_code_cost_usage_USD_total`, `claude_code_lines_of_code_count_total`,
`claude_code_pull_request_count_total`, `claude_code_session_count_total`, `claude_code_token_usage_tokens_total`.

Cost and token metrics are broken down by `model`, `type`, `query_source` (`main` / `subagent` / `auxiliary`),
`agent_name`, `skill_name`, `mcp_server_name`, `mcp_tool_name`, `effort`.

### Logs

Loki, `service_name="claude-code"`. Event types: `user_prompt`, `assistant_response`, `api_request`, `tool_decision`,
`tool_result`, `hook_execution_start`, `hook_execution_complete`.

- **api_request** - `cost_usd`, `cost_usd_micros`, `duration_ms`, `input_tokens`, `output_tokens`, `cache_read_tokens`,
  `cache_creation_tokens`, `model`, `effort`, `speed`, `query_source`, `agent_name`, `skill_name`, `mcp_server_name`,
  `mcp_tool_name`, `request_id`, `client_request_id`, `prompt_id`
- **tool_result / tool_decision** - `tool_name`, `tool_use_id`, `tool_source`, `duration_ms`, `success`,
  `tool_input_size_bytes`, `tool_result_size_bytes`, `decision`, `decision_type`, `decision_source`, `mcp_server_scope`
- **hook_execution_\*** - `hook_name`, `hook_event`, `hook_source`, `num_hooks`, `num_blocking`, `num_success`,
  `num_cancelled`, `num_non_blocking_error`, `total_duration_ms`
- **user_prompt / assistant_response** - `prompt_id`, `message_uuid`, `prompt_length`, `response_length`

Prompt text is **redacted** (`prompt: "<REDACTED>"`, only `prompt_length` kept) because `OTEL_LOG_USER_PROMPTS` is unset.
Keep it that way unless there is a deliberate decision to store prompt bodies in Loki.

`query_source` in logs is finer than in metrics: `repl_main_thread`, `repl_main_thread:outputStyle:Concise`,
`agent:builtin:general-purpose`, `agent_summary`, `generate_session_title`, `sdk`.

Log attributes map onto the local jsonl:

| Loki attribute | jsonl field |
|---|---|
| `request_id` (`req_011Cen…`) | `requestId` |
| `prompt_id` | `promptId` |
| `message_uuid` | `uuid` |
| `tool_use_id` | tool_use id in message content blocks |

## Gotchas

**Attributes are structured metadata, not stream labels.** Indexed Loki labels are only `service_name, job, level, host,
instance, namespace, pod, container, filename, stream, unit`. Everything Claude-Code-specific must be filtered after the selector:

```logql
{service_name="claude-code", event_name="api_request"}     # returns nothing
{service_name="claude-code"} | event_name=`api_request`    # correct
```

`/loki/api/v1/label/event_name/values` returns `{"status":"success"}` with no `data` - not an error, just the wrong endpoint
for structured metadata. Same for `session_id`.

**Env is captured at process start.** Settings changes apply to new sessions only; a long-running session needs a restart
to pick up a new exporter config.

## Constraints

- `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE=cumulative` is required by the Prometheus OTLP receiver.
- The generic `OTEL_EXPORTER_OTLP_ENDPOINT` moves **all** signals that lack a signal-specific override, and metrics depend on it.
  If it is repointed (e.g. everything through a collector), verify metrics still land.
- Signal-specific endpoints take the full path; the generic one gets `/v1/<signal>` appended.
- `settings.json` also carries unrelated keys (permissions, `model`, hooks). Only the OTEL keys are in scope here.

## Verification

```bash
# metrics arriving
curl -s 'https://prometheus.example/api/v1/query?query=claude_code_session_count_total'

# session_id on metrics (empty data array = not enabled, or no such session within retention)
curl -s 'https://prometheus.example/api/v1/label/session_id/values'

# logs arriving - note ns timestamps and the structured-metadata filter
now=$(date +%s)
curl -s -G 'https://loki.example/loki/api/v1/query_range' \
  --data-urlencode 'query={service_name="claude-code"} | event_name=`api_request`' \
  --data-urlencode "start=$((now-3600))000000000" --data-urlencode "end=${now}000000000" \
  --data-urlencode 'limit=5'
```

Start a fresh Claude Code session first. Allow one `OTEL_METRIC_EXPORT_INTERVAL` (60 s) for metrics and one
`OTEL_LOGS_EXPORT_INTERVAL` (5 s) for logs before concluding anything is broken.

## What the local jsonl already covers

Each session file has a `type:"cost-state"` record with native `totalCostUSD`, per-model `modelUsage` (including the auxiliary
haiku calls), `totalAPIDuration`, `totalAPIDurationWithoutRetries`, `totalToolDuration`, `totalDuration`,
`totalLinesAdded` / `Removed`. Per-message records carry `requestId`, `promptId`, `uuid`, `effort`, `permissionMode`,
`service_tier`, thinking tokens and the 1h / 5m cache split.

Telemetry-only data: edit accept / reject decisions with their source, hook execution timing and outcomes, per-request
wall-clock duration and ttft, token attribution per MCP tool, session `start_type` (fresh vs resume), the CLI-vs-user
active-time split, terminal / host fingerprint.
