use crate::model::{Agent, SessionSummary};
use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Row {
    pub agent: Agent,
    pub mtime: u64,
    pub size: u64,
    pub version: u32,
    pub summary: SessionSummary,
}

/// Opens the db file, creating it and the schema when missing.
///
/// # Errors
/// Fails when the file cannot be opened or the schema cannot be created.
pub fn open(path: &Path) -> anyhow::Result<Connection> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let c = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS sessions (
            path TEXT PRIMARY KEY,
            agent TEXT NOT NULL,
            mtime INTEGER NOT NULL,
            size INTEGER NOT NULL,
            version INTEGER NOT NULL,
            summary TEXT NOT NULL,
            raw BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS tags (
            agent TEXT NOT NULL,
            id TEXT NOT NULL,
            tags TEXT NOT NULL,
            PRIMARY KEY (agent, id)
         );
         CREATE TABLE IF NOT EXISTS telemetry (
            id TEXT PRIMARY KEY,
            fetched INTEGER NOT NULL,
            data TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS rtk (
            path TEXT PRIMARY KEY,
            rows TEXT NOT NULL
         );",
    )
    .context("init schema")?;
    migrate_subagent_ids(&c);
    Ok(c)
}

/// Claude subagent ids changed from `<agentId>` to `<parentSid>~<agentId>`; re-key the tags and
/// telemetry stored under the old id. A copied subagent file exists under several parents, so the
/// row is copied to each new id before the old one is dropped. Finds nothing once migrated.
fn migrate_subagent_ids(c: &Connection) {
    let Ok(mut st) = c.prepare("SELECT path FROM sessions WHERE path LIKE '%subagents%'") else {
        return;
    };
    let mut new_ids: HashMap<String, Vec<String>> = HashMap::new();
    for path in st
        .query_map([], |r| r.get::<_, String>(0))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = Path::new(&path);
        let name = |p: Option<&Path>| p?.file_name()?.to_str().map(str::to_string);
        let dir = path.parent();
        let (Some(aid), Some(sid)) = (
            path.file_stem()
                .and_then(|x| x.to_str())
                .and_then(|x| x.strip_prefix("agent-")),
            name(dir.and_then(Path::parent)).filter(|_| name(dir).as_deref() == Some("subagents")),
        ) else {
            continue;
        };
        new_ids
            .entry(aid.to_string())
            .or_default()
            .push(format!("{sid}~{aid}"));
    }
    let claude = Agent::Claude.as_str();
    for (aid, news) in new_ids {
        for new in news {
            warn(
                "migrate tags",
                c.execute(
                    "INSERT OR IGNORE INTO tags (agent, id, tags) SELECT agent, ?1, tags FROM tags WHERE agent=?2 AND id=?3",
                    params![new, claude, aid],
                ),
            );
            warn(
                "migrate telemetry",
                c.execute(
                    "INSERT OR IGNORE INTO telemetry (id, fetched, data) SELECT ?1, fetched, data FROM telemetry WHERE id=?2",
                    params![new, aid],
                ),
            );
        }
        warn(
            "migrate tags",
            c.execute(
                "DELETE FROM tags WHERE agent=?1 AND id=?2",
                params![claude, aid],
            ),
        );
        warn(
            "migrate telemetry",
            c.execute("DELETE FROM telemetry WHERE id=?1", params![aid]),
        );
    }
}

pub fn load_all(c: &Connection) -> HashMap<PathBuf, Row> {
    let mut st = c
        .prepare("SELECT path, agent, mtime, size, version, summary FROM sessions")
        .expect("static SQL matches the schema from open()");
    st.query_map([], |r| {
        let path: String = r.get(0)?;
        let agent: String = r.get(1)?;
        let summary: String = r.get(5)?;
        Ok((
            path,
            agent,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            summary,
        ))
    })
    .expect("query takes no parameters")
    .filter_map(|r| r.ok())
    .filter_map(|(path, agent, mtime, size, version, summary)| {
        let agent = Agent::parse(&agent)?;
        let summary = serde_json::from_str(&summary).ok()?;
        Some((
            PathBuf::from(path),
            Row {
                agent,
                mtime: mtime as u64,
                size: size as u64,
                version: version as u32,
                summary,
            },
        ))
    })
    .collect()
}

/// Logs a failed write; the index still serves from memory, but the change is lost on restart.
pub fn warn<T>(what: &str, r: rusqlite::Result<T>) {
    if let Err(e) = r {
        eprintln!("db: {what} failed: {e}");
    }
}

/// zstd blob as stored in `sessions.raw`; done by the caller so it can run off the db lock.
pub fn compress(raw: &[u8]) -> Vec<u8> {
    zstd::encode_all(raw, 3).expect("zstd encoding of an in-memory slice cannot fail")
}

/// Insert or replace full row; `blob` is `compress`ed raw. False when the write failed.
pub fn upsert(c: &Connection, path: &Path, row: &Row, blob: &[u8]) -> bool {
    let r = c.execute(
        "INSERT OR REPLACE INTO sessions (path, agent, mtime, size, version, summary, raw) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            path.to_string_lossy(),
            row.agent.as_str(),
            row.mtime as i64,
            row.size as i64,
            row.version as i64,
            serde_json::to_string(&row.summary).unwrap_or_default(),
            blob
        ],
    );
    let ok = r.is_ok();
    warn("upsert session", r);
    ok
}

/// Update only summary + version (raw unchanged).
pub fn update_summary(c: &Connection, path: &Path, version: u32, summary: &SessionSummary) {
    warn(
        "update session summary",
        c.execute(
            "UPDATE sessions SET version=?1, summary=?2 WHERE path=?3",
            params![
                version as i64,
                serde_json::to_string(summary).unwrap_or_default(),
                path.to_string_lossy()
            ],
        ),
    );
}

pub fn raw(c: &Connection, path: &Path) -> Option<Vec<u8>> {
    let blob: Vec<u8> = c
        .query_row(
            "SELECT raw FROM sessions WHERE path=?1",
            params![path.to_string_lossy()],
            |r| r.get(0),
        )
        .optional()
        .ok()??;
    zstd::decode_all(&blob[..]).ok()
}

/// User-set tags per (agent, session id).
pub fn load_tags(c: &Connection) -> HashMap<(Agent, String), Vec<String>> {
    let mut st = c
        .prepare("SELECT agent, id, tags FROM tags")
        .expect("static SQL matches the schema from open()");
    st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })
    .expect("query takes no parameters")
    .filter_map(|r| r.ok())
    .filter_map(|(a, id, t)| Some(((Agent::parse(&a)?, id), serde_json::from_str(&t).ok()?)))
    .collect()
}

pub fn set_tags(c: &Connection, agent: Agent, id: &str, tags: &[String]) {
    if tags.is_empty() {
        warn(
            "delete tags",
            c.execute(
                "DELETE FROM tags WHERE agent=?1 AND id=?2",
                params![agent.as_str(), id],
            ),
        );
    } else {
        warn(
            "save tags",
            c.execute(
                "INSERT OR REPLACE INTO tags (agent, id, tags) VALUES (?1,?2,?3)",
                params![
                    agent.as_str(),
                    id,
                    serde_json::to_string(tags).unwrap_or_default()
                ],
            ),
        );
    }
}

/// Stored OTel telemetry JSON of a Claude session (`null` = nothing was recorded) and when it was
/// fetched (unix s).
pub fn telemetry(c: &Connection, id: &str) -> Option<(u64, String)> {
    c.query_row(
        "SELECT fetched, data FROM telemetry WHERE id=?1",
        params![id],
        |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
    )
    .optional()
    .ok()?
}

pub fn save_telemetry(c: &Connection, id: &str, fetched: u64, data: &str) {
    // A later `null` means the backends' retention ran out, not that the stored data was wrong.
    warn(
        "save telemetry",
        c.execute(
            "INSERT INTO telemetry (id, fetched, data) VALUES (?1,?2,?3)
         ON CONFLICT(id) DO UPDATE SET fetched=excluded.fetched, data=CASE WHEN excluded.data='null' THEN data ELSE excluded.data END",
            params![id, fetched as i64, data],
        ),
    );
}

/// Stored rtk history rows (JSON) of the run a session file belongs to.
pub fn rtk(c: &Connection, path: &Path) -> Option<String> {
    c.query_row(
        "SELECT rows FROM rtk WHERE path=?1",
        params![path.to_string_lossy()],
        |r| r.get(0),
    )
    .optional()
    .ok()?
}

pub fn save_rtk(c: &Connection, path: &Path, rows: &str) {
    warn(
        "save rtk",
        c.execute(
            "INSERT INTO rtk (path, rows) VALUES (?1,?2) ON CONFLICT(path) DO UPDATE SET rows=excluded.rows WHERE rows != excluded.rows",
            params![path.to_string_lossy(), rows],
        ),
    );
}
