pub mod claude;
pub mod codex;
pub mod pi;

use serde_json::Value;

/// Parse JSONL bytes, yielding values (skips broken lines).
pub fn read_jsonl(data: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    for line in data.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            out.push(v);
        }
    }
    out
}

pub fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|x| x.to_string())
}
pub fn u(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}
