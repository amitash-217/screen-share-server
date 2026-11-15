//! Client networking against a real server. Needs a Postgres in `DATABASE_URL`;
//! without one each test prints a note and passes.

use std::net::SocketAddr;
use std::time::Duration;

use client::addr::ServerAddr;
use client::net::{self, NetEvent};
use protocol::{ClientMsg, Credentials, ServerMsg};
use server::api::{router, AppState};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

async fn start_server() -> Option<ServerAddr> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set: skipping");
        return None;
    };
    let pool = sqlx::PgPool::connect(&url).await.expect("connect to test database");
    sqlx::migrate!("../server/migrations").run(&pool).await.expect("migrate");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(AppState::new(pool, true)).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some(ServerAddr::parse(&addr.to_string(), false).unwrap())
}

/// Usernames must be unique in a shared database and at most 24 characters.
fn unique_name() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("t{nanos}")
}

fn creds(name: &str, password: &str) -> Credentials {
    Credentials { username: name.into(), password: password.into() }
}

async fn next_event(rx: &mut UnboundedReceiver<NetEvent>) -> NetEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("timed out").expect("link ended")
}

async fn wait_for(rx: &mut UnboundedReceiver<NetEvent>, pred: impl Fn(&NetEvent) -> bool) -> NetEvent {
    loop {
        let ev = next_event(rx).await;
        if pred(&ev) {
            return ev;
        }
    }
}

#[tokio::test]
async fn ping_reaches_a_server_and_fails_on_a_dead_port() {
    let Some(addr) = start_server().await else { return };
    net::ping(&addr).await.unwrap();

    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = ServerAddr::parse(&dead.local_addr().unwrap().to_string(), false).unwrap();
    drop(dead);
    let err = net::ping(&dead_addr).await.unwrap_err();
    assert!(err.contains("could not connect"), "{err}");
}

#[tokio::test]
async fn signup_login_and_readable_errors() {
    let Some(addr) = start_server().await else { return };
    let name = unique_name();

    let created = net::authenticate(&addr, true, &creds(&name, "password1")).await.unwrap();
    assert_eq!(created.username, name);
    let again = net::authenticate(&addr, true, &creds(&name, "password1")).await.unwrap_err();
    assert!(again.contains("taken"), "{again}");

    let session = net::authenticate(&addr, false, &creds(&name, "password1")).await.unwrap();
    assert_eq!(session.user_id, created.user_id);
    let wrong = net::authenticate(&addr, false, &creds(&name, "not-it-at-all")).await.unwrap_err();
    assert!(wrong.contains("invalid username or password"), "{wrong}");
}

#[tokio::test]
async fn websocket_delivers_rooms_and_replies() {
    let Some(addr) = start_server().await else { return };
    let session = net::authenticate(&addr, true, &creds(&unique_name(), "password1")).await.unwrap();

    let (events, mut rx) = unbounded_channel();
    let ws = net::spawn_ws(addr, session.token, events);
    wait_for(&mut rx, |e| matches!(e, NetEvent::Connected)).await;
    wait_for(&mut rx, |e| matches!(e, NetEvent::Server(ServerMsg::Rooms { .. }))).await;

    ws.send(ClientMsg::CreateRoom { permanent: false }).unwrap();
    match wait_for(&mut rx, |e| matches!(e, NetEvent::Server(ServerMsg::RoomCreated { .. }))).await {
        NetEvent::Server(ServerMsg::RoomCreated { room }) => assert!(!room.permanent),
        _ => unreachable!(),
    }

    // Dropping the sender ends the link and reports it.
    drop(ws);
    wait_for(&mut rx, |e| matches!(e, NetEvent::Disconnected)).await;
}

#[tokio::test]
async fn a_rejected_token_reports_auth_expired_instead_of_retrying() {
    let Some(addr) = start_server().await else { return };
    let (events, mut rx) = unbounded_channel();
    let _ws = net::spawn_ws(addr, "0".repeat(64), events);
    wait_for(&mut rx, |e| matches!(e, NetEvent::AuthExpired)).await;
}
