//! What the screens show, and how server messages change it. No UI types here,
//! so it can be tested without a window.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use protocol::{ClientMsg, IceServer, Peer, RoomCode, RoomSummary, ServerMsg, UserId};

use crate::net::NetEvent;

/// Audio level (0 to 1, as WebRTC reports it) above which someone counts as speaking.
const SPEAKING_LEVEL: f32 = 0.01;
/// How long the speaking indicator stays on after the last loud moment.
const SPEAKING_HOLD: Duration = Duration::from_millis(600);

/// State of the media connection to one other person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Connected,
    Lost,
}

impl LinkState {
    pub fn label(self) -> &'static str {
        match self {
            LinkState::Connecting => "connecting…",
            LinkState::Connected => "connected",
            LinkState::Lost => "connection lost",
        }
    }
}

/// The live session the user is in right now.
#[derive(Debug, Clone)]
pub struct CurrentRoom {
    pub code: RoomCode,
    /// Everyone else in the room.
    pub peers: Vec<Peer>,
    /// TURN/STUN servers for the media connections (used from phase 5).
    pub ice: Vec<IceServer>,
}

#[derive(Debug, Default)]
pub struct AppModel {
    pub me: UserId,
    pub me_name: String,
    pub connected: bool,
    pub rooms: Vec<RoomSummary>,
    /// Join requests waiting on rooms this user hosts.
    pub pending: Vec<(RoomCode, Peer)>,
    pub current: Option<CurrentRoom>,
    /// Whether the user's own microphone is muted. Survives leaving a room.
    pub muted: bool,
    /// Whether the user is sharing their own screen right now.
    pub sharing: bool,
    links: HashMap<UserId, LinkState>,
    /// Other people whose screens are being shown.
    sharers: HashSet<UserId>,
    /// When each person (including the user) last made noise.
    loud: HashMap<UserId, Instant>,
    notice: Option<String>,
}

impl AppModel {
    pub fn new(me: UserId, me_name: String) -> Self {
        Self { me, me_name, ..Self::default() }
    }

    pub fn link_state(&self, user: UserId) -> Option<LinkState> {
        self.links.get(&user).copied()
    }

    pub fn set_link(&mut self, user: UserId, state: LinkState) {
        if self.current.as_ref().is_some_and(|c| c.peers.iter().any(|p| p.user_id == user)) {
            self.links.insert(user, state);
        }
    }

    /// Records a measured audio level. A muted user never counts as speaking.
    pub fn note_level(&mut self, user: UserId, level: f32, now: Instant) {
        if level >= SPEAKING_LEVEL && !(user == self.me && self.muted) {
            self.loud.insert(user, now);
        }
    }

    pub fn is_speaking(&self, user: UserId, now: Instant) -> bool {
        self.loud.get(&user).is_some_and(|t| now.saturating_duration_since(*t) < SPEAKING_HOLD)
    }

    /// Records that `user` started or stopped sharing. Only people in the room count.
    pub fn set_sharer(&mut self, user: UserId, on: bool) {
        if !on {
            self.sharers.remove(&user);
        } else if self.current.as_ref().is_some_and(|c| c.peers.iter().any(|p| p.user_id == user)) {
            self.sharers.insert(user);
        }
    }

    pub fn is_sharer(&self, user: UserId) -> bool {
        self.sharers.contains(&user)
    }

    fn clear_current(&mut self) {
        self.current = None;
        self.sharing = false;
        self.sharers.clear();
        self.links.clear();
        self.loud.clear();
    }

    /// Whether this user hosts the room. Usernames are unique, so the name is enough.
    pub fn is_mine(&self, room: &RoomSummary) -> bool {
        room.host == self.me_name
    }

    pub fn in_own_room(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|c| self.rooms.iter().any(|r| r.code == c.code && self.is_mine(r)))
    }

    /// A one-off message for the status bar; `Some("")` clears it.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    pub fn resolve_pending(&mut self, code: &RoomCode, user: UserId) {
        self.pending.retain(|(c, p)| !(c == code && p.user_id == user));
    }

    pub fn left_room(&mut self) {
        self.clear_current();
    }

    /// Applies one event and returns messages the client should send in reply.
    pub fn apply(&mut self, event: NetEvent) -> Vec<ClientMsg> {
        match event {
            NetEvent::Connected => {
                self.connected = true;
                self.notice = Some(String::new());
            }
            NetEvent::Disconnected => {
                self.connected = false;
                self.clear_current();
                // The server sends the requests again on reconnect.
                self.pending.clear();
                self.notice = Some("Connection lost, reconnecting…".into());
            }
            NetEvent::AuthExpired => self.connected = false,
            NetEvent::Server(msg) => return self.apply_server(msg),
        }
        Vec::new()
    }

    fn apply_server(&mut self, msg: ServerMsg) -> Vec<ClientMsg> {
        match msg {
            ServerMsg::Rooms { rooms } => {
                self.rooms = rooms;
                let gone = self.current.as_ref().is_some_and(|c| !self.rooms.iter().any(|r| r.code == c.code));
                if gone {
                    self.clear_current();
                }
            }
            ServerMsg::RoomCreated { room } => {
                let code = room.code.clone();
                let temporary = !room.permanent;
                if !self.rooms.iter().any(|r| r.code == room.code) {
                    self.rooms.push(room);
                }
                // The server never enters a new room by itself; a temporary room starts in use.
                if temporary {
                    return vec![ClientMsg::EnterRoom { code }];
                }
            }
            ServerMsg::RoomClosed { code } => {
                self.rooms.retain(|r| r.code != code);
                self.pending.retain(|(c, _)| *c != code);
                if self.current.as_ref().is_some_and(|c| c.code == code) {
                    self.clear_current();
                    self.notice = Some(format!("Room {} was closed", code.as_str()));
                }
            }
            ServerMsg::RoomEnded { code } => {
                if self.current.as_ref().is_some_and(|c| c.code == code) {
                    self.clear_current();
                    self.notice = Some(format!("You were removed from room {}", code.as_str()));
                }
            }
            ServerMsg::JoinRequested { code, user } => {
                if !self.pending.iter().any(|(c, p)| *c == code && p.user_id == user.user_id) {
                    self.pending.push((code, user));
                }
            }
            ServerMsg::JoinDecided { code, accepted } => {
                self.notice = Some(if accepted {
                    format!("Your request was accepted: room {} is in your list", code.as_str())
                } else {
                    format!("Your request to join room {} was denied", code.as_str())
                });
            }
            ServerMsg::Entered { code, peers, ice } => {
                self.clear_current();
                self.current = Some(CurrentRoom { code, peers, ice });
            }
            ServerMsg::PeerJoined { peer } => {
                if let Some(cur) = &mut self.current {
                    if peer.user_id != self.me && !cur.peers.iter().any(|p| p.user_id == peer.user_id) {
                        cur.peers.push(peer);
                    }
                }
            }
            ServerMsg::PeerLeft { user_id } => {
                if let Some(cur) = &mut self.current {
                    cur.peers.retain(|p| p.user_id != user_id);
                }
                self.links.remove(&user_id);
                self.loud.remove(&user_id);
                self.sharers.remove(&user_id);
            }
            // Media negotiation is handled by the mesh, not the model.
            ServerMsg::Signal { .. } => {}
            ServerMsg::Error { message } => self.notice = Some(message),
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(s: &str) -> RoomCode {
        RoomCode::parse(s).unwrap()
    }

    fn peer(id: UserId, name: &str) -> Peer {
        Peer { user_id: id, username: name.into() }
    }

    fn room(c: &str, host: &str, permanent: bool) -> RoomSummary {
        RoomSummary { code: code(c), host: host.into(), permanent }
    }

    fn server(m: ServerMsg) -> NetEvent {
        NetEvent::Server(m)
    }

    fn model() -> AppModel {
        AppModel::new(1, "me".into())
    }

    #[test]
    fn a_new_temporary_room_is_entered_but_a_permanent_one_is_not() {
        let mut m = model();
        let out = m.apply(server(ServerMsg::RoomCreated { room: room("aaaaa", "me", false) }));
        assert!(matches!(&out[..], [ClientMsg::EnterRoom { code: c }] if c.as_str() == "aaaaa"));
        let out = m.apply(server(ServerMsg::RoomCreated { room: room("bbbbb", "me", true) }));
        assert!(out.is_empty());
        assert_eq!(m.rooms.len(), 2);
    }

    #[test]
    fn room_list_marks_my_rooms() {
        let mut m = model();
        m.apply(server(ServerMsg::Rooms { rooms: vec![room("aaaaa", "me", true), room("bbbbb", "you", false)] }));
        assert!(m.is_mine(&m.rooms[0]));
        assert!(!m.is_mine(&m.rooms[1]));
    }

    #[test]
    fn entering_tracks_peers() {
        let mut m = model();
        m.apply(server(ServerMsg::Rooms { rooms: vec![room("aaaaa", "me", false)] }));
        m.apply(server(ServerMsg::Entered { code: code("aaaaa"), peers: vec![peer(2, "bob")], ice: vec![] }));
        assert!(m.in_own_room());
        m.apply(server(ServerMsg::PeerJoined { peer: peer(3, "cy") }));
        m.apply(server(ServerMsg::PeerJoined { peer: peer(3, "cy") }));
        m.apply(server(ServerMsg::PeerJoined { peer: peer(1, "me") }));
        assert_eq!(m.current.as_ref().unwrap().peers.len(), 2);
        m.apply(server(ServerMsg::PeerLeft { user_id: 2 }));
        assert_eq!(m.current.as_ref().unwrap().peers[0].user_id, 3);
    }

    #[test]
    fn closing_the_current_room_clears_it_with_a_notice() {
        let mut m = model();
        m.apply(server(ServerMsg::Rooms { rooms: vec![room("aaaaa", "you", false)] }));
        m.apply(server(ServerMsg::Entered { code: code("aaaaa"), peers: vec![], ice: vec![] }));
        m.apply(server(ServerMsg::RoomClosed { code: code("aaaaa") }));
        assert!(m.current.is_none());
        assert!(m.rooms.is_empty());
        assert!(m.take_notice().unwrap().contains("aaaaa"));
        assert!(m.take_notice().is_none());
    }

    #[test]
    fn closing_another_room_keeps_the_current_one() {
        let mut m = model();
        m.apply(server(ServerMsg::Rooms { rooms: vec![room("aaaaa", "you", false), room("bbbbb", "you", true)] }));
        m.apply(server(ServerMsg::Entered { code: code("aaaaa"), peers: vec![], ice: vec![] }));
        m.apply(server(ServerMsg::RoomClosed { code: code("bbbbb") }));
        assert!(m.current.is_some());
        assert!(m.take_notice().is_none());
    }

    #[test]
    fn join_requests_dedupe_and_go_away_when_resolved_or_the_room_closes() {
        let mut m = model();
        let req = || ServerMsg::JoinRequested { code: code("aaaaa"), user: peer(2, "bob") };
        m.apply(server(req()));
        m.apply(server(req()));
        m.apply(server(ServerMsg::JoinRequested { code: code("aaaaa"), user: peer(3, "cy") }));
        assert_eq!(m.pending.len(), 2);
        m.resolve_pending(&code("aaaaa"), 2);
        assert_eq!(m.pending.len(), 1);
        m.apply(server(ServerMsg::RoomClosed { code: code("aaaaa") }));
        assert!(m.pending.is_empty());
    }

    #[test]
    fn removed_from_the_room_ends_the_session_with_a_notice() {
        let mut m = model();
        m.apply(server(ServerMsg::Entered { code: code("aaaaa"), peers: vec![], ice: vec![] }));
        m.apply(server(ServerMsg::RoomEnded { code: code("aaaaa") }));
        assert!(m.current.is_none());
        assert!(m.take_notice().unwrap().contains("removed"));
    }

    #[test]
    fn disconnect_resets_live_state_and_connect_clears_the_notice() {
        let mut m = model();
        m.apply(NetEvent::Connected);
        assert!(m.connected);
        m.apply(server(ServerMsg::Entered { code: code("aaaaa"), peers: vec![], ice: vec![] }));
        m.apply(server(ServerMsg::JoinRequested { code: code("aaaaa"), user: peer(2, "bob") }));
        m.apply(NetEvent::Disconnected);
        assert!(!m.connected && m.current.is_none() && m.pending.is_empty());
        assert!(m.take_notice().unwrap().contains("reconnecting"));
        m.apply(NetEvent::Connected);
        assert_eq!(m.take_notice().as_deref(), Some(""));
    }

    #[test]
    fn join_decisions_and_errors_become_notices() {
        let mut m = model();
        m.apply(server(ServerMsg::JoinDecided { code: code("aaaaa"), accepted: false }));
        assert!(m.take_notice().unwrap().contains("denied"));
        m.apply(server(ServerMsg::JoinDecided { code: code("aaaaa"), accepted: true }));
        assert!(m.take_notice().unwrap().contains("accepted"));
        m.apply(server(ServerMsg::Error { message: "the room is full".into() }));
        assert_eq!(m.take_notice().as_deref(), Some("the room is full"));
    }

    #[test]
    fn speaking_lasts_a_moment_after_the_last_loud_sample() {
        let mut m = model();
        let t0 = Instant::now();
        assert!(!m.is_speaking(2, t0));
        m.note_level(2, 0.2, t0);
        assert!(m.is_speaking(2, t0 + Duration::from_millis(300)));
        assert!(!m.is_speaking(2, t0 + Duration::from_millis(900)));
        m.note_level(3, 0.001, t0);
        assert!(!m.is_speaking(3, t0), "quiet noise is not speech");
    }

    #[test]
    fn a_muted_user_never_shows_as_speaking() {
        let mut m = model();
        m.muted = true;
        m.note_level(1, 0.5, Instant::now());
        assert!(!m.is_speaking(1, Instant::now()));
    }

    #[test]
    fn sharers_are_people_in_the_room_and_vanish_when_they_leave_or_the_room_ends() {
        let mut m = model();
        m.apply(server(ServerMsg::Entered { code: code("aaaaa"), peers: vec![peer(2, "bob")], ice: vec![] }));
        m.set_sharer(2, true);
        m.set_sharer(9, true);
        assert!(m.is_sharer(2));
        assert!(!m.is_sharer(9));
        m.set_sharer(2, false);
        assert!(!m.is_sharer(2));

        m.set_sharer(2, true);
        m.apply(server(ServerMsg::PeerLeft { user_id: 2 }));
        assert!(!m.is_sharer(2));

        m.apply(server(ServerMsg::PeerJoined { peer: peer(3, "cy") }));
        m.set_sharer(3, true);
        m.sharing = true;
        m.left_room();
        assert!(!m.is_sharer(3));
        assert!(!m.sharing);
    }

    #[test]
    fn link_state_only_tracks_people_in_the_room_and_resets_with_it() {
        let mut m = model();
        m.apply(server(ServerMsg::Entered { code: code("aaaaa"), peers: vec![peer(2, "bob")], ice: vec![] }));
        m.set_link(2, LinkState::Connected);
        m.set_link(9, LinkState::Connected);
        assert_eq!(m.link_state(2), Some(LinkState::Connected));
        assert_eq!(m.link_state(9), None);
        m.note_level(2, 0.3, Instant::now());
        m.apply(server(ServerMsg::PeerLeft { user_id: 2 }));
        assert_eq!(m.link_state(2), None);
        assert!(!m.is_speaking(2, Instant::now()));

        m.apply(server(ServerMsg::PeerJoined { peer: peer(3, "cy") }));
        m.set_link(3, LinkState::Connecting);
        m.left_room();
        assert_eq!(m.link_state(3), None);
    }

    #[test]
    fn a_refreshed_room_list_drops_a_room_that_vanished() {
        let mut m = model();
        m.apply(server(ServerMsg::Rooms { rooms: vec![room("aaaaa", "you", true)] }));
        m.apply(server(ServerMsg::Entered { code: code("aaaaa"), peers: vec![], ice: vec![] }));
        m.apply(server(ServerMsg::Rooms { rooms: vec![] }));
        assert!(m.current.is_none());
    }
}
