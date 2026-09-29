use super::{read_jsonl, s, u};
use crate::model::*;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// `skip`: request ids whose usage another file counts (`SessionSummary::dup_request_ids`).
pub fn parse(path: &Path, data: &[u8], skip: &HashSet<String>) -> anyhow::Result<Parsed> {
    let lines = read_jsonl(data);
    let file_id = path
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_string();
    let is_sub = file_id.starts_with("agent-");
    // Path: <proj>/<sid>/subagents/agent-x.jsonl. Older versions wrote a flat
    // <proj>/agent-x.jsonl whose parent cannot be told from the path; those keep the bare id.
    let parent_id = path
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == "subagents"))
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|x| x.to_str())
        .filter(|_| is_sub)
        .map(str::to_string);
    // Claude Code writes the same `agent-<id>` file under every session that continues the parent,
    // so the bare agent id is not unique; scoping it by the parent session is. No `/`, as ids go
    // into URLs and `agent/id` tag keys.
    let scope = parent_id.as_deref().unwrap_or(&file_id);
    let children_scoped = parent_id.is_some() || path.with_extension("").join("subagents").is_dir();
    let mut events: Vec<Event> = Vec::new();
    // Streaming writes one line per content block under the same requestId; early lines carry
    // partial output_tokens, so usage is settled per request and summed after the loop.
    let mut req_usage: HashMap<String, (Usage, Option<f64>, Option<usize>)> = HashMap::new();
    let mut usage = Usage::default();
    let mut cost = 0.0f64;
    let mut has_cost = false;
    let mut title: Option<String> = None;
    let mut summary_title: Option<String> = None;
    let mut cwd = String::new();
    let mut sid: Option<String> = None;
    let mut git_branch = None;
    let mut version = None;
    let mut children: Vec<String> = Vec::new();
    let mut hooks: BTreeMap<String, u32> = BTreeMap::new();
    let mut skills: BTreeMap<String, u32> = BTreeMap::new();
    let mut next_id = 0u32;
    let mut id = || {
        next_id += 1;
        next_id
    };

    for v in &lines {
        let ty = s(v, "type").unwrap_or_default();
        if cwd.is_empty() {
            if let Some(c) = s(v, "cwd") {
                cwd = c;
            }
        }
        if sid.is_none() {
            sid = s(v, "sessionId");
        }
        if git_branch.is_none() {
            git_branch = s(v, "gitBranch").filter(|x| !x.is_empty());
        }
        if version.is_none() {
            version = s(v, "version");
        }
        let ts = s(v, "timestamp");
        match ty.as_str() {
            "summary" => {
                summary_title = s(v, "summary");
            }
            "user" => {
                let msg = &v["message"];
                if let Some(c) = slash_command(&msg["content"]) {
                    *skills.entry(c).or_default() += 1;
                }
                match &msg["content"] {
                    Value::String(t) => {
                        if !t.is_empty() {
                            push_user(&mut events, id(), ts.clone(), t, &mut title);
                        }
                    }
                    Value::Array(blocks) => {
                        let mut texts = Vec::new();
                        for b in blocks {
                            match s(b, "type").as_deref() {
                                Some("text") => {
                                    if let Some(t) = s(b, "text") {
                                        texts.push(t);
                                    }
                                }
                                Some("image") => texts.push("[image]".into()),
                                Some("tool_result") => {
                                    let mut e = Event::new(id(), EventKind::ToolResult);
                                    e.ts = ts.clone();
                                    e.tool_call_id = s(b, "tool_use_id");
                                    e.is_error = b["is_error"].as_bool().unwrap_or(false);
                                    e.text = Some(tool_result_text(&b["content"]));
                                    // Bash extras (stderr, interrupted, timeouts, background,
                                    // sandbox).
                                    let tr = &v["toolUseResult"];
                                    if tr.get("stdout").is_some() {
                                        let mut meta = serde_json::Map::new();
                                        for k in [
                                            "stderr",
                                            "interrupted",
                                            "timedOutAfterMs",
                                            "backgroundTaskId",
                                            "dangerouslyDisableSandbox",
                                            "returnCodeInterpretation",
                                        ] {
                                            match tr.get(k) {
                                                Some(Value::Bool(false))
                                                | Some(Value::Null)
                                                | None => {}
                                                Some(Value::String(x)) if x.trim().is_empty() => {}
                                                Some(x) => {
                                                    meta.insert(k.into(), x.clone());
                                                }
                                            }
                                        }
                                        if !meta.is_empty() {
                                            e.meta = Some(Value::Object(meta));
                                        }
                                    }
                                    // Subagent link.
                                    if let Some(aid) = v["toolUseResult"]["agentId"].as_str() {
                                        let cid = if children_scoped {
                                            format!("{scope}~{aid}")
                                        } else {
                                            aid.to_string()
                                        };
                                        if !children.contains(&cid) {
                                            children.push(cid.clone());
                                        }
                                        e.child_session_id = Some(cid);
                                        let mut meta = serde_json::Map::new();
                                        for k in ["status", "resolvedModel", "description"] {
                                            if let Some(x) = v["toolUseResult"].get(k) {
                                                meta.insert(k.into(), x.clone());
                                            }
                                        }
                                        e.meta = Some(Value::Object(meta));
                                    }
                                    events.push(e);
                                }
                                _ => {}
                            }
                        }
                        if !texts.is_empty() {
                            let t = texts.join("\n");
                            push_user(&mut events, id(), ts.clone(), &t, &mut title);
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let msg = &v["message"];
                let model = s(msg, "model").filter(|m| m != "<synthetic>");
                let req = s(v, "requestId");
                let mut usage_key = None;
                if let Some(us) = msg.get("usage") {
                    let uu = Usage {
                        input: u(us, "input_tokens"),
                        output: u(us, "output_tokens"),
                        cache_read: u(us, "cache_read_input_tokens"),
                        cache_write: u(us, "cache_creation_input_tokens"),
                        cache_write_1h: u(&us["cache_creation"], "ephemeral_1h_input_tokens"),
                        reasoning: u(&us["output_tokens_details"], "thinking_tokens"),
                    };
                    let key = req
                        .clone()
                        .unwrap_or_else(|| s(v, "uuid").unwrap_or_default());
                    if !uu.is_zero() && !key.is_empty() && !skip.contains(&key) {
                        let r = req_usage.entry(key.clone()).or_default();
                        if uu.output >= r.0.output {
                            r.1 = model.as_deref().and_then(|m| crate::pricing::cost(m, &uu));
                            r.0 = uu;
                        }
                        usage_key = Some(key);
                    }
                }
                let first_ev = events.len();
                if let Some(blocks) = msg["content"].as_array() {
                    for b in blocks {
                        match s(b, "type").as_deref() {
                            Some("text") => {
                                let t = s(b, "text").unwrap_or_default();
                                if t.trim().is_empty() {
                                    continue;
                                }
                                let mut e = Event::new(id(), EventKind::Assistant);
                                e.ts = ts.clone();
                                e.model = model.clone();
                                e.request_id = req.clone();
                                e.text = Some(t);
                                events.push(e);
                            }
                            Some("thinking") => {
                                let mut e = Event::new(id(), EventKind::Thinking);
                                e.ts = ts.clone();
                                e.model = model.clone();
                                e.request_id = req.clone();
                                e.text = s(b, "thinking");
                                events.push(e);
                            }
                            Some("tool_use") => {
                                let name = s(b, "name").unwrap_or_default();
                                if name == "Skill" {
                                    if let Some(sk) = s(&b["input"], "skill") {
                                        *skills.entry(sk).or_default() += 1;
                                    }
                                }
                                let is_agent = name == "Agent" || name == "Task";
                                let mut e = Event::new(
                                    id(),
                                    if is_agent {
                                        EventKind::Subagent
                                    } else {
                                        EventKind::ToolCall
                                    },
                                );
                                e.ts = ts.clone();
                                e.model = model.clone();
                                e.tool_call_id = s(b, "id");
                                e.tool_name = Some(name);
                                e.text = Some(json_text(&b["input"]));
                                if is_agent {
                                    let inp = &b["input"];
                                    let mut meta = serde_json::Map::new();
                                    for k in ["subagent_type", "description", "model"] {
                                        if let Some(x) = inp.get(k) {
                                            if !x.is_null() {
                                                meta.insert(k.into(), x.clone());
                                            }
                                        }
                                    }
                                    e.meta = Some(Value::Object(meta));
                                }
                                events.push(e);
                            }
                            _ => {}
                        }
                    }
                }
                if usage_key.is_some() && events.len() == first_ev {
                    // Request produced no event (whitespace-only text, unknown block type): still
                    // need a place to attach its usage/cost, so stats don't under-report it.
                    let mut e = Event::new(id(), EventKind::Assistant);
                    e.ts = ts.clone();
                    e.model = model.clone();
                    e.request_id = req.clone();
                    e.text = Some(String::new());
                    events.push(e);
                }
                if let Some(r) = usage_key.and_then(|k| req_usage.get_mut(&k)) {
                    if r.2.is_none() && events.len() > first_ev {
                        r.2 = Some(first_ev);
                    }
                }
                // Model-only (empty content) message: still record model.
                if let Some(m) = &model {
                    if !events.iter().any(|e| e.model.as_deref() == Some(m)) {
                        let mut e = Event::new(id(), EventKind::ModelChange);
                        e.ts = ts.clone();
                        e.model = Some(m.clone());
                        e.text = Some(m.clone());
                        events.push(e);
                    }
                }
            }
            "attachment" => {
                let a = &v["attachment"];
                if s(a, "type").is_some_and(|t| t.starts_with("hook_")) {
                    if let Some(k) = s(a, "command").or_else(|| s(a, "hookName")) {
                        *hooks.entry(k).or_default() += 1;
                    }
                }
            }
            "system" => {
                let sub = s(v, "subtype").unwrap_or_default();
                if sub == "stop_hook_summary" {
                    for h in v["hookInfos"].as_array().into_iter().flatten() {
                        if let Some(c) = s(h, "command") {
                            *hooks.entry(c).or_default() += 1;
                        }
                    }
                } else if sub == "compact_boundary" {
                    let mut e = Event::new(id(), EventKind::Compaction);
                    e.ts = ts.clone();
                    e.text = Some("context compacted".into());
                    events.push(e);
                } else if let Some(c) = s(v, "content") {
                    let c = strip_tags(&c);
                    if !c.trim().is_empty() && !c.contains("<local-command") {
                        let mut e = Event::new(id(), EventKind::System);
                        e.ts = ts.clone();
                        e.text = Some(c);
                        e.meta = Some(serde_json::json!({ "subtype": sub }));
                        events.push(e);
                    }
                }
            }
            _ => {}
        }
    }

    let mut request_ids: Vec<String> = req_usage.keys().cloned().collect();
    request_ids.sort_unstable();
    for (u, c, idx) in req_usage.into_values() {
        usage.add(&u);
        if let Some(c) = c {
            cost += c;
            has_cost = true;
        }
        if let Some(e) = idx.and_then(|i| events.get_mut(i)) {
            e.usage = Some(u);
            e.cost = c;
        }
    }

    // Link Subagent tool_use events to child ids via their tool_result.
    let pairs: Vec<(String, String, Option<Value>)> = events
        .iter()
        .filter(|e| e.kind == EventKind::ToolResult)
        .filter_map(|e| {
            let rm = e
                .meta
                .as_ref()
                .and_then(|m| m.get("resolvedModel"))
                .cloned();
            Some((
                e.tool_call_id.clone().unwrap_or_default(),
                e.child_session_id.clone()?,
                rm,
            ))
        })
        .collect();
    for e in events.iter_mut().filter(|e| e.kind == EventKind::Subagent) {
        if let Some((_, cid, rm)) = pairs
            .iter()
            .find(|(tid, _, _)| Some(tid) == e.tool_call_id.as_ref())
        {
            e.child_session_id = Some(cid.clone());
            if let (Some(rm), Some(Value::Object(m))) = (rm, e.meta.as_mut()) {
                m.insert("resolvedModel".into(), rm.clone());
            }
        }
    }

    // Insert model change markers where model switches between assistant events.
    let mut last_model: Option<String> = None;
    let mut i = 0;
    while i < events.len() {
        if let Some(m) = events[i].model.clone() {
            if last_model.as_ref() != Some(&m)
                && last_model.is_some()
                && events[i].kind != EventKind::ModelChange
            {
                let mut e = Event::new(id(), EventKind::ModelChange);
                e.ts = events[i].ts.clone();
                e.model = Some(m.clone());
                e.text = Some(format!("switched to {}", m));
                events.insert(i, e);
                i += 1;
            }
            last_model = Some(m);
        }
        i += 1;
    }

    let id_final = match file_id.strip_prefix("agent-") {
        Some(aid) => match &parent_id {
            Some(p) => format!("{p}~{aid}"),
            None => aid.to_string(),
        },
        None => sid.unwrap_or(file_id),
    };
    let mut summary = SessionSummary {
        agent: Agent::Claude,
        id: id_final,
        path: path.to_path_buf(),
        cwd,
        title: summary_title
            .or(title)
            .unwrap_or_else(|| "(untitled)".into()),
        started: None,
        ended: None,
        models: vec![],
        user_msgs: 0,
        assistant_msgs: 0,
        tool_calls: 0,
        tools: vec![],
        usage,
        cost: if has_cost { Some(cost) } else { None },
        parent_id,
        children,
        git_branch,
        version,
        spawn_tool_call_id: None,
        archived: false,
        hooks: sorted_counts(hooks),
        skills: sorted_counts(skills),
        cmds: vec![],
        subagents: vec![],
        agent_type: None,
        tags: vec![],
        source: String::new(),
        max_context: 0,
        buckets: vec![],
        request_ids,
        dup_request_ids: vec![],
        context_window: None,
    };
    if is_sub {
        // Subagent meta file.
        let meta_path = path.with_extension("meta.json");
        if let Ok(m) = std::fs::read(&meta_path) {
            if let Ok(m) = serde_json::from_slice::<Value>(&m) {
                let t = s(&m, "agentType").unwrap_or_default();
                let d = s(&m, "description").unwrap_or_default();
                summary.title = format!("[{}] {}", t, d);
                if !t.is_empty() {
                    summary.agent_type = Some(t);
                }
                summary.spawn_tool_call_id = s(&m, "toolUseId");
            }
        }
    }
    summarize_from_events(&mut summary, &mut events);
    Ok(Parsed { summary, events })
}

fn push_user(
    events: &mut Vec<Event>,
    id: u32,
    ts: Option<String>,
    t: &str,
    title: &mut Option<String>,
) {
    let clean = strip_tags(t);
    let clean = clean.trim();
    if clean.is_empty() {
        return;
    }
    // Skip pure command/meta echoes.
    if clean.starts_with("<command-name>") || clean.starts_with("<local-command") {
        return;
    }
    if title.is_none() && !is_boilerplate(clean) {
        *title = Some(first_line_title(clean));
    }
    let mut e = Event::new(id, EventKind::User);
    e.ts = ts;
    e.text = Some(clean.to_string());
    events.push(e);
}

fn tool_result_text(c: &Value) -> String {
    match c {
        Value::String(t) => t.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match s(p, "type").as_deref() {
                Some("text") => s(p, "text").unwrap_or_default(),
                Some("image") => "[image]".to_string(),
                _ => json_text(p),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        o => json_text(o),
    }
}

/// Built-in slash commands that are not skills.
const BUILTIN_COMMANDS: &[&str] = &[
    "clear",
    "model",
    "config",
    "login",
    "logout",
    "add-dir",
    "resume",
    "rename",
    "feedback",
    "reload-plugins",
    "reload-skills",
    "compact",
    "help",
    "exit",
    "cost",
    "status",
    "doctor",
    "memory",
    "permissions",
    "mcp",
    "agents",
    "hooks",
    "plugins",
    "context",
    "usage",
    "export",
    "release-notes",
    "vim",
    "terminal-setup",
];

/// `<command-name>/foo</command-name>` in a user message -> "foo" (skills / custom commands only).
fn slash_command(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(t) => t.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| s(b, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let start = text.find("<command-name>")? + "<command-name>".len();
    let end = text[start..].find("</command-name>")? + start;
    let name = text[start..end].trim().trim_start_matches('/').to_string();
    if name.is_empty() || BUILTIN_COMMANDS.contains(&name.as_str()) {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(req: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-09-01T10:00:00.000Z","requestId":"{req}","message":{{"model":"claude-sonnet-5","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
        )
    }

    #[test]
    fn subagent_ids_are_scoped_by_parent_session() {
        let data = assistant("r1");
        let a = parse(
            Path::new("/p/S1/subagents/agent-a1.jsonl"),
            data.as_bytes(),
            &HashSet::new(),
        )
        .unwrap();
        let b = parse(
            Path::new("/p/S2/subagents/agent-a1.jsonl"),
            data.as_bytes(),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(
            (a.summary.id.as_str(), a.summary.parent_id.as_deref()),
            ("S1~a1", Some("S1"))
        );
        assert_eq!(
            (b.summary.id.as_str(), b.summary.parent_id.as_deref()),
            ("S2~a1", Some("S2"))
        );

        let parent = r#"{"type":"user","timestamp":"2026-09-01T10:00:00.000Z","sessionId":"S1","toolUseResult":{"agentId":"a1"},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
        let dir = std::env::temp_dir().join(format!("agent-sessions-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("S1/subagents")).unwrap();
        let p = parse(&dir.join("S1.jsonl"), parent.as_bytes(), &HashSet::new()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(p.summary.children, ["S1~a1"]);
    }

    #[test]
    fn flat_subagent_layout_keeps_bare_ids() {
        let sub = parse(
            Path::new("/projects/proj/agent-x.jsonl"),
            assistant("r1").as_bytes(),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(
            (sub.summary.id.as_str(), sub.summary.parent_id.as_deref()),
            ("x", None)
        );

        let parent = r#"{"type":"user","timestamp":"2026-09-01T10:00:00.000Z","sessionId":"S1","toolUseResult":{"agentId":"x"},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
        let p = parse(
            Path::new("/projects/proj/S1.jsonl"),
            parent.as_bytes(),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(p.summary.children, ["x"]);
    }

    #[test]
    fn skipped_request_ids_are_not_counted() {
        let data = assistant("r1");
        let path = Path::new("/p/S1.jsonl");
        let full = parse(path, data.as_bytes(), &HashSet::new()).unwrap();
        assert_eq!(
            (full.summary.usage.output, full.summary.request_ids.clone()),
            (5, vec!["r1".to_string()])
        );
        let skip = HashSet::from(["r1".to_string()]);
        let deduped = parse(path, data.as_bytes(), &skip).unwrap();
        assert!(deduped.summary.usage.is_zero() && deduped.summary.cost.is_none());
        assert!(deduped.summary.buckets.is_empty());
    }
}
