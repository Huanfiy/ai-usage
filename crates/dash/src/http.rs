use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use ai_usage_protocol::{
    hash_token, host_id_from_token, is_valid_claim_hash, IngestRequest, JoinCreated,
    JoinPollResponse, JoinRequest, JoinStatus, JOIN_IP_LIMIT, JOIN_PENDING_MAX, JOIN_TTL_SECS,
};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use rand::{Rng, RngCore};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;

use crate::config::DashConfig;
use crate::db::{self, Db};
use crate::pricing::PriceBook;
use crate::query::{self, QueryFilter};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub pricing: Arc<RwLock<PriceBook>>,
    pub config: DashConfig,
    pub join_hits: Arc<Mutex<Vec<(String, Instant)>>>,
}

#[derive(RustEmbed)]
#[folder = "web-dist"]
struct Assets;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/ingest", post(ingest))
        .route("/v1/summary", get(summary))
        .route("/v1/series", get(series))
        .route("/v1/breakdown", get(breakdown))
        .route("/v1/distributions", get(distributions))
        .route("/v1/activity", get(activity))
        .route("/v1/sessions", get(sessions))
        .route("/v1/hosts", get(hosts))
        .route("/v1/hosts/{host_id}", delete(delete_host))
        .route("/v1/cursor-accounts", get(cursor_accounts))
        .route("/v1/filters", get(filters))
        .route("/v1/join", post(create_join))
        .route("/v1/join/{join_id}", get(poll_join))
        .route("/v1/joins", get(list_joins))
        .route("/v1/joins/{join_id}/approve", post(approve_join))
        .route("/v1/joins/{join_id}/deny", post(deny_join))
        .route("/v1/tokens", get(list_tokens))
        .route("/v1/tokens/{host_id}", delete(revoke_token))
        .fallback(static_handler)
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn ingest(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let token = bearer(&headers).ok_or(ApiError::Unauthorized)?;
    let token_hash = hash_token(token);
    let host_id = st
        .db
        .with(|c| {
            let row = db::lookup_token(c, &token_hash)?;
            match row {
                Some(r) if !r.revoked => Ok(r.host_id),
                _ => anyhow::bail!("unauthorized"),
            }
        })
        .map_err(|_| ApiError::Unauthorized)?;

    let raw = decode_body(&headers, &body).map_err(|e| ApiError::Bad(e))?;
    let req: IngestRequest =
        serde_json::from_slice(&raw).map_err(|e| ApiError::Bad(e.to_string()))?;
    let hostname = req
        .hostname
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unknown".into());
    let agent_version = req.agent_version.clone();
    let timezone = req.timezone.clone();
    let resp = st
        .db
        .with(|c| {
            crate::ingest::ingest(
                c,
                &host_id,
                &hostname,
                agent_version.as_deref(),
                timezone.as_deref(),
                req,
            )
        })
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(resp).unwrap()))
}

#[derive(Debug, Deserialize)]
struct CommonQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    host: Option<String>,
    source: Option<String>,
    model: Option<String>,
    project: Option<String>,
    by: Option<String>,
    limit: Option<i64>,
    hide_projects: Option<bool>,
}

fn filter_from(st: &AppState, q: &CommonQuery) -> QueryFilter {
    QueryFilter::from_params(
        q.from,
        q.to,
        q.host.clone(),
        q.source.clone(),
        q.model.clone(),
        q.project.clone(),
        q.hide_projects.unwrap_or(st.config.hide_projects),
    )
}

async fn summary(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CommonQuery>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let f = filter_from(&st, &q);
    let book = st.pricing.read().unwrap();
    let out = st
        .db
        .with(|c| query::summary(c, &book, &f))
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(out).unwrap()))
}

async fn series(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CommonQuery>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let f = filter_from(&st, &q);
    let book = st.pricing.read().unwrap();
    let out = st
        .db
        .with(|c| query::series(c, &book, &f))
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "points": out })))
}

async fn breakdown(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CommonQuery>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let f = filter_from(&st, &q);
    let by = q.by.clone().unwrap_or_else(|| "tool".into());
    let book = st.pricing.read().unwrap();
    let out = st
        .db
        .with(|c| query::breakdown(c, &book, &f, &by))
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "items": out })))
}

async fn distributions(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CommonQuery>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let f = filter_from(&st, &q);
    let book = st.pricing.read().unwrap();
    let out = st
        .db
        .with(|c| query::distributions(c, &book, &f))
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(out).unwrap()))
}

async fn activity(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CommonQuery>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let f = filter_from(&st, &q);
    let book = st.pricing.read().unwrap();
    let out = st
        .db
        .with(|c| query::activity(c, &book, &f))
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(out).unwrap()))
}

async fn sessions(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CommonQuery>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let f = filter_from(&st, &q);
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let out = st
        .db
        .with(|c| query::sessions(c, &f, limit))
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "items": out })))
}

async fn hosts(State(st): State<AppState>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let out = st.db.with(query::hosts).map_err(ApiError::internal)?;
    Ok(Json(json!({ "items": out })))
}

async fn cursor_accounts(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let out = st
        .db
        .with(db::list_cursor_accounts)
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "items": out })))
}

async fn filters(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CommonQuery>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let f = filter_from(&st, &q);
    let out = st
        .db
        .with(|c| query::filter_options(c, &f))
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(out).unwrap()))
}

async fn create_join(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<JoinRequest>,
) -> Result<Json<JoinCreated>, ApiError> {
    let hostname = body.hostname.trim();
    if hostname.is_empty() {
        return Err(ApiError::Bad("hostname required".into()));
    }
    if !is_valid_claim_hash(&body.claim_hash) {
        return Err(ApiError::Bad("invalid claim_hash".into()));
    }
    if !allow_join_ip(&st, &client_ip(&headers)) {
        return Err(ApiError::Bad("too many join requests".into()));
    }
    let now = Utc::now();
    let now_s = now.to_rfc3339();
    let expires_at = (now + chrono::Duration::seconds(JOIN_TTL_SECS as i64)).to_rfc3339();
    let join_id = random_hex(16);
    let confirm_pin = format!("{:04}", rand::thread_rng().gen_range(0..10000));
    let hostname = hostname.to_string();
    let agent_version = body
        .agent_version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    st.db
        .with(|c| {
            db::expire_stale_joins(c, &now_s)?;
            let n = db::pending_join_count(c, &now_s)?;
            if n >= JOIN_PENDING_MAX as i64 {
                anyhow::bail!("too many pending joins");
            }
            db::insert_join(
                c,
                &join_id,
                &body.claim_hash,
                &confirm_pin,
                &hostname,
                agent_version.as_deref(),
                &now_s,
                &expires_at,
            )
        })
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("too many pending") {
                ApiError::Bad(msg)
            } else {
                ApiError::internal(e)
            }
        })?;
    Ok(Json(JoinCreated {
        join_id,
        confirm_pin,
        expires_in: JOIN_TTL_SECS,
    }))
}

async fn poll_join(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(join_id): Path<String>,
) -> Result<Json<JoinPollResponse>, ApiError> {
    let secret = bearer(&headers).ok_or(ApiError::Unauthorized)?;
    let claim_hash = hash_token(secret);
    let now = Utc::now().to_rfc3339();
    let out = st
        .db
        .with(|c| {
            db::expire_stale_joins(c, &now)?;
            let row = db::get_join(c, &join_id)?.ok_or_else(|| anyhow::anyhow!("not found"))?;
            if row.claim_hash != claim_hash {
                anyhow::bail!("unauthorized");
            }
            let resp = match row.status.as_str() {
                "pending" => JoinPollResponse {
                    status: JoinStatus::Pending,
                    token: None,
                    host_id: None,
                    confirm_pin: Some(row.confirm_pin),
                },
                "approved" => match db::claim_join(c, &join_id)? {
                    Some((token, host_id)) => JoinPollResponse {
                        status: JoinStatus::Approved,
                        token: Some(token),
                        host_id: Some(host_id),
                        confirm_pin: None,
                    },
                    None => JoinPollResponse {
                        status: JoinStatus::Expired,
                        token: None,
                        host_id: None,
                        confirm_pin: None,
                    },
                },
                "denied" => JoinPollResponse {
                    status: JoinStatus::Denied,
                    token: None,
                    host_id: None,
                    confirm_pin: None,
                },
                _ => JoinPollResponse {
                    status: JoinStatus::Expired,
                    token: None,
                    host_id: None,
                    confirm_pin: None,
                },
            };
            Ok(resp)
        })
        .map_err(|e| {
            let msg = e.to_string();
            if msg == "unauthorized" {
                ApiError::Unauthorized
            } else if msg == "not found" {
                ApiError::Bad("join not found".into())
            } else {
                ApiError::internal(e)
            }
        })?;
    Ok(Json(out))
}

async fn list_joins(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let now = Utc::now().to_rfc3339();
    let items = st
        .db
        .with(|c| {
            db::expire_stale_joins(c, &now)?;
            db::list_pending_joins(c, &now)
        })
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "items": items })))
}

async fn approve_join(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(join_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let now = Utc::now().to_rfc3339();
    st.db
        .with(|c| {
            db::expire_stale_joins(c, &now)?;
            let row = db::get_join(c, &join_id)?.ok_or_else(|| anyhow::anyhow!("not found"))?;
            if row.status != "pending" {
                anyhow::bail!("not pending");
            }
            if row.expires_at <= now {
                anyhow::bail!("expired");
            }
            let token = new_token();
            let token_hash = hash_token(&token);
            let host_id = host_id_from_token(&token);
            let prefix: String = token.chars().take(12).collect();
            db::insert_token(c, &token_hash, &host_id, &prefix, None, &row.hostname)?;
            if !db::approve_join(c, &join_id, &token, &host_id)? {
                let _ = db::revoke_token(c, &host_id);
                anyhow::bail!("not pending");
            }
            Ok(())
        })
        .map_err(|e| {
            let msg = e.to_string();
            if msg == "not found" || msg == "not pending" || msg == "expired" {
                ApiError::Bad(msg)
            } else {
                ApiError::internal(e)
            }
        })?;
    Ok(Json(json!({ "ok": true })))
}

async fn deny_join(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(join_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let now = Utc::now().to_rfc3339();
    let ok = st
        .db
        .with(|c| {
            db::expire_stale_joins(c, &now)?;
            db::deny_join(c, &join_id)
        })
        .map_err(ApiError::internal)?;
    if !ok {
        return Err(ApiError::Bad("join not found".into()));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn list_tokens(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let items = st.db.with(db::list_tokens).map_err(ApiError::internal)?;
    Ok(Json(json!({ "items": items })))
}

async fn revoke_token(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let ok = st
        .db
        .with(|c| db::revoke_token(c, &host_id))
        .map_err(ApiError::internal)?;
    if !ok {
        return Err(ApiError::Bad("token not found".into()));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete_host(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(host_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let outcome = st
        .db
        .with(|c| db::delete_host(c, &host_id))
        .map_err(ApiError::internal)?;
    match outcome {
        db::DeleteHostOutcome::Deleted => Ok(Json(json!({ "ok": true }))),
        db::DeleteHostOutcome::NotFound => Err(ApiError::Bad("host not found".into())),
        db::DeleteHostOutcome::NotRevoked => {
            Err(ApiError::Bad("revoke token before deleting host".into()))
        }
        db::DeleteHostOutcome::AccountScoped => Err(ApiError::Bad(
            "account-scoped host cannot be deleted".into(),
        )),
    }
}

async fn static_handler(uri: Uri) -> Response {
    if uri.path().starts_with("/v1/") {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            json!({"error": "not found"}).to_string(),
        )
            .into_response();
    }
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(file) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return (
            [(header::CONTENT_TYPE, mime.essence_str().to_string())],
            file.data,
        )
            .into_response();
    }
    if let Some(file) = Assets::get("index.html") {
        return (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            file.data,
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(str::trim)
}

fn require_ui(st: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if st.config.ui_token.is_empty() {
        return Ok(());
    }
    match bearer(headers) {
        Some(t) if t == st.config.ui_token => Ok(()),
        _ => Err(ApiError::Unauthorized),
    }
}

fn decode_body(headers: &HeaderMap, body: &Bytes) -> Result<Vec<u8>, String> {
    let enc = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if enc.eq_ignore_ascii_case("gzip") {
        let mut dec = GzDecoder::new(body.as_ref());
        let mut out = Vec::new();
        dec.read_to_end(&mut out).map_err(|e| e.to_string())?;
        Ok(out)
    } else {
        Ok(body.to_vec())
    }
}

pub fn new_token() -> String {
    format!("aiu_{}", random_hex(32))
}

fn random_hex(nbytes: usize) -> String {
    let mut bytes = vec![0u8; nbytes];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn allow_join_ip(st: &AppState, ip: &str) -> bool {
    let now = Instant::now();
    let window = Duration::from_secs(JOIN_TTL_SECS);
    let mut hits = st.join_hits.lock().expect("join hits");
    hits.retain(|(_, t)| now.duration_since(*t) < window);
    if hits.iter().filter(|(k, _)| k == ip).count() >= JOIN_IP_LIMIT {
        return false;
    }
    hits.push((ip.to_string(), now));
    true
}

enum ApiError {
    Unauthorized,
    Bad(String),
    Internal(String),
}

impl ApiError {
    fn internal(err: impl std::fmt::Display) -> Self {
        Self::Internal(err.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error":"unauthorized"})),
            )
                .into_response(),
            Self::Bad(msg) => {
                (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response()
            }
            Self::Internal(msg) => {
                tracing::error!("{msg}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":"internal"})),
                )
                    .into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DashConfig;
    use crate::pricing::PriceBook;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        let book = PriceBook::load(dir.path(), None).unwrap();
        let state = AppState {
            db: Arc::new(db),
            pricing: Arc::new(RwLock::new(book)),
            config: DashConfig::default(),
            join_hits: Arc::new(Mutex::new(Vec::new())),
        };
        (router(state), dir)
    }

    async fn post_json(app: Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
        (status, body)
    }

    async fn get_auth(app: Router, uri: &str, bearer: &str) -> (StatusCode, Value) {
        let res = app
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("authorization", format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
        (status, body)
    }

    async fn get_json(app: Router, uri: &str) -> (StatusCode, Value) {
        let res = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn unknown_v1_is_json_404() {
        let (app, _dir) = app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/v1/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "not found");
    }

    #[tokio::test]
    async fn distributions_route_shape() {
        let (app, _dir) = app();
        let (status, body) = get_json(app, "/v1/distributions").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["host"].is_array());
        assert!(body["source"].is_array());
        assert!(body["model"].is_array());
        assert!(body["project"].is_array());
    }

    #[tokio::test]
    async fn activity_route_has_168_cells() {
        let (app, _dir) = app();
        let (status, body) = get_json(app, "/v1/activity").await;
        assert_eq!(status, StatusCode::OK);
        let cells = body["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 168);
        assert_eq!(cells[0]["dow"], 0);
        assert_eq!(cells[0]["hour"], 0);
        assert_eq!(cells[167]["dow"], 6);
        assert_eq!(cells[167]["hour"], 23);
        assert_eq!(cells[0]["tokens"], 0);
        assert!(cells[0]["cost_usd"].is_number());
    }

    fn join_body(secret: &str) -> Value {
        json!({
            "hostname": "box",
            "agent_version": "0.1.0",
            "claim_hash": hash_token(secret),
        })
    }

    #[tokio::test]
    async fn join_approve_claim_once() {
        let (app, _dir) = app();
        let secret = "claim-secret-one";
        let (st, created) = post_json(app.clone(), "/v1/join", join_body(secret)).await;
        assert_eq!(st, StatusCode::OK, "{created}");
        let join_id = created["join_id"].as_str().unwrap().to_string();
        let pin = created["confirm_pin"].as_str().unwrap().to_string();
        assert_eq!(pin.len(), 4);
        assert_eq!(created["expires_in"], JOIN_TTL_SECS);

        let (st, poll) = get_auth(app.clone(), &format!("/v1/join/{join_id}"), secret).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(poll["status"], "pending");
        assert_eq!(poll["confirm_pin"], pin);

        let (st, listed) = get_json(app.clone(), "/v1/joins").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(listed["items"].as_array().unwrap().len(), 1);
        assert_eq!(listed["items"][0]["confirm_pin"], pin);

        let (st, _) = post_json(
            app.clone(),
            &format!("/v1/joins/{join_id}/approve"),
            json!({}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (st, claimed) = get_auth(app.clone(), &format!("/v1/join/{join_id}"), secret).await;
        assert_eq!(st, StatusCode::OK, "{claimed}");
        assert_eq!(claimed["status"], "approved");
        let token = claimed["token"].as_str().unwrap();
        assert!(token.starts_with("aiu_"));
        assert!(claimed["host_id"].as_str().unwrap().len() == 32);

        let (st, again) = get_auth(app.clone(), &format!("/v1/join/{join_id}"), secret).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(again["status"], "expired");
        assert!(again.get("token").is_none() || again["token"].is_null());

        let (st, listed) = get_json(app, "/v1/joins").await;
        assert_eq!(st, StatusCode::OK);
        assert!(listed["items"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn join_wrong_secret_is_401() {
        let (app, _dir) = app();
        let (st, created) = post_json(app.clone(), "/v1/join", join_body("right")).await;
        assert_eq!(st, StatusCode::OK);
        let join_id = created["join_id"].as_str().unwrap();
        let (st, _) = get_auth(app, &format!("/v1/join/{join_id}"), "wrong").await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn join_deny_then_poll() {
        let (app, _dir) = app();
        let secret = "deny-me";
        let (st, created) = post_json(app.clone(), "/v1/join", join_body(secret)).await;
        assert_eq!(st, StatusCode::OK);
        let join_id = created["join_id"].as_str().unwrap();
        let (st, _) = post_json(app.clone(), &format!("/v1/joins/{join_id}/deny"), json!({})).await;
        assert_eq!(st, StatusCode::OK);
        let (st, poll) = get_auth(app, &format!("/v1/join/{join_id}"), secret).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(poll["status"], "denied");
        assert!(poll.get("token").is_none() || poll["token"].is_null());
    }

    #[tokio::test]
    async fn join_rejects_bad_claim_hash() {
        let (app, _dir) = app();
        let (st, _) = post_json(
            app,
            "/v1/join",
            json!({"hostname": "box", "claim_hash": "nope"}),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }
}
