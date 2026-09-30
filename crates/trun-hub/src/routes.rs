//! HTTP routes. JSON everywhere; live data over Server-Sent Events.

use crate::AppState;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::convert::Infallible;

use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use trun_proto::{
    ApiError, CancelRun, CreateRun, Event, EventKind, FleetEvent, HealthInfo, LogPage, RunSummary,
    Stream,
};
use trun_store::{LogQuery, RunQuery};

const COOKIE: &str = "trun_token";
/// Rows fetched from the store per page when replaying history into a stream.
const REPLAY_PAGE: u32 = 5000;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/runs", get(list_runs).post(create_run))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/logs", get(run_logs))
        .route("/runs/{id}/events", get(run_events))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/metrics", get(run_metrics))
        .route("/runs/{id}/messages", get(run_messages))
        .route("/runs/{id}/panels", get(run_panels))
        .route("/runs/{id}/panels/stream", get(panels_stream))
        .route("/projects", get(list_projects))
        .route("/hosts", get(list_hosts).post(add_host))
        .route("/hosts/{name}", axum::routing::delete(remove_host))
        .route("/stream", get(fleet_stream))
        .route("/shutdown", post(shutdown))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .route("/api/health", get(health))
        .nest("/api", api)
        .fallback(get(root_or_static))
        .layer(middleware::from_fn_with_state(state.clone(), check_host))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

struct ApiErr(StatusCode, String);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(ApiError { error: self.1 })).into_response()
    }
}

impl From<anyhow::Error> for ApiErr {
    fn from(e: anyhow::Error) -> Self {
        ApiErr(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl From<tokio::task::JoinError> for ApiErr {
    fn from(e: tokio::task::JoinError) -> Self {
        ApiErr(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

type ApiResult<T> = Result<T, ApiErr>;

fn not_found(what: &str) -> ApiErr {
    ApiErr(StatusCode::NOT_FOUND, format!("no run matches '{what}'"))
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Reject requests not addressed to loopback (defends against DNS rebinding).
async fn check_host(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let ok = [
        format!("127.0.0.1:{}", st.port),
        format!("localhost:{}", st.port),
        format!("[::1]:{}", st.port),
    ]
    .iter()
    .any(|h| h == host);
    if !ok {
        return ApiErr(StatusCode::FORBIDDEN, "bad Host header".into()).into_response();
    }
    next.run(req).await
}

async fn require_token(State(st): State<AppState>, req: Request, next: Next) -> Response {
    if token_ok(req.headers(), &st.token) {
        next.run(req).await
    } else {
        ApiErr(
            StatusCode::UNAUTHORIZED,
            "missing or invalid token (see ~/.trun/hub.token, or open the UI with `trun ui`)"
                .into(),
        )
        .into_response()
    }
}

fn token_ok(headers: &HeaderMap, token: &str) -> bool {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    if bearer.is_some_and(|b| constant_eq(b, token)) {
        return true;
    }
    cookie_value(headers, COOKIE).is_some_and(|c| constant_eq(&c, token))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| {
            v.split(';').find_map(|kv| {
                let (k, val) = kv.trim().split_once('=')?;
                (k == name).then(|| val.to_string())
            })
        })
}

fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[derive(Deserialize)]
struct RootQuery {
    t: Option<String>,
}

/// `/?t=<token>` sets the auth cookie and redirects to a clean URL; everything else
/// is the embedded UI.
async fn root_or_static(
    State(st): State<AppState>,
    Query(q): Query<RootQuery>,
    req: Request,
) -> Response {
    if let Some(t) = q.t {
        if constant_eq(&t, &st.token) {
            let mut res = Redirect::to(req.uri().path()).into_response();
            let cookie =
                format!("{COOKIE}={t}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000");
            if let Ok(v) = cookie.parse() {
                res.headers_mut().insert(header::SET_COOKIE, v);
            }
            return res;
        }
        return ApiErr(StatusCode::UNAUTHORIZED, "invalid token".into()).into_response();
    }
    crate::ui::static_handler(req.uri().clone()).await
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health(State(st): State<AppState>) -> Json<HealthInfo> {
    Json(HealthInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        build: trun_proto::build_id(),
        pid: std::process::id(),
        data_dir: st.data_dir.clone(),
        started_at: st.started_at,
        now: trun_proto::now_ms(),
    })
}

async fn shutdown(State(st): State<AppState>) -> StatusCode {
    st.shutdown.notify_waiters();
    StatusCode::ACCEPTED
}

async fn create_run(
    State(st): State<AppState>,
    Json(req): Json<CreateRun>,
) -> ApiResult<(StatusCode, Json<RunSummary>)> {
    // Remote run: forward to the host's daemon, then mirror what it returns.
    if let Some(host) = req.host.clone().filter(|h| !h.is_empty() && h != "local") {
        if !st.links.has(&host) {
            return Err(ApiErr(
                StatusCode::BAD_REQUEST,
                format!("unknown host '{host}' (see `trun hosts`)"),
            ));
        }
        let client = remote_client(&st, &host).await?;
        let mut remote_req = req.clone();
        remote_req.host = None;
        let mut summary: RunSummary = client
            .post_json("/runs", &remote_req)
            .await
            .map_err(|e| ApiErr(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
        st.links.localize(&host, &mut summary);
        st.agent.mirror_summary(&host, summary.clone());
        let mirrored = st.agent.summary(&summary.id).unwrap_or(summary);
        return Ok((StatusCode::CREATED, Json(mirrored)));
    }
    let agent = st.agent.clone();
    let summary = tokio::task::spawn_blocking(move || agent.start(req))
        .await?
        .map_err(|e| ApiErr(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok((StatusCode::CREATED, Json(summary)))
}

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    active: bool,
    project: Option<String>,
    name: Option<String>,
    limit: Option<u32>,
}

async fn list_runs(
    State(st): State<AppState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<RunSummary>>> {
    let store = st.store.clone();
    let query = RunQuery {
        active_only: q.active,
        project: q.project,
        name_glob: q.name,
        limit: q.limit,
    };
    let mut runs = tokio::task::spawn_blocking(move || store.list_runs(&query)).await??;
    // Overlay in-memory state: fresher, and includes runs not yet flushed to the index.
    let live = st.agent.active_summaries();
    for l in live {
        match runs.iter_mut().find(|r| r.id == l.id) {
            Some(r) => *r = l,
            None if !q.active || !l.lifecycle.is_terminal() => runs.insert(0, l),
            None => {}
        }
    }
    if q.active {
        runs.retain(|r| !r.lifecycle.is_terminal());
    }
    runs.sort_by_key(|r| std::cmp::Reverse(r.created_at));
    Ok(Json(runs))
}

/// Resolve an id / id prefix / name, preferring live in-memory state.
async fn resolve(st: &AppState, reference: &str) -> ApiResult<RunSummary> {
    if let Some(s) = st.agent.summary(reference) {
        return Ok(s);
    }
    let upper = reference.to_ascii_uppercase();
    let mut live: Vec<RunSummary> = st.agent.active_summaries();
    live.sort_by_key(|r| std::cmp::Reverse(r.created_at));
    if let Some(s) = live
        .iter()
        .find(|s| s.id.starts_with(&upper) || s.name == reference)
    {
        return Ok(s.clone());
    }
    let store = st.store.clone();
    let r = reference.to_string();
    let found = tokio::task::spawn_blocking(move || store.resolve_run(&r)).await??;
    let found = found.ok_or_else(|| not_found(reference))?;
    Ok(st.agent.summary(&found.id).unwrap_or(found))
}

async fn get_run(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<RunSummary>> {
    Ok(Json(resolve(&st, &id).await?))
}

#[derive(Deserialize, Default)]
struct LogsQuery {
    #[serde(default)]
    since_seq: u64,
    before_seq: Option<u64>,
    tail: Option<u32>,
    limit: Option<u32>,
    grep: Option<String>,
    stream: Option<String>,
    /// Keep superseded progress-bar redraws.
    #[serde(default)]
    raw: bool,
}

async fn run_logs(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Json<LogPage>> {
    let run = resolve(&st, &id).await?;
    let grep = match q.grep.as_deref().filter(|g| !g.is_empty()) {
        Some(g) => Some(
            regex::Regex::new(g)
                .map_err(|e| ApiErr(StatusCode::BAD_REQUEST, format!("bad grep regex: {e}")))?,
        ),
        None => None,
    };
    let stream = match q.stream.as_deref() {
        None | Some("") | Some("all") => None,
        Some("stdout") => Some(Stream::Stdout),
        Some("stderr") => Some(Stream::Stderr),
        Some(other) => {
            return Err(ApiErr(
                StatusCode::BAD_REQUEST,
                format!("unknown stream '{other}'"),
            ));
        }
    };
    let query = LogQuery {
        since_seq: q.since_seq,
        before_seq: q.before_seq,
        tail: q.tail,
        limit: q.limit,
        grep,
        stream,
    };
    let store = st.store.clone();
    let lines =
        tokio::task::spawn_blocking(move || store.logs(&run.project, &run.id, &query)).await??;
    let next_seq = lines.last().map(|l| l.seq).unwrap_or(q.since_seq);
    let lines = if q.raw {
        lines
    } else {
        collapse_provisional(lines)
    };
    Ok(Json(LogPage { lines, next_seq }))
}

/// A provisional (`cr`) line is replaced by the next line on the same stream, so in
/// history only the last provisional line of each stream (if nothing followed it)
/// is still "on screen".
fn collapse_provisional(lines: Vec<trun_proto::LogLine>) -> Vec<trun_proto::LogLine> {
    let mut out: Vec<trun_proto::LogLine> = Vec::with_capacity(lines.len());
    let mut pending: [Option<usize>; 2] = [None, None];
    for l in lines {
        let slot = (l.stream.as_i64() - 1) as usize;
        if let Some(i) = pending[slot].take() {
            out[i].text.clear();
            out[i].seq = u64::MAX; // mark superseded
        }
        if l.cr {
            pending[slot] = Some(out.len());
        }
        out.push(l);
    }
    out.retain(|l| l.seq != u64::MAX);
    out
}

#[derive(Deserialize, Default)]
struct EventsQuery {
    since_seq: Option<u64>,
}

type SseTx = mpsc::Sender<Result<SseEvent, Infallible>>;

/// Full event stream for one run: history from `since_seq`, then live, then an
/// `end` event carrying the final summary once the run is over.
async fn run_events(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let run = resolve(&st, &id).await?;
    // EventSource reconnects send Last-Event-ID; it wins over the query.
    let since = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .or(q.since_seq)
        .unwrap_or(0);
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        if let Err(()) = stream_run(st, run, since, tx).await {
            // Client went away; nothing to do.
        }
    });
    Ok(Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

async fn send_event(tx: &SseTx, ev: &Event) -> Result<(), ()> {
    let sse = SseEvent::default()
        .id(ev.seq.to_string())
        .event("event")
        .json_data(ev)
        .map_err(|_| ())?;
    tx.send(Ok(sse)).await.map_err(|_| ())
}

async fn send_end(tx: &SseTx, summary: &RunSummary) -> Result<(), ()> {
    let sse = SseEvent::default()
        .event("end")
        .json_data(summary)
        .map_err(|_| ())?;
    tx.send(Ok(sse)).await.map_err(|_| ())
}

fn is_terminal_event(ev: &Event) -> bool {
    matches!(ev.kind, EventKind::Lifecycle { state, .. } if state.is_terminal())
}

/// Replay `(since, until)` from the store in pages. Returns the last seq sent.
async fn replay_from_store(
    st: &AppState,
    run: &RunSummary,
    mut since: u64,
    until: Option<u64>,
    tx: &SseTx,
) -> Result<u64, ()> {
    loop {
        let store = st.store.clone();
        let (project, id) = (run.project.clone(), run.id.clone());
        let page = tokio::task::spawn_blocking(move || {
            store.events(&project, &id, since, until, Some(REPLAY_PAGE))
        })
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
        let n = page.len();
        for ev in &page {
            send_event(tx, ev).await?;
            since = ev.seq;
        }
        if n < REPLAY_PAGE as usize {
            return Ok(since);
        }
    }
}

async fn stream_run(st: AppState, run: RunSummary, since: u64, tx: SseTx) -> Result<(), ()> {
    let Some(mut sub) = st.agent.subscribe(&run.id, since) else {
        // Not in memory: the run finished a while ago (or was lost). Replay and end.
        replay_from_store(&st, &run, since, None, &tx).await?;
        let final_summary = resolve(&st, &run.id).await.unwrap_or(run);
        return send_end(&tx, &final_summary).await;
    };

    let mut last = since;
    if sub.ring_start > last + 1 {
        last = replay_from_store(&st, &run, last, Some(sub.ring_start), &tx)
            .await?
            .max(last);
    }
    loop {
        let mut ended = false;
        for ev in sub.backlog.drain(..) {
            if ev.seq <= last {
                continue;
            }
            send_event(&tx, &ev).await?;
            last = ev.seq;
            ended |= is_terminal_event(&ev);
        }
        if ended {
            let s = st.agent.summary(&run.id).unwrap_or(run);
            return send_end(&tx, &s).await;
        }
        loop {
            match sub.live.recv().await {
                Ok(ev) => {
                    if ev.seq <= last {
                        continue;
                    }
                    send_event(&tx, &ev).await?;
                    last = ev.seq;
                    if is_terminal_event(&ev) {
                        let s = st.agent.summary(&run.id).unwrap_or(run);
                        return send_end(&tx, &s).await;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => break,
                Err(broadcast::error::RecvError::Closed) => {
                    let s = resolve(&st, &run.id).await.unwrap_or(run);
                    return send_end(&tx, &s).await;
                }
            }
        }
        // Lagged: resubscribe from where we are; fill any gap from the store.
        match st.agent.subscribe(&run.id, last) {
            Some(s) => {
                sub = s;
                if sub.ring_start > last + 1 {
                    let store = st.store.clone();
                    tokio::task::spawn_blocking(move || store.flush())
                        .await
                        .map_err(|_| ())?;
                    last = replay_from_store(&st, &run, last, Some(sub.ring_start), &tx)
                        .await?
                        .max(last);
                }
            }
            None => {
                replay_from_store(&st, &run, last, None, &tx).await?;
                let s = resolve(&st, &run.id).await.unwrap_or(run);
                return send_end(&tx, &s).await;
            }
        }
    }
}

async fn cancel_run(
    State(st): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<CancelRun>>,
) -> ApiResult<Json<RunSummary>> {
    let run = resolve(&st, &id).await?;
    let req = body.map(|b| b.0).unwrap_or_default();
    if let Some(host) = run.via.clone() {
        let client = remote_client(&st, &host).await?;
        let mut s: RunSummary = client
            .post_json(&format!("/runs/{}/cancel", run.id), &req)
            .await
            .map_err(|e| ApiErr(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
        st.links.localize(&host, &mut s);
        st.agent.mirror_summary(&host, s.clone());
        return Ok(Json(s));
    }
    if !st.agent.cancel(&run.id, req.force) {
        return Err(ApiErr(
            StatusCode::CONFLICT,
            format!("run {} is not active ({})", run.id, run.lifecycle.as_str()),
        ));
    }
    Ok(Json(st.agent.summary(&run.id).unwrap_or(run)))
}

#[derive(Deserialize, Default)]
struct MetricsQuery {
    /// Comma-separated names; default: every metric the run has reported.
    names: Option<String>,
    max_points: Option<usize>,
}

async fn run_metrics(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<MetricsQuery>,
) -> ApiResult<Json<Vec<trun_proto::MetricSeries>>> {
    let run = resolve(&st, &id).await?;
    let names: Vec<String> = match q.names.as_deref() {
        Some(n) if !n.is_empty() => n
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => run.metrics.keys().cloned().collect(),
    };
    let max_points = q.max_points.unwrap_or(1500).clamp(4, 100_000);
    let store = st.store.clone();
    let series = tokio::task::spawn_blocking(move || {
        store.metric_series(&run.project, &run.id, &names, max_points)
    })
    .await??;
    Ok(Json(series))
}

/// Notes and warn/error/info log events: the "what should I know" feed.
async fn run_messages(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Event>>> {
    let run = resolve(&st, &id).await?;
    let store = st.store.clone();
    let events = tokio::task::spawn_blocking(move || {
        store.events_of_kind(
            &run.project,
            &run.id,
            &["log", "note", "alert", "check_error"],
            500,
        )
    })
    .await??;
    Ok(Json(events))
}

fn load_panels(st: &AppState, run: &RunSummary) -> crate::panels::PanelSet {
    crate::panels::load(
        run.config_root.as_deref().map(std::path::Path::new),
        &st.home,
        &run.name,
    )
}

async fn run_panels(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let run = resolve(&st, &id).await?;
    if let Some(host) = &run.via {
        let client = remote_client(&st, host).await?;
        let v: serde_json::Value = client
            .get_json(&format!("/runs/{}/panels", run.id))
            .await
            .map_err(|e| ApiErr(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
        return Ok(Json(v).into_response());
    }
    let st2 = st.clone();
    Ok(Json(tokio::task::spawn_blocking(move || load_panels(&st2, &run)).await?).into_response())
}

/// The run's panel set now, then again every time a panel or dashboard file changes.
async fn panels_stream(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let run = resolve(&st, &id).await?;
    if let Some(host) = run.via.clone() {
        return proxy_remote_panels(&st, &host, &run.id).await;
    }
    let (mut project_rx, mut global_rx) = st
        .watcher
        .subscribe(run.config_root.as_deref().map(std::path::Path::new));
    let (tx, rx) = mpsc::channel::<Result<SseEvent, Infallible>>(8);
    tokio::spawn(async move {
        let mut project_open = true;
        loop {
            let (st2, run2) = (st.clone(), run.clone());
            let Ok(set) = tokio::task::spawn_blocking(move || load_panels(&st2, &run2)).await
            else {
                return;
            };
            let Ok(ev) = SseEvent::default().event("panels").json_data(&set) else {
                return;
            };
            if tx.send(Ok(ev)).await.is_err() {
                return;
            }
            tokio::select! {
                r = project_rx.recv(), if project_open => {
                    if matches!(r, Err(broadcast::error::RecvError::Closed)) { project_open = false; }
                }
                r = global_rx.recv() => {
                    if matches!(r, Err(broadcast::error::RecvError::Closed)) { return; }
                }
                _ = tx.closed() => return,
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

/// The link to `host`, waiting briefly if it is still connecting.
async fn remote_client(st: &AppState, host: &str) -> ApiResult<crate::remote::RemoteClient> {
    st.links
        .connected(host)
        .await
        .map_err(|e| ApiErr(StatusCode::SERVICE_UNAVAILABLE, e))
}

/// Remote runs' panel files live on their host: relay its panels stream.
async fn proxy_remote_panels(st: &AppState, host: &str, run_id: &str) -> ApiResult<Response> {
    let client = remote_client(st, host).await?;
    let mut upstream = client
        .sse(&format!("/runs/{run_id}/panels/stream"))
        .await
        .map_err(|e| ApiErr(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    let (tx, rx) = mpsc::channel::<Result<SseEvent, Infallible>>(8);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = upstream.next() => match msg {
                    Ok(Some((event, data))) => {
                        if tx.send(Ok(SseEvent::default().event(event).data(data))).await.is_err() {
                            return;
                        }
                    }
                    _ => return,
                },
                _ = tx.closed() => return,
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

async fn list_hosts(State(st): State<AppState>) -> Json<Vec<trun_proto::HostState>> {
    Json(st.links.states())
}

async fn add_host(
    State(st): State<AppState>,
    Json(mut h): Json<trun_proto::HostConfig>,
) -> ApiResult<StatusCode> {
    h.name = h.name.trim().to_string();
    if h.name.is_empty()
        || h.name == "local"
        || !h
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err(ApiErr(
            StatusCode::BAD_REQUEST,
            "host names use letters, digits, '-', '_' or '.' (and 'local' is reserved)".into(),
        ));
    }
    if h.target.trim().is_empty() {
        h.target = h.name.clone();
    }
    if h.ssh.is_empty() {
        h.ssh = vec!["ssh".into()];
    }
    if h.trun_path.trim().is_empty() {
        h.trun_path = ".local/bin/trun".into();
    }
    st.links.add(h)?;
    Ok(StatusCode::CREATED)
}

async fn remove_host(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    if st.links.remove(&name)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiErr(StatusCode::NOT_FOUND, format!("no host '{name}'")))
    }
}

async fn list_projects(
    State(st): State<AppState>,
) -> ApiResult<Json<Vec<trun_proto::ProjectInfo>>> {
    let store = st.store.clone();
    Ok(Json(
        tokio::task::spawn_blocking(move || store.projects()).await??,
    ))
}

/// Fleet-wide run summary updates.
async fn fleet_stream(State(st): State<AppState>) -> Response {
    let mut rx = st.agent.fleet();
    let (tx, out) = mpsc::channel::<Result<SseEvent, Infallible>>(256);
    tokio::spawn(async move {
        loop {
            let s = match rx.recv().await {
                Ok(s) => s,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            let Ok(ev) = SseEvent::default()
                .event("run")
                .json_data(FleetEvent::Run(s))
            else {
                continue;
            };
            if tx.send(Ok(ev)).await.is_err() {
                break;
            }
        }
    });
    Sse::new(ReceiverStream::new(out))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trun_proto::LogLine;

    fn l(seq: u64, stream: Stream, text: &str, cr: bool) -> LogLine {
        LogLine {
            seq,
            ts: 0,
            stream,
            text: text.into(),
            cr,
        }
    }

    #[test]
    fn collapse_keeps_final_state_per_stream() {
        let lines = vec![
            l(1, Stream::Stdout, "start", false),
            l(2, Stream::Stdout, "10%", true),
            l(3, Stream::Stderr, "warn", false),
            l(4, Stream::Stdout, "50%", true),
            l(5, Stream::Stdout, "100%", false),
            l(6, Stream::Stdout, "next 3%", true),
        ];
        let seqs: Vec<u64> = collapse_provisional(lines).iter().map(|l| l.seq).collect();
        assert_eq!(seqs, vec![1, 3, 5, 6]);
    }

    #[test]
    fn token_checks() {
        let mut h = HeaderMap::new();
        assert!(!token_ok(&h, "abc"));
        h.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert!(token_ok(&h, "abc"));
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "x=1; trun_token=abc".parse().unwrap());
        assert!(token_ok(&h, "abc"));
        assert!(!token_ok(&h, "abd"));
    }
}
