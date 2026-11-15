//! End-to-end tests: a real server on a local port, real WebSocket clients.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMsg, RoomCode, RoomSummary, ServerMsg, UserId};
use serde_json::json;
use server::api::{router, AppState};
use server::turn::TurnConfig;
use sqlx::PgPool;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Client {
    ws: Ws,
    id: UserId,
    /// The room list every connection starts with.
    first_rooms: Vec<RoomSummary>,
}

impl Client {
    async fn send(&mut self, msg: ClientMsg) {
        self.ws.send(Message::Text(serde_json::to_string(&msg).unwrap().into())).await.unwrap();
    }

    /// Next server message, or `None` once the server closed the socket.
    async fn next(&mut self) -> Option<ServerMsg> {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(3), self.ws.next())
                .await
                .expect("timed out waiting for the server");
            match frame {
                Some(Ok(Message::Text(t))) => return Some(serde_json::from_str(t.as_str()).unwrap()),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
                Some(Ok(_)) => {}
            }
        }
    }

    /// Skips messages until one matches.
    async fn expect(&mut self, pred: impl Fn(&ServerMsg) -> bool) -> ServerMsg {
        loop {
            let msg = self.next().await.expect("connection closed while waiting");
            if pred(&msg) {
                return msg;
            }
        }
    }

    async fn error(&mut self) -> String {
        match self.expect(|m| matches!(m, ServerMsg::Error { .. })).await {
            ServerMsg::Error { message } => message,
            _ => unreachable!(),
        }
    }

    async fn create(&mut self, permanent: bool) -> RoomCode {
        self.send(ClientMsg::CreateRoom { permanent }).await;
        match self.expect(|m| matches!(m, ServerMsg::RoomCreated { .. })).await {
            ServerMsg::RoomCreated { room } => room.code,
            _ => unreachable!(),
        }
    }

    async fn enter(&mut self, code: &RoomCode) -> ServerMsg {
        self.send(ClientMsg::EnterRoom { code: code.clone() }).await;
        self.expect(|m| matches!(m, ServerMsg::Entered { .. })).await
    }
}

async fn serve(state: AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn spawn(pool: &PgPool) -> SocketAddr {
    serve(AppState::new(pool.clone(), true)).await
}

/// Inserts a user and a live session directly, skipping the slow password hash.
async fn make_user(pool: &PgPool, name: &str) -> (UserId, String) {
    let (id,): (i64,) = sqlx::query_as("INSERT INTO users (username, password_hash) VALUES ($1, 'x') RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    let token = server::auth::new_token();
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '1 day')")
        .bind(server::auth::token_hash(&token))
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    (id, token)
}

async fn add_member(pool: &PgPool, code: &RoomCode, user: UserId) {
    sqlx::query("INSERT INTO room_members (room_code, user_id) VALUES ($1, $2)")
        .bind(code.as_str())
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
}

async fn room_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM rooms").fetch_one(pool).await.unwrap()
}

async fn connect(addr: SocketAddr, user: &(UserId, String)) -> Client {
    let mut req = format!("ws://{addr}/ws").into_client_request().unwrap();
    req.headers_mut().insert("authorization", format!("Bearer {}", user.1).parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let mut client = Client { ws, id: user.0, first_rooms: vec![] };
    if let ServerMsg::Rooms { rooms } = client.expect(|m| matches!(m, ServerMsg::Rooms { .. })).await {
        client.first_rooms = rooms;
    }
    client
}

#[sqlx::test]
async fn join_flow_from_request_to_signal(pool: PgPool) {
    let (host_u, guest_u) = (make_user(&pool, "host").await, make_user(&pool, "guest").await);
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let mut guest = connect(addr, &guest_u).await;
    let code = host.create(false).await;

    guest.send(ClientMsg::RequestJoin { code: code.clone() }).await;
    match host.expect(|m| matches!(m, ServerMsg::JoinRequested { .. })).await {
        ServerMsg::JoinRequested { code: c, user } => {
            assert_eq!(c, code);
            assert_eq!((user.user_id, user.username.as_str()), (guest.id, "guest"));
        }
        _ => unreachable!(),
    }

    host.send(ClientMsg::DecideJoin { code: code.clone(), user_id: guest.id, accept: true }).await;
    guest.expect(|m| matches!(m, ServerMsg::JoinDecided { accepted: true, .. })).await;
    match guest.expect(|m| matches!(m, ServerMsg::Rooms { .. })).await {
        ServerMsg::Rooms { rooms } => {
            assert_eq!(rooms.len(), 1);
            assert_eq!(rooms[0].code, code);
            assert!(!rooms[0].permanent);
        }
        _ => unreachable!(),
    }

    match host.enter(&code).await {
        ServerMsg::Entered { peers, .. } => assert!(peers.is_empty()),
        _ => unreachable!(),
    }
    match guest.enter(&code).await {
        ServerMsg::Entered { peers, .. } => {
            assert_eq!(peers.len(), 1);
            assert_eq!(peers[0].user_id, host.id);
        }
        _ => unreachable!(),
    }
    match host.expect(|m| matches!(m, ServerMsg::PeerJoined { .. })).await {
        ServerMsg::PeerJoined { peer } => assert_eq!(peer.user_id, guest.id),
        _ => unreachable!(),
    }

    guest.send(ClientMsg::Signal { to: host.id, payload: json!({ "sdp": "offer" }) }).await;
    match host.expect(|m| matches!(m, ServerMsg::Signal { .. })).await {
        ServerMsg::Signal { from, payload } => {
            assert_eq!(from, guest.id);
            assert_eq!(payload["sdp"], "offer");
        }
        _ => unreachable!(),
    }

    guest.send(ClientMsg::LeaveRoom).await;
    match host.expect(|m| matches!(m, ServerMsg::PeerLeft { .. })).await {
        ServerMsg::PeerLeft { user_id } => assert_eq!(user_id, guest.id),
        _ => unreachable!(),
    }
}

#[sqlx::test]
async fn accepted_member_rejoins_without_asking_again(pool: PgPool) {
    let (host_u, guest_u) = (make_user(&pool, "host").await, make_user(&pool, "guest").await);
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let code = host.create(false).await;
    add_member(&pool, &code, guest_u.0).await;
    // The host stays inside, so the temporary room is not empty when the guest drops.
    host.enter(&code).await;

    let mut guest = connect(addr, &guest_u).await;
    assert_eq!(guest.first_rooms.len(), 1);
    assert_eq!(guest.first_rooms[0].code, code);
    guest.enter(&code).await;
    drop(guest);

    let mut guest = connect(addr, &guest_u).await;
    assert_eq!(guest.first_rooms.len(), 1);
    guest.enter(&code).await;
}

#[sqlx::test]
async fn room_limits(pool: PgPool) {
    let (a, b) = (make_user(&pool, "aaa").await, make_user(&pool, "bbb").await);
    let addr = spawn(&pool).await;

    let mut host = connect(addr, &a).await;
    host.create(true).await;
    host.create(false).await;
    host.create(false).await;
    host.send(ClientMsg::CreateRoom { permanent: false }).await;
    assert!(host.error().await.contains("at most 3"));

    let mut other = connect(addr, &b).await;
    other.create(true).await;
    other.send(ClientMsg::CreateRoom { permanent: true }).await;
    assert!(other.error().await.contains("permanent"));
}

#[sqlx::test]
async fn temporary_room_dies_with_its_host(pool: PgPool) {
    let (host_u, guest_u) = (make_user(&pool, "host").await, make_user(&pool, "guest").await);
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let code = host.create(false).await;
    add_member(&pool, &code, guest_u.0).await;
    let mut guest = connect(addr, &guest_u).await;
    guest.enter(&code).await;

    drop(host);
    match guest.expect(|m| matches!(m, ServerMsg::RoomClosed { .. })).await {
        ServerMsg::RoomClosed { code: c } => assert_eq!(c, code),
        _ => unreachable!(),
    }
    assert_eq!(room_count(&pool).await, 0);
}

#[sqlx::test]
async fn permanent_room_keeps_running_without_its_host(pool: PgPool) {
    let host_u = make_user(&pool, "host").await;
    let (first_u, second_u) = (make_user(&pool, "first").await, make_user(&pool, "second").await);
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let code = host.create(true).await;
    add_member(&pool, &code, first_u.0).await;
    add_member(&pool, &code, second_u.0).await;
    let mut first = connect(addr, &first_u).await;
    first.enter(&code).await;

    // Host leaves: nobody is kicked and the room stays.
    drop(host);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(room_count(&pool).await, 1);

    // A member can still join, even though the host has never been inside.
    let mut second = connect(addr, &second_u).await;
    match second.enter(&code).await {
        ServerMsg::Entered { peers, .. } => assert_eq!(peers[0].user_id, first.id),
        _ => unreachable!(),
    }
    first.expect(|m| matches!(m, ServerMsg::PeerJoined { .. })).await;

    // A newcomer's request waits for the host, as before.
    let stranger_u = make_user(&pool, "stranger").await;
    let mut stranger = connect(addr, &stranger_u).await;
    stranger.send(ClientMsg::RequestJoin { code }).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut host = connect(addr, &host_u).await;
    host.expect(|m| matches!(m, ServerMsg::JoinRequested { .. })).await;
}

#[sqlx::test]
async fn temporary_room_is_destroyed_when_the_last_person_leaves(pool: PgPool) {
    let host_u = make_user(&pool, "host").await;
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let code = host.create(false).await;
    host.enter(&code).await;
    assert_eq!(room_count(&pool).await, 1);

    host.send(ClientMsg::LeaveRoom).await;
    host.expect(|m| matches!(m, ServerMsg::RoomClosed { .. })).await;
    assert_eq!(room_count(&pool).await, 0);
}

#[sqlx::test]
async fn permissions_and_denials(pool: PgPool) {
    let (host_u, guest_u, stranger_u) =
        (make_user(&pool, "host").await, make_user(&pool, "guest").await, make_user(&pool, "stranger").await);
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let mut guest = connect(addr, &guest_u).await;
    let mut stranger = connect(addr, &stranger_u).await;
    let code = host.create(true).await;

    stranger.send(ClientMsg::EnterRoom { code: code.clone() }).await;
    assert!(stranger.error().await.contains("no access"));
    stranger.send(ClientMsg::DecideJoin { code: code.clone(), user_id: guest.id, accept: true }).await;
    assert!(stranger.error().await.contains("only the host"));
    stranger.send(ClientMsg::DeleteRoom { code: code.clone() }).await;
    assert!(stranger.error().await.contains("only the host"));
    stranger.send(ClientMsg::RequestJoin { code: RoomCode::parse("zzzzz").unwrap() }).await;
    assert!(stranger.error().await.contains("not found"));
    host.send(ClientMsg::RequestJoin { code: code.clone() }).await;
    assert!(host.error().await.contains("you host"));

    guest.send(ClientMsg::RequestJoin { code: code.clone() }).await;
    host.expect(|m| matches!(m, ServerMsg::JoinRequested { .. })).await;
    host.send(ClientMsg::DecideJoin { code: code.clone(), user_id: guest.id, accept: false }).await;
    guest.expect(|m| matches!(m, ServerMsg::JoinDecided { accepted: false, .. })).await;
    guest.send(ClientMsg::EnterRoom { code: code.clone() }).await;
    assert!(guest.error().await.contains("no access"));

    host.send(ClientMsg::DecideJoin { code, user_id: guest.id, accept: true }).await;
    assert!(host.error().await.contains("no such"));
}

#[sqlx::test]
async fn removed_member_is_kicked_and_loses_access(pool: PgPool) {
    let (host_u, guest_u) = (make_user(&pool, "host").await, make_user(&pool, "guest").await);
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let code = host.create(true).await;
    add_member(&pool, &code, guest_u.0).await;
    let mut guest = connect(addr, &guest_u).await;
    guest.enter(&code).await;

    host.send(ClientMsg::RemoveMember { code: code.clone(), user_id: guest.id }).await;
    guest.expect(|m| matches!(m, ServerMsg::RoomEnded { .. })).await;
    guest.expect(|m| matches!(m, ServerMsg::RoomClosed { .. })).await;
    guest.send(ClientMsg::EnterRoom { code }).await;
    assert!(guest.error().await.contains("no access"));
}

#[sqlx::test]
async fn deleting_a_room_tells_everyone(pool: PgPool) {
    let (host_u, guest_u) = (make_user(&pool, "host").await, make_user(&pool, "guest").await);
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let code = host.create(true).await;
    add_member(&pool, &code, guest_u.0).await;
    let mut guest = connect(addr, &guest_u).await;
    guest.enter(&code).await;

    host.send(ClientMsg::DeleteRoom { code: code.clone() }).await;
    guest.expect(|m| matches!(m, ServerMsg::RoomClosed { .. })).await;
    assert_eq!(room_count(&pool).await, 0);

    // Deleting freed the host's permanent slot.
    host.create(true).await;
}

#[sqlx::test]
async fn room_holds_at_most_four_people(pool: PgPool) {
    let host_u = make_user(&pool, "host").await;
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let code = host.create(true).await;

    let mut guests = Vec::new();
    for i in 0..5 {
        let u = make_user(&pool, &format!("guest{i}")).await;
        add_member(&pool, &code, u.0).await;
        guests.push(connect(addr, &u).await);
    }
    for guest in guests.iter_mut().take(4) {
        guest.enter(&code).await;
    }
    guests[4].send(ClientMsg::EnterRoom { code }).await;
    assert!(guests[4].error().await.contains("full"));
}

#[sqlx::test]
async fn new_connection_replaces_the_old_one(pool: PgPool) {
    let u = make_user(&pool, "solo").await;
    let addr = spawn(&pool).await;
    let mut first = connect(addr, &u).await;
    let mut second = connect(addr, &u).await;
    assert!(first.next().await.is_none(), "old socket should be closed");
    second.create(false).await;
}

#[sqlx::test]
async fn host_sees_requests_made_while_offline(pool: PgPool) {
    let (host_u, guest_u) = (make_user(&pool, "host").await, make_user(&pool, "guest").await);
    let addr = spawn(&pool).await;
    let mut host = connect(addr, &host_u).await;
    let code = host.create(true).await;
    drop(host);

    let mut guest = connect(addr, &guest_u).await;
    guest.send(ClientMsg::RequestJoin { code: code.clone() }).await;
    // Give the server a moment to store the request before the host returns.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut host = connect(addr, &host_u).await;
    match host.expect(|m| matches!(m, ServerMsg::JoinRequested { .. })).await {
        ServerMsg::JoinRequested { code: c, user } => {
            assert_eq!(c, code);
            assert_eq!(user.user_id, guest.id);
        }
        _ => unreachable!(),
    }
}

#[sqlx::test]
async fn join_requests_are_rate_limited(pool: PgPool) {
    let u = make_user(&pool, "spammer").await;
    let addr = spawn(&pool).await;
    let mut c = connect(addr, &u).await;
    for _ in 0..10 {
        c.send(ClientMsg::RequestJoin { code: RoomCode::parse("zzzzz").unwrap() }).await;
        assert!(c.error().await.contains("not found"));
    }
    c.send(ClientMsg::RequestJoin { code: RoomCode::parse("zzzzz").unwrap() }).await;
    assert!(c.error().await.contains("slow down"));
}

#[sqlx::test]
async fn entering_a_room_returns_turn_credentials(pool: PgPool) {
    let u = make_user(&pool, "host").await;
    let state = AppState::new(pool.clone(), true).with_turn(TurnConfig { secret: "s".into(), host: "turn.test".into() });
    let addr = serve(state).await;
    let mut host = connect(addr, &u).await;
    let code = host.create(false).await;
    match host.enter(&code).await {
        ServerMsg::Entered { ice, .. } => {
            assert_eq!(ice[0].urls[0], "stun:turn.test:3478");
            assert!(ice[0].username.as_deref().unwrap().ends_with(&format!(":{}", host.id)));
            assert!(ice[0].credential.is_some());
        }
        _ => unreachable!(),
    }
}

#[sqlx::test]
async fn bad_input_and_missing_auth(pool: PgPool) {
    let u = make_user(&pool, "host").await;
    let addr = spawn(&pool).await;

    let no_auth = format!("ws://{addr}/ws").into_client_request().unwrap();
    assert!(tokio_tungstenite::connect_async(no_auth).await.is_err());

    let mut c = connect(addr, &u).await;
    c.ws.send(Message::Text("not json".into())).await.unwrap();
    assert!(c.error().await.contains("malformed"));
    c.ws.send(Message::Text(r#"{"type":"enter_room","code":"TOOLONG"}"#.into())).await.unwrap();
    assert!(c.error().await.contains("malformed"));
}
