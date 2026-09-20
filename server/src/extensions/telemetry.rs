//! Per-session enrichment from Claude Code OTel telemetry (Loki events + Prometheus metrics).
//! Optional: endpoints come from `~/.config/agent-sessions/config.toml` (`[telemetry]` `loki_url`,
//! `prom_url`), overridden by `LOKI_URL` / `PROM_URL` env vars. Any failure or absence of data
//! yields `None`, so the session view never depends on it.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Req {
    pub ttft_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub cost_usd: Option<f64>,
    pub effort: Option<String>,
    pub speed: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Tool {
    pub duration_ms: Option<u64>,
    pub success: Option<bool>,
    pub input_bytes: Option<u64>,
    pub result_bytes: Option<u64>,
    pub decision: Option<String>,
    pub decision_source: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct HookStat {
    pub count: u32,
    pub total_ms: u64,
    pub blocking: u32,
    pub errors: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Telemetry {
    /// request_id -> api_request event
    pub requests: HashMap<String, Req>,
    /// tool_use_id -> tool_result + tool_decision
    pub tools: HashMap<String, Tool>,
    /// hook name -> aggregate
    pub hooks: HashMap<String, HookStat>,
    pub cost_usd: Option<f64>,
    pub api_requests: u32,
    pub active_cli_s: Option<f64>,
    pub active_user_s: Option<f64>,
    pub loc_added: Option<u64>,
    pub loc_removed: Option<u64>,
    pub commits: Option<u64>,
    pub prs: Option<u64>,
    pub start_type: Option<String>,
}

impl Telemetry {
    pub fn keep_joinable_only(&mut self) {
        *self = Telemetry {
            requests: std::mem::take(&mut self.requests),
            tools: std::mem::take(&mut self.tools),
            ..Default::default()
        };
    }
    fn is_empty(&self) -> bool {
        self.requests.is_empty()
            && self.tools.is_empty()
            && self.hooks.is_empty()
            && self.active_cli_s.is_none()
            && self.loc_added.is_none()
    }
}

pub fn enabled() -> bool {
    let c = &crate::config::get().telemetry;
    c.loki_url.is_some() || c.prom_url.is_some()
}

/// The flag is false when a backend failed, so an empty result is not mistaken for "nothing was
/// recorded".
pub async fn fetch(
    session_id: &str,
    started: Option<&str>,
    ended: Option<&str>,
    session_metrics: bool,
) -> (Option<Telemetry>, bool) {
    // session_id comes from file content and is interpolated unescaped into LogQL/PromQL below.
    if !session_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        || session_id.is_empty()
    {
        return (None, false);
    }
    // One shared client: the background sync fetches every session in a row, and a client per
    // fetch meant a fresh DNS lookup and TLS handshake each time, which resolvers (WSL) drop.
    static CLIENT: std::sync::OnceLock<Option<reqwest::Client>> = std::sync::OnceLock::new();
    let Some(client) = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .ok()
    }) else {
        return (None, false);
    };
    let mut t = Telemetry::default();
    let mut complete = true;
    if let Some(loki) = &crate::config::get().telemetry.loki_url {
        if let Err(e) = fetch_loki(client, loki, session_id, started, ended, &mut t).await {
            eprintln!("loki {}: {:#}", session_id, e);
            complete = false;
        }
    }
    if let (true, Some(prom)) = (session_metrics, &crate::config::get().telemetry.prom_url) {
        if let Err(e) = fetch_prom(client, prom, session_id, &mut t).await {
            eprintln!("prom {}: {:#}", session_id, e);
            complete = false;
        }
    }
    ((!t.is_empty()).then_some(t), complete)
}

fn num<T: std::str::FromStr>(m: &serde_json::Map<String, serde_json::Value>, k: &str) -> Option<T> {
    m.get(k)?.as_str()?.parse().ok()
}
fn str_(m: &serde_json::Map<String, serde_json::Value>, k: &str) -> Option<String> {
    m.get(k)?.as_str().map(String::from)
}

async fn fetch_loki(
    client: &reqwest::Client,
    base: &str,
    session_id: &str,
    started: Option<&str>,
    ended: Option<&str>,
    t: &mut Telemetry,
) -> anyhow::Result<()> {
    const LIMIT: usize = 5000;
    const NS: u128 = 1_000_000_000;
    let query = format!(
        r#"{{service_name="claude-code"}} | session_id="{}" | event_name=~"api_request|tool_result|tool_decision|hook_execution_complete""#,
        session_id
    );
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    // Pad both ends: events are flushed in batches and the last ones land after the session's
    // final message.
    let mut start = started
        .and_then(rfc3339_ns)
        .map(|s| s.saturating_sub(300 * NS))
        .unwrap_or(now_ns - 30 * 86_400 * NS)
        .to_string();
    let end = ended
        .and_then(rfc3339_ns)
        .map(|e| e.saturating_add(3600 * NS).min(now_ns))
        .unwrap_or(now_ns)
        .to_string();
    loop {
        let r: serde_json::Value = client
            .get(format!("{}/loki/api/v1/query_range", base))
            .query(&[
                ("query", query.as_str()),
                ("limit", &LIMIT.to_string()),
                ("direction", "forward"),
                ("start", &start),
                ("end", &end),
            ])
            .send()
            .await
            .map_err(reqwest::Error::without_url)?
            .error_for_status()?
            .json()
            .await?;
        let mut n = 0usize;
        let mut last_ns: u128 = 0;
        for stream in r["data"]["result"].as_array().into_iter().flatten() {
            let m = match stream["stream"].as_object() {
                Some(m) => m,
                None => continue,
            };
            let entries = stream["values"].as_array().map(|v| v.len()).unwrap_or(0);
            n += entries;
            for v in stream["values"].as_array().into_iter().flatten() {
                if let Some(ts) = v[0].as_str().and_then(|x| x.parse::<u128>().ok()) {
                    last_ns = last_ns.max(ts);
                }
            }
            match m.get("event_name").and_then(|x| x.as_str()) {
                Some("api_request") => {
                    let Some(id) = str_(m, "request_id") else {
                        continue;
                    };
                    let cost: Option<f64> = num(m, "cost_usd");
                    t.api_requests += entries as u32;
                    if let Some(c) = cost {
                        *t.cost_usd.get_or_insert(0.0) += c * entries as f64;
                    }
                    t.requests.insert(
                        id,
                        Req {
                            ttft_ms: num(m, "ttft_ms"),
                            duration_ms: num(m, "duration_ms"),
                            cost_usd: cost,
                            effort: str_(m, "effort"),
                            speed: str_(m, "speed"),
                        },
                    );
                }
                Some("tool_result") => {
                    let Some(id) = str_(m, "tool_use_id") else {
                        continue;
                    };
                    let e = t.tools.entry(id).or_default();
                    e.duration_ms = num(m, "duration_ms");
                    e.success = m
                        .get("success")
                        .and_then(|x| x.as_str())
                        .map(|x| x == "true");
                    e.input_bytes = num(m, "tool_input_size_bytes");
                    e.result_bytes = num(m, "tool_result_size_bytes");
                }
                Some("tool_decision") => {
                    let Some(id) = str_(m, "tool_use_id") else {
                        continue;
                    };
                    let e = t.tools.entry(id).or_default();
                    e.decision = str_(m, "decision");
                    e.decision_source = str_(m, "source");
                }
                Some("hook_execution_complete") => {
                    let name = str_(m, "hook_name")
                        .or_else(|| str_(m, "hook_event"))
                        .unwrap_or_default();
                    let h = t.hooks.entry(name).or_default();
                    let k = entries as u32;
                    h.count += k;
                    h.total_ms += num::<u64>(m, "total_duration_ms").unwrap_or(0) * k as u64;
                    h.blocking += num::<u32>(m, "num_blocking").unwrap_or(0) * k;
                    h.errors += num::<u32>(m, "num_non_blocking_error").unwrap_or(0) * k;
                }
                _ => {}
            }
        }
        if n < LIMIT || last_ns == 0 {
            break;
        }
        start = (last_ns + 1).to_string();
    }
    Ok(())
}

async fn fetch_prom(
    client: &reqwest::Client,
    base: &str,
    session_id: &str,
    t: &mut Telemetry,
) -> anyhow::Result<()> {
    // last_over_time: instant query alone only sees series scraped within the last 5 minutes.
    let q = format!(
        r#"sum by (__name__, type, start_type) (last_over_time({{__name__=~"claude_code_(active_time_seconds|lines_of_code_count|commit_count|pull_request_count|session_count)_total", session_id="{}"}}[90d]))"#,
        session_id
    );
    let r: serde_json::Value = client
        .get(format!("{}/api/v1/query", base))
        .query(&[("query", q.as_str())])
        .send()
        .await
        .map_err(reqwest::Error::without_url)?
        .error_for_status()?
        .json()
        .await?;
    for s in r["data"]["result"].as_array().into_iter().flatten() {
        let m = &s["metric"];
        let val: f64 = s["value"][1]
            .as_str()
            .and_then(|x| x.parse().ok())
            .unwrap_or(0.0);
        let ty = m["type"].as_str().unwrap_or("");
        match m["__name__"].as_str().unwrap_or("") {
            "claude_code_active_time_seconds_total" => match ty {
                "cli" => t.active_cli_s = Some(val),
                "user" => t.active_user_s = Some(val),
                _ => {}
            },
            "claude_code_lines_of_code_count_total" => match ty {
                "added" => t.loc_added = Some(val as u64),
                "removed" => t.loc_removed = Some(val as u64),
                _ => {}
            },
            "claude_code_commit_count_total" => t.commits = Some(val as u64),
            "claude_code_pull_request_count_total" => t.prs = Some(val as u64),
            "claude_code_session_count_total" => {
                if let Some(st) = m["start_type"].as_str() {
                    t.start_type = Some(st.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn unix_s(ts: &str) -> Option<u64> {
    rfc3339_ns(ts).map(|ns| (ns / 1_000_000_000) as u64)
}

/// Minimal RFC3339 (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) -> unix nanoseconds; session timestamps are
/// always UTC `Z`.
fn rfc3339_ns(s: &str) -> Option<u128> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>());
    let (y, mo, da) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (hms, frac) = time
        .split_once('.')
        .map(|(a, b)| (a, Some(b)))
        .unwrap_or((time, None));
    let mut tp = hms.split(':').map(|x| x.parse::<i64>());
    let (h, mi, se) = (tp.next()?.ok()?, tp.next()?.ok()?, tp.next()?.ok()?);
    // Days from civil (Howard Hinnant).
    let (y, mo) = if mo <= 2 {
        (y - 1, mo + 9)
    } else {
        (y, mo - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * mo + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + h * 3600 + mi * 60 + se;
    let mut ns = u128::try_from(secs).ok()? * 1_000_000_000;
    if let Some(f) = frac {
        if !f.is_empty() && !f.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let f: String = f.chars().take(9).collect();
        let pad = 9 - f.len();
        ns += f.parse::<u128>().ok()? * 10u128.pow(pad as u32);
    }
    Some(ns)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rfc3339() {
        assert_eq!(super::rfc3339_ns("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            super::rfc3339_ns("2026-09-12T13:08:11.718Z"),
            Some(1789218491718000000)
        );
    }

    #[test]
    fn rfc3339_pre_1970_rejected() {
        assert_eq!(super::rfc3339_ns("1969-12-31T23:59:59Z"), None);
    }

    #[test]
    fn rfc3339_non_ascii_fraction_rejected() {
        assert_eq!(super::rfc3339_ns("2026-09-12T13:08:11.7µ8Z"), None);
    }
}
