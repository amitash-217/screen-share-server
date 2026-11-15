use std::net::SocketAddr;

use axum::body::{to_bytes, Body};
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use server::api::{router, AppState};
use sqlx::PgPool;
use tower::ServiceExt;

fn app(pool: PgPool, signup_enabled: bool) -> Router {
    router(AppState::new(pool, signup_enabled)).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4000))))
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>, token: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn creds(u: &str, p: &str) -> Option<Value> {
    Some(json!({ "username": u, "password": p }))
}

#[sqlx::test]
async fn signup_then_me_then_logout(pool: PgPool) {
    let app = app(pool, true);
    let (status, body) = call(&app, "POST", "/api/signup", creds("Alice", "password1"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "alice");
    let token = body["token"].as_str().unwrap().to_owned();

    let (status, body) = call(&app, "GET", "/api/me", None, Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "alice");

    let (status, _) = call(&app, "POST", "/api/logout", None, Some(&token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, "GET", "/api/me", None, Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn me_rejects_missing_and_bogus_token(pool: PgPool) {
    let app = app(pool, true);
    assert_eq!(call(&app, "GET", "/api/me", None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, "GET", "/api/me", None, Some("nope")).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn duplicate_username_is_case_insensitive(pool: PgPool) {
    let app = app(pool, true);
    assert_eq!(call(&app, "POST", "/api/signup", creds("bob_1", "password1"), None).await.0, StatusCode::OK);
    let (status, _) = call(&app, "POST", "/api/signup", creds("BOB_1", "password2"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn signup_validates_input(pool: PgPool) {
    let app = app(pool, true);
    assert_eq!(call(&app, "POST", "/api/signup", creds("ab", "password1"), None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(call(&app, "POST", "/api/signup", creds("carol", "short"), None).await.0, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn signup_can_be_closed(pool: PgPool) {
    let app = app(pool, false);
    let (status, _) = call(&app, "POST", "/api/signup", creds("dave", "password1"), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn login_works_and_wrong_credentials_look_identical(pool: PgPool) {
    let app = app(pool, true);
    call(&app, "POST", "/api/signup", creds("erin", "password1"), None).await;

    let (status, body) = call(&app, "POST", "/api/login", creds("ERIN", "password1"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["token"].as_str().is_some());

    let (s1, b1) = call(&app, "POST", "/api/login", creds("erin", "wrongpass"), None).await;
    let (s2, b2) = call(&app, "POST", "/api/login", creds("nobody", "wrongpass"), None).await;
    assert_eq!((s1, s2), (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED));
    assert_eq!(b1, b2);
}

#[sqlx::test]
async fn repeated_failures_lock_the_username(pool: PgPool) {
    let app = app(pool, true);
    call(&app, "POST", "/api/signup", creds("frank", "password1"), None).await;
    for _ in 0..5 {
        let (status, _) = call(&app, "POST", "/api/login", creds("frank", "wrongpass"), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // Even the right password is refused while locked.
    let (status, _) = call(&app, "POST", "/api/login", creds("frank", "password1"), None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test]
async fn expired_session_is_rejected(pool: PgPool) {
    let app = app(pool.clone(), true);
    let (_, body) = call(&app, "POST", "/api/signup", creds("gina", "password1"), None).await;
    let token = body["token"].as_str().unwrap();
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'").execute(&pool).await.unwrap();
    assert_eq!(call(&app, "GET", "/api/me", None, Some(token)).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn admin_create_and_reset(pool: PgPool) {
    server::admin::create_user(&pool, "hank", "password1").await.unwrap();
    assert!(server::admin::create_user(&pool, "hank", "password1").await.is_err());

    let app = app(pool.clone(), false);
    let (_, body) = call(&app, "POST", "/api/login", creds("hank", "password1"), None).await;
    let old_token = body["token"].as_str().unwrap().to_owned();

    server::admin::reset_password(&pool, "hank", "newpassword").await.unwrap();
    assert_eq!(call(&app, "GET", "/api/me", None, Some(&old_token)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, "POST", "/api/login", creds("hank", "password1"), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, "POST", "/api/login", creds("hank", "newpassword"), None).await.0, StatusCode::OK);
    assert!(server::admin::reset_password(&pool, "ghost", "newpassword").await.is_err());
}
