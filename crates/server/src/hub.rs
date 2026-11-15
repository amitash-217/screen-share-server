use std::collections::HashMap;
use std::sync::Mutex;

use protocol::{Peer, RoomCode, ServerMsg, UserId, MAX_ROOM_SIZE};
use tokio::sync::mpsc;

pub type Tx = mpsc::Sender<ServerMsg>;

struct Conn {
    id: u64,
    username: String,
    tx: Tx,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    conns: HashMap<UserId, Conn>,
    /// Users currently inside each live room, in join order.
    rooms: HashMap<RoomCode, Vec<UserId>>,
    in_room: HashMap<UserId, RoomCode>,
}

/// Who is connected and who is inside which room. Memory only; the database
/// holds rooms and membership. Never hold the lock across an `.await`.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<Inner>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RoomFull;

impl Hub {
    /// Drops the user's current connection, which makes its socket close.
    /// Returns whether there was one.
    pub fn take(&self, user: UserId) -> bool {
        self.inner.lock().unwrap().conns.remove(&user).is_some()
    }

    pub fn insert(&self, user: UserId, username: &str, tx: Tx) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.conns.insert(user, Conn { id, username: username.to_owned(), tx });
        id
    }

    /// Removes the connection only if it is still the one with `id`.
    pub fn remove_if(&self, user: UserId, id: u64) -> bool {
        let mut inner = self.inner.lock().unwrap();
        match inner.conns.get(&user) {
            Some(c) if c.id == id => inner.conns.remove(&user).is_some(),
            _ => false,
        }
    }

    pub fn is_online(&self, user: UserId) -> bool {
        self.inner.lock().unwrap().conns.contains_key(&user)
    }

    /// Queues a message; a client whose queue is full loses it.
    pub fn send(&self, user: UserId, msg: ServerMsg) {
        if let Some(c) = self.inner.lock().unwrap().conns.get(&user) {
            let _ = c.tx.try_send(msg);
        }
    }

    pub fn send_many(&self, users: impl IntoIterator<Item = UserId>, msg: &ServerMsg) {
        let inner = self.inner.lock().unwrap();
        for user in users {
            if let Some(c) = inner.conns.get(&user) {
                let _ = c.tx.try_send(msg.clone());
            }
        }
    }

    /// Puts `user` into `room` and returns the peers already there.
    /// The caller must have taken the user out of any other room first.
    pub fn join(&self, room: &RoomCode, user: UserId) -> Result<Vec<Peer>, RoomFull> {
        let mut inner = self.inner.lock().unwrap();
        let members = inner.rooms.entry(room.clone()).or_default();
        if members.len() >= MAX_ROOM_SIZE {
            return Err(RoomFull);
        }
        let existing = members.clone();
        members.push(user);
        inner.in_room.insert(user, room.clone());
        Ok(existing
            .into_iter()
            .filter_map(|id| inner.conns.get(&id).map(|c| Peer { user_id: id, username: c.username.clone() }))
            .collect())
    }

    /// Takes the user out of their room. Returns the room and who is still in it.
    pub fn leave(&self, user: UserId) -> Option<(RoomCode, Vec<UserId>)> {
        let mut inner = self.inner.lock().unwrap();
        let room = inner.in_room.remove(&user)?;
        let remaining = {
            let members = inner.rooms.get_mut(&room)?;
            members.retain(|&u| u != user);
            members.clone()
        };
        if remaining.is_empty() {
            inner.rooms.remove(&room);
        }
        Some((room, remaining))
    }

    /// Empties a room and returns the users who were in it.
    pub fn clear_room(&self, room: &RoomCode) -> Vec<UserId> {
        let mut inner = self.inner.lock().unwrap();
        let users = inner.rooms.remove(room).unwrap_or_default();
        for u in &users {
            inner.in_room.remove(u);
        }
        users
    }

    pub fn room_of(&self, user: UserId) -> Option<RoomCode> {
        self.inner.lock().unwrap().in_room.get(&user).cloned()
    }

    pub fn peer(&self, user: UserId) -> Option<Peer> {
        let inner = self.inner.lock().unwrap();
        inner.conns.get(&user).map(|c| Peer { user_id: user, username: c.username.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(s: &str) -> RoomCode {
        RoomCode::parse(s).unwrap()
    }

    fn online(hub: &Hub, user: UserId) {
        let (tx, _rx) = mpsc::channel(8);
        hub.insert(user, &format!("u{user}"), tx);
    }

    #[test]
    fn join_lists_existing_peers_and_leave_reports_remaining() {
        let hub = Hub::default();
        (1..=2).for_each(|u| online(&hub, u));
        assert!(hub.join(&code("aaaaa"), 1).unwrap().is_empty());
        let peers = hub.join(&code("aaaaa"), 2).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].user_id, 1);
        assert_eq!(hub.room_of(2), Some(code("aaaaa")));
        assert_eq!(hub.leave(1), Some((code("aaaaa"), vec![2])));
        assert_eq!(hub.leave(2), Some((code("aaaaa"), vec![])));
        assert_eq!(hub.leave(2), None);
    }

    #[test]
    fn room_is_capped() {
        let hub = Hub::default();
        let room = code("bbbbb");
        for u in 1..=MAX_ROOM_SIZE as i64 {
            online(&hub, u);
            hub.join(&room, u).unwrap();
        }
        online(&hub, 99);
        assert!(matches!(hub.join(&room, 99), Err(RoomFull)));
        assert_eq!(hub.room_of(99), None);
    }

    #[test]
    fn stale_connection_cannot_remove_the_new_one() {
        let hub = Hub::default();
        let (tx1, _r1) = mpsc::channel(8);
        let old = hub.insert(1, "a", tx1);
        assert!(hub.take(1));
        let (tx2, _r2) = mpsc::channel(8);
        let new = hub.insert(1, "a", tx2);
        assert!(!hub.remove_if(1, old));
        assert!(hub.is_online(1));
        assert!(hub.remove_if(1, new));
    }

    #[test]
    fn clear_room_empties_everyone() {
        let hub = Hub::default();
        (1..=2).for_each(|u| online(&hub, u));
        hub.join(&code("ccccc"), 1).unwrap();
        hub.join(&code("ccccc"), 2).unwrap();
        let mut kicked = hub.clear_room(&code("ccccc"));
        kicked.sort();
        assert_eq!(kicked, vec![1, 2]);
        assert_eq!(hub.room_of(1), None);
    }
}
