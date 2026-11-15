use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::{header, request::Parts, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use protocol::{Credentials, Session};
use sqlx::PgPool;

use crate::auth;
use crate::hub::Hub;
use crate::ratelimit::Limiters;
use crate::turn::TurnConfig;
use crate::ws;

const SESSION_DAYS: i32 = 30;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub signup_enabled: bool,
    pub limiters: Arc<Limiters>,
    pub hub: Arc<Hub>,
    pub turn: Option<Arc<TurnConfig>>,
    /// Behind a reverse proxy: take the client address from `X-Forwarded-For`.
    pub trust_proxy: bool,
}

impl AppState {
    pub fn new(pool: PgPool, signup_enabled: bool) -> Self {
        Self { pool, signup_enabled, limiters: Arc::default(), hub: Arc::default(), turn: None, trust_proxy: false }
    }

    /// Only enable this when a reverse proxy you control is the sole way to reach the server;
    /// otherwise anyone can pick the address their rate limit is counted under.
    pub fn with_trusted_proxy(mut self) -> Self {
        self.trust_proxy = true;
        self
    }

    /// The address rate limits are counted under.
    fn client_ip(&self, headers: &HeaderMap, peer: SocketAddr) -> String {
        let forwarded = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
        pick_ip(self.trust_proxy, forwarded, peer.ip()).to_string()
    }

    pub fn with_turn(mut self, turn: TurnConfig) -> Self {
        self.turn = Some(Arc::new(turn));
        self
    }

    /// STUN/TURN servers for a client entering a room. Empty when TURN is not configured.
    pub fn ice_servers(&self, user: i64) -> Vec<protocol::IceServer> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        self.turn.as_ref().map(|t| t.ice_servers(user, now)).unwrap_or_default()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws::ws_handler))
        .route("/api/signup", post(signup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .with_state(state)
}

pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    Forbidden(&'static str),
    Conflict(&'static str),
    TooManyRequests,
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError::Internal(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "invalid username or password"),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, "too many attempts, try again later"),
            ApiError::Internal(e) => {
                tracing::error!("internal error: {e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

/// The signed-in user, resolved from `Authorization: Bearer <token>`.
pub struct AuthUser {
    pub user_id: i64,
    pub username: String,
    token_hash: Vec<u8>,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthorized)?;
        let token_hash = auth::token_hash(token);
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT u.id, u.username FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.token_hash = $1 AND s.expires_at > now()",
        )
        .bind(&token_hash)
        .fetch_optional(&state.pool)
        .await?;
        let (user_id, username) = row.ok_or(ApiError::Unauthorized)?;
        Ok(AuthUser { user_id, username, token_hash })
    }
}

async fn new_session(pool: &PgPool, user_id: i64, username: String) -> Result<Session, ApiError> {
    let token = auth::new_token();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, now() + make_interval(days => $3))",
    )
    .bind(auth::token_hash(&token))
    .bind(user_id)
    .bind(SESSION_DAYS)
    .execute(pool)
    .await?;
    Ok(Session { token, user_id, username })
}

async fn signup(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Credentials>,
) -> Result<Json<Session>, ApiError> {
    if !state.signup_enabled {
        return Err(ApiError::Forbidden("signup is closed on this server"));
    }
    let ip = state.client_ip(&headers, addr);
    if state.limiters.signup_ip.blocked(&ip) {
        return Err(ApiError::TooManyRequests);
    }
    state.limiters.signup_ip.record(&ip);

    let username = protocol::validate_username(&body.username).map_err(ApiError::BadRequest)?;
    protocol::validate_password(&body.password).map_err(ApiError::BadRequest)?;

    let password = body.password;
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password)).await??;
    let inserted: Result<(i64,), sqlx::Error> =
        sqlx::query_as("INSERT INTO users (username, password_hash) VALUES ($1, $2) RETURNING id")
            .bind(&username)
            .bind(hash)
            .fetch_one(&state.pool)
            .await;
    let (user_id,) = match inserted {
        Ok(row) => row,
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::Conflict("username is taken"))
        }
        Err(e) => return Err(e.into()),
    };
    Ok(Json(new_session(&state.pool, user_id, username).await?))
}

async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Credentials>,
) -> Result<Json<Session>, ApiError> {
    let ip = state.client_ip(&headers, addr);
    let username = body.username.trim().to_ascii_lowercase();
    if state.limiters.login_ip.blocked(&ip) || state.limiters.login_user.blocked(&username) {
        return Err(ApiError::TooManyRequests);
    }
    if body.password.chars().count() > protocol::PASSWORD_MAX {
        return Err(ApiError::BadRequest("password too long"));
    }

    let row: Option<(i64, String, String)> =
        sqlx::query_as("SELECT id, username, password_hash FROM users WHERE username = $1")
            .bind(&username)
            .fetch_optional(&state.pool)
            .await?;

    // Verify against a dummy hash for unknown users so timing does not leak existence.
    let (stored_hash, found) = match &row {
        Some((_, _, h)) => (h.clone(), true),
        None => (auth::dummy_hash().to_owned(), false),
    };
    let password = body.password;
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&stored_hash, &password)).await?;

    match row {
        Some((user_id, username, _)) if ok && found => {
            state.limiters.login_user.clear(&username);
            Ok(Json(new_session(&state.pool, user_id, username).await?))
        }
        _ => {
            state.limiters.login_ip.record(&ip);
            state.limiters.login_user.record(&username);
            Err(ApiError::Unauthorized)
        }
    }
}

async fn logout(State(state): State<AppState>, user: AuthUser) -> Result<StatusCode, ApiError> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(&user.token_hash)
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn me(user: AuthUser) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "user_id": user.user_id, "username": user.username }))
}

/// The last `X-Forwarded-For` entry is the one our own proxy appended, so it cannot be forged by
/// the client; earlier entries can. Anything unreadable falls back to the connecting address.
fn pick_ip(trust_proxy: bool, forwarded: Option<&str>, peer: IpAddr) -> IpAddr {
    if !trust_proxy {
        return peer;
    }
    forwarded.and_then(|v| v.rsplit(',').next()).and_then(|v| v.trim().parse().ok()).unwrap_or(peer)
}

#[cfg(test)]
mod ip_tests {
    use super::*;

    #[test]
    fn forwarded_for_only_counts_behind_a_trusted_proxy_and_only_its_last_entry() {
        let peer: IpAddr = "10.0.0.2".parse().unwrap();
        let header = Some("6.6.6.6, 203.0.113.9");
        assert_eq!(pick_ip(false, header, peer), peer);
        assert_eq!(pick_ip(true, header, peer), "203.0.113.9".parse::<IpAddr>().unwrap());
        assert_eq!(pick_ip(true, None, peer), peer);
        assert_eq!(pick_ip(true, Some("garbage"), peer), peer);
    }
}
