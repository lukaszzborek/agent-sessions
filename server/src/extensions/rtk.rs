//! Joins a bench trial's rtk history (`<trial>/rtk-data/history.db`) onto the shell calls it
//! rewrote.
//! The history is copied into our db per session, so it survives the trial dir being deleted.

use crate::model::{is_shell_tool, Event, EventKind};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::path::Path;

/// Sets `meta.rtk` on every shell tool call: the rtk rows it produced, or `[]` when rtk left it
/// alone.
/// No-op when the trial has no rtk history, on disk or stored.
pub fn annotate(conn: &Connection, trial: Option<&Path>, session: &Path, events: &mut [Event]) {
    let stored = || serde_json::from_str(&crate::db::rtk(conn, session)?).ok();
    let Some(rows) = trial
        .and_then(|t| snapshot(conn, t, session))
        .or_else(stored)
    else {
        return;
    };
    let calls: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.kind == EventKind::ToolCall && e.tool_name.as_deref().is_some_and(is_shell_tool)
        })
        .map(|(i, _)| i)
        .collect();
    let mut per_call: Vec<Vec<Value>> = vec![vec![]; calls.len()];
    for (ts, row) in rows {
        // rtk normalizes quoting and flags in `original_cmd`, so text can't be matched; a row is
        // logged while its call runs, which makes the latest call started before it the owner.
        let owner = calls.iter().rposition(|&i| {
            events[i]
                .ts
                .as_deref()
                .is_some_and(|t| millis(t) <= millis(&ts))
        });
        if let Some(c) = owner {
            per_call[c].push(row);
        }
    }
    for (&i, r) in calls.iter().zip(per_call) {
        let meta = events[i].meta.get_or_insert_with(|| json!({}));
        if let Some(m) = meta.as_object_mut() {
            m.insert("rtk".into(), Value::Array(r));
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SS.mmm` prefix: both sides are UTC but differ in precision and zone suffix.
fn millis(ts: &str) -> &str {
    ts.get(..23).unwrap_or(ts)
}

/// Copies the trial's rtk history into our db under the session's path; returns the rows read.
pub fn snapshot(conn: &Connection, trial: &Path, session: &Path) -> Option<Vec<(String, Value)>> {
    let rows = rows(&trial.join("rtk-data/history.db"))?;
    crate::db::save_rtk(
        conn,
        session,
        &serde_json::to_string(&rows).unwrap_or_default(),
    );
    Some(rows)
}

fn rows(db: &Path) -> Option<Vec<(String, Value)>> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let mut stmt = conn
        .prepare("select timestamp, original_cmd, rtk_cmd, input_tokens, output_tokens, saved_tokens, exec_time_ms from commands order by id")
        .ok()?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                json!({
                    "cmd": r.get::<_, String>(1)?,
                    "rtk_cmd": r.get::<_, String>(2)?,
                    "input": r.get::<_, i64>(3)?,
                    "output": r.get::<_, i64>(4)?,
                    "saved": r.get::<_, i64>(5)?,
                    "ms": r.get::<_, i64>(6)?,
                }),
            ))
        })
        .ok()?
        .flatten()
        .collect();
    Some(rows)
}
