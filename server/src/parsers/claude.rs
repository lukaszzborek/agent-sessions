use super::{read_jsonl, s, u};
use crate::model::*;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub fn parse(path: &Path, data: &[u8]) -> anyhow::Result<Parsed> {
    let lines = read_jsonl(data);
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
                                        e.child_session_id = Some(aid.to_string());
                                        if !children.contains(&aid.to_string()) {
                                            children.push(aid.to_string());
                                        }
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
                    if !uu.is_zero() {
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

    let file_id = path
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_string();
    let is_sub = file_id.starts_with("agent-");
    let id_final = if is_sub {
        file_id.trim_start_matches("agent-").to_string()
    } else {
        sid.unwrap_or(file_id)
    };
    let parent_id = if is_sub {
        // Path: <proj>/<sid>/subagents/agent-x.jsonl.
        path.parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|x| x.to_str())
            .map(|x| x.to_string())
    } else {
        None
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
