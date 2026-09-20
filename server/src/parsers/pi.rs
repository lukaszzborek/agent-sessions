use super::{read_jsonl, s, u};
use crate::model::*;
use serde_json::Value;
use std::path::Path;

pub fn parse(path: &Path, data: &[u8]) -> anyhow::Result<Parsed> {
    let lines = read_jsonl(data);
    let mut events: Vec<Event> = Vec::new();
    let mut usage = Usage::default();
    let mut cost = 0.0f64;
    let mut has_cost = false;
    let mut title: Option<String> = None;
    let mut cwd = String::new();
    let mut sid = String::new();
    let mut started = None;
    let mut cur_model: Option<String> = None;
    let mut next_id = 0u32;
    let mut id = || {
        next_id += 1;
        next_id
    };

    for v in &lines {
        let ty = s(v, "type").unwrap_or_default();
        let ts = s(v, "timestamp");
        match ty.as_str() {
            "session" => {
                sid = s(v, "id").unwrap_or_default();
                cwd = s(v, "cwd").unwrap_or_default();
                started = ts.clone();
            }
            "model_change" => {
                let m = format!(
                    "{}/{}",
                    s(v, "provider").unwrap_or_default(),
                    s(v, "modelId").unwrap_or_default()
                );
                let mut e = Event::new(id(), EventKind::ModelChange);
                e.ts = ts.clone();
                e.model = Some(m.clone());
                e.text = Some(if cur_model.is_some() {
                    format!("switched to {}", m)
                } else {
                    m.clone()
                });
                events.push(e);
                cur_model = Some(m);
            }
            "thinking_level_change" => {
                let mut e = Event::new(id(), EventKind::System);
                e.ts = ts.clone();
                e.text = Some(format!(
                    "thinking level: {}",
                    s(v, "thinkingLevel").unwrap_or_default()
                ));
                events.push(e);
            }
            "compaction" => {
                let mut e = Event::new(id(), EventKind::Compaction);
                e.ts = ts.clone();
                e.text = Some("context compacted".into());
                events.push(e);
            }
            "message" => {
                let m = &v["message"];
                let role = s(m, "role").unwrap_or_default();
                match role.as_str() {
                    "user" => {
                        let t = blocks_text(&m["content"]);
                        let t = t.trim();
                        if t.is_empty() {
                            continue;
                        }
                        if title.is_none() {
                            title = Some(first_line_title(t));
                        }
                        let mut e = Event::new(id(), EventKind::User);
                        e.ts = ts.clone();
                        e.text = Some(t.to_string());
                        events.push(e);
                    }
                    "assistant" => {
                        let model = match (s(m, "provider"), s(m, "model")) {
                            (Some(p), Some(mm)) => Some(format!("{}/{}", p, mm)),
                            (_, Some(mm)) => Some(mm),
                            _ => cur_model.clone(),
                        };
                        let mut ev_usage = None;
                        let mut ev_cost = None;
                        if let Some(us) = m.get("usage") {
                            let uu = Usage {
                                input: u(us, "input"),
                                output: u(us, "output"),
                                cache_read: u(us, "cacheRead"),
                                cache_write: u(us, "cacheWrite"),
                                cache_write_1h: 0,
                                reasoning: u(us, "reasoning"),
                            };
                            usage.add(&uu);
                            if let Some(c) = us["cost"]["total"].as_f64() {
                                cost += c;
                                has_cost = true;
                                ev_cost = Some(c);
                            }
                            ev_usage = Some(uu);
                        }
                        if let Some(blocks) = m["content"].as_array() {
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
                                        e.text = Some(t);
                                        e.usage = ev_usage.take();
                                        e.cost = ev_cost.take();
                                        events.push(e);
                                    }
                                    Some("thinking") => {
                                        let t = s(b, "thinking").unwrap_or_default();
                                        if t.trim().is_empty() {
                                            continue;
                                        }
                                        let mut e = Event::new(id(), EventKind::Thinking);
                                        e.ts = ts.clone();
                                        e.model = model.clone();
                                        e.text = Some(t);
                                        e.usage = ev_usage.take();
                                        e.cost = ev_cost.take();
                                        events.push(e);
                                    }
                                    Some("toolCall") => {
                                        let mut e = Event::new(id(), EventKind::ToolCall);
                                        e.ts = ts.clone();
                                        e.model = model.clone();
                                        e.tool_call_id = s(b, "id");
                                        e.tool_name = s(b, "name");
                                        let args = &b["arguments"];
                                        e.text = Some(
                                            match args.get("command").and_then(|c| c.as_str()) {
                                                Some(c)
                                                    if args
                                                        .as_object()
                                                        .is_some_and(|o| o.len() <= 2) =>
                                                {
                                                    c.to_string()
                                                }
                                                _ => json_text(args),
                                            },
                                        );
                                        e.usage = ev_usage.take();
                                        e.cost = ev_cost.take();
                                        events.push(e);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        if let Some(uu) = ev_usage {
                            // usage without any block: attach to marker.
                            let mut e = Event::new(id(), EventKind::Assistant);
                            e.ts = ts.clone();
                            e.model = model.clone();
                            e.text = Some(String::new());
                            e.usage = Some(uu);
                            events.push(e);
                        }
                    }
                    "toolResult" => {
                        let mut e = Event::new(id(), EventKind::ToolResult);
                        e.ts = ts.clone();
                        e.tool_call_id = s(m, "toolCallId");
                        e.tool_name = s(m, "toolName");
                        e.is_error = m["isError"].as_bool().unwrap_or(false);
                        e.text = Some(blocks_text(&m["content"]));
                        events.push(e);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if sid.is_empty() {
        sid = path
            .file_stem()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_string();
    }
    let mut summary = SessionSummary {
        agent: Agent::Pi,
        id: sid,
        path: path.to_path_buf(),
        cwd,
        title: title.unwrap_or_else(|| "(untitled)".into()),
        started: None,
        ended: None,
        models: vec![],
        user_msgs: 0,
        assistant_msgs: 0,
        tool_calls: 0,
        tools: vec![],
        usage,
        cost: if has_cost { Some(cost) } else { None },
        parent_id: None,
        children: vec![],
        git_branch: None,
        version: None,
        spawn_tool_call_id: None,
        archived: false,
        hooks: vec![],
        skills: vec![],
        cmds: vec![],
        subagents: vec![],
        agent_type: None,
        tags: vec![],
        source: String::new(),
        max_context: 0,
        buckets: vec![],
        context_window: None,
    };
    summarize_from_events(&mut summary, &mut events);
    if summary.started.is_none() {
        summary.started = started;
    }
    Ok(Parsed { summary, events })
}

fn blocks_text(c: &Value) -> String {
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
