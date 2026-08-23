use std::sync::{Arc, RwLock};

use ai_usage_protocol::{hash_token, host_id_from_token, IngestRequest};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use rand::RngCore;
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
        .route("/v1/filters", get(filters))
        .route("/v1/tokens", get(list_tokens).post(create_token))
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

#[derive(Deserialize)]
struct CreateToken {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
}

async fn create_token(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateToken>,
) -> Result<Json<Value>, ApiError> {
    require_ui(&st, &headers)?;
    let token = new_token();
    let token_hash = hash_token(&token);
    let host_id = host_id_from_token(&token);
    let prefix: String = token.chars().take(12).collect();
    let hostname = body
        .hostname
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unnamed".into());
    st.db
        .with(|c| {
            db::insert_token(
                c,
                &token_hash,
                &host_id,
                &prefix,
                body.label.as_deref(),
                &hostname,
            )
        })
        .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "token": token,
        "host_id": host_id,
        "token_prefix": prefix,
    })))
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
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("aiu_{}", hex::encode(bytes))
}

pub fn bootstrap_token_if_empty(db: &Db) -> Result<Option<(String, String)>, anyhow::Error> {
    db.with(|c| {
        if db::token_count(c)? > 0 {
            return Ok(None);
        }
        let token = new_token();
        let hash = hash_token(&token);
        let host_id = host_id_from_token(&token);
        let prefix: String = token.chars().take(12).collect();
        db::insert_token(c, &hash, &host_id, &prefix, Some("local"), "local")?;
        Ok(Some((token, host_id)))
    })
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
        };
        (router(state), dir)
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
}
