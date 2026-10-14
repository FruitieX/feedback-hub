//! HTTP API: anonymous feedback submission for the widget/apps, and a
//! token-protected management API used by the triage UI, CLI and worker.

use crate::{
    apps::AppInfo,
    db::{Database, Event, Feedback, ListFilter, NewFeedback, STATUSES, Screenshot},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
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
        .allow_private_network(true);

    let public = Router::new()
        .route("/api/feedback", post(submit))
        .route("/api/apps", get(list_apps))
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
        .route("/api/worker/claim", post(claim))
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
        .layer(middleware::from_fn(cache_headers))
        // Outermost so preflights for paths shared with admin routes are answered
        // here instead of hitting the token check.
        .layer(cors)
        .with_state(server)
}

async fn cache_headers(request: Request, next: Next) -> Response {
    let is_widget = request.uri().path() == "/widget.js";
    let mut response = next.run(request).await;
    if is_widget {
        // Apps load the widget cross-origin; keep it fresh so fixes roll out quickly.
        let headers = response.headers_mut();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=300"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    response
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
    /// Data URLs (`data:image/png;base64,...`).
    #[serde(default)]
    screenshots: Vec<String>,
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
        .map(|s| decode_image(s))
        .collect::<std::result::Result<Vec<_>, _>>()
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
    let id = tokio::task::spawn_blocking(move || db.create(new))
        .await
        .map_err(|e| anyhow::anyhow!(e))??;
    tracing::info!(id, "feedback received");
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
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
}

async fn detail(State(server): State<Server>, Path(id): Path<i64>) -> Result<Json<Detail>> {
    let feedback = server.db.get(id)?.ok_or_else(not_found)?;
    Ok(Json(Detail {
        feedback,
        screenshots: server.db.screenshots(id)?,
        events: server.db.events(id)?,
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
struct UpdateBody {
    status: String,
    note: Option<String>,
    author: Option<String>,
    agent: Option<String>,
}

fn author(value: Option<String>) -> String {
    clip(value, 100).unwrap_or_else(|| "admin".into())
}

async fn update(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<Feedback>> {
    server.db.get(id)?.ok_or_else(not_found)?;
    if !STATUSES.contains(&body.status.as_str()) {
        return Err(bad_request(format!(
            "status must be one of {}",
            STATUSES.join(", ")
        )));
    }
    server.db.set_status(
        id,
        &body.status,
        &author(body.author),
        body.note.as_deref(),
        body.agent.as_deref(),
    )?;
    Ok(Json(server.db.get(id)?.ok_or_else(not_found)?))
}

#[derive(Deserialize)]
struct QueueBody {
    agent: Option<String>,
    note: Option<String>,
    author: Option<String>,
}

async fn queue(
    State(server): State<Server>,
    Path(id): Path<i64>,
    Json(body): Json<QueueBody>,
) -> Result<Json<Feedback>> {
    server.db.get(id)?.ok_or_else(not_found)?;
    let agent = clip(body.agent, 32);
    server.db.set_status(
        id,
        "queued",
        &author(body.author),
        body.note.as_deref(),
        agent.as_deref(),
    )?;
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

async fn claim(State(server): State<Server>, Json(body): Json<ClaimBody>) -> Result<Response> {
    let worker = clip(Some(body.worker), 100).unwrap_or_else(|| "worker".into());
    Ok(
        match server.db.claim(
            &worker,
            &body.agents,
            &body.default_agent,
            body.include_open,
        )? {
            Some(item) => Json(item).into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        },
    )
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
        let server = Server {
            db,
            config: Arc::new(Config {
                admin_token: "secret".into(),
                allowed_origins: vec!["https://*.example.org".into()],
                static_dir: None,
                apps: vec![],
            }),
        };
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
            json!({"app": "Demo", "message": "Button broken", "screenshots": [shot], "context": {"logs": []}}),
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

        let claim_body = json!({"worker": "w", "agents": ["claude"], "defaultAgent": "claude"});
        let (status, _) = send(&router, "POST", "/api/worker/claim", claim_body, &[AUTH]).await;
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
            "/api/worker/claim",
            claim_body.clone(),
            &[AUTH],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "in_progress");
        assert_eq!(body["attempts"], 1);
        let (status, _) = send(&router, "POST", "/api/worker/claim", claim_body, &[AUTH]).await;
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
