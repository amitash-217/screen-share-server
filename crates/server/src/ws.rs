//! WebSocket endpoint: one connection per signed-in user.
//!
//! A user is "online" while connected. A temporary room is destroyed when its
//! host disconnects or when its last participant leaves. A permanent room keeps
//! running without its host; only accepting new members needs the host online.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMsg, Peer, RoomCode, RoomSummary, ServerMsg, UserId};
use tokio::sync::mpsc;

use crate::api::{AppState, AuthUser};
use crate::hub::RoomFull;
use crate::rooms::{self, CreateError, Decision, RequestOutcome, RoomRow};

const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const OUTBOX_LEN: usize = 256;

pub async fn ws_handler(State(state): State<AppState>, user: AuthUser, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_MESSAGE_BYTES).on_upgrade(move |socket| run(socket, state, user))
}

/// A failure shown to the client, or an internal error that is only logged.
enum Fail {
    User(&'static str),
    Internal,
}

impl From<sqlx::Error> for Fail {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("database error: {e}");
        Fail::Internal
    }
}

type R<T = ()> = Result<T, Fail>;

async fn run(socket: WebSocket, state: AppState, user: AuthUser) {
    let uid = user.user_id;
    let (tx, mut rx) = mpsc::channel::<ServerMsg>(OUTBOX_LEN);

    // A second login replaces the first: close the old socket and run its disconnect rules.
    if state.hub.take(uid) {
        cleanup(&state, uid).await;
    }
    let conn_id = state.hub.insert(uid, &user.username, tx);

    let (mut sink, mut stream) = socket.split();
    let mut writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(json) = serde_json::to_string(&msg) else { continue };
            if sink.send(Message::Text(json.into())).await.is_err() {
                return;
            }
        }
        let _ = sink.close().await;
    });

    if announce_online(&state, &user).await.is_err() {
        state.hub.send(uid, ServerMsg::Error { message: "internal error".into() });
    }

    loop {
        tokio::select! {
            _ = &mut writer => break,
            msg = stream.next() => match msg {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(text.as_str()) {
                    Ok(msg) => {
                        if let Err(fail) = handle(&state, &user, msg).await {
                            let message = match fail {
                                Fail::User(m) => m,
                                Fail::Internal => "internal error",
                            };
                            state.hub.send(uid, ServerMsg::Error { message: message.into() });
                        }
                    }
                    Err(_) => state.hub.send(uid, ServerMsg::Error { message: "malformed message".into() }),
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            }
        }
    }

    writer.abort();
    if state.hub.remove_if(uid, conn_id) {
        cleanup(&state, uid).await;
    }
}

fn summary(row: &RoomRow) -> RoomSummary {
    RoomSummary { code: row.code.clone(), host: row.host.clone(), permanent: row.permanent }
}

async fn send_room_list(state: &AppState, uid: UserId) -> R {
    let rooms = rooms::rooms_for_user(&state.pool, uid).await?;
    let rooms = rooms.iter().map(summary).collect();
    state.hub.send(uid, ServerMsg::Rooms { rooms });
    Ok(())
}

/// First messages on a new connection: the room list and waiting join requests.
async fn announce_online(state: &AppState, user: &AuthUser) -> R {
    let uid = user.user_id;
    send_room_list(state, uid).await?;
    for (code, peer) in rooms::pending_for_host(&state.pool, uid).await? {
        state.hub.send(uid, ServerMsg::JoinRequested { code, user: peer });
    }
    Ok(())
}

async fn handle(state: &AppState, user: &AuthUser, msg: ClientMsg) -> R {
    let uid = user.user_id;
    match msg {
        ClientMsg::CreateRoom { permanent } => {
            let code = match rooms::create_room(&state.pool, uid, permanent).await {
                Ok(code) => code,
                Err(CreateError::TooManyRooms) => return Err(Fail::User("you can host at most 3 rooms")),
                Err(CreateError::PermanentExists) => return Err(Fail::User("you already have a permanent room")),
                Err(CreateError::Db(e)) => return Err(e.into()),
            };
            let room = RoomSummary { code, host: user.username.clone(), permanent };
            state.hub.send(uid, ServerMsg::RoomCreated { room });
        }
        ClientMsg::DeleteRoom { code } => {
            let Some(audience) = rooms::delete_room(&state.pool, &code, uid).await? else {
                return Err(Fail::User("only the host can delete a room"));
            };
            state.hub.clear_room(&code);
            state.hub.send_many(audience, &ServerMsg::RoomClosed { code });
        }
        ClientMsg::RequestJoin { code } => request_join(state, user, code).await?,
        ClientMsg::DecideJoin { code, user_id, accept } => {
            match rooms::decide(&state.pool, &code, uid, user_id, accept).await? {
                Decision::NotHost => return Err(Fail::User("only the host can decide join requests")),
                Decision::NoRequest => return Err(Fail::User("no such join request")),
                Decision::Done => {}
            }
            state.hub.send(user_id, ServerMsg::JoinDecided { code, accepted: accept });
            if accept {
                send_room_list(state, user_id).await?;
            }
        }
        ClientMsg::RemoveMember { code, user_id } => {
            if user_id == uid {
                return Err(Fail::User("the host cannot remove themselves"));
            }
            if !rooms::remove_member(&state.pool, &code, uid, user_id).await? {
                return Err(Fail::User("not a member of your room"));
            }
            if state.hub.room_of(user_id).as_ref() == Some(&code) {
                leave_room(state, user_id).await?;
                state.hub.send(user_id, ServerMsg::RoomEnded { code: code.clone() });
            }
            state.hub.send(user_id, ServerMsg::RoomClosed { code });
        }
        ClientMsg::EnterRoom { code } => enter_room(state, user, code).await?,
        ClientMsg::LeaveRoom => leave_room(state, uid).await?,
        ClientMsg::Signal { to, payload } => {
            let mine = state.hub.room_of(uid);
            if to == uid || mine.is_none() || mine != state.hub.room_of(to) {
                return Err(Fail::User("that peer is not in your room"));
            }
            state.hub.send(to, ServerMsg::Signal { from: uid, payload });
        }
    }
    Ok(())
}

async fn request_join(state: &AppState, user: &AuthUser, code: RoomCode) -> R {
    let key = user.user_id.to_string();
    if state.limiters.join_request_user.blocked(&key) {
        return Err(Fail::User("too many join requests, slow down"));
    }
    state.limiters.join_request_user.record(&key);

    match rooms::add_request(&state.pool, &code, user.user_id).await? {
        RequestOutcome::NoRoom => Err(Fail::User("room not found")),
        RequestOutcome::OwnRoom => Err(Fail::User("you host this room")),
        RequestOutcome::RoomQueueFull => Err(Fail::User("that room has too many pending requests")),
        RequestOutcome::UserQueueFull => Err(Fail::User("you have too many pending requests")),
        RequestOutcome::AlreadyMember => {
            state.hub.send(user.user_id, ServerMsg::JoinDecided { code, accepted: true });
            Ok(())
        }
        RequestOutcome::Queued { host_id } => {
            let peer = Peer { user_id: user.user_id, username: user.username.clone() };
            state.hub.send(host_id, ServerMsg::JoinRequested { code, user: peer });
            Ok(())
        }
    }
}

/// Joins the live session of a room. Needs membership; a temporary room also
/// needs its host online, though one without its host is already destroyed.
async fn enter_room(state: &AppState, user: &AuthUser, code: RoomCode) -> R {
    let uid = user.user_id;
    let Some(room) = rooms::room_access(&state.pool, &code, uid).await? else {
        return Err(Fail::User("no access to that room"));
    };
    if !room.permanent && room.host_id != uid && !state.hub.is_online(room.host_id) {
        return Err(Fail::User("the host is offline"));
    }
    if state.hub.room_of(uid).as_ref() == Some(&code) {
        return Err(Fail::User("you are already in this room"));
    }
    leave_room(state, uid).await?;

    let existing = match state.hub.join(&code, uid) {
        Ok(peers) => peers,
        Err(RoomFull) => return Err(Fail::User("the room is full")),
    };
    let me = Peer { user_id: uid, username: user.username.clone() };
    state.hub.send_many(existing.iter().map(|p| p.user_id), &ServerMsg::PeerJoined { peer: me });
    state.hub.send(uid, ServerMsg::Entered { code, peers: existing, ice: state.ice_servers(uid) });
    Ok(())
}

/// Takes the user out of their current room, if any. A temporary room that
/// becomes empty is destroyed.
async fn leave_room(state: &AppState, uid: UserId) -> R {
    let Some((code, remaining)) = state.hub.leave(uid) else { return Ok(()) };
    state.hub.send_many(remaining.iter().copied(), &ServerMsg::PeerLeft { user_id: uid });
    if remaining.is_empty() {
        if let Some(audience) = rooms::delete_temporary(&state.pool, &code).await? {
            state.hub.send_many(audience, &ServerMsg::RoomClosed { code });
        }
    }
    Ok(())
}

async fn cleanup(state: &AppState, uid: UserId) {
    if let Err(Fail::Internal) | Err(Fail::User(_)) = disconnect(state, uid).await {
        tracing::warn!(user = uid, "disconnect cleanup did not finish");
    }
}

/// The host going away destroys the temporary rooms they host. Permanent rooms
/// keep running without them.
async fn disconnect(state: &AppState, uid: UserId) -> R {
    leave_room(state, uid).await?;
    for (code, permanent) in rooms::hosted_rooms(&state.pool, uid).await? {
        if permanent {
            continue;
        }
        state.hub.clear_room(&code);
        if let Some(audience) = rooms::delete_temporary(&state.pool, &code).await? {
            state.hub.send_many(audience, &ServerMsg::RoomClosed { code });
        }
    }
    Ok(())
}
