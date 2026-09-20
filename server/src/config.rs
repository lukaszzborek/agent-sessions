//! `~/.config/agent-sessions/config.toml`, with env overrides (`LOKI_URL`, `PROM_URL`).
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Name of the built-in source: the current user's `~/.claude`, `~/.codex`, `~/.pi`.
pub const USER_SOURCE: &str = "user";

/// Extra location to index; its sessions carry `name` as their `source`.
#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub name: String,
    pub path: PathBuf,
    /// Glob patterns relative to `path`, matching dirs that hold that agent's `*.jsonl` logs
    /// (searched recursively).
    /// When none of the three is set, `path` is treated as a home dir (`.claude/projects`, …).
    pub claude: Option<String>,
    pub codex: Option<String>,
    pub pi: Option<String>,
    /// File name of a JSON file with `{"tags": ["key:value", …]}`, looked up in the ancestor dirs
    /// of each session file (nearest wins); its tags are applied to the session. Other keys are
    /// ignored.
    pub meta: Option<String>,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

impl Source {
    pub fn home(name: &str, path: PathBuf) -> Source {
        Source {
            name: name.into(),
            path,
            claude: Some(".claude/projects".into()),
            codex: Some(".codex/sessions".into()),
            pi: Some(".pi/agent/sessions".into()),
            meta: None,
            enabled: true,
        }
    }
}

fn enabled() -> bool {
    true
}

/// `[telemetry]`: Claude Code OTel enrichment.
#[derive(Debug, Deserialize)]
pub struct Telemetry {
    /// Off: no telemetry is fetched, stored or shown.
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub loki_url: Option<String>,
    pub prom_url: Option<String>,
}

/// `[rtk]`: rtk history of bench trials.
#[derive(Debug, Deserialize)]
pub struct Rtk {
    /// Off: no rtk history is read, stored or shown.
    #[serde(default = "enabled")]
    pub enabled: bool,
}

impl Default for Telemetry {
    fn default() -> Telemetry {
        Telemetry {
            enabled: true,
            loki_url: None,
            prom_url: None,
        }
    }
}

impl Default for Rtk {
    fn default() -> Rtk {
        Rtk { enabled: true }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub telemetry: Telemetry,
    #[serde(default)]
    pub rtk: Rtk,
    /// Only the enabled, validly named entries survive loading.
    #[serde(default)]
    pub sources: Vec<Source>,
}

pub fn get() -> &'static Config {
    static C: OnceLock<Config> = OnceLock::new();
    C.get_or_init(|| {
        let config_dir = dirs::config_dir();
        let new_path = config_dir
            .as_ref()
            .map(|d| d.join("agent-sessions/config.toml"));
        let mut c: Config = new_path
            .clone()
            .and_then(|p| std::fs::read_to_string(&p).ok().map(|b| (p, b)))
            .and_then(|(p, b)| {
                toml::from_str(&b)
                    .map_err(|e| eprintln!("config {}: {}", p.display(), e))
                    .ok()
            })
            .unwrap_or_default();
        // Renamed `ai-sessions/config.json` -> `agent-sessions/config.toml`; formats differ so
        // this is not auto-converted, just flagged.
        if new_path.as_ref().is_some_and(|p| !p.exists()) {
            if let Some(old) = config_dir.map(|d| d.join("ai-sessions/config.json")) {
                if old.is_file() {
                    eprintln!(
                        "config: found old {} - ignored, port its settings to {}",
                        old.display(),
                        new_path.unwrap().display()
                    );
                }
            }
        }
        let t = &mut c.telemetry;
        if let Ok(v) = std::env::var("LOKI_URL") {
            t.loki_url = Some(v);
        }
        if let Ok(v) = std::env::var("PROM_URL") {
            t.prom_url = Some(v);
        }
        t.loki_url = t
            .loki_url
            .take()
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim_end_matches('/').to_string());
        t.prom_url = t
            .prom_url
            .take()
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim_end_matches('/').to_string());
        let mut names = HashSet::from([USER_SOURCE.to_string()]);
        c.sources.retain(|s| {
            // Checked first so a disabled entry doesn't reserve its name.
            if !s.enabled {
                return false;
            }
            let ok = !s.name.trim().is_empty() && names.insert(s.name.clone());
            if !ok {
                eprintln!(
                    "config: source {:?} skipped (name empty, duplicate or reserved)",
                    s.name
                );
            }
            ok
        });
        for s in c
            .sources
            .iter_mut()
            .filter(|s| s.claude.is_none() && s.codex.is_none() && s.pi.is_none())
        {
            *s = Source {
                meta: s.meta.take(),
                ..Source::home(&s.name, s.path.clone())
            };
        }
        c
    })
}
