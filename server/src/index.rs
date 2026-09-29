use crate::config::{Source, USER_SOURCE};
use crate::db;
use crate::model::*;
use crate::parsers;
use anyhow::Context;
use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

/// Bump when SessionSummary gains fields so stale DB rows get reparsed from stored raw.
const VERSION: u32 = 12;

// A poisoned lock only means another thread panicked mid-access; the data is still usable.
fn lock(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(PoisonError::into_inner)
}

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

pub fn stat(p: &Path) -> (u64, u64) {
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

/// `skip`: Claude request ids whose usage another file counts (`SessionSummary::dup_request_ids`).
pub fn parse_bytes(
    agent: Agent,
    path: &Path,
    data: &[u8],
    titles: &HashMap<String, String>,
    skip: &HashSet<String>,
) -> anyhow::Result<Parsed> {
    match agent {
        Agent::Claude => parsers::claude::parse(path, data, skip),
        Agent::Codex => parsers::codex::parse(path, data, titles),
        Agent::Pi => parsers::pi::parse(path, data),
    }
}

/// Session file content: the file on disk, else the copy stored in the db.
pub fn read_source(path: &Path, conn: &Mutex<Connection>) -> Option<Vec<u8>> {
    std::fs::read(path)
        .ok()
        .or_else(|| db::raw(&lock(conn), path))
}

/// Forked/continued Claude sessions copy earlier lines (same requestId + usage) into a new file, so
/// each request is counted only in the file that started first; the others list it as a duplicate.
fn mark_duplicate_requests(sessions: &mut [SessionSummary]) {
    let mut order: Vec<usize> = (0..sessions.len()).collect();
    // Total order, so ties (copied files, forks replaying old timestamps) pick the same owner on
    // every build.
    order.sort_by(|&a, &b| {
        let (x, y) = (&sessions[a], &sessions[b]);
        (&x.started, &x.ended, &x.path).cmp(&(&y.started, &y.ended, &y.path))
    });
    let mut seen: HashSet<String> = HashSet::new();
    for i in order {
        let ids = std::mem::take(&mut sessions[i].request_ids);
        let (dups, fresh): (Vec<String>, Vec<String>) =
            ids.into_iter().partition(|id| seen.contains(id));
        seen.extend(fresh);
        sessions[i].dup_request_ids = dups;
    }
}

/// Result of reparsing a session with its duplicated requests skipped.
#[derive(Debug, Clone)]
struct Deduped {
    /// (mtime, size) of the file when parsed
    stat: (u64, u64),
    dup_request_ids: Vec<String>,
    usage: Usage,
    cost: Option<f64>,
    buckets: Vec<Bucket>,
    max_context: u64,
}

/// Per session path; only touched by `build`, which is serialized.
static DEDUPED: Mutex<BTreeMap<PathBuf, Deduped>> = Mutex::new(BTreeMap::new());

#[derive(Debug)]
enum Outcome {
    /// unchanged, reuse DB row
    Keep(SessionSummary),
    /// new/changed on disk: row + zstd blob of the raw file
    Store(db::Row, Vec<u8>),
    /// on-disk file gone, stale version: reparsed from stored raw
    Resummarize(db::Row),
}

/// `rows` mirrors the db's `sessions` table (load it once with `db::load_all`); it is updated in
/// place so later builds skip re-reading every summary.
/// Locks `conn` only for the final write loop; walk, parse, compression and meta-file reads run
/// without holding it, so handlers needing the db are not blocked by a full reparse.
pub fn build(
    roots: &Roots,
    conn: &Mutex<Connection>,
    rows: &mut HashMap<PathBuf, db::Row>,
) -> Index {
    // Price change must recompute stored costs, same as a parser change.
    let version = VERSION ^ crate::pricing::fingerprint();
    let t0 = std::time::Instant::now();
    let prev = &*rows;
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
    for (p, r) in prev {
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
            let row = prev.get(&path);
            if !present {
                let mut s = row?.summary.clone();
                s.archived = true;
                if row?.version == version {
                    return Some((path, Outcome::Keep(s)));
                }
                // Stale summary: reparse from the stored copy.
                let fresh = db::raw(&lock(conn), &path)
                    .and_then(|raw| parse_bytes(agent, &path, &raw, &titles, &HashSet::new()).ok())
                    .map(|p| p.summary);
                // Unparseable: keep the stale summary (and its old version) for the next build.
                let Some(mut summary) = fresh else {
                    return Some((path, Outcome::Keep(s)));
                };
                summary.tags = std::mem::take(&mut s.tags);
                summary.source = std::mem::take(&mut s.source);
                summary.archived = true;
                return Some((
                    path,
                    Outcome::Resummarize(db::Row {
                        agent,
                        mtime: row?.mtime,
                        size: row?.size,
                        version,
                        summary,
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
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(e) => {
                    // Present but unreadable (locked, permissions): the stored copy beats losing
                    // the session.
                    eprintln!("cannot read {}: {}", path.display(), e);
                    let mut s = row?.summary.clone();
                    s.archived = false;
                    return Some((path, Outcome::Keep(s)));
                }
            };
            match parse_bytes(agent, &path, &data, &titles, &HashSet::new()) {
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
                        db::compress(&data),
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
    // Meta tags are persisted in the summary so archived sessions keep them after the source dir is
    // cleaned. Read before taking the lock: it is file I/O.
    let mut tag_cache: HashMap<PathBuf, Option<Vec<String>>> = HashMap::new();
    let results: Vec<_> = results
        .into_iter()
        .map(|(path, o)| {
            let source = roots.source_of(&path).map(|s| s.name.as_str());
            let auto = roots.meta_of(&path).and_then(|m| {
                tag_cache
                    .entry(m.clone())
                    .or_insert_with(|| meta_tags(&m, source.unwrap_or(USER_SOURCE)))
                    .clone()
            });
            (path, o, source, auto)
        })
        .collect();
    let guard = lock(conn);
    let tx: &Connection = &guard;
    db::warn("begin", tx.execute_batch("BEGIN"));
    for (path, o, source, auto) in results {
        match o {
            Outcome::Keep(mut s) => {
                // The stored copy of a non-UTF-8 path is lossy.
                s.path = path.clone();
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
                    // Keeps the row's own version: an unreadable file falls back to a stale row.
                    if let Some(r) = rows.get_mut(&path) {
                        db::update_summary(tx, &path, r.version, &s);
                        r.summary = s.clone();
                    }
                }
                sessions.push(s);
            }
            Outcome::Store(mut row, blob) => {
                // meta_tags() returns None when the meta file is unreadable (still being
                // written); keep whatever tags were stored for this path rather than wiping them.
                row.summary.tags = auto
                    .or_else(|| rows.get(&path).map(|r| r.summary.tags.clone()))
                    .unwrap_or_default();
                row.summary.source = source.unwrap_or(USER_SOURCE).to_string();
                let saved = db::upsert(tx, &path, &row, &blob);
                stored += 1;
                sessions.push(row.summary.clone());
                // Left out of the mirror on failure, so the next build parses and stores it again.
                if saved {
                    rows.insert(path, row);
                }
            }
            Outcome::Resummarize(mut row) => {
                if let Some(t) = auto {
                    row.summary.tags = t;
                }
                if let Some(src) = source {
                    row.summary.source = src.to_string();
                }
                db::update_summary(tx, &path, version, &row.summary);
                sessions.push(row.summary.clone());
                rows.insert(path, row);
            }
        }
    }
    db::warn("commit", tx.execute_batch("COMMIT"));
    let manual = db::load_tags(tx);
    drop(guard);
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
    // After the (agent, id) dedupe above, so a dropped path's copy can't take a request from the
    // file that stays.
    mark_duplicate_requests(&mut sessions);
    // Reparsing without the duplicated requests is the expensive part, so it is reused until the
    // file or its duplicate set changes.
    let prior = std::mem::take(&mut *DEDUPED.lock().unwrap_or_else(PoisonError::into_inner));
    let deduped: Vec<(PathBuf, Deduped)> = sessions
        .par_iter_mut()
        .filter(|s| !s.dup_request_ids.is_empty())
        .filter_map(|s| {
            let stat = stat(&s.path);
            let d = match prior
                .get(&s.path)
                .filter(|d| d.stat == stat && d.dup_request_ids == s.dup_request_ids)
            {
                Some(d) => d.clone(),
                None => {
                    let skip: HashSet<String> = s.dup_request_ids.iter().cloned().collect();
                    let p = read_source(&s.path, conn)
                        .and_then(|data| parse_bytes(s.agent, &s.path, &data, &titles, &skip).ok())?
                        .summary;
                    Deduped {
                        stat,
                        dup_request_ids: s.dup_request_ids.clone(),
                        usage: p.usage,
                        cost: p.cost,
                        buckets: p.buckets,
                        max_context: p.max_context,
                    }
                }
            };
            (s.usage, s.cost, s.buckets, s.max_context) =
                (d.usage.clone(), d.cost, d.buckets.clone(), d.max_context);
            Some((s.path.clone(), d))
        })
        .collect();
    *DEDUPED.lock().unwrap_or_else(PoisonError::into_inner) = deduped.into_iter().collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(ts: &str, requests: &[&str]) -> String {
        requests
            .iter()
            .map(|r| {
                format!(
                    r#"{{"type":"assistant","timestamp":"{ts}","requestId":"{r}","message":{{"model":"claude-sonnet-5","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
                ) + "\n"
            })
            .collect()
    }

    #[test]
    fn forked_session_does_not_double_count_shared_requests() {
        let none = HashSet::new();
        let titles = HashMap::new();
        let parse = |p: &str, data: &str, skip: &HashSet<String>| {
            parse_bytes(Agent::Claude, Path::new(p), data.as_bytes(), &titles, skip)
                .unwrap()
                .summary
        };
        let orig = parse(
            "/p/a.jsonl",
            &fixture("2026-09-01T10:00:00.000Z", &["r1", "r2"]),
            &none,
        );
        // The fork replays r1/r2 (same ids, same old timestamps) and adds r3.
        let fork_data = fixture("2026-09-01T10:00:00.000Z", &["r1", "r2"])
            + &fixture("2026-09-02T10:00:00.000Z", &["r3"]);
        let fork = parse("/p/b.jsonl", &fork_data, &none);
        assert_eq!(fork.usage.output, 15);

        // Fork listed first: the earlier-started file still owns the shared requests.
        let mut sessions = vec![fork, orig];
        sessions[0].started = Some("2026-09-01T10:00:00.000Z".into());
        sessions[1].started = Some("2026-08-31T10:00:00.000Z".into());
        mark_duplicate_requests(&mut sessions);
        assert_eq!(sessions[0].dup_request_ids, ["r1", "r2"]);
        assert!(sessions[1].dup_request_ids.is_empty());

        let skip: HashSet<String> = sessions[0].dup_request_ids.iter().cloned().collect();
        let deduped = parse("/p/b.jsonl", &fork_data, &skip);
        assert_eq!(deduped.usage.output, 5);
    }

    #[test]
    fn tied_started_is_owned_by_the_same_file_regardless_of_order() {
        let none = HashSet::new();
        let titles = HashMap::new();
        let parse = |p: &str| {
            parse_bytes(
                Agent::Claude,
                Path::new(p),
                fixture("2026-09-01T10:00:00.000Z", &["r1"]).as_bytes(),
                &titles,
                &none,
            )
            .unwrap()
            .summary
        };
        for paths in [["/p/a.jsonl", "/p/b.jsonl"], ["/p/b.jsonl", "/p/a.jsonl"]] {
            let mut sessions: Vec<_> = paths.iter().map(|p| parse(p)).collect();
            assert_eq!(sessions[0].started, sessions[1].started);
            mark_duplicate_requests(&mut sessions);
            let owner: Vec<_> = sessions
                .iter()
                .filter(|s| s.dup_request_ids.is_empty())
                .map(|s| s.path.clone())
                .collect();
            assert_eq!(owner, [PathBuf::from("/p/a.jsonl")]);
        }
    }
}
