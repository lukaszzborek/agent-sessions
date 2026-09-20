use super::{read_jsonl, s, u};
use crate::model::*;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn parse(path: &Path, data: &[u8], titles: &HashMap<String, String>) -> anyhow::Result<Parsed> {
    let lines = read_jsonl(data);
    let mut events: Vec<Event> = Vec::new();
    let mut usage = Usage::default();
    let mut cost = 0.0f64;
    let mut has_cost = false;
    let mut title: Option<String> = None;
    let mut cwd = String::new();
    let mut sid = String::new();
    let mut started = None;
    let mut parent_id: Option<String> = None;
    let mut agent_type: Option<String> = None;
    let mut sub_label: Option<String> = None;
    let mut version = None;
    let mut cur_model: Option<String> = None;
    let mut git_branch = None;
    let mut context_window = None;
    let mut next_id = 0u32;
    let mut id = || {
        next_id += 1;
        next_id
    };

    for v in &lines {
        let ty = s(v, "type").unwrap_or_default();
        let ts = s(v, "timestamp");
        let p = &v["payload"];
        match ty.as_str() {
            "session_meta" => {
                sid = s(p, "id").unwrap_or_default();
                cwd = s(p, "cwd").unwrap_or_default();
                started = s(p, "timestamp");
                version = s(p, "cli_version");
                git_branch = s(&p["git"], "branch");
                parent_id = s(p, "parent_thread_id");
                if let Some(sub) = p["source"].get("subagent") {
                    match sub {
                        Value::String(k) => {
                            agent_type = Some(k.clone());
                            sub_label = Some(format!("[{}]", k));
                        }
                        Value::Object(_) => {
                            let sp = &sub["thread_spawn"];
                            if parent_id.is_none() {
                                parent_id = s(sp, "parent_thread_id");
                            }
                            agent_type = s(sp, "agent_role");
                            sub_label = Some(format!(
                                "[{} {}]",
                                s(sp, "agent_role").unwrap_or_default(),
                                s(sp, "agent_nickname").unwrap_or_default()
                            ));
                        }
                        _ => {}
                    }
                }
            }
            "turn_context" => {
                if let Some(m) = s(p, "model") {
                    if cur_model.as_ref() != Some(&m) {
                        let mut e = Event::new(id(), EventKind::ModelChange);
                        e.ts = ts.clone();
                        e.model = Some(m.clone());
                        e.text = Some(if cur_model.is_some() {
                            format!("switched to {}", m)
                        } else {
                            m.clone()
                        });
                        if let Some(eff) = s(p, "effort").or_else(|| s(&p["reasoning"], "effort")) {
                            e.meta = Some(serde_json::json!({ "effort": eff }));
                        }
                        events.push(e);
                        cur_model = Some(m);
                    }
                }
            }
            "response_item" => {
                let pt = s(p, "type").unwrap_or_default();
                match pt.as_str() {
                    "message" => {
                        let role = s(p, "role").unwrap_or_default();
                        let text = content_text(&p["content"]);
                        let clean = strip_tags(&text);
                        let clean = clean.trim();
                        if clean.is_empty() {
                            let raw = text.trim();
                            if role == "user"
                                && (raw.starts_with("<user_action>")
                                    || raw.starts_with("<turn_aborted>"))
                            {
                                let mut e = Event::new(id(), EventKind::System);
                                e.ts = ts.clone();
                                e.text = Some(raw.to_string());
                                events.push(e);
                            }
                            continue;
                        }
                        match role.as_str() {
                            "user" => {
                                if is_boilerplate(clean) {
                                    let mut e = Event::new(id(), EventKind::System);
                                    e.ts = ts.clone();
                                    e.text = Some(clean.to_string());
                                    events.push(e);
                                    continue;
                                }
                                if title.is_none() {
                                    title = Some(first_line_title(clean));
                                }
                                let mut e = Event::new(id(), EventKind::User);
                                e.ts = ts.clone();
                                e.text = Some(clean.to_string());
                                events.push(e);
                            }
                            "assistant" => {
                                let mut e = Event::new(id(), EventKind::Assistant);
                                e.ts = ts.clone();
                                e.model = cur_model.clone();
                                e.text = Some(clean.to_string());
                                if let Some(ph) = s(p, "phase") {
                                    e.meta = Some(serde_json::json!({ "phase": ph }));
                                }
                                events.push(e);
                            }
                            // Developer/system instructions are noise.
                            _ => {}
                        }
                    }
                    "reasoning" => {
                        let mut parts: Vec<String> = Vec::new();
                        if let Some(a) = p["summary"].as_array() {
                            for x in a {
                                if let Some(t) = s(x, "text") {
                                    parts.push(t);
                                }
                            }
                        }
                        if let Some(a) = p["content"].as_array() {
                            for x in a {
                                if let Some(t) = s(x, "text") {
                                    parts.push(t);
                                }
                            }
                        }
                        if parts.is_empty() {
                            continue;
                        }
                        let mut e = Event::new(id(), EventKind::Thinking);
                        e.ts = ts.clone();
                        e.model = cur_model.clone();
                        e.text = Some(parts.join("\n\n"));
                        events.push(e);
                    }
                    "function_call" | "custom_tool_call" => {
                        let name = s(p, "name").unwrap_or_default();
                        let is_spawn = name == "spawn_agent";
                        let mut e = Event::new(
                            id(),
                            if is_spawn {
                                EventKind::Subagent
                            } else {
                                EventKind::ToolCall
                            },
                        );
                        e.ts = ts.clone();
                        e.model = cur_model.clone();
                        e.tool_call_id = s(p, "call_id");
                        let raw = p
                            .get("arguments")
                            .or_else(|| p.get("input"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        let parsed = match &raw {
                            Value::String(sx) => {
                                serde_json::from_str::<Value>(sx).unwrap_or(raw.clone())
                            }
                            o => o.clone(),
                        };
                        // exec_command / exec: show command directly.
                        let cmd = parsed
                            .as_object()
                            .filter(|o| o.len() <= 3)
                            .and_then(|o| o.get("cmd")?.as_str());
                        let text = match cmd {
                            Some(c) => c.to_string(),
                            None => json_text(&parsed),
                        };
                        if is_spawn {
                            let mut meta = serde_json::Map::new();
                            for k in ["agent_type", "model", "nickname"] {
                                if let Some(x) = parsed.get(k) {
                                    meta.insert(k.into(), x.clone());
                                }
                            }
                            e.meta = Some(Value::Object(meta));
                        }
                        e.tool_name = Some(name);
                        e.text = Some(text);
                        events.push(e);
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        let mut e = Event::new(id(), EventKind::ToolResult);
                        e.ts = ts.clone();
                        e.tool_call_id = s(p, "call_id");
                        let out = content_text(&p["output"]);
                        // exec_command outputs are JSON {output, metadata:{exit_code}}.
                        let mut text = out.clone();
                        if let Ok(j) = serde_json::from_str::<Value>(&out) {
                            if let Some(o) = j.get("output").and_then(|x| x.as_str()) {
                                text = o.to_string();
                                if let Some(code) = j["metadata"]["exit_code"].as_i64() {
                                    e.meta = Some(serde_json::json!({ "exit_code": code }));
                                    if code != 0 {
                                        e.is_error = true;
                                        text = format!("[exit {}]\n{}", code, text);
                                    }
                                }
                            } else if let Some(aid) = j.get("agent_id").and_then(|x| x.as_str()) {
                                e.child_session_id = Some(aid.to_string());
                            }
                        }
                        e.text = Some(text);
                        events.push(e);
                    }
                    "web_search_call" => {
                        let mut e = Event::new(id(), EventKind::ToolCall);
                        e.ts = ts.clone();
                        e.model = cur_model.clone();
                        e.tool_name = Some("web_search".into());
                        e.text = Some(
                            s(&p["action"], "query").unwrap_or_else(|| json_text(&p["action"])),
                        );
                        events.push(e);
                    }
                    _ => {}
                }
            }
            "event_msg" => {
                let pt = s(p, "type").unwrap_or_default();
                match pt.as_str() {
                    "token_count" => {
                        if let Some(w) = p["info"]["model_context_window"].as_u64() {
                            context_window = Some(w);
                        }
                        // input_tokens here already includes cached tokens.
                        if let (Some(n), Some(last)) = (
                            p["info"]["last_token_usage"]["input_tokens"].as_u64(),
                            events.last_mut(),
                        ) {
                            last.context = Some(n);
                        }
                        // Without a model the delta cannot be priced and, since totals are
                        // cumulative, consuming it now would make it unpriceable forever even
                        // once a model becomes known; wait.
                        if let (Some(t), Some(model)) =
                            (p["info"].get("total_token_usage"), cur_model.clone())
                        {
                            let cached = u(t, "cached_input_tokens");
                            let total = Usage {
                                input: u(t, "input_tokens").saturating_sub(cached),
                                output: u(t, "output_tokens"),
                                cache_read: cached,
                                cache_write: u(t, "cache_write_input_tokens"),
                                cache_write_1h: 0,
                                reasoning: u(t, "reasoning_output_tokens"),
                            };
                            // Totals are cumulative; price the delta at the current model.
                            let delta = Usage {
                                input: total.input.saturating_sub(usage.input),
                                output: total.output.saturating_sub(usage.output),
                                cache_read: total.cache_read.saturating_sub(usage.cache_read),
                                cache_write: total.cache_write.saturating_sub(usage.cache_write),
                                cache_write_1h: 0,
                                reasoning: total.reasoning.saturating_sub(usage.reasoning),
                            };
                            // token_count trails the turn it accounts for; bill it to the last
                            // event, falling back to a marker when the turn produced none.
                            if events.is_empty() {
                                let mut e = Event::new(id(), EventKind::Assistant);
                                e.ts = ts.clone();
                                e.model = Some(model.clone());
                                e.text = Some(String::new());
                                events.push(e);
                            }
                            let last = events.last_mut().expect("just ensured non-empty");
                            last.usage.get_or_insert_with(Usage::default).add(&delta);
                            if let Some(c) = crate::pricing::cost(&model, &delta) {
                                cost += c;
                                has_cost = true;
                                last.cost = Some(last.cost.unwrap_or(0.0) + c);
                            }
                            usage = total;
                        }
                    }
                    "context_compacted" => {
                        let mut e = Event::new(id(), EventKind::Compaction);
                        e.ts = ts.clone();
                        e.text = Some("context compacted".into());
                        events.push(e);
                    }
                    "turn_aborted" => {
                        let mut e = Event::new(id(), EventKind::System);
                        e.ts = ts.clone();
                        e.text = Some(format!(
                            "turn aborted: {}",
                            s(p, "reason").unwrap_or_default()
                        ));
                        events.push(e);
                    }
                    "entered_review_mode" | "exited_review_mode" => {
                        let mut e = Event::new(id(), EventKind::System);
                        e.ts = ts.clone();
                        e.text = Some(pt.replace('_', " "));
                        events.push(e);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Link spawn_agent calls to child ids via their outputs.
    let pairs: Vec<(String, String)> = events
        .iter()
        .filter(|e| e.kind == EventKind::ToolResult)
        .filter_map(|e| {
            Some((
                e.tool_call_id.clone().unwrap_or_default(),
                e.child_session_id.clone()?,
            ))
        })
        .collect();
    let mut children = Vec::new();
    for e in events.iter_mut().filter(|e| e.kind == EventKind::Subagent) {
        if let Some((_, cid)) = pairs
            .iter()
            .find(|(tid, _)| Some(tid) == e.tool_call_id.as_ref())
        {
            e.child_session_id = Some(cid.clone());
            children.push(cid.clone());
        }
    }

    if sid.is_empty() {
        sid = path
            .file_stem()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_string();
    }
    let mut t = titles
        .get(&sid)
        .cloned()
        .or(title)
        .unwrap_or_else(|| "(untitled)".into());
    if let Some(l) = sub_label {
        t = format!("{} {}", l, t);
    }
    let mut summary = SessionSummary {
        agent: Agent::Codex,
        id: sid,
        path: path.to_path_buf(),
        cwd,
        title: t,
        started,
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
        hooks: vec![],
        skills: vec![],
        cmds: vec![],
        subagents: vec![],
        agent_type,
        tags: vec![],
        source: String::new(),
        max_context: 0,
        buckets: vec![],
        context_window,
    };
    let st = summary.started.take();
    summarize_from_events(&mut summary, &mut events);
    if summary.started.is_none() {
        summary.started = st;
    }
    Ok(Parsed { summary, events })
}

fn content_text(c: &Value) -> String {
    match c {
        Value::String(t) => t.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match s(p, "type").as_deref() {
                Some("input_text") | Some("output_text") | Some("text") => {
                    s(p, "text").unwrap_or_default()
                }
                Some("input_image") | Some("image") => "[image]".to_string(),
                _ => json_text(p),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        o => json_text(o),
    }
}

/// ~/.codex/session_index.jsonl: {id, thread_name, updated_at}; last wins.
pub fn load_titles(codex_homes: &[PathBuf]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for home in codex_homes {
        if let Ok(data) = std::fs::read(home.join("session_index.jsonl")) {
            let lines = read_jsonl(&data);
            for v in lines {
                if let (Some(i), Some(n)) = (s(&v, "id"), s(&v, "thread_name")) {
                    m.insert(i, n);
                }
            }
        }
    }
    m
}
