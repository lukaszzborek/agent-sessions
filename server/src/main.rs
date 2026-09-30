mod config;
mod db;
mod extensions;
mod index;
mod model;
mod parsers;
mod pricing;

use anyhow::Context;
use axum::{
    extract::Request,
    extract::{Path, State},
    http::{header, StatusCode, Uri},
    middleware::Next,
    response::{sse, IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use extensions::{rtk, telemetry};
use index::{Index, Roots};
use model::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};
use tokio_stream::StreamExt;

#[derive(Debug, rust_embed::Embed)]
#[folder = "../ui/dist/"]
struct Assets;

/// Last parsed session file, so the detail and its lazy event fetches parse it once.
#[derive(Debug)]
struct ParsedCache {
    path: PathBuf,
    /// (mtime, size) of the file when parsed
    stat: (u64, u64),
    dup_request_ids: Vec<String>,
    parsed: Arc<Parsed>,
}

#[derive(Debug)]
struct AppState {
    roots: Roots,
    index: RwLock<Index>,
    db: Mutex<turso::Connection>,
    /// Mirror of the db's `sessions` table, so a build does not reload every summary. The lock
    /// also serializes index builds.
    rows: tokio::sync::Mutex<HashMap<PathBuf, db::Row>>,
    titles: HashMap<String, String>,
    /// keys ("agent/id") of sessions whose summary or file changed
    changed: tokio::sync::broadcast::Sender<Vec<String>>,
    /// When telemetry was last fetched per session id, complete or not.
    telemetry_tried: Mutex<HashMap<String, Instant>>,
    parsed: Mutex<Option<ParsedCache>>,
}

impl AppState {
    // A poisoned lock only means another request panicked mid-access; the data is still usable.
    fn db(&self) -> MutexGuard<'_, turso::Connection> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn index(&self) -> RwLockReadGuard<'_, Index> {
        self.index.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_index(&self, idx: Index) {
        *self.index.write().unwrap_or_else(PoisonError::into_inner) = idx;
    }

    /// Runs `f` on the blocking pool: db access and file I/O must not stall async workers.
    async fn blocking<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&AppState) -> T + Send + 'static,
    ) -> T {
        let st = self.clone();
        tokio::task::spawn_blocking(move || f(&st))
            .await
            .expect("blocking task panicked")
    }

    /// Rebuilds the index off the async runtime and publishes it. Serialized, so an older
    /// snapshot can never overwrite a newer one.
    async fn rebuild(self: &Arc<Self>) -> usize {
        let mut rows = self.rows.lock().await;
        let mut owned = std::mem::take(&mut *rows);
        let (idx, owned) = self
            .blocking(move |st| {
                let idx = index::build(&st.roots, &st.db, &mut owned);
                (idx, owned)
            })
            .await;
        *rows = owned;
        let n = idx.sessions.len();
        self.set_index(idx);
        n
    }
}

type S = State<Arc<AppState>>;

fn is_local(authority: &str) -> bool {
    let name = authority
        .rsplit_once(':')
        .map_or(authority, |(name, port)| {
            if port.ends_with(']') {
                authority
            } else {
                name
            }
        });
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

// Binding to loopback does not stop DNS rebinding: a page on a hostile domain re-resolved to
// 127.0.0.1 would be same-origin with this server and could read every transcript. The port is
// not compared so the vite dev proxy, which forwards its own Host, keeps working.
// The Origin check covers the reverse: any site can fire a no-cors POST (refresh, tag edits) at
// 127.0.0.1 with a local Host, but browsers then send its own Origin. Non-browser clients send
// none.
async fn local_host_only(req: Request, next: Next) -> Response {
    if is_local_request(req.headers()) {
        next.run(req).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

fn is_local_request(headers: &header::HeaderMap) -> bool {
    let get = |name| headers.get(name).and_then(|h| h.to_str().ok());
    is_local(get(header::HOST).unwrap_or_default())
        && get(header::ORIGIN).is_none_or(|o| {
            o.split_once("://")
                .is_some_and(|(_, authority)| is_local(authority))
        })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Bound before the db opens: a second instance on the same port should get the bind hint, not
    // the db lock error.
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7777);
    let addr = format!("127.0.0.1:{}", port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "cannot bind {}: {} (another instance running? set PORT=...)",
                addr, e
            );
            std::process::exit(1);
        }
    };
    let roots = Roots::default_roots()?;
    let db = Mutex::new(db::open(&roots.db_file)?);
    eprintln!("db: {}", roots.db_file.display());
    match config::path() {
        Some(p) => eprintln!("config: {}", p.display()),
        None => eprintln!("config: no config dir on this OS, using defaults"),
    }
    if config::get().telemetry.enabled && telemetry::enabled() {
        eprintln!("telemetry: enabled");
    }
    let mut rows = db::load_all(&db.lock().unwrap_or_else(PoisonError::into_inner));
    let index = index::build(&roots, &db, &mut rows);
    let titles = parsers::codex::load_titles(&roots.codex_homes());
    let (changed, _) = tokio::sync::broadcast::channel(16);
    let state = Arc::new(AppState {
        roots,
        index: RwLock::new(index),
        db,
        rows: tokio::sync::Mutex::new(rows),
        titles,
        changed,
        telemetry_tried: Mutex::new(HashMap::new()),
        parsed: Mutex::new(None),
    });
    tokio::spawn(watch_files(state.clone()));
    tokio::spawn(sync_external(state.clone()));

    let app = Router::new()
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/{agent}/{id}", get(get_session))
        .route("/api/sessions/{agent}/{id}/events/{eid}", get(get_event))
        .route("/api/sessions/{agent}/{id}/telemetry", get(get_telemetry))
        .route("/api/sessions/{agent}/{id}/tags", post(set_tags))
        .route("/api/refresh", post(refresh))
        .route("/api/watch", get(watch))
        .fallback(static_handler)
        .layer(axum::middleware::from_fn(local_host_only))
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(state);
    eprintln!("listening on http://{}", addr);
    if std::env::args().any(|a| a == "--open") {
        open_browser(&format!("http://{}", addr));
    }
    axum::serve(listener, app).await.context("serve http")?;
    Ok(())
}

fn open_browser(url: &str) {
    // WSL has xdg-open but usually no Linux browser behind it, so hand the URL to Windows.
    let (cmd, args): (&str, &[&str]) = if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        ("explorer.exe", &[])
    } else if cfg!(target_os = "windows") {
        ("cmd", &["/c", "start", ""])
    } else if cfg!(target_os = "macos") {
        ("open", &[])
    } else {
        ("xdg-open", &[])
    };
    if let Err(e) = std::process::Command::new(cmd).args(args).arg(url).spawn() {
        eprintln!("cannot open browser ({}): {}", cmd, e);
    }
}

async fn list_sessions(State(st): S) -> Json<Vec<SessionSummary>> {
    let mut sessions = st.index().sessions.clone();
    // Indexer bookkeeping, not for the UI.
    for s in &mut sessions {
        s.dup_request_ids.clear();
    }
    Json(sessions)
}

async fn refresh(State(st): S) -> Json<usize> {
    Json(st.rebuild().await)
}

#[derive(Debug, serde::Deserialize)]
struct TagEdit {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

/// Edit user tags; meta-file-derived tags come back on rebuild even if removed. Returns the
/// session's effective tags.
async fn set_tags(
    State(st): S,
    Path((agent, id)): Path<(String, String)>,
    Json(edit): Json<TagEdit>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let a = Agent::parse(&agent).ok_or(StatusCode::NOT_FOUND)?;
    find(&st, &agent, &id).ok_or(StatusCode::NOT_FOUND)?;
    let sid = id.clone();
    st.blocking(move |st| {
        let conn = st.db();
        let mut tags = db::load_tags(&conn)
            .remove(&(a, sid.clone()))
            .unwrap_or_default();
        for t in edit.add.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            if !tags.iter().any(|x| x == t) {
                tags.push(t.to_string());
            }
        }
        tags.retain(|t| !edit.remove.contains(t));
        db::set_tags(&conn, a, &sid, &tags);
    })
    .await;
    st.rebuild().await;
    let _ = st.changed.send(vec![format!("{agent}/{id}")]);
    Ok(Json(
        find(&st, &agent, &id).map(|s| s.tags).unwrap_or_default(),
    ))
}

/// "agent/id" -> serialized summary, to tell which sessions a rebuild changed.
fn summaries_json(idx: &Index) -> HashMap<String, String> {
    idx.sessions
        .iter()
        .map(|s| {
            (
                format!("{}/{}", s.agent.as_str(), s.id),
                serde_json::to_string(s).unwrap_or_default(),
            )
        })
        .collect()
}

/// Poll session dirs; on any file change rebuild index and broadcast affected session keys.
async fn watch_files(st: Arc<AppState>) {
    let mut snap = index::snapshot(&st.roots);
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let cur = st.blocking(|st| index::snapshot(&st.roots)).await;
        let changed: HashSet<PathBuf> = cur
            .iter()
            .filter(|(p, m)| snap.get(*p) != Some(m))
            .map(|(p, _)| p.clone())
            .chain(snap.keys().filter(|p| !cur.contains_key(*p)).cloned())
            .collect();
        if changed.is_empty() {
            continue;
        }
        // A changed meta file isn't any session's path; map it to the run dir it tags instead.
        let changed_run_dirs: HashSet<PathBuf> = changed
            .iter()
            .filter(|p| {
                st.roots
                    .sources
                    .iter()
                    .any(|s| s.meta.as_deref() == p.file_name().and_then(|x| x.to_str()))
            })
            .filter_map(|p| p.parent().map(std::path::Path::to_path_buf))
            .collect();
        // A build also changes sessions whose own file did not (a parent gaining a child, request
        // ownership moving), so those are found by comparing against the previous index.
        let before = st.blocking(|st| summaries_json(&st.index())).await;
        st.rebuild().await;
        // run_dir_of stats ancestors of every session, so it only runs when a meta file changed.
        let keys: Vec<String> = st
            .blocking(move |st| {
                st.index()
                    .sessions
                    .iter()
                    .filter(|s| {
                        changed.contains(&s.path)
                            || before.get(&format!("{}/{}", s.agent.as_str(), s.id))
                                != Some(&serde_json::to_string(s).unwrap_or_default())
                            || (!changed_run_dirs.is_empty()
                                && st
                                    .roots
                                    .run_dir_of(&s.path)
                                    .is_some_and(|d| changed_run_dirs.contains(&d)))
                    })
                    .map(|s| format!("{}/{}", s.agent.as_str(), s.id))
                    .collect()
            })
            .await;
        snap = cur;
        let _ = st.changed.send(keys);
    }
}

async fn watch(
    State(st): S,
) -> sse::Sse<impl tokio_stream::Stream<Item = Result<sse::Event, std::convert::Infallible>>> {
    let rx = tokio_stream::wrappers::BroadcastStream::new(st.changed.subscribe());
    let stream = rx.filter_map(|r| r.ok()).map(|keys| {
        Ok(sse::Event::default()
            .json_data(keys)
            .expect("string list always serializes"))
    });
    sse::Sse::new(stream).keep_alive(sse::KeepAlive::default())
}

fn find(st: &AppState, agent: &str, id: &str) -> Option<SessionSummary> {
    let agent = Agent::parse(agent)?;
    let idx = st.index();
    idx.by_key
        .get(&(agent, id.to_string()))
        .map(|&i| idx.sessions[i].clone())
}

async fn parse_session(
    st: Arc<AppState>,
    agent: String,
    id: String,
) -> Result<(SessionSummary, Vec<Event>), StatusCode> {
    let summary = find(&st, &agent, &id).ok_or(StatusCode::NOT_FOUND)?;
    let indexed = summary.clone();
    let parsed = st
        .blocking(move |st| {
            let path = &summary.path;
            let stat = index::stat(path);
            let cached = st.parsed.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(c) = cached.as_ref().filter(|c| {
                c.path == *path && c.stat == stat && c.dup_request_ids == summary.dup_request_ids
            }) {
                return Ok(c.parsed.clone());
            }
            drop(cached);
            let data = index::read_source(path, &st.db)
                .ok_or_else(|| anyhow::anyhow!("no source on disk or in db"))?;
            let skip: HashSet<String> = summary.dup_request_ids.iter().cloned().collect();
            let parsed = Arc::new(index::parse_bytes(
                summary.agent,
                path,
                &data,
                &st.titles,
                &skip,
            )?);
            *st.parsed.lock().unwrap_or_else(PoisonError::into_inner) = Some(ParsedCache {
                path: path.clone(),
                stat,
                dup_request_ids: summary.dup_request_ids,
                parsed: parsed.clone(),
            });
            anyhow::Ok(parsed)
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // Keep children links from the index (computed across files).
    let mut s = parsed.summary.clone();
    s.request_ids.clear();
    s.children = indexed.children;
    s.title = indexed.title;
    s.archived = indexed.archived;
    let mut events = parsed.events.clone();
    // Link subagent spawn events to children via child's spawn_tool_call_id.
    {
        let idx = st.index();
        for cid in &s.children {
            if let Some(&ci) = idx.by_key.get(&(s.agent, cid.clone())) {
                let child = &idx.sessions[ci];
                if let Some(tid) = &child.spawn_tool_call_id {
                    for e in events.iter_mut() {
                        if e.kind == EventKind::Subagent && e.tool_call_id.as_ref() == Some(tid) {
                            e.child_session_id = Some(cid.clone());
                        }
                    }
                }
            }
        }
        // Fallback: unlinked subagent events get unlinked children in order.
        let linked: Vec<String> = events
            .iter()
            .filter_map(|e| e.child_session_id.clone())
            .collect();
        let mut spare: Vec<String> = s
            .children
            .iter()
            .filter(|c| !linked.contains(c))
            .cloned()
            .collect();
        for e in events
            .iter_mut()
            .filter(|e| e.kind == EventKind::Subagent && e.child_session_id.is_none())
        {
            if spare.is_empty() {
                break;
            }
            e.child_session_id = Some(spare.remove(0));
        }
    }
    Ok((s, events))
}

async fn get_session(
    State(st): S,
    Path((agent, id)): Path<(String, String)>,
) -> Result<Json<SessionDetail>, StatusCode> {
    let (summary, mut events) = parse_session(st.clone(), agent, id).await?;
    if config::get().rtk.enabled {
        let path = summary.path.clone();
        events = st
            .blocking(move |st| {
                rtk::annotate(
                    &st.db(),
                    st.roots.run_dir_of(&path).as_deref(),
                    &path,
                    &mut events,
                );
                events
            })
            .await;
    }
    truncate_events(&mut events);
    Ok(Json(SessionDetail { summary, events }))
}

async fn get_event(
    State(st): S,
    Path((agent, id, eid)): Path<(String, String, u32)>,
) -> Result<Json<Event>, StatusCode> {
    let (_, events) = parse_session(st, agent, id).await?;
    events
        .into_iter()
        .find(|e| e.id == eid)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

/// Claude-only OTel enrichment; `null` for other agents or when nothing was recorded.
async fn get_telemetry(
    State(st): S,
    Path((agent, id)): Path<(String, String)>,
) -> Json<Option<telemetry::Telemetry>> {
    match find(&st, &agent, &id) {
        Some(s) if s.agent == Agent::Claude && config::get().telemetry.enabled => {
            Json(telemetry_of(&st, &s).await)
        }
        _ => Json(None),
    }
}

/// Telemetry of a Claude session, kept in the db so it outlives Loki/Prometheus retention.
async fn telemetry_of(
    st: &Arc<AppState>,
    summary: &SessionSummary,
) -> Option<telemetry::Telemetry> {
    let sid = summary.id.clone();
    let stored = st.blocking(move |st| db::telemetry(&st.db(), &sid)).await;
    let stored_data = || {
        stored.as_ref().and_then(|(_, d)| {
            serde_json::from_str::<Option<telemetry::Telemetry>>(d)
                .ok()
                .flatten()
        })
    };
    // Exporters flush within minutes, so a fetch made well after the last event is final.
    let settled = summary
        .ended
        .as_deref()
        .and_then(telemetry::unix_s)
        .map(|e| e + 600);
    let is_final =
        matches!((&stored, settled), (Some((fetched, _)), Some(settled)) if *fetched >= settled);
    if is_final || !telemetry::enabled() {
        return stored_data();
    }
    // A live session is never final, and every UI reload asks again; an incomplete fetch is not
    // stored, so the attempt time is kept apart from the db's `fetched`.
    let recent = {
        let mut tried = st
            .telemetry_tried
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let recent = tried
            .get(&summary.id)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60));
        if !recent {
            tried.insert(summary.id.clone(), Instant::now());
        }
        recent
    };
    if recent {
        return stored_data();
    }
    // Subagent events are exported under the root session's id; per-request/per-tool ids stay
    // unique, but session-level counters would be the parent's, so children get only the joinable
    // parts.
    let agent = summary.agent.as_str();
    let mut root = summary.clone();
    let mut visited = HashSet::from([root.id.clone()]);
    while let Some(p) = root
        .parent_id
        .clone()
        .and_then(|p| find(st, agent, &p))
        .filter(|p| visited.insert(p.id.clone()))
    {
        root = p;
    }
    let (mut t, complete) = telemetry::fetch(
        &root.id,
        summary.started.as_deref(),
        summary.ended.as_deref(),
        root.id == summary.id,
    )
    .await;
    if root.id != summary.id {
        if let Some(t) = t.as_mut() {
            t.keep_joinable_only();
        }
    }
    if complete {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let sid = summary.id.clone();
        let data = serde_json::to_string(&t).unwrap_or_default();
        st.blocking(move |st| db::save_telemetry(&st.db(), &sid, now, &data))
            .await;
    }
    t.or_else(stored_data)
}

/// Pulls telemetry and rtk history of every session into the db, so sessions nobody opened are
/// kept too.
async fn sync_external(st: Arc<AppState>) {
    loop {
        let sessions = st.index().sessions.clone();
        let cfg = config::get();
        for s in &sessions {
            if cfg.rtk.enabled {
                let path = s.path.clone();
                st.blocking(move |st| {
                    if let Some(dir) = st.roots.run_dir_of(&path) {
                        rtk::snapshot(&st.db(), &dir, &path);
                    }
                })
                .await;
            }
            if s.agent == Agent::Claude && cfg.telemetry.enabled {
                telemetry_of(&st, s).await;
            }
        }
        tokio::time::sleep(Duration::from_secs(600)).await;
    }
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path).or_else(|| Assets::get("index.html")) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                f.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
