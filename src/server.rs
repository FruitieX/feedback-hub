//! HTTP API: anonymous feedback submission for the widget/apps, and a
//! token-protected management API used by the triage UI, CLI and worker.

use crate::{
    apps::AppInfo,
    db::{
        Database, Event, Feedback, FeedbackEdit, ListFilter, NewFeedback, NewScreenshot, STATUSES,
        Screenshot,
    },
    runs::{AgentChoice, LAST_CHOICE, NewRun, Run, RunEvent, RunFilter, RunUpdate, WorkerStatus},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
};

pub const MAX_SCREENSHOTS: usize = 8;
pub const MAX_SCREENSHOT_BYTES: usize = 12 * 1024 * 1024;
const MAX_MESSAGE_CHARS: usize = 20_000;

pub struct Config {
    pub admin_token: String,
    pub allowed_origins: Vec<String>,
    pub static_dir: Option<PathBuf>,
    pub apps: Vec<AppInfo>,
}

#[derive(Clone)]
pub struct Server {
    pub db: Database,
    pub config: Arc<Config>,
    /// Latest heartbeat per worker; kept in memory since it is only a liveness view.
    pub workers: Arc<Mutex<BTreeMap<String, WorkerStatus>>>,
    /// Workers asked to re-read their agents' model lists on the next heartbeat.
    pub refresh_models: Arc<Mutex<BTreeSet<String>>>,
    /// ETag for the static files (widget, triage UI), from their contents.
    pub static_etag: Option<Arc<str>>,
    /// Ticks after every change, for the triage UI's live updates (`/api/changes`).
    pub changes: broadcast::Sender<()>,
}

/// Hashes every file under the static directory. Files served from the Nix
/// store all have a 1970 mtime, so Last-Modified can't tell builds apart.
fn static_etag(dir: &std::path::Path) -> Option<Arc<str>> {
    fn collect(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    collect(dir, &mut files);
    if files.is_empty() {
        return None;
    }
    files.sort();
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for file in files {
        digest.update(
            file.strip_prefix(dir)
                .unwrap_or(&file)
                .to_string_lossy()
                .as_bytes(),
        );
        digest.update(&std::fs::read(&file).unwrap_or_default());
    }
    let hex: String = digest.finish().as_ref()[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(format!("\"{hex}\"").into())
}

impl Server {
    pub fn new(db: Database, config: Config) -> Self {
        let static_etag = config.static_dir.as_deref().and_then(static_etag);
        Self {
            db,
            config: Arc::new(config),
            workers: Arc::default(),
            refresh_models: Arc::default(),
            static_etag,
            changes: broadcast::channel(16).0,
        }
    }

    /// Display names for a choice, e.g. `{"model": "Opus 5.5", "effort": "high"}`,
    /// resolving defaults from what the workers report.
    fn choice_labels(&self, choice: &AgentChoice) -> serde_json::Value {
        let workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
        let agent = workers
            .values()
            .flat_map(|w| w.agents.iter())
            .find(|a| a.name == choice.agent);
        let model = choice
            .model
            .as_deref()
            .and_then(|id| agent?.models.iter().find(|m| m.id == id));
        let model_label = match (
            &choice.model,
            model,
            agent.and_then(|a| a.default_model.as_deref()),
        ) {
            (Some(_), Some(m), _) => m.name.clone(),
            (Some(id), None, _) => id.clone(),
            (None, _, Some(default)) => format!("{default} (default)"),
            (None, _, None) => "default model".into(),
        };
        let effort_label = match (
            &choice.effort,
            model.and_then(|m| m.default_effort.as_deref()),
        ) {
            (Some(effort), _) => format!("{effort} effort"),
            (None, Some(default)) => format!("{default} effort (default)"),
            (None, None) if model.is_some_and(|m| m.efforts.is_empty()) => {
                "no effort setting".into()
            }
            (None, None) => "default effort".into(),
        };
        json!({ "agent": choice.agent, "model": model_label, "effort": effort_label })
    }

    /// Checks a model/effort choice against what online workers report for the agent.
    fn validate_run(&self, run: &NewRun) -> Result<()> {
        if run.agent == "hermes"
            && run
                .model
                .as_deref()
                .is_some_and(crate::models::hermes_model_forbidden)
        {
            return Err(bad_request(
                "Hermes can't run Claude models (it would use the Claude subscription outside the claude CLI); use the claude agent",
            ));
        }
        let workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
        let offers: Vec<&crate::runs::AgentInfo> = workers
            .values()
            .flat_map(|w| w.agents.iter())
            .filter(|a| a.name == run.agent)
            .collect();
        if offers.is_empty() {
            // No worker online to ask; the worker reports bad choices when it runs.
            return Ok(());
        }
        let errors: Vec<String> = offers
            .iter()
            .filter_map(|a| {
                a.validate(run.model.as_deref(), run.effort.as_deref())
                    .err()
            })
            .collect();
        if errors.len() == offers.len() {
            return Err(bad_request(errors.join("; ")));
        }
        Ok(())
    }
}

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        tracing::error!("{err:#}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    }
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn conflict(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, msg.into())
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

type Result<T> = std::result::Result<T, ApiError>;

/// Matches an origin against patterns like `https://*.example.org` or
/// `http://localhost:*`.
pub fn origin_allowed(patterns: &[String], origin: &str) -> bool {
    patterns.iter().any(|pattern| {
        if pattern == "*" || pattern == origin {
            return true;
        }
        let Some((scheme, rest)) = pattern.split_once("://") else {
            return false;
        };
        let Some(origin_rest) = origin.strip_prefix(&format!("{scheme}://")) else {
            return false;
        };
        let (pattern_host, pattern_port) = match rest.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (rest, None),
        };
        let (origin_host, origin_port) = match origin_rest.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => (host, Some(port)),
            _ => (origin_rest, None),
        };
        let port_ok = match pattern_port {
            Some("*") => true,
            other => other == origin_port,
        };
        let host_ok = match pattern_host.strip_prefix("*.") {
            Some(suffix) => origin_host
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1),
            None => pattern_host == origin_host,
        };
        port_ok && host_ok
    })
}

pub fn app(server: Server) -> Router {
    let patterns = server.config.allowed_origins.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            origin
                .to_str()
                .is_ok_and(|origin| origin_allowed(&patterns, origin))
        }))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE])
        .allow_private_network(true)
        // The widget sends the owner's session cookie when dispatching agents.
        .allow_credentials(true);

    let public = Router::new()
        .route("/api/feedback", post(submit))
        .route("/api/apps", get(list_apps))
        .route("/api/widget/session", get(widget_session))
        .route("/api/session", axum::routing::delete(end_session))
        .layer(DefaultBodyLimit::max(
            MAX_SCREENSHOTS * MAX_SCREENSHOT_BYTES * 4 / 3 + 1024 * 1024,
        ));

    let admin = Router::new()
        .route("/api/feedback", get(list))
        .route(
            "/api/feedback/{id}",
            get(detail).patch(update).delete(remove),
        )
        .route("/api/feedback/{id}/screenshots/{idx}", get(screenshot))
        .route("/api/feedback/{id}/events", post(comment))
        .route("/api/feedback/{id}/queue", post(queue))
        .route("/api/session", post(start_session))
        .route("/api/agent-choice", get(agent_choice))
        .route("/api/runs", get(list_runs).post(create_runs))
        .route("/api/runs/{id}", get(run_detail).patch(update_run))
        .route(
            "/api/runs/{id}/events",
            get(run_events).post(add_run_events),
        )
        .route("/api/runs/{id}/commands", post(add_command))
        .route("/api/runs/{id}/commands/take", post(take_commands))
        .route("/api/worker/claim", post(legacy_claim))
        .route("/api/worker/claim-run", post(claim))
        .route("/api/worker/heartbeat", post(heartbeat))
        .route("/api/workers", get(list_workers))
        .route("/api/workers/refresh", post(refresh_workers))
        .route("/api/changes", get(changes))
        .route_layer(middleware::from_fn_with_state(
            server.clone(),
            require_token,
        ));

    let mut router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(public)
        .merge(admin);

    if let Some(dir) = &server.config.static_dir {
        let index = dir.join("index.html");
        router = router.fallback_service(
            ServeDir::new(dir)
                .fallback(ServeFile::new(index))
                .append_index_html_on_directories(true),
        );
    }

    router
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(middleware::from_fn_with_state(
            server.clone(),
            cache_headers,
        ))
        .layer(middleware::from_fn_with_state(
            server.clone(),
            announce_changes,
        ))
        // Outermost so preflights for paths shared with admin routes are answered
        // here instead of hitting the token check.
        .layer(cors)
        .with_state(server)
}

/// Static files are revalidated by content ETag. Apps load the widget
/// cross-origin and keep it for a few minutes; the triage UI always revalidates.
async fn cache_headers(State(server): State<Server>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let is_widget = path == "/widget.js";
    let is_static = server.static_etag.is_some()
        && matches!(*request.method(), Method::GET | Method::HEAD)
        && !path.starts_with("/api/")
        && path != "/healthz";
    if !is_static {
        return next.run(request).await;
    }
    let etag = server.static_etag.clone().expect("checked");
    // Never answer from the (meaningless) modification time.
    request.headers_mut().remove(header::IF_MODIFIED_SINCE);
    let matches = request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == &*etag || t.trim() == "*"));
    request.headers_mut().remove(header::IF_NONE_MATCH);
    let mut response = if matches {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        next.run(request).await
    };
    if !(response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED) {
        return response;
    }
    let headers = response.headers_mut();
    headers.remove(header::LAST_MODIFIED);
    if let Ok(value) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if is_widget {
            "public, max-age=300"
        } else {
            "no-cache"
        }),
    );
    if is_widget {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    response
}

/// Tells open triage UIs that something changed after every successful write,
/// except the workers' routine polling, which changes nothing they show.
async fn announce_changes(State(server): State<Server>, request: Request, next: Next) -> Response {
    let writes = !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;
    let quiet = matches!(path.as_str(), "/api/worker/heartbeat" | "/api/session")
        || path.ends_with("/commands/take")
        || (path.starts_with("/api/worker/claim") && response.status() == StatusCode::NO_CONTENT);
    if writes && !quiet && path.starts_with("/api/") && response.status().is_success() {
        // Nobody listening is fine.
        let _ = server.changes.send(());
    }
    response
}

/// Server-sent events: a `change` event whenever data shown in the triage UI
/// may have changed; the UI then reloads what it shows.
async fn changes(
    State(server): State<Server>,
) -> Sse<impl futures_util::Stream<Item = std::result::Result<SseEvent, Infallible>>> {
    let stream = futures_util::stream::unfold(server.changes.subscribe(), |mut rx| async move {
        match rx.recv().await {
            // Missed ticks still mean "reload".
            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                Some((Ok(SseEvent::default().event("change").data("")), rx))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_token(State(server): State<Server>, request: Request, next: Next) -> Response {
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if server.config.admin_token.is_empty()
        || !constant_time_eq(provided.as_bytes(), server.config.admin_token.as_bytes())
    {
        return ApiError(StatusCode::UNAUTHORIZED, "missing or invalid token".into())
            .into_response();
    }
    next.run(request).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Submission {
    app: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    message: String,
    reporter: Option<String>,
    url: Option<String>,
    title: Option<String>,
    user_agent: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    context: serde_json::Value,
    /// Hand the item straight to the owner's last-used agent (needs the owner's session cookie).
    #[serde(default)]
    dispatch: bool,
    /// Data URLs (`data:image/png;base64,...`).
    #[serde(default)]
    screenshots: Vec<String>,
    /// Optional caption per screenshot, by position.
    #[serde(default)]
    screenshot_notes: Vec<Option<String>>,
}

fn default_kind() -> String {
    "bug".into()
}

fn clip(value: Option<String>, max: usize) -> Option<String> {
    value
        .map(|v| v.trim().chars().take(max).collect::<String>())
        .filter(|v| !v.is_empty())
}

pub fn decode_image(data_url: &str) -> std::result::Result<(String, Vec<u8>), String> {
    let (meta, data) = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .ok_or("screenshot must be a data URL")?;
    let mime = meta
        .strip_suffix(";base64")
        .ok_or("screenshot must be base64 encoded")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "screenshot is not valid base64")?;
    if bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err("screenshot is too large".into());
    }
    let magic_ok = match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/webp" => bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => return Err(format!("unsupported screenshot type {mime}")),
    };
    if !magic_ok {
        return Err("screenshot content does not match its type".into());
    }
    Ok((mime.to_string(), bytes))
}

async fn submit(
    State(server): State<Server>,
    headers: HeaderMap,
    Json(body): Json<Submission>,
) -> Result<impl IntoResponse> {
    // Browsers always send Origin on cross-origin POSTs; native apps and the CLI don't.
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && !origin_allowed(&server.config.allowed_origins, origin)
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "origin not allowed".into()));
    }
    let app = body.app.trim().to_lowercase();
    if app.is_empty()
        || app.len() > 64
        || !app
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err(bad_request("app must be a short identifier"));
    }
    let kind = match body.kind.as_str() {
        "bug" | "idea" | "other" => body.kind,
        _ => return Err(bad_request("kind must be bug, idea or other")),
    };
    if body.message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(bad_request("message is too long"));
    }
    if body.message.trim().is_empty() && body.screenshots.is_empty() {
        return Err(bad_request("add a message or a screenshot"));
    }
    if body.screenshots.len() > MAX_SCREENSHOTS {
        return Err(bad_request(format!(
            "at most {MAX_SCREENSHOTS} screenshots are allowed"
        )));
    }
    let screenshots = body
        .screenshots
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (mime, bytes) = decode_image(s)?;
            let note = clip(body.screenshot_notes.get(i).cloned().flatten(), 1000);
            Ok(NewScreenshot { mime, bytes, note })
        })
        .collect::<std::result::Result<Vec<_>, String>>()
        .map_err(bad_request)?;
    let context = match body.context {
        serde_json::Value::Object(_) => body.context,
        _ => json!({}),
    };
    if context.to_string().len() > 256 * 1024 {
        return Err(bad_request("context is too large"));
    }
    let source = clip(body.source, 32).unwrap_or_else(|| "web".into());
    let db = server.db.clone();
    let new = NewFeedback {
        app,
        kind,
        message: body.message.trim().to_string(),
        reporter: clip(body.reporter, 100),
        page_url: clip(body.url, 2000),
        page_title: clip(body.title, 300),
        user_agent: clip(body.user_agent, 500),
        source,
        context,
        screenshots,
    };
    let dispatch = body.dispatch && is_owner(&server, &headers);
    let id = tokio::task::spawn_blocking(move || db.create(new))
        .await
        .map_err(|e| anyhow::anyhow!(e))??;
    tracing::info!(id, "feedback received");
    if !dispatch {
        return Ok((
            StatusCode::CREATED,
            Json(json!({ "id": id, "dispatched": false })),
        ));
    }
    let choice = last_choice(&server)?;
    let mut new = NewRun {
        agent: choice.agent,
        model: choice.model,
        effort: choice.effort,
        note: None,
    };
    if server.validate_run(&new).is_err() {
        // The remembered model is no longer offered; use the agent's defaults.
        new.model = None;
        new.effort = None;
    }
    let run = server.db.create_run(id, &new, "widget")?.ok();
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "dispatched": run.is_some(), "run": run })),
    ))
}

const OWNER_COOKIE: &str = "fh_owner";

/// The owner's session cookie value: derived from the admin token, so rotating
/// the token signs every browser out.
fn owner_session(token: &str) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, token.as_bytes());
    ring::hmac::sign(&key, b"feedback-hub owner session v1")
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_owner(server: &Server, headers: &HeaderMap) -> bool {
    if server.config.admin_token.is_empty() {
        return false;
    }
    let expected = owner_session(&server.config.admin_token);
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .any(|(name, value)| {
            name == OWNER_COOKIE && constant_time_eq(value.as_bytes(), expected.as_bytes())
        })
}

fn last_choice(server: &Server) -> Result<AgentChoice> {
    Ok(server
        .db
        .setting::<AgentChoice>(LAST_CHOICE)?
        .unwrap_or(AgentChoice {
            agent: "claude".into(),
            model: None,
            effort: None,
        }))
}

/// Called by the triage UI after sign-in: lets the widget on the owner's
/// apps (same site) recognise the owner and offer "send & fix".
async fn start_session(State(server): State<Server>) -> Response {
    let cookie = format!(
        "{OWNER_COOKIE}={}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Lax",
        owner_session(&server.config.admin_token)
    );
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}

async fn end_session() -> Response {
    let cookie = format!("{OWNER_COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax");
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}

/// Tells the widget whether the owner is using it, and which agent "send & fix" uses.
async fn widget_session(
    State(server): State<Server>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    if !is_owner(&server, &headers) {
        return Ok(Json(json!({ "owner": false })));
    }
    let choice = last_choice(&server)?;
    let labels = server.choice_labels(&choice);
    Ok(Json(
        json!({ "owner": true, "choice": choice, "labels": labels }),
    ))
}

async fn agent_choice(State(server): State<Server>) -> Result<Json<AgentChoice>> {
    Ok(Json(last_choice(&server)?))
}

fn remember_choice(server: &Server, run: &NewRun) -> Result<()> {
    server.db.set_setting(
        LAST_CHOICE,
        &AgentChoice {
            agent: run.agent.clone(),
            model: run.model.clone(),
            effort: run.effort.clone(),
        },
    )?;
    Ok(())
}

async fn list_apps(State(server): State<Server>) -> Json<Vec<AppInfo>> {
    Json(server.config.apps.clone())
}

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
    app: Option<String>,
    limit: Option<i64>,
}

async fn list(
    State(server): State<Server>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<Feedback>>> {
    let statuses: Vec<String> = query
        .status
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "all")
        .map(String::from)
        .collect();
    if let Some(bad) = statuses.iter().find(|s| !STATUSES.contains(&s.as_str())) {
        return Err(bad_request(format!("unknown status {bad}")));
    }
    let filter = ListFilter {
        statuses,
        app: query.app.filter(|a| !a.is_empty()),
        limit: query.limit.unwrap_or(200),
    };
    Ok(Json(server.db.list(&filter)?))
}

#[derive(serde::Serialize)]
struct Detail {
    #[serde(flatten)]
    feedback: Feedback,
    screenshots: Vec<Screenshot>,
    events: Vec<Event>,
    runs: Vec<Run>,
}

async fn detail(State(server): State<Server>, Path(id): Path<i64>) -> Result<Json<Detail>> {
    let feedback = server.db.get(id)?.ok_or_else(not_found)?;
    Ok(Json(Detail {
        feedback,
        screenshots: server.db.screenshots(id)?,
        events: server.db.events(id)?,
        runs: server.db.list_runs(&RunFilter {
            active_only: false,
            feedback_id: Some(id),
            limit: 50,
        })?,
    }))
}

async fn screenshot(
    State(server): State<Server>,
    Path((id, idx)): Path<(i64, i64)>,
) -> Result<Response> {
    let shot = server
        .db
        .screenshots(id)?
        .into_iter()
        .find(|s| s.idx == idx)
        .ok_or_else(not_found)?;
    let bytes = tokio::fs::read(server.db.screenshot_path(id, idx, &shot.mime))
        .await
        .map_err(|_| not_found())?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, shot.mime)
        .header(header::CACHE_CONTROL, "private, max-age=86400")
        .body(Body::from(bytes))
        .unwrap())
}

#[derive(Deserialize)]
struct ScreenshotNote {
    idx: i64,
    note: Option<String>,
}

#[derive(Deserialize)]
struct UpdateBody {
    status: Option<String>,
    note: Option<String>,
    author: Option<String>,
    agent: Option<String>,
    /// Edits to the report itself.
    message: Option<String>,
    kind: Option<String>,
    #[serde(default)]
    screenshot_notes: Vec<ScreenshotNote>,
}

fn author(value: Option<String>) -> String {
    clip(value, 100).unwrap_or_else(|| "admin".into())
}

async fn update(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<Feedback>> {
    let feedback = server.db.get(id)?.ok_or_else(not_found)?;
    let who = author(body.author.clone());
    if body.message.is_some() || body.kind.is_some() || !body.screenshot_notes.is_empty() {
        if let Some(kind) = &body.kind
            && !["bug", "idea", "other"].contains(&kind.as_str())
        {
            return Err(bad_request("kind must be bug, idea or other"));
        }
        let message = body.message.as_deref().map(str::trim);
        if message.is_some_and(|m| m.chars().count() > MAX_MESSAGE_CHARS) {
            return Err(bad_request("message is too long"));
        }
        let edit = FeedbackEdit {
            message: message.map(String::from),
            kind: body.kind.clone(),
            screenshot_notes: body
                .screenshot_notes
                .into_iter()
                .map(|n| (n.idx, clip(n.note, 1000)))
                .collect(),
        };
        server.db.edit(id, &edit, &who)?;
    }
    match body.status.as_deref() {
        None => {}
        Some(status) if !STATUSES.contains(&status) => {
            return Err(bad_request(format!(
                "status must be one of {}",
                STATUSES.join(", ")
            )));
        }
        Some("queued") => {
            // Queued items need a run for a worker to pick them up.
            let agent = clip(body.agent, 32)
                .or(feedback.agent)
                .unwrap_or_else(|| "claude".into());
            let new = NewRun {
                agent,
                model: None,
                effort: None,
                note: clip(body.note, MAX_NOTE_CHARS),
            };
            server
                .db
                .create_run(id, &new, &who)?
                .map_err(|run| conflict(format!("#{id} already has active run #{run}")))?;
        }
        Some(status) => {
            server.db.set_status(
                id,
                status,
                &who,
                body.note.as_deref(),
                body.agent.as_deref(),
            )?;
        }
    }
    Ok(Json(server.db.get(id)?.ok_or_else(not_found)?))
}

const MAX_NOTE_CHARS: usize = 4000;

#[derive(Deserialize)]
struct QueueBody {
    agent: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    note: Option<String>,
    author: Option<String>,
}

fn identifier(value: Option<String>, what: &str) -> Result<Option<String>> {
    let value = clip(value, 100);
    if let Some(v) = &value
        && !v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/@[]".contains(c))
    {
        return Err(bad_request(format!("invalid {what} {v:?}")));
    }
    Ok(value)
}

impl QueueBody {
    fn new_run(&self) -> Result<NewRun> {
        Ok(NewRun {
            agent: identifier(self.agent.clone(), "agent")?.unwrap_or_else(|| "claude".into()),
            model: identifier(self.model.clone(), "model")?,
            effort: identifier(self.effort.clone(), "effort")?,
            note: clip(self.note.clone(), MAX_NOTE_CHARS),
        })
    }
}

async fn queue(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Json(body): Json<QueueBody>,
) -> Result<Json<Feedback>> {
    server.db.get(id)?.ok_or_else(not_found)?;
    let new = body.new_run()?;
    server.validate_run(&new)?;
    server
        .db
        .create_run(id, &new, &author(body.author.clone()))?
        .map_err(|run| conflict(format!("#{id} already has active run #{run}")))?;
    remember_choice(&server, &new)?;
    Ok(Json(server.db.get(id)?.ok_or_else(not_found)?))
}

#[derive(Deserialize)]
struct CommentBody {
    body: String,
    author: Option<String>,
    kind: Option<String>,
}

async fn comment(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Json(body): Json<CommentBody>,
) -> Result<StatusCode> {
    server.db.get(id)?.ok_or_else(not_found)?;
    if body.body.trim().is_empty() {
        return Err(bad_request("comment is empty"));
    }
    let kind = match body.kind.as_deref() {
        Some("agent") => "agent",
        _ => "comment",
    };
    server
        .db
        .add_event(id, &author(body.author), kind, body.body.trim())?;
    Ok(StatusCode::CREATED)
}

async fn remove(State(server): State<Server>, Path(id): Path<i64>) -> Result<StatusCode> {
    if server.db.delete(id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaimBody {
    worker: String,
    agents: Vec<String>,
    default_agent: String,
    #[serde(default)]
    include_open: bool,
}

/// Workers from before runs existed must not claim runs they can't execute.
async fn legacy_claim() -> ApiError {
    ApiError(
        StatusCode::GONE,
        "this worker is outdated; update feedback-hub on the worker machine".into(),
    )
}

async fn claim(State(server): State<Server>, Json(body): Json<ClaimBody>) -> Result<Response> {
    let worker = clip(Some(body.worker), 100).unwrap_or_else(|| "worker".into());
    Ok(
        match server.db.claim_run(
            &worker,
            &body.agents,
            &body.default_agent,
            body.include_open,
        )? {
            Some(run) => Json(run).into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        },
    )
}

async fn heartbeat(
    State(server): State<Server>,
    Json(mut status): Json<WorkerStatus>,
) -> Result<Json<serde_json::Value>> {
    status.name = clip(Some(status.name), 100).unwrap_or_else(|| "worker".into());
    status.last_seen = crate::db::now();
    let refresh = server
        .refresh_models
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&status.name);
    let mut workers = server.workers.lock().unwrap_or_else(|p| p.into_inner());
    workers.insert(status.name.clone(), status);
    Ok(Json(json!({ "refresh_models": refresh })))
}

/// Asks every known worker to re-read its agents' model lists.
async fn refresh_workers(State(server): State<Server>) -> StatusCode {
    let names: Vec<String> = server
        .workers
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .keys()
        .cloned()
        .collect();
    server
        .refresh_models
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .extend(names);
    StatusCode::ACCEPTED
}

async fn list_workers(State(server): State<Server>) -> Json<Vec<WorkerStatus>> {
    let workers = server.workers.lock().unwrap_or_else(|p| p.into_inner());
    Json(workers.values().cloned().collect())
}

#[derive(Deserialize)]
struct CreateRunsBody {
    ids: Vec<i64>,
    #[serde(flatten)]
    run: QueueBody,
}

async fn create_runs(
    State(server): State<Server>,
    Json(body): Json<CreateRunsBody>,
) -> Result<Json<serde_json::Value>> {
    if body.ids.is_empty() || body.ids.len() > 100 {
        return Err(bad_request("select between 1 and 100 items"));
    }
    let new = body.run.new_run()?;
    server.validate_run(&new)?;
    remember_choice(&server, &new)?;
    let author = author(body.run.author.clone());
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    for id in body.ids {
        if server.db.get(id)?.is_none() {
            skipped.push(json!({ "id": id, "reason": "not found" }));
            continue;
        }
        match server.db.create_run(id, &new, &author)? {
            Ok(run) => created.push(run),
            Err(active) => skipped
                .push(json!({ "id": id, "reason": format!("already has active run #{active}") })),
        }
    }
    Ok(Json(json!({ "created": created, "skipped": skipped })))
}

#[derive(Deserialize)]
struct RunListQuery {
    active: Option<bool>,
    feedback: Option<i64>,
    limit: Option<i64>,
}

async fn list_runs(
    State(server): State<Server>,
    Query(query): Query<RunListQuery>,
) -> Result<Json<Vec<Run>>> {
    Ok(Json(server.db.list_runs(&RunFilter {
        active_only: query.active.unwrap_or(false),
        feedback_id: query.feedback,
        limit: query.limit.unwrap_or(100),
    })?))
}

#[derive(serde::Serialize)]
struct RunDetail {
    #[serde(flatten)]
    run: Run,
    events: Vec<RunEvent>,
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<i64>,
    limit: Option<i64>,
}

async fn run_detail(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<RunDetail>> {
    let run = server.db.get_run(id)?.ok_or_else(not_found)?;
    let events = server
        .db
        .run_events(id, query.after.unwrap_or(0), query.limit.unwrap_or(1000))?;
    Ok(Json(RunDetail { run, events }))
}

async fn run_events(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<RunEvent>>> {
    server.db.get_run(id)?.ok_or_else(not_found)?;
    Ok(Json(server.db.run_events(
        id,
        query.after.unwrap_or(0),
        query.limit.unwrap_or(1000),
    )?))
}

const MAX_EVENT_CHARS: usize = 8000;
/// The prompt a run started with, kept whole so the owner can see what the agent was told.
const MAX_PROMPT_CHARS: usize = 100_000;

#[derive(Deserialize)]
struct NewRunEvent {
    kind: String,
    body: String,
}

async fn add_run_events(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Json(events): Json<Vec<NewRunEvent>>,
) -> Result<StatusCode> {
    server.db.get_run(id)?.ok_or_else(not_found)?;
    if events.len() > 500 {
        return Err(bad_request("at most 500 events per request"));
    }
    let events: Vec<(String, String)> = events
        .into_iter()
        .map(|e| {
            let mut body = e.body;
            let max = if e.kind == "prompt" {
                MAX_PROMPT_CHARS
            } else {
                MAX_EVENT_CHARS
            };
            if body.chars().count() > max {
                body = body.chars().take(max).collect::<String>() + "…";
            }
            (e.kind.chars().take(32).collect(), body)
        })
        .collect();
    server.db.add_run_events(id, &events)?;
    Ok(StatusCode::CREATED)
}

async fn update_run(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Json(update): Json<RunUpdate>,
) -> Result<Json<Run>> {
    server.db.get_run(id)?.ok_or_else(not_found)?;
    if let Some(status) = &update.status
        && !crate::runs::RUN_STATUSES.contains(&status.as_str())
    {
        return Err(bad_request(format!("unknown run status {status}")));
    }
    Ok(Json(
        server.db.update_run(id, &update)?.ok_or_else(not_found)?,
    ))
}

#[derive(Deserialize)]
struct CommandBody {
    kind: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    interrupt: bool,
    author: Option<String>,
}

async fn add_command(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Json(body): Json<CommandBody>,
) -> Result<Json<Run>> {
    let run = server.db.get_run(id)?.ok_or_else(not_found)?;
    let text = body.body.trim();
    match body.kind.as_str() {
        "stop" => {
            if !crate::runs::ACTIVE.contains(&run.status.as_str()) {
                return Err(conflict(format!("run #{id} is not active")));
            }
        }
        "message" => {
            if text.is_empty() {
                return Err(bad_request("message is empty"));
            }
            if text.chars().count() > MAX_MESSAGE_CHARS {
                return Err(bad_request("message is too long"));
            }
            if !crate::runs::ACTIVE.contains(&run.status.as_str()) && run.session_id.is_none() {
                return Err(conflict(format!(
                    "run #{id} has no agent session to continue; queue a new run instead"
                )));
            }
        }
        _ => return Err(bad_request("kind must be message or stop")),
    }
    Ok(Json(server.db.add_command(
        id,
        &body.kind,
        text,
        body.interrupt,
        &author(body.author),
    )?))
}

async fn take_commands(
    State(server): State<Server>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<crate::runs::Command>>> {
    server.db.get_run(id)?.ok_or_else(not_found)?;
    Ok(Json(server.db.take_commands(id)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000";

    fn test_server() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path(), "DELETE").unwrap();
        let server = Server::new(
            db,
            Config {
                admin_token: "secret".into(),
                allowed_origins: vec!["https://*.example.org".into()],
                static_dir: None,
                apps: vec![],
            },
        );
        (app(server), dir)
    }

    async fn send(
        router: &Router,
        method: &str,
        uri: &str,
        body: serde_json::Value,
        headers: &[(&str, &str)],
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = router
            .clone()
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    const AUTH: (&str, &str) = ("authorization", "Bearer secret");

    #[tokio::test]
    async fn changes_stream_ticks_after_writes_only() {
        let (router, _dir) = test_server();
        let (status, _) = send(&router, "GET", "/api/changes", json!(null), &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let req = Request::get("/api/changes")
            .header("authorization", "Bearer secret")
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        assert_eq!(res.headers()["content-type"], "text/event-stream");
        let mut body = res.into_body();
        let wait = std::time::Duration::from_millis(300);

        let heartbeat = json!({"name": "w", "agents": [], "running": [], "max_parallel": 1});
        let (status, _) = send(&router, "POST", "/api/worker/heartbeat", heartbeat, &[AUTH]).await;
        assert!(status.is_success());
        assert!(
            tokio::time::timeout(wait, body.frame()).await.is_err(),
            "heartbeats are quiet"
        );

        let submit = json!({"app": "demo", "message": "hi"});
        let (status, _) = send(
            &router,
            "POST",
            "/api/feedback",
            submit,
            &[("origin", "https://demo.example.org")],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let frame = tokio::time::timeout(wait, body.frame())
            .await
            .expect("a change event")
            .unwrap()
            .unwrap();
        let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
        assert!(text.starts_with("event: change"), "{text}");
    }

    #[test]
    fn origin_patterns() {
        let patterns = vec![
            "https://*.example.org".to_string(),
            "http://localhost:*".to_string(),
        ];
        assert!(origin_allowed(&patterns, "https://app.example.org"));
        assert!(!origin_allowed(&patterns, "https://example.org.evil.com"));
        assert!(!origin_allowed(&patterns, "https://evilexample.org"));
        assert!(!origin_allowed(&patterns, "http://app.example.org"));
        assert!(origin_allowed(&patterns, "http://localhost:5173"));
        assert!(!origin_allowed(&patterns, "http://localhost.evil.com:5173"));
    }

    #[tokio::test]
    async fn submit_and_manage_feedback() {
        let (router, _dir) = test_server();
        let shot = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(PNG)
        );
        let (status, body) = send(
            &router,
            "POST",
            "/api/feedback",
            json!({"app": "Demo", "message": "Button broken (see #1)", "screenshots": [shot], "screenshotNotes": ["Save does nothing"], "context": {"logs": []}}),
            &[("origin", "https://demo.example.org")],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = body["id"].as_i64().unwrap();

        let (status, _) = send(&router, "GET", "/api/feedback", json!(null), &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, body) = send(
            &router,
            "GET",
            "/api/feedback?status=open",
            json!(null),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[0]["app"], "demo");
        assert_eq!(body[0]["screenshot_count"], 1);

        let res = router
            .clone()
            .oneshot(
                Request::get(format!("/api/feedback/{id}/screenshots/0"))
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.headers()["content-type"], "image/png");
        assert_eq!(
            &res.into_body().collect().await.unwrap().to_bytes()[..],
            PNG
        );

        let (status, body) = send(
            &router,
            "POST",
            &format!("/api/feedback/{id}/queue"),
            json!({"agent": "codex"}),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "queued");

        let (status, _) = send(&router, "POST", "/api/worker/claim", json!({}), &[AUTH]).await;
        assert_eq!(status, StatusCode::GONE, "outdated workers are refused");

        let claim_body = json!({"worker": "w", "agents": ["claude"], "defaultAgent": "claude"});
        let (status, _) = send(
            &router,
            "POST",
            "/api/worker/claim-run",
            claim_body,
            &[AUTH],
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "worker without codex must not claim"
        );

        let claim_body =
            json!({"worker": "w", "agents": ["claude", "codex"], "defaultAgent": "claude"});
        let (status, body) = send(
            &router,
            "POST",
            "/api/worker/claim-run",
            claim_body.clone(),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "running");
        assert_eq!(body["agent"], "codex");
        assert_eq!(body["feedback_id"], id);
        let (status, _) = send(
            &router,
            "POST",
            "/api/worker/claim-run",
            claim_body,
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = send(
            &router,
            "POST",
            &format!("/api/feedback/{id}/events"),
            json!({"body": "Fixed in abc123", "author": "claude", "kind": "agent"}),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, body) = send(
            &router,
            "PATCH",
            &format!("/api/feedback/{id}"),
            json!({"status": "resolved", "note": "done"}),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "resolved");

        let (_, body) = send(
            &router,
            "GET",
            &format!("/api/feedback/{id}"),
            json!(null),
            &[AUTH],
        )
        .await;
        let kinds: Vec<_> = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].clone())
            .collect();
        assert_eq!(
            kinds,
            vec![
                json!("status"),
                json!("claimed"),
                json!("agent"),
                json!("status")
            ]
        );
        assert_eq!(body["screenshots"][0]["mime"], "image/png");
        assert_eq!(body["screenshots"][0]["note"], "Save does nothing");
    }

    #[tokio::test]
    async fn bulk_queue_steer_and_stream_runs() {
        let (router, _dir) = test_server();
        let mut ids = vec![];
        for message in ["one", "two"] {
            let (_, body) = send(
                &router,
                "POST",
                "/api/feedback",
                json!({"app": "demo", "message": message}),
                &[],
            )
            .await;
            ids.push(body["id"].as_i64().unwrap());
        }
        let (status, body) = send(
            &router,
            "POST",
            "/api/runs",
            json!({"ids": ids, "agent": "claude", "model": "opus", "effort": "high", "note": "Keep it small"}),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["created"].as_array().unwrap().len(), 2);
        let (_, body) = send(
            &router,
            "POST",
            "/api/runs",
            json!({"ids": [ids[0]], "agent": "claude"}),
            &[AUTH],
        )
        .await;
        assert_eq!(
            body["skipped"].as_array().unwrap().len(),
            1,
            "already active"
        );
        let (status, _) = send(
            &router,
            "POST",
            "/api/runs",
            json!({"ids": [ids[0]], "agent": "claude", "model": "opus; rm -rf"}),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let claim = json!({"worker": "w", "agents": ["claude"], "defaultAgent": "claude"});
        let (_, run) = send(&router, "POST", "/api/worker/claim-run", claim, &[AUTH]).await;
        let run_id = run["id"].as_i64().unwrap();
        assert_eq!(run["model"], "opus");
        assert_eq!(run["note"], "Keep it small");

        let (status, _) = send(
            &router,
            "POST",
            &format!("/api/runs/{run_id}/events"),
            json!([{"kind": "text", "body": "Looking at it"}, {"kind": "tool", "body": "Bash: ls"}]),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (_, run) = send(
            &router,
            "POST",
            &format!("/api/runs/{run_id}/commands"),
            json!({"kind": "message", "body": "Check the mobile layout too"}),
            &[AUTH],
        )
        .await;
        assert_eq!(run["pending_messages"], 1);
        let (_, commands) = send(
            &router,
            "POST",
            &format!("/api/runs/{run_id}/commands/take"),
            json!({}),
            &[AUTH],
        )
        .await;
        assert_eq!(commands[0]["body"], "Check the mobile layout too");

        let (status, run) = send(
            &router,
            "PATCH",
            &format!("/api/runs/{run_id}"),
            json!({"status": "succeeded", "session_id": "s-1", "cost_usd": 0.25}),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(run["finished_at"].is_string());

        let (_, detail) = send(
            &router,
            "GET",
            &format!("/api/runs/{run_id}?after=0"),
            json!(null),
            &[AUTH],
        )
        .await;
        let kinds: Vec<&str> = detail["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"tool") && kinds.contains(&"user"));

        let (_, active) = send(
            &router,
            "GET",
            "/api/runs?active=true",
            json!(null),
            &[AUTH],
        )
        .await;
        assert_eq!(
            active.as_array().unwrap().len(),
            1,
            "the other run is still queued"
        );

        let heartbeat = json!({"name": "w", "agents": [{"name": "claude", "efforts": ["low", "high"], "live_steering": true,
            "models": [{"id": "opus", "name": "Opus 5.5", "description": null, "efforts": ["low", "high", "max"], "default_effort": null},
                       {"id": "haiku-old", "name": "Haiku 4.5", "description": null, "efforts": [], "default_effort": null}]}],
            "running": [run_id], "max_parallel": 2});
        let (status, reply) = send(
            &router,
            "POST",
            "/api/worker/heartbeat",
            heartbeat.clone(),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reply["refresh_models"], false);
        let (_, workers) = send(&router, "GET", "/api/workers", json!(null), &[AUTH]).await;
        assert_eq!(workers[0]["agents"][0]["models"][0]["id"], "opus");
        send(&router, "POST", "/api/workers/refresh", json!({}), &[AUTH]).await;
        let (_, reply) = send(&router, "POST", "/api/worker/heartbeat", heartbeat, &[AUTH]).await;
        assert_eq!(
            reply["refresh_models"], true,
            "refresh is handed to the worker once"
        );

        // With a worker online, only the models and efforts it reports are accepted.
        let queue = |model: &str, effort: Option<&str>| json!({"ids": [ids[1]], "agent": "claude", "model": model, "effort": effort});
        let (_, run2) = send(
            &router,
            "GET",
            "/api/runs?active=true",
            json!(null),
            &[AUTH],
        )
        .await;
        let queued = run2
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "queued")
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        send(
            &router,
            "POST",
            &format!("/api/runs/{queued}/commands"),
            json!({"kind": "stop"}),
            &[AUTH],
        )
        .await;
        for (model, effort, ok) in [
            ("sonnet", None, false),
            ("opus", Some("ultra"), false),
            ("haiku-old", Some("low"), false),
            ("opus", Some("max"), true),
        ] {
            let (status, _) =
                send(&router, "POST", "/api/runs", queue(model, effort), &[AUTH]).await;
            assert_eq!(status == StatusCode::OK, ok, "{model} {effort:?}");
        }
        let (status, _) = send(
            &router,
            "POST",
            "/api/runs",
            json!({"ids": [ids[0]], "agent": "hermes", "model": "anthropic/claude-sonnet-5"}),
            &[AUTH],
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "hermes must not run Claude models"
        );
    }

    #[tokio::test]
    async fn owner_can_dispatch_from_the_widget_with_last_choice() {
        let (router, _dir) = test_server();
        let origin = ("origin", "https://demo.example.org");
        let submit = json!({"app": "demo", "message": "Fix it", "dispatch": true});
        let (status, body) =
            send(&router, "POST", "/api/feedback", submit.clone(), &[origin]).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["dispatched"], false, "strangers can't dispatch agents");
        let (_, session) = send(
            &router,
            "GET",
            "/api/widget/session",
            json!(null),
            &[origin],
        )
        .await;
        assert_eq!(session["owner"], false);

        let res = router
            .clone()
            .oneshot(
                Request::post("/api/session")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let set_cookie = res.headers()["set-cookie"].to_str().unwrap().to_string();
        assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Lax"));
        let cookie = set_cookie.split(';').next().unwrap().to_string();
        let owner = [origin, ("cookie", cookie.as_str())];

        let (_, session) = send(&router, "GET", "/api/widget/session", json!(null), &owner).await;
        assert_eq!(session["owner"], true);
        assert_eq!(
            session["choice"]["agent"], "claude",
            "default before anything was queued"
        );

        let id = body["id"].as_i64().unwrap();
        send(
            &router,
            "POST",
            &format!("/api/feedback/{id}/queue"),
            json!({"agent": "codex", "model": "gpt-6-sol", "effort": "high"}),
            &[AUTH],
        )
        .await;
        let (_, choice) = send(&router, "GET", "/api/agent-choice", json!(null), &[AUTH]).await;
        assert_eq!(
            choice,
            json!({"agent": "codex", "model": "gpt-6-sol", "effort": "high"})
        );

        let (_, body) = send(&router, "POST", "/api/feedback", submit, &owner).await;
        assert_eq!(body["dispatched"], true);
        assert_eq!(body["run"]["agent"], "codex");
        assert_eq!(body["run"]["model"], "gpt-6-sol");
        assert_eq!(body["run"]["requested_by"], "widget");
        let bad = [origin, ("cookie", "fh_owner=forged")];
        let (_, session) = send(&router, "GET", "/api/widget/session", json!(null), &bad).await;
        assert_eq!(session["owner"], false);
    }

    #[tokio::test]
    async fn edits_feedback_and_keeps_history() {
        let (router, _dir) = test_server();
        let shot = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(PNG)
        );
        let (_, body) = send(
            &router,
            "POST",
            "/api/feedback",
            json!({"app": "demo", "message": "Typo here", "screenshots": [shot]}),
            &[],
        )
        .await;
        let id = body["id"].as_i64().unwrap();
        let (status, item) = send(
            &router,
            "PATCH",
            &format!("/api/feedback/{id}"),
            json!({"message": "Typo in the header", "kind": "idea", "screenshot_notes": [{"idx": 0, "note": "Top left"}]}),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(item["message"], "Typo in the header");
        assert_eq!(item["status"], "open", "editing doesn't change status");
        let (_, detail) = send(
            &router,
            "GET",
            &format!("/api/feedback/{id}"),
            json!(null),
            &[AUTH],
        )
        .await;
        assert_eq!(detail["screenshots"][0]["note"], "Top left");
        let edited = &detail["events"][0];
        assert_eq!(edited["kind"], "edited");
        assert!(edited["body"].as_str().unwrap().contains("> Typo here"));
        let (status, _) = send(
            &router,
            "PATCH",
            &format!("/api/feedback/{id}"),
            json!({"kind": "nope"}),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn static_files_revalidate_by_content_not_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let web = tempfile::tempdir().unwrap();
        std::fs::write(web.path().join("widget.js"), "v1").unwrap();
        std::fs::write(web.path().join("index.html"), "<html></html>").unwrap();
        let make = || {
            app(Server::new(
                Database::open(dir.path(), "DELETE").unwrap(),
                Config {
                    admin_token: "secret".into(),
                    allowed_origins: vec![],
                    static_dir: Some(web.path().to_path_buf()),
                    apps: vec![],
                },
            ))
        };
        let get = |router: Router, headers: &[(&str, &str)]| {
            let mut req = Request::get("/widget.js");
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            let req = req.body(Body::empty()).unwrap();
            async move { router.oneshot(req).await.unwrap() }
        };
        let res = get(make(), &[]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get("last-modified").is_none());
        assert_eq!(res.headers()["cache-control"], "public, max-age=300");
        let etag = res.headers()["etag"].to_str().unwrap().to_string();

        let res = get(make(), &[("if-none-match", &etag)]).await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(res.headers()["access-control-allow-origin"], "*");
        // Browsers holding a copy from before ETags revalidate by date; they must get the file.
        let res = get(
            make(),
            &[("if-modified-since", "Thu, 01 Jan 1970 00:00:01 GMT")],
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);

        // A new build has a new ETag, so the old one no longer matches.
        std::fs::write(web.path().join("widget.js"), "v2").unwrap();
        let res = get(make(), &[("if-none-match", &etag)]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_ne!(res.headers()["etag"].to_str().unwrap(), etag);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"v2");

        let res = make()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.headers()["cache-control"], "no-cache");
    }

    #[tokio::test]
    async fn answers_cors_preflight_for_submissions() {
        let (router, _dir) = test_server();
        let res = router
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/api/feedback")
                    .header("origin", "https://demo.example.org")
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "content-type")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()["access-control-allow-origin"],
            "https://demo.example.org"
        );
    }

    #[tokio::test]
    async fn rejects_bad_submissions() {
        let (router, _dir) = test_server();
        let (status, _) = send(
            &router,
            "POST",
            "/api/feedback",
            json!({"app": "demo", "message": "hi"}),
            &[("origin", "https://evil.test")],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(
            &router,
            "POST",
            "/api/feedback",
            json!({"app": "demo"}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = send(
            &router,
            "POST",
            "/api/feedback",
            json!({"app": "demo", "screenshots": ["data:image/png;base64,aGVsbG8="]}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = send(
            &router,
            "POST",
            "/api/feedback",
            json!({"app": "../etc", "message": "x"}),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
