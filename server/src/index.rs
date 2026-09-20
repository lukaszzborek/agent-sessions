use crate::config::{Source, USER_SOURCE};
use crate::db;
use crate::model::*;
use crate::parsers;
use anyhow::Context;
use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

/// Bump when SessionSummary gains fields so stale DB rows get reparsed from stored raw.
const VERSION: u32 = 11;

/// Data dir was renamed `ai-sessions` -> `agent-sessions`; carry the old sessions.db (archived
/// sessions + tags have no other copy) over on first run after the upgrade.
fn migrate_data_dir(data_root: &Path, new_dir: &Path) {
    let old_dir = data_root.join("ai-sessions");
    if new_dir.exists() || !old_dir.exists() {
        return;
    }
    match std::fs::rename(&old_dir, new_dir) {
        Ok(()) => eprintln!(
            "migrated data dir {} -> {}",
            old_dir.display(),
            new_dir.display()
        ),
        Err(e) => eprintln!(
            "cannot migrate data dir {} -> {}: {} (move it manually or sessions.db will be recreated empty)",
            old_dir.display(),
            new_dir.display(),
            e
        ),
    }
}

#[derive(Debug)]
pub struct Roots {
    /// Built-in `user` source (own home) followed by the enabled `sources` from config.
    pub sources: Vec<Source>,
    pub db_file: PathBuf,
}

impl Roots {
    /// # Errors
    /// Fails when the OS reports no home directory.
    pub fn default_roots() -> anyhow::Result<Roots> {
        let home = dirs::home_dir().context("no home directory")?;
        let data_root = dirs::data_dir().unwrap_or_else(|| home.join(".local/share"));
        let data = data_root.join("agent-sessions");
        migrate_data_dir(&data_root, &data);
        let mut sources = vec![Source::home(USER_SOURCE, home)];
        sources.extend(crate::config::get().sources.iter().cloned());
        Ok(Roots {
            sources,
            db_file: data.join("sessions.db"),
        })
    }
    pub fn codex_homes(&self) -> Vec<PathBuf> {
        self.all()
            .into_iter()
            .filter(|(a, _)| *a == Agent::Codex)
            .filter_map(|(_, p)| p.parent().map(Path::to_path_buf))
            .collect()
    }
    /// (agent, session dir); globs resolved fresh each call so new dirs are picked up by the
    /// watcher
    fn all(&self) -> Vec<(Agent, PathBuf)> {
        let mut out = Vec::new();
        for s in &self.sources {
            for (agent, pat) in [
                (Agent::Claude, &s.claude),
                (Agent::Codex, &s.codex),
                (Agent::Pi, &s.pi),
            ] {
                let Some(pat) = pat else { continue };
                // Only `pat` is a glob; the base path may legitimately contain `[`, `*`, `?`.
                let full = format!(
                    "{}/{}",
                    glob::Pattern::escape(&s.path.to_string_lossy()),
                    pat
                );
                match glob::glob(&full) {
                    Ok(dirs) => {
                        out.extend(dirs.flatten().filter(|d| d.is_dir()).map(|d| (agent, d)))
                    }
                    Err(e) => eprintln!("source {}: bad pattern {}: {}", s.name, full, e),
                }
            }
        }
        out
    }
    /// Source owning `path` by location (deepest match); lets files gone from disk follow config
    /// changes.
    /// Matched on the literal part of each pattern, not on `path` alone: the built-in source's
    /// `path` is the whole home dir and would otherwise claim the archived sessions of every
    /// removed source below it.
    fn source_of(&self, path: &Path) -> Option<&Source> {
        let depth = |s: &Source| {
            [&s.claude, &s.codex, &s.pi]
                .into_iter()
                .flatten()
                .map(|pat| s.path.join(literal_prefix(pat)))
                .filter(|base| path.starts_with(base))
                .map(|base| base.as_os_str().len())
                .max()
        };
        self.sources
            .iter()
            .filter_map(|s| depth(s).map(|d| (d, s)))
            .max_by_key(|(d, _)| *d)
            .map(|(_, s)| s)
    }
    /// The source's `meta` file in the nearest ancestor dir of a session file.
    fn meta_of(&self, path: &Path) -> Option<PathBuf> {
        let s = self.source_of(path)?;
        let meta = s.meta.as_ref()?;
        path.ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(&s.path))
            .map(|p| p.join(meta))
            .find(|m| m.is_file())
    }
    /// Dir of the run a session file belongs to (the one holding its meta file), if any.
    pub fn run_dir_of(&self, path: &Path) -> Option<PathBuf> {
        self.meta_of(path)?.parent().map(Path::to_path_buf)
    }
}

/// Leading components of a glob pattern that contain no glob syntax.
fn literal_prefix(pat: &str) -> PathBuf {
    Path::new(pat)
        .components()
        .take_while(|c| !c.as_os_str().to_string_lossy().contains(['*', '?', '[']))
        .collect()
}

/// Tags from the `tags` string array of a meta file, led by the source name so everything it
/// describes shares one tag.
/// None when the file is unreadable or not JSON (still being written), so tags already stored are
/// kept.
fn meta_tags(meta: &Path, source: &str) -> Option<Vec<String>> {
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(meta).ok()?).ok()?;
    let mut tags = vec![source.to_string()];
    for t in m["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str())
        .map(str::trim)
    {
        if !t.is_empty() && !tags.iter().any(|x| x == t) {
            tags.push(t.to_string());
        }
    }
    Some(tags)
}

#[derive(Debug)]
pub struct Index {
    pub sessions: Vec<SessionSummary>,
    /// (agent, id) -> position
    pub by_key: HashMap<(Agent, String), usize>,
}

fn jsonl_files(root: &Path) -> Vec<PathBuf> {
    if !root.exists() {
        return vec![];
    }
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .map(|e| e.into_path())
        .collect()
}

fn stat(p: &Path) -> (u64, u64) {
    let m = std::fs::metadata(p).ok();
    let mtime = m
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (mtime, m.map(|m| m.len()).unwrap_or(0))
}

/// path -> (mtime, size) of every session file on disk; cheap change detection for the watcher
pub fn snapshot(roots: &Roots) -> HashMap<PathBuf, (u64, u64)> {
    let files: Vec<PathBuf> = roots
        .all()
        .into_iter()
        .flat_map(|(_, r)| jsonl_files(&r))
        .collect();
    // The meta file lands after the session files are copied out, so its arrival must trigger a
    // re-tag.
    let metas: Vec<PathBuf> = files.iter().filter_map(|p| roots.meta_of(p)).collect();
    files
        .into_iter()
        .chain(metas)
        .map(|p| {
            let m = stat(&p);
            (p, m)
        })
        .collect()
}

pub fn parse_bytes(
    agent: Agent,
    path: &Path,
    data: &[u8],
    titles: &HashMap<String, String>,
) -> anyhow::Result<Parsed> {
    match agent {
        Agent::Claude => parsers::claude::parse(path, data),
        Agent::Codex => parsers::codex::parse(path, data, titles),
        Agent::Pi => parsers::pi::parse(path, data),
    }
}

#[derive(Debug)]
enum Outcome {
    /// unchanged, reuse DB row
    Keep(SessionSummary),
    /// new/changed on disk: store raw + summary
    Store(db::Row, Vec<u8>),
    /// on-disk file gone, stale version: reparsed from stored raw
    Resummarize(db::Row),
}

/// Locks `conn` only for the DB read up front and the write loop at the end; the walk + parse in
/// between (the slow part on a full reparse) runs without holding it, so async handlers on other
/// runtime workers are not blocked.
pub fn build(roots: &Roots, conn: &Mutex<Connection>) -> Index {
    // Price change must recompute stored costs, same as a parser change.
    let version = VERSION ^ crate::pricing::fingerprint();
    let t0 = std::time::Instant::now();
    let rows = db::load_all(
        &conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let titles = parsers::codex::load_titles(&roots.codex_homes());

    // Patterns can match nested dirs and sources can overlap, so one file may be reached several
    // times; keyed by path so it is indexed once.
    let mut found: HashMap<PathBuf, Agent> = HashMap::new();
    for (agent, root) in roots.all() {
        for p in jsonl_files(&root) {
            found.insert(p, agent);
        }
    }
    let mut work: Vec<(Agent, PathBuf, bool)> = found
        .into_iter()
        .map(|(p, agent)| (agent, p, true))
        .collect();
    let on_disk = work.len();
    for (p, r) in &rows {
        // A row whose path no longer matches any configured source (source removed/disabled,
        // glob changed) is dropped rather than surfaced as archived, matching the file-still-on-
        // disk-but-unreached case above (also silently skipped).
        if !p.exists() && roots.source_of(p).is_some() {
            work.push((r.agent, p.clone(), false));
        }
    }

    let results: Vec<(PathBuf, Outcome)> = work
        .into_par_iter()
        .filter_map(|(agent, path, present)| {
            let row = rows.get(&path);
            if !present {
                let mut s = row?.summary.clone();
                s.archived = true;
                if row?.version == version {
                    return Some((path, Outcome::Keep(s)));
                }
                // Stale summary; raw fetched later on main thread (conn not Sync).
                return Some((
                    path,
                    Outcome::Resummarize(db::Row {
                        agent,
                        mtime: row?.mtime,
                        size: row?.size,
                        version,
                        summary: s,
                    }),
                ));
            }
            let (mtime, size) = stat(&path);
            if let Some(r) = row {
                if r.mtime == mtime && r.size == size && r.version == version {
                    let mut s = r.summary.clone();
                    // File is back on disk: a stored `archived=true` (e.g. mount came back) is stale.
                    s.archived = false;
                    return Some((path, Outcome::Keep(s)));
                }
            }
            let data = std::fs::read(&path).ok()?;
            match parse_bytes(agent, &path, &data, &titles) {
                Ok(p) => Some((
                    path,
                    Outcome::Store(
                        db::Row {
                            agent,
                            mtime,
                            size,
                            version,
                            summary: p.summary,
                        },
                        data,
                    ),
                )),
                Err(e) => {
                    eprintln!("parse error {}: {}", path.display(), e);
                    None
                }
            }
        })
        .collect();

    let mut stored = 0;
    let mut sessions: Vec<SessionSummary> = Vec::with_capacity(results.len());
    let mut tag_cache: HashMap<PathBuf, Option<Vec<String>>> = HashMap::new();
    let guard = conn
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let conn: &Connection = &guard;
    let _ = conn.execute_batch("BEGIN");
    for (path, o) in results {
        let source = roots.source_of(&path).map(|s| s.name.as_str());
        // Meta tags are persisted in the summary so archived sessions keep them after the source
        // dir is cleaned.
        let auto = roots.meta_of(&path).and_then(|m| {
            tag_cache
                .entry(m.clone())
                .or_insert_with(|| meta_tags(&m, source.unwrap_or(USER_SOURCE)))
                .clone()
        });
        match o {
            Outcome::Keep(mut s) => {
                let mut dirty = rows
                    .get(&path)
                    .is_some_and(|r| r.summary.archived != s.archived);
                if let Some(t) = auto.filter(|t| *t != s.tags) {
                    s.tags = t;
                    dirty = true;
                }
                // Path moved under another source in config (renamed, or a new overlapping entry).
                if let Some(src) = source.filter(|src| *src != s.source) {
                    s.source = src.to_string();
                    dirty = true;
                }
                if dirty {
                    db::update_summary(conn, &path, version, &s);
                }
                sessions.push(s);
            }
            Outcome::Store(mut row, raw) => {
                // meta_tags() returns None when the meta file is unreadable (still being
                // written); keep whatever tags were stored for this path rather than wiping them.
                row.summary.tags = auto
                    .or_else(|| rows.get(&path).map(|r| r.summary.tags.clone()))
                    .unwrap_or_default();
                row.summary.source = source.unwrap_or(USER_SOURCE).to_string();
                db::upsert(conn, &path, &row, &raw);
                stored += 1;
                sessions.push(row.summary);
            }
            Outcome::Resummarize(mut row) => {
                if let Some(raw) = db::raw(conn, &path) {
                    if let Ok(p) = parse_bytes(row.agent, &path, &raw, &titles) {
                        let old_tags = std::mem::take(&mut row.summary.tags);
                        let old_source = std::mem::take(&mut row.summary.source);
                        row.summary = p.summary;
                        row.summary.source = source.map_or(old_source, str::to_string);
                        row.summary.archived = true;
                        row.summary.tags = auto.unwrap_or(old_tags);
                        db::update_summary(conn, &path, version, &row.summary);
                    }
                }
                sessions.push(row.summary);
            }
        }
    }
    let _ = conn.execute_batch("COMMIT");
    let manual = db::load_tags(conn);
    for s in sessions.iter_mut() {
        if let Some(t) = manual.get(&(s.agent, s.id.clone())) {
            for x in t {
                if !s.tags.contains(x) {
                    s.tags.push(x.clone());
                }
            }
        }
    }

    // Drop empty sessions (no user and no assistant messages).
    sessions.retain(|s| s.user_msgs + s.assistant_msgs > 0 || s.parent_id.is_some());
    // Same (agent, id) can exist under two paths (Claude moves the file into a worktree project
    // dir; old path stays in DB as archived) - keep one: live over archived, then most recent.
    sessions.sort_by(|a, b| a.archived.cmp(&b.archived).then(b.ended.cmp(&a.ended)));
    let mut seen: HashSet<(Agent, String)> = HashSet::new();
    sessions.retain(|s| seen.insert((s.agent, s.id.clone())));
    // Link children.
    let mut by_key: HashMap<(Agent, String), usize> = HashMap::new();
    for (i, s) in sessions.iter().enumerate() {
        by_key.insert((s.agent, s.id.clone()), i);
    }
    let links: Vec<(usize, String)> = sessions
        .iter()
        .filter_map(|s| {
            s.parent_id.as_ref().and_then(|p| {
                by_key
                    .get(&(s.agent, p.clone()))
                    .map(|&pi| (pi, s.id.clone()))
            })
        })
        .collect();
    for (pi, cid) in links {
        if !sessions[pi].children.contains(&cid) {
            sessions[pi].children.push(cid);
        }
    }
    sessions.sort_by(|a, b| b.started.cmp(&a.started));
    let by_key = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| ((s.agent, s.id.clone()), i))
        .collect();
    let archived = sessions.iter().filter(|s| s.archived).count();
    eprintln!(
        "indexed {} sessions ({} files on disk, {} stored, {} archived) in {:.1}s",
        sessions.len(),
        on_disk,
        stored,
        archived,
        t0.elapsed().as_secs_f32()
    );
    Index { sessions, by_key }
}
