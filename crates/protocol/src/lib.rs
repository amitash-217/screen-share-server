//! Wire types shared by the server and the desktop client.

use serde::{Deserialize, Serialize};

pub const MAX_ROOMS_PER_USER: usize = 3;
pub const MAX_ROOM_SIZE: usize = 4;
pub const ROOM_CODE_LEN: usize = 5;

/// Five lowercase ASCII letters, e.g. `kqzvm`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RoomCode(String);

impl RoomCode {
    pub fn parse(s: &str) -> Option<Self> {
        let ok = s.len() == ROOM_CODE_LEN && s.bytes().all(|b| b.is_ascii_lowercase());
        ok.then(|| Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RoomCode {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Self::parse(&s).ok_or_else(|| format!("invalid room code: {s:?}"))
    }
}

impl From<RoomCode> for String {
    fn from(c: RoomCode) -> String {
        c.0
    }
}

pub type UserId = i64;

pub const USERNAME_MIN: usize = 3;
pub const USERNAME_MAX: usize = 24;
pub const PASSWORD_MIN: usize = 8;
/// Upper bound keeps argon2 hashing cost bounded for hostile input.
pub const PASSWORD_MAX: usize = 128;

/// Returns the normalized (lowercase) username, or why it is invalid.
pub fn validate_username(s: &str) -> Result<String, &'static str> {
    let s = s.trim().to_ascii_lowercase();
    if !(USERNAME_MIN..=USERNAME_MAX).contains(&s.len()) {
        return Err("username must be 3 to 24 characters");
    }
    if !s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
        return Err("username may only contain letters, digits and underscore");
    }
    Ok(s)
}

pub fn validate_password(s: &str) -> Result<(), &'static str> {
    if !(PASSWORD_MIN..=PASSWORD_MAX).contains(&s.chars().count()) {
        return Err("password must be 8 to 128 characters");
    }
    Ok(())
}

/// Body of `POST /api/signup` and `POST /api/login`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Returned by signup and login. Send `token` as `Authorization: Bearer <token>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub user_id: UserId,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSummary {
    pub code: RoomCode,
    pub host: String,
    pub permanent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub user_id: UserId,
    pub username: String,
}

/// TURN/STUN servers the client should hand to libwebrtc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

/// What clients put in the `payload` of `Signal` to set up a peer connection.
/// The server only relays it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RtcSignal {
    /// `renegotiate` is true when the offer changes an established connection
    /// (a share started or stopped) and false when it opens a fresh one.
    Offer {
        sdp: String,
        #[serde(default)]
        renegotiate: bool,
    },
    Answer { sdp: String },
    Candidate { mid: String, index: i32, sdp: String },
    /// The sender stopped sharing their screen.
    ShareEnded,
    /// The sender is sharing again on a connection that already carried a share.
    ShareStarted,
    /// Asks the side that opened the connection to offer again, because the sender changed what
    /// it sends (a share started or stopped). Only that side ever offers, so offers never cross.
    Renegotiate,
}

/// Client to server, sent over the authenticated WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    CreateRoom { permanent: bool },
    DeleteRoom { code: RoomCode },
    RequestJoin { code: RoomCode },
    DecideJoin { code: RoomCode, user_id: UserId, accept: bool },
    RemoveMember { code: RoomCode, user_id: UserId },
    EnterRoom { code: RoomCode },
    LeaveRoom,
    /// SDP/ICE relay to one peer in the current room.
    Signal { to: UserId, payload: serde_json::Value },
}

/// Server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Rooms { rooms: Vec<RoomSummary> },
    RoomCreated { room: RoomSummary },
    /// The room left this user's list for good: deleted, destroyed, or they were removed.
    RoomClosed { code: RoomCode },
    /// The user was taken out of the live session (the host removed them); the room is gone from their list too.
    RoomEnded { code: RoomCode },
    JoinRequested { code: RoomCode, user: Peer },
    JoinDecided { code: RoomCode, accepted: bool },
    Entered { code: RoomCode, peers: Vec<Peer>, ice: Vec<IceServer> },
    PeerJoined { peer: Peer },
    PeerLeft { user_id: UserId },
    Signal { from: UserId, payload: serde_json::Value },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_code_accepts_five_lowercase_letters() {
        assert!(RoomCode::parse("abcde").is_some());
    }

    #[test]
    fn room_code_rejects_bad_input() {
        for bad in ["", "abcd", "abcdef", "ABCDE", "abc1e", "abcd "] {
            assert!(RoomCode::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn username_is_normalized_and_checked() {
        assert_eq!(validate_username("  Bob_1 ").unwrap(), "bob_1");
        for bad in ["ab", "has space", "dash-ed", &"x".repeat(25)] {
            assert!(validate_username(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn password_length_is_bounded() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("longenough").is_ok());
        assert!(validate_password(&"x".repeat(129)).is_err());
    }

    #[test]
    fn rtc_signals_round_trip_inside_a_signal_message() {
        let sig = RtcSignal::Candidate { mid: "0".into(), index: 0, sdp: "candidate:1 1 UDP".into() };
        let msg = ClientMsg::Signal { to: 7, payload: serde_json::to_value(&sig).unwrap() };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMsg = serde_json::from_str(&json).unwrap();
        let ClientMsg::Signal { payload, .. } = back else { panic!("wrong variant") };
        assert_eq!(serde_json::from_value::<RtcSignal>(payload).unwrap(), sig);
        assert!(serde_json::from_str::<RtcSignal>(r#"{"kind":"bogus"}"#).is_err());
        // Offers from before renegotiation existed still parse as fresh connections.
        let old: RtcSignal = serde_json::from_str(r#"{"kind":"offer","sdp":"v=0"}"#).unwrap();
        assert_eq!(old, RtcSignal::Offer { sdp: "v=0".into(), renegotiate: false });
        let ended = serde_json::to_value(RtcSignal::ShareEnded).unwrap();
        assert_eq!(serde_json::from_value::<RtcSignal>(ended).unwrap(), RtcSignal::ShareEnded);
        let again = serde_json::to_value(RtcSignal::ShareStarted).unwrap();
        assert_eq!(serde_json::from_value::<RtcSignal>(again).unwrap(), RtcSignal::ShareStarted);
    }

    #[test]
    fn messages_round_trip() {
        let msg = ClientMsg::RequestJoin { code: RoomCode::parse("abcde").unwrap() };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"request_join","code":"abcde"}"#);
        assert!(serde_json::from_str::<ClientMsg>(&json).is_ok());
        assert!(serde_json::from_str::<ClientMsg>(r#"{"type":"request_join","code":"NOPE"}"#).is_err());
    }
}
