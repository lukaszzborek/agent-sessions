//! USD per 1M tokens. Built-in table + optional additions/overrides at
//! ~/.config/agent-sessions/prices.toml (`[model-prefix]` tables with `input`, `output` and
//! optional `cache_read`, `cache_write_5m`, `cache_write_1h`).
use crate::model::Usage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(from = "FilePrice")]
pub struct Price {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub output: f64,
}

/// Shape accepted in prices.toml; providers without cache pricing bill those tokens as input.
#[derive(Debug, Deserialize)]
struct FilePrice {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write_5m: Option<f64>,
    cache_write_1h: Option<f64>,
}

impl From<FilePrice> for Price {
    fn from(p: FilePrice) -> Price {
        Price {
            input: p.input,
            cache_read: p.cache_read.unwrap_or(p.input),
            cache_write_5m: p.cache_write_5m.unwrap_or(p.input),
            cache_write_1h: p.cache_write_1h.unwrap_or(p.input),
            output: p.output,
        }
    }
}

const fn anthropic(input: f64, output: f64, cache_read: f64) -> Price {
    Price {
        input,
        cache_read,
        cache_write_5m: input * 1.25,
        cache_write_1h: input * 2.0,
        output,
    }
}
const fn openai(input: f64, cached: f64, output: f64) -> Price {
    Price {
        input,
        cache_read: cached,
        cache_write_5m: input,
        cache_write_1h: input,
        output,
    }
}

fn builtin() -> BTreeMap<String, Price> {
    let t: &[(&str, Price)] = &[
        // Anthropic (2026-06 list prices).
        ("claude-fable-5-1", anthropic(10.0, 50.0, 0.25)),
        ("claude-mythos-5-1", anthropic(10.0, 50.0, 0.25)),
        ("claude-fable-5", anthropic(10.0, 50.0, 1.0)),
        ("claude-mythos-5", anthropic(10.0, 50.0, 1.0)),
        ("claude-opus-5", anthropic(5.0, 25.0, 0.5)),
        ("claude-opus-4-8", anthropic(5.0, 25.0, 0.5)),
        ("claude-opus-4-7", anthropic(5.0, 25.0, 0.5)),
        ("claude-opus-4-6", anthropic(5.0, 25.0, 0.5)),
        ("claude-opus-4-5", anthropic(5.0, 25.0, 0.5)),
        ("claude-opus-4", anthropic(15.0, 75.0, 1.5)),
        ("claude-sonnet-5", anthropic(2.0, 10.0, 0.2)),
        ("claude-sonnet-4-6", anthropic(3.0, 15.0, 0.3)),
        ("claude-sonnet-4", anthropic(3.0, 15.0, 0.3)),
        ("claude-haiku-4-5", anthropic(1.0, 5.0, 0.1)),
        // OpenAI (developers.openai.com/api/docs/pricing, 2026-09).
        ("gpt-5.6-sol", openai(4.0, 0.40, 20.0)),
        ("gpt-5.6-terra", openai(2.0, 0.20, 12.0)),
        ("gpt-5.6-luna", openai(0.2, 0.02, 1.2)),
        ("gpt-5.5", openai(5.0, 0.50, 30.0)),
        ("gpt-5.4-mini", openai(0.75, 0.075, 4.5)),
        ("gpt-5.4-nano", openai(0.2, 0.02, 1.25)),
        ("gpt-5.4", openai(2.5, 0.25, 15.0)),
        ("gpt-5.3-codex", openai(1.75, 0.175, 14.0)),
        ("gpt-5.2", openai(1.75, 0.175, 14.0)),
        ("gpt-5.1", openai(1.25, 0.125, 10.0)),
        ("gpt-5-mini", openai(0.25, 0.025, 2.0)),
        ("gpt-5-nano", openai(0.05, 0.005, 0.4)),
        ("gpt-5", openai(1.25, 0.125, 10.0)),
    ];
    t.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn table() -> &'static BTreeMap<String, Price> {
    static T: OnceLock<BTreeMap<String, Price>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = builtin();
        if let Some(p) = dirs::config_dir().map(|d| d.join("agent-sessions/prices.toml")) {
            if let Ok(b) = std::fs::read_to_string(&p) {
                match toml::from_str::<BTreeMap<String, Price>>(&b) {
                    Ok(o) => {
                        eprintln!("loaded {} prices from {}", o.len(), p.display());
                        t.extend(o);
                    }
                    Err(e) => eprintln!("bad {}: {}", p.display(), e),
                }
            }
        }
        t
    })
}

/// Changes whenever any price does, so stored costs can be invalidated.
pub fn fingerprint() -> u32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (k, p) in table() {
        k.hash(&mut h);
        for v in [
            p.input,
            p.cache_read,
            p.cache_write_5m,
            p.cache_write_1h,
            p.output,
        ] {
            v.to_bits().hash(&mut h);
        }
    }
    h.finish() as u32
}

/// Longest-prefix match on model id (provider prefix like "xai/" stripped).
pub fn price_for(model: &str) -> Option<Price> {
    let m = model.rsplit('/').next().unwrap_or(model);
    table()
        .iter()
        .filter(|(k, _)| m.starts_with(k.as_str()))
        .max_by_key(|(k, _)| k.len())
        .map(|(_, p)| *p)
}

pub fn cost(model: &str, u: &Usage) -> Option<f64> {
    let p = price_for(model)?;
    let cw5 = u.cache_write.saturating_sub(u.cache_write_1h);
    Some(
        (u.input as f64 * p.input
            + u.cache_read as f64 * p.cache_read
            + cw5 as f64 * p.cache_write_5m
            + u.cache_write_1h as f64 * p.cache_write_1h
            + u.output as f64 * p.output)
            / 1e6,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_price_defaults_cache_to_input() {
        let t: BTreeMap<String, Price> =
            toml::from_str("[\"gpt-5.6-sol\"]\ninput = 2.0\noutput = 8.0\ncache_read = 0.5")
                .unwrap();
        let p = t["gpt-5.6-sol"];
        assert_eq!(
            (p.cache_read, p.cache_write_5m, p.cache_write_1h),
            (0.5, 2.0, 2.0)
        );
    }
}
