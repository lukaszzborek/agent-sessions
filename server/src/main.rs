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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard};
use tokio_stream::StreamExt;

#[derive(Debug, rust_embed::Embed)]
#[folder = "../ui/dist/"]
struct Assets;

#[derive(Debug)]
struct AppState {
    roots: Roots,
    index: RwLock<Index>,
    db: Mutex<rusqlite::Connection>,
    titles: std::collections::HashMap<String, String>,
    /// keys ("agent/id") of sessions whose files changed on disk
    changed: tokio::sync::broadcast::Sender<Vec<String>>,
}

impl AppState {
    // A poisoned lock only means another request panicked mid-access; the data is still usable.
    fn db(&self) -> MutexGuard<'_, rusqlite::Connection> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn index(&self) -> RwLockReadGuard<'_, Index> {
        self.index.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_index(&self, idx: Index) {
        *self.index.write().unwrap_or_else(PoisonError::into_inner) = idx;
    }

    /// Rebuilds the index off the async runtime.
    async fn build_index(self: &Arc<Self>) -> Index {
        let st = self.clone();
        tokio::task::spawn_blocking(move || index::build(&st.roots, &st.db))
            .await
            .expect("index build panicked")
    }
}

type S = State<Arc<AppState>>;

// Binding to loopback does not stop DNS rebinding: a page on a hostile domain re-resolved to
// 127.0.0.1 would be same-origin with this server and could read every transcript. The port is
// not compared so the vite dev proxy, which forwards its own Host, keeps working.
async fn local_host_only(req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let name = host.rsplit_once(':').map_or(
        host,
        |(name, port)| {
            if port.ends_with(']') {
                host
            } else {
                name
            }
        },
    );
    if matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
        next.run(req).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let roots = Roots::default_roots()?;
    let db = Mutex::new(db::open(&roots.db_file)?);
    eprintln!("db: {}", roots.db_file.display());
    if config::get().telemetry.enabled && telemetry::enabled() {
        eprintln!("telemetry: enabled");
    }
    let index = index::build(&roots, &db);
    let titles = parsers::codex::load_titles(&roots.codex_homes());
    let (changed, _) = tokio::sync::broadcast::channel(16);
    let state = Arc::new(AppState {
        roots,
        index: RwLock::new(index),
        db,
        titles,
        changed,
    });
    tokio::spawn(watch_files(state.clone()));
    tokio::spawn(sync_external(state.clone()));

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7777);
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
    let addr = format!("127.0.0.1:{}", port);
    eprintln!("listening on http://{}", addr);
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
    Json(st.index().sessions.clone())
}

async fn rebuild(st: &Arc<AppState>) -> usize {
    let idx = st.build_index().await;
    let n = idx.sessions.len();
    st.set_index(idx);
    n
}

async fn refresh(State(st): S) -> Json<usize> {
    Json(rebuild(&st).await)
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
    {
        let conn = st.db();
        let mut tags = db::load_tags(&conn)
            .remove(&(a, id.clone()))
            .unwrap_or_default();
        for t in edit.add.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            if !tags.iter().any(|x| x == t) {
                tags.push(t.to_string());
            }
        }
        tags.retain(|t| !edit.remove.contains(t));
        db::set_tags(&conn, a, &id, &tags);
    }
    rebuild(&st).await;
    Ok(Json(
        find(&st, &agent, &id).map(|s| s.tags).unwrap_or_default(),
    ))
}

/// Poll session dirs; on any file change rebuild index and broadcast affected session keys.
async fn watch_files(st: Arc<AppState>) {
    let mut snap = index::snapshot(&st.roots);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let st2 = st.clone();
        let cur = tokio::task::spawn_blocking(move || index::snapshot(&st2.roots))
            .await
            .expect("snapshot panicked");
        let mut changed: Vec<&std::path::PathBuf> = cur
            .iter()
            .filter(|(p, m)| snap.get(*p) != Some(m))
            .map(|(p, _)| p)
            .collect();
        changed.extend(snap.keys().filter(|p| !cur.contains_key(*p)));
        if changed.is_empty() {
            continue;
        }
        // A changed meta file isn't any session's path; map it to the run dir it tags instead.
        let changed_run_dirs: std::collections::HashSet<&std::path::Path> = changed
            .iter()
            .filter(|p| {
                st.roots
                    .sources
                    .iter()
                    .any(|s| s.meta.as_deref() == p.file_name().and_then(|x| x.to_str()))
            })
            .filter_map(|p| p.parent())
            .collect();
        let idx = st.build_index().await;
        let keys: Vec<String> = idx
            .sessions
            .iter()
            .filter(|s| {
                changed.contains(&&s.path)
                    || st
                        .roots
                        .run_dir_of(&s.path)
                        .is_some_and(|d| changed_run_dirs.contains(d.as_path()))
            })
            .map(|s| format!("{}/{}", s.agent.as_str(), s.id))
            .collect();
        st.set_index(idx);
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
    let path = summary.path.clone();
    let st2 = st.clone();
    let parsed = tokio::task::spawn_blocking(move || {
        let data = std::fs::read(&path)
            .ok()
            .or_else(|| db::raw(&st2.db(), &path))
            .ok_or_else(|| anyhow::anyhow!("no source on disk or in db"))?;
        index::parse_bytes(summary.agent, &path, &data, &st2.titles)
    })
    .await
    .expect("session parse panicked")
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // Keep children links from the index (computed across files).
    let mut s = parsed.summary;
    s.children = summary.children;
    s.title = summary.title;
    s.archived = summary.archived;
    let mut events = parsed.events;
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
        rtk::annotate(
            &st.db(),
            st.roots.run_dir_of(&summary.path).as_deref(),
            &summary.path,
            &mut events,
        );
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
    let stored = db::telemetry(&st.db(), &summary.id);
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
    // Subagent events are exported under the root session's id; per-request/per-tool ids stay
    // unique, but session-level counters would be the parent's, so children get only the joinable
    // parts.
    let agent = summary.agent.as_str();
    let mut root = summary.clone();
    while let Some(p) = root.parent_id.clone().and_then(|p| find(st, agent, &p)) {
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
        db::save_telemetry(
            &st.db(),
            &summary.id,
            now,
            &serde_json::to_string(&t).unwrap_or_default(),
        );
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
            if let Some(dir) = st.roots.run_dir_of(&s.path).filter(|_| cfg.rtk.enabled) {
                rtk::snapshot(&st.db(), &dir, &s.path);
            }
            if s.agent == Agent::Claude && cfg.telemetry.enabled {
                telemetry_of(&st, s).await;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
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
