use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Pi,
}

impl Agent {
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Pi => "pi",
        }
    }
    pub fn parse(s: &str) -> Option<Agent> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "pi" => Some(Agent::Pi),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Subset of cache_write billed at 1h TTL rate (Anthropic).
    #[serde(default)]
    pub cache_write_1h: u64,
    pub reasoning: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cache_write_1h += o.cache_write_1h;
        self.reasoning += o.reasoning;
    }
    pub fn is_zero(&self) -> bool {
        *self == Usage::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    User,
    Assistant,
    Thinking,
    ToolCall,
    ToolResult,
    Subagent,
    ModelChange,
    Compaction,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: u32,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// API request id (Claude assistant/thinking events); joins OTel `api_request` telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Text body: message text, thinking, tool output, tool input (pretty JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// USD priced from `usage` at this event's model (None when the model has no price entry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Prompt tokens sent on this API call (uncached + cache read + cache write) = context size at
    /// that point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    /// For Subagent events: id of child session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    /// Extra small structured info (e.g. subagent type, description).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

impl Event {
    pub fn new(id: u32, kind: EventKind) -> Self {
        Event {
            id,
            kind,
            ts: None,
            model: None,
            tool_name: None,
            tool_call_id: None,
            request_id: None,
            text: None,
            truncated: false,
            is_error: false,
            usage: None,
            cost: None,
            context: None,
            child_session_id: None,
            meta: None,
        }
    }
}

fn user_source() -> String {
    crate::config::USER_SOURCE.into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub agent: Agent,
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    pub models: Vec<String>,
    pub user_msgs: u32,
    pub assistant_msgs: u32,
    pub tool_calls: u32,
    /// tool name -> count
    pub tools: Vec<(String, u32)>,
    pub usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Tool call id in the parent session that spawned this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_tool_call_id: Option<String>,
    /// Source file no longer on disk; content served from DB.
    #[serde(default)]
    pub archived: bool,
    /// hook command (or hook name) -> fire count (Claude only)
    #[serde(default)]
    pub hooks: Vec<(String, u32)>,
    /// skill / slash command name -> invocation count (Claude only)
    #[serde(default)]
    pub skills: Vec<(String, u32)>,
    /// shell program (e.g. "git commit", "grep") -> count across Bash/exec tool calls
    #[serde(default)]
    pub cmds: Vec<(String, u32)>,
    /// subagent type (e.g. "Explore", "general-purpose") -> spawn count
    #[serde(default)]
    pub subagents: Vec<(String, u32)>,
    /// For subagent sessions: the agent type this session ran as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// Labels for finding/comparing sessions: bench trial tags (`task:x`, `arm:y`, …) + user-set
    /// ones.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Where the file was indexed from: `user` (own home) or the `name` of a config `sources`
    /// entry. Set by the indexer.
    #[serde(default = "user_source")]
    pub source: String,
    /// Largest `Event::context` in this session (own calls only, subagents have separate windows).
    #[serde(default)]
    pub max_context: u64,
    /// Model context window when the log records it (Codex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Usage split by UTC hour + model, so stats can attribute long / multi-model sessions
    /// correctly.
    #[serde(default)]
    pub buckets: Vec<Bucket>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bucket {
    /// UTC hour, `yyyy-mm-ddThh`; hourly so the UI can regroup into local days.
    pub hour: String,
    pub model: String,
    pub usage: Usage,
    pub cost: f64,
    pub tool_calls: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionDetail {
    pub summary: SessionSummary,
    pub events: Vec<Event>,
}

/// Result of parsing one file: summary + full events (untruncated).
#[derive(Debug)]
pub struct Parsed {
    pub summary: SessionSummary,
    pub events: Vec<Event>,
}

pub const TRUNCATE_AT: usize = 4_000;

pub fn truncate_events(events: &mut [Event]) {
    for e in events {
        if let Some(t) = &e.text {
            if t.len() > TRUNCATE_AT {
                let mut cut = TRUNCATE_AT;
                while !t.is_char_boundary(cut) {
                    cut -= 1;
                }
                e.text = Some(t[..cut].to_string());
                e.truncated = true;
            }
        }
    }
}

pub fn is_shell_tool(name: &str) -> bool {
    matches!(
        name,
        "Bash" | "bash" | "shell" | "exec_command" | "shell_command" | "local_shell"
    )
}

const WRAPPERS: &[&str] = &[
    "sudo", "nohup", "time", "env", "rtk", "command", "exec", "timeout", "xargs", "proxy", "do",
    "then", "else", "!", "{", "(",
];
const SKIP: &[&str] = &[
    "cd", "echo", "true", "false", "sleep", "if", "elif", "fi", "done", "for", "while", "until",
    "case", "esac", "in", "[", "[[", "}", ")", "export", "set", "local", "return", "exit",
    "printf", "read", "test", "wait", "break", "continue", "shift",
];
/// programs whose first non-flag arg is a subcommand worth keeping
const SUBCMD: &[&str] = &[
    "git",
    "gh",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "npx",
    "bun",
    "dotnet",
    "docker",
    "kubectl",
    "just",
    "pip",
    "pip3",
    "uv",
    "go",
    "make",
    "systemctl",
    "apt",
    "brew",
    "az",
    "aws",
    "gcloud",
    "terraform",
    "helm",
    "rtk",
    "codex",
    "claude",
    "poetry",
    "nx",
];

/// Extract the programs invoked by a shell command line: "cd x && git add . | head" -> ["git add",
/// "head"].
pub fn shell_progs(cmd: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Drop heredoc bodies: `cmd <<'EOF' ... EOF`.
    let cmd = strip_heredocs(cmd);
    let cmd = cmd.as_str();
    // Split into simple commands.
    let mut segs: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut it = cmd.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = it.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                // `2>&1` is a redirect, not a separator.
                '&' if cur.ends_with('>') => cur.push(c),
                '\n' | ';' | '|' | '&' | '`' => {
                    segs.push(std::mem::take(&mut cur));
                }
                '$' if it.peek() == Some(&'(') => {
                    it.next();
                    segs.push(std::mem::take(&mut cur));
                }
                _ => cur.push(c),
            },
        }
    }
    segs.push(cur);
    for seg in segs {
        let mut toks = seg.split_whitespace().map(|t| {
            t.trim_matches(|c| {
                c == '(' || c == ')' || c == '{' || c == '}' || c == '"' || c == '\''
            })
        });
        let mut prog = None;
        for t in toks.by_ref() {
            if t.is_empty() || t.contains('=') && !t.starts_with('-') && prog.is_none() {
                continue;
            }
            let base = t.rsplit('/').next().unwrap_or(t);
            if WRAPPERS.contains(&base) {
                continue;
            }
            if SKIP.contains(&base) {
                break;
            }
            if base.starts_with('-') {
                continue;
            }
            prog = Some(base.to_string());
            break;
        }
        let Some(mut prog) = prog else { continue };
        if SUBCMD.contains(&prog.as_str()) {
            // First word-like arg, skipping flags and values of common value-taking flags (-C dir,
            // -n ns).
            let mut prev = "";
            let sub = toks.find(|t| {
                let ok = !t.is_empty()
                    && !t.starts_with('-')
                    && !matches!(prev, "-C" | "-c" | "-n" | "--namespace" | "--context")
                    && t.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':' || c == '_')
                    && t.len() <= 20
                    && !t.chars().all(|c| c.is_ascii_digit());
                prev = t;
                ok
            });
            if let Some(sub) = sub {
                prog = format!("{} {}", prog, sub);
            }
        }
        if !out.contains(&prog) {
            out.push(prog);
        }
        if out.len() >= 6 {
            break;
        }
    }
    out
}

fn strip_heredocs(cmd: &str) -> String {
    let mut out = String::new();
    let mut lines = cmd.lines();
    while let Some(l) = lines.next() {
        out.push_str(l);
        out.push('\n');
        if let Some(i) = l.find("<<") {
            // Not a here-string (`<<<`) or a shift (`1<<4`): a heredoc tag starts with a letter or
            // `_`, optionally quoted, right after `<<` (or `<<-`).
            if l[i + 2..].starts_with('<') {
                continue;
            }
            let tag = l[i + 2..].trim_start_matches('-').trim_start();
            let tag = tag
                .split(|c: char| c.is_whitespace() || c == ';' || c == '|' || c == '&')
                .next()
                .unwrap_or("");
            let tag = tag.trim_matches(|c| c == '\'' || c == '"');
            if !tag.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                continue;
            }
            for b in lines.by_ref() {
                if b.trim() == tag {
                    break;
                }
            }
        }
    }
    out
}

/// Tag shell tool calls with `meta.cmds` (programs invoked).
pub fn annotate_shell(events: &mut [Event]) {
    for e in events {
        if e.kind != EventKind::ToolCall || !e.tool_name.as_deref().is_some_and(is_shell_tool) {
            continue;
        }
        let Some(t) = &e.text else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else {
            continue;
        };
        let cmd = match &v["command"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            _ => match v["cmd"].as_str() {
                Some(s) => s.to_string(),
                None => continue,
            },
        };
        let progs = shell_progs(&cmd);
        if progs.is_empty() {
            continue;
        }
        let m = e
            .meta
            .get_or_insert_with(|| serde_json::Value::Object(Default::default()));
        if let Some(o) = m.as_object_mut() {
            o.insert("cmds".into(), serde_json::json!(progs));
        }
    }
}

pub fn summarize_from_events(s: &mut SessionSummary, events: &mut [Event]) {
    annotate_shell(events);
    let mut tools: std::collections::BTreeMap<String, u32> = Default::default();
    let mut cmds: std::collections::BTreeMap<String, u32> = Default::default();
    let mut subs: std::collections::BTreeMap<String, u32> = Default::default();
    let mut buckets: std::collections::BTreeMap<(String, String), Bucket> = Default::default();
    let mut cur_model: Option<&str> = None;
    let mut cur_hour: Option<&str> = None;
    for e in events.iter_mut() {
        if e.context.is_none() {
            e.context = e
                .usage
                .as_ref()
                .map(|u| u.input + u.cache_read + u.cache_write);
        }
        s.max_context = s.max_context.max(e.context.unwrap_or(0));
    }
    for e in events.iter() {
        match e.kind {
            EventKind::User => s.user_msgs += 1,
            EventKind::Assistant => s.assistant_msgs += 1,
            EventKind::ToolCall | EventKind::Subagent => {
                s.tool_calls += 1;
                if e.kind == EventKind::Subagent {
                    *subs.entry(subagent_type(e)).or_default() += 1;
                }
                if let Some(n) = &e.tool_name {
                    *tools.entry(n.clone()).or_default() += 1;
                }
                if let Some(a) = e.meta.as_ref().and_then(|m| m["cmds"].as_array()) {
                    for c in a.iter().filter_map(|x| x.as_str()) {
                        *cmds.entry(c.to_string()).or_default() += 1;
                    }
                }
            }
            _ => {}
        }
        if let Some(m) = &e.model {
            if !s.models.contains(m) {
                s.models.push(m.clone());
            }
            cur_model = Some(m);
        }
        if let Some(h) = e.ts.as_deref().and_then(|t| t.get(..13)) {
            cur_hour = Some(h);
        }
        let is_tool = matches!(e.kind, EventKind::ToolCall | EventKind::Subagent);
        if let (Some(h), true) = (cur_hour, is_tool || e.usage.is_some() || e.cost.is_some()) {
            let model = cur_model.unwrap_or("?");
            let b = buckets
                .entry((h.to_string(), model.to_string()))
                .or_default();
            if let Some(u) = &e.usage {
                b.usage.add(u);
            }
            b.cost += e.cost.unwrap_or(0.0);
            b.tool_calls += is_tool as u32;
        }
        if let Some(ts) = &e.ts {
            if s.started.as_deref().is_none_or(|x| ts.as_str() < x) {
                s.started = Some(ts.clone());
            }
            if s.ended.as_deref().is_none_or(|x| ts.as_str() > x) {
                s.ended = Some(ts.clone());
            }
        }
    }
    s.tools = sorted_counts(tools);
    s.cmds = sorted_counts(cmds);
    s.subagents = sorted_counts(subs);
    s.buckets = buckets
        .into_iter()
        .map(|((hour, model), b)| Bucket { hour, model, ..b })
        .collect();
}

/// Agent type of a spawn event: `subagent_type` (Claude) / `agent_type` (Codex), else "?".
fn subagent_type(e: &Event) -> String {
    e.meta
        .as_ref()
        .and_then(|m| {
            m["subagent_type"]
                .as_str()
                .or_else(|| m["agent_type"].as_str())
        })
        .unwrap_or("?")
        .to_string()
}

pub fn sorted_counts(m: std::collections::BTreeMap<String, u32>) -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

/// Injected/boilerplate user texts that make bad titles.
pub fn is_boilerplate(t: &str) -> bool {
    let t = t.trim_start();
    t.starts_with("# AGENTS.md instructions")
        || t.starts_with("Base directory for this skill")
        || t.starts_with("Caveat:")
        || t.starts_with("Stop hook feedback")
        || t.starts_with("[Request interrupted")
        || t.starts_with("<command-name>")
        || t.starts_with("<local-command")
}

pub fn first_line_title(text: &str) -> String {
    let t = strip_tags(text);
    let t = t.trim();
    let line = t.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut out: String = line.chars().take(100).collect();
    if line.chars().count() > 100 {
        out.push('…');
    }
    if out.is_empty() {
        "(untitled)".into()
    } else {
        out
    }
}

/// Remove <tag>...</tag> blocks commonly injected (system-reminder, environment_context, ...).
pub fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let name_end = after
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(after.len());
        let name = &after[..name_end];
        if name.is_empty() || !after[name_end..].starts_with('>') {
            out.push('<');
            rest = after;
            continue;
        }
        let close = format!("</{}>", name);
        if let Some(ci) = after.find(&close) {
            rest = &after[ci + close.len()..];
        } else {
            out.push('<');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

pub fn json_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::shell_progs;
    #[test]
    fn progs() {
        let f = |s: &str| shell_progs(s).join(",");
        assert_eq!(f("cd ui && npm run build 2>&1 | tail -3"), "npm run,tail");
        assert_eq!(
            f("PORT=7799 nohup ./server/target/release/agent-sessions >/dev/null 2>&1 &"),
            "agent-sessions"
        );
        assert_eq!(
            f("git -C .. status --short; cargo build --release"),
            "git status,cargo build"
        );
        assert_eq!(
            f("cat README.md | head -60 && ls server ui && grep -rn -i \"bash\" . | head"),
            "cat,head,ls,grep"
        );
        assert_eq!(
            f("for i in 1 2; do curl -sf x && break; sleep 1; done"),
            "curl"
        );
        assert_eq!(
            f("F=$(grep -l x *.jsonl | head -1); echo \"$F\""),
            "grep,head"
        );
        assert_eq!(
            f("python3 - <<'EOF'\nimport json; x = 'a && b'\nEOF\nrtk git status"),
            "python3,git status"
        );
    }

    #[test]
    fn heredoc_detection() {
        use super::strip_heredocs;
        // shift, not a heredoc: must not swallow the next line looking for a tag that never comes.
        assert_eq!(strip_heredocs("a=1<<4\ngit status"), "a=1<<4\ngit status\n");
        // here-string, not a heredoc.
        assert_eq!(
            strip_heredocs("cat <<< \"$x\"\ngit status"),
            "cat <<< \"$x\"\ngit status\n"
        );
        // real heredoc still consumes its body (dropped, along with the terminator line).
        assert_eq!(
            strip_heredocs("cat <<EOF\nbody\nEOF\ngit status"),
            "cat <<EOF\ngit status\n"
        );
    }
}
