export type Agent = 'claude' | 'codex' | 'pi'
export interface Usage { input: number; output: number; cache_read: number; cache_write: number; cache_write_1h: number; reasoning: number }
export type Kind = 'user' | 'assistant' | 'thinking' | 'tool_call' | 'tool_result' | 'subagent' | 'model_change' | 'compaction' | 'system'
export interface Event {
  id: number; kind: Kind; ts?: string; model?: string; tool_name?: string; tool_call_id?: string; request_id?: string
  text?: string; truncated?: boolean; is_error?: boolean; usage?: Usage; cost?: number; context?: number; child_session_id?: string; meta?: Record<string, any>
}
export interface Session {
  agent: Agent; id: string; path: string; cwd: string; title: string; started?: string; ended?: string
  models: string[]; user_msgs: number; assistant_msgs: number; tool_calls: number; tools: [string, number][]
  usage: Usage; cost?: number; parent_id?: string; children: string[]; git_branch?: string; archived?: boolean; version?: string; spawn_tool_call_id?: string
  hooks: [string, number][]; skills: [string, number][]; cmds: [string, number][]; subagents: [string, number][]; agent_type?: string
  tags: string[]; source: string; max_context: number; context_window?: number
  buckets: { hour: string; model: string; usage: Usage; cost: number; tool_calls: number }[]
}
export interface Detail { summary: Session; events: Event[] }
// Claude OTel enrichment (Loki events + Prometheus counters); null when not configured or nothing recorded
export interface TeleReq { ttft_ms?: number; duration_ms?: number; cost_usd?: number; effort?: string; speed?: string }
export interface TeleTool { duration_ms?: number; success?: boolean; input_bytes?: number; result_bytes?: number; decision?: string; decision_source?: string }
export interface Telemetry {
  requests: Record<string, TeleReq>; tools: Record<string, TeleTool>
  hooks: Record<string, { count: number; total_ms: number; blocking: number; errors: number }>
  cost_usd?: number; api_requests: number; active_cli_s?: number; active_user_s?: number
  loc_added?: number; loc_removed?: number; commits?: number; prs?: number; start_type?: string
}
