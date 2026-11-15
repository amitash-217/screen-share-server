//! Database side of rooms, membership and join requests.

use protocol::{Peer, RoomCode, UserId, MAX_ROOMS_PER_USER, ROOM_CODE_LEN};
use rand::Rng;
use sqlx::PgPool;

pub const MAX_PENDING_PER_ROOM: i64 = 50;
pub const MAX_PENDING_PER_USER: i64 = 20;

type Res<T> = Result<T, sqlx::Error>;

pub struct RoomRow {
    pub code: RoomCode,
    pub host_id: UserId,
    pub host: String,
    pub permanent: bool,
}

type RawRoom = (String, i64, String, bool);

fn to_row((code, host_id, host, permanent): RawRoom) -> Option<RoomRow> {
    Some(RoomRow { code: RoomCode::parse(&code)?, host_id, host, permanent })
}

const ROOM_SELECT: &str = "SELECT r.code, r.host_id, u.username, r.permanent \
                           FROM rooms r JOIN users u ON u.id = r.host_id";

/// Rooms the user hosts or belongs to.
pub async fn rooms_for_user(pool: &PgPool, user: UserId) -> Res<Vec<RoomRow>> {
    let rows: Vec<RawRoom> = sqlx::query_as(&format!(
        "{ROOM_SELECT} WHERE r.host_id = $1 \
         OR EXISTS (SELECT 1 FROM room_members m WHERE m.room_code = r.code AND m.user_id = $1) \
         ORDER BY r.created_at"
    ))
    .bind(user)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().filter_map(to_row).collect())
}

/// The room, only if the user hosts it or is a member.
pub async fn room_access(pool: &PgPool, code: &RoomCode, user: UserId) -> Res<Option<RoomRow>> {
    let row: Option<RawRoom> = sqlx::query_as(&format!(
        "{ROOM_SELECT} WHERE r.code = $1 AND (r.host_id = $2 \
         OR EXISTS (SELECT 1 FROM room_members m WHERE m.room_code = r.code AND m.user_id = $2))"
    ))
    .bind(code.as_str())
    .bind(user)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(to_row))
}

pub enum CreateError {
    TooManyRooms,
    PermanentExists,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for CreateError {
    fn from(e: sqlx::Error) -> Self {
        CreateError::Db(e)
    }
}

fn random_code() -> String {
    let mut rng = rand::thread_rng();
    (0..ROOM_CODE_LEN).map(|_| rng.gen_range(b'a'..=b'z') as char).collect()
}

pub async fn create_room(pool: &PgPool, user: UserId, permanent: bool) -> Result<RoomCode, CreateError> {
    let mut tx = pool.begin().await?;
    // Serialize concurrent creates by one user so the limits cannot be raced past.
    sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE").bind(user).execute(&mut *tx).await?;
    let (count, has_permanent): (i64, bool) =
        sqlx::query_as("SELECT count(*), COALESCE(bool_or(permanent), false) FROM rooms WHERE host_id = $1")
            .bind(user)
            .fetch_one(&mut *tx)
            .await?;
    if count >= MAX_ROOMS_PER_USER as i64 {
        return Err(CreateError::TooManyRooms);
    }
    if permanent && has_permanent {
        return Err(CreateError::PermanentExists);
    }
    for _ in 0..10 {
        let code = random_code();
        let done = sqlx::query("INSERT INTO rooms (code, host_id, permanent) VALUES ($1, $2, $3) ON CONFLICT (code) DO NOTHING")
            .bind(&code)
            .bind(user)
            .bind(permanent)
            .execute(&mut *tx)
            .await?;
        if done.rows_affected() == 1 {
            tx.commit().await?;
            return Ok(RoomCode::parse(&code).expect("generated code is valid"));
        }
    }
    Err(CreateError::Db(sqlx::Error::Protocol("could not allocate a room code".into())))
}

const AUDIENCE_SQL: &str = "SELECT host_id FROM rooms WHERE code = $1 \
     UNION SELECT user_id FROM room_members WHERE room_code = $1 \
     UNION SELECT user_id FROM join_requests WHERE room_code = $1";

/// Everyone whose client shows this room: host, members and pending requesters.
async fn audience(tx: &mut sqlx::PgConnection, code: &RoomCode) -> Res<Vec<UserId>> {
    let rows: Vec<(i64,)> = sqlx::query_as(AUDIENCE_SQL).bind(code.as_str()).fetch_all(tx).await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Deletes the room if `host` owns it. Returns who to tell, or `None` if not allowed.
pub async fn delete_room(pool: &PgPool, code: &RoomCode, host: UserId) -> Res<Option<Vec<UserId>>> {
    let mut tx = pool.begin().await?;
    let who = audience(&mut tx, code).await?;
    let done = sqlx::query("DELETE FROM rooms WHERE code = $1 AND host_id = $2")
        .bind(code.as_str())
        .bind(host)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((done.rows_affected() == 1).then_some(who))
}

/// Deletes the room only if it is temporary. Returns who to tell.
pub async fn delete_temporary(pool: &PgPool, code: &RoomCode) -> Res<Option<Vec<UserId>>> {
    let mut tx = pool.begin().await?;
    let who = audience(&mut tx, code).await?;
    let done = sqlx::query("DELETE FROM rooms WHERE code = $1 AND NOT permanent")
        .bind(code.as_str())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((done.rows_affected() == 1).then_some(who))
}

/// Temporary rooms cannot outlive the process that hosted them.
pub async fn purge_temporary(pool: &PgPool) -> Res<u64> {
    Ok(sqlx::query("DELETE FROM rooms WHERE NOT permanent").execute(pool).await?.rows_affected())
}

pub async fn hosted_rooms(pool: &PgPool, host: UserId) -> Res<Vec<(RoomCode, bool)>> {
    let rows: Vec<(String, bool)> =
        sqlx::query_as("SELECT code, permanent FROM rooms WHERE host_id = $1").bind(host).fetch_all(pool).await?;
    Ok(rows.into_iter().filter_map(|(c, p)| Some((RoomCode::parse(&c)?, p))).collect())
}

pub async fn member_ids(pool: &PgPool, code: &RoomCode) -> Res<Vec<UserId>> {
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT user_id FROM room_members WHERE room_code = $1").bind(code.as_str()).fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Join requests waiting on rooms this user hosts, oldest first.
pub async fn pending_for_host(pool: &PgPool, host: UserId) -> Res<Vec<(RoomCode, Peer)>> {
    let rows: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT jr.room_code, u.id, u.username FROM join_requests jr \
         JOIN rooms r ON r.code = jr.room_code JOIN users u ON u.id = jr.user_id \
         WHERE r.host_id = $1 ORDER BY jr.created_at",
    )
    .bind(host)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(c, user_id, username)| Some((RoomCode::parse(&c)?, Peer { user_id, username })))
        .collect())
}

pub enum RequestOutcome {
    NoRoom,
    OwnRoom,
    AlreadyMember,
    RoomQueueFull,
    UserQueueFull,
    Queued { host_id: UserId },
}

pub async fn add_request(pool: &PgPool, code: &RoomCode, user: UserId) -> Res<RequestOutcome> {
    let host: Option<(i64,)> =
        sqlx::query_as("SELECT host_id FROM rooms WHERE code = $1").bind(code.as_str()).fetch_optional(pool).await?;
    let Some((host_id,)) = host else { return Ok(RequestOutcome::NoRoom) };
    if host_id == user {
        return Ok(RequestOutcome::OwnRoom);
    }
    let is_member: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM room_members WHERE room_code = $1 AND user_id = $2)")
            .bind(code.as_str())
            .bind(user)
            .fetch_one(pool)
            .await?;
    if is_member {
        return Ok(RequestOutcome::AlreadyMember);
    }
    let already_pending: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM join_requests WHERE room_code = $1 AND user_id = $2)")
            .bind(code.as_str())
            .bind(user)
            .fetch_one(pool)
            .await?;
    if !already_pending {
        let per_room: i64 = sqlx::query_scalar("SELECT count(*) FROM join_requests WHERE room_code = $1")
            .bind(code.as_str())
            .fetch_one(pool)
            .await?;
        if per_room >= MAX_PENDING_PER_ROOM {
            return Ok(RequestOutcome::RoomQueueFull);
        }
        let per_user: i64 = sqlx::query_scalar("SELECT count(*) FROM join_requests WHERE user_id = $1")
            .bind(user)
            .fetch_one(pool)
            .await?;
        if per_user >= MAX_PENDING_PER_USER {
            return Ok(RequestOutcome::UserQueueFull);
        }
        sqlx::query("INSERT INTO join_requests (room_code, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(code.as_str())
            .bind(user)
            .execute(pool)
            .await?;
    }
    Ok(RequestOutcome::Queued { host_id })
}

pub enum Decision {
    NotHost,
    NoRequest,
    Done,
}

pub async fn decide(pool: &PgPool, code: &RoomCode, host: UserId, user: UserId, accept: bool) -> Res<Decision> {
    let mut tx = pool.begin().await?;
    let is_host: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM rooms WHERE code = $1 AND host_id = $2)")
        .bind(code.as_str())
        .bind(host)
        .fetch_one(&mut *tx)
        .await?;
    if !is_host {
        return Ok(Decision::NotHost);
    }
    let removed = sqlx::query("DELETE FROM join_requests WHERE room_code = $1 AND user_id = $2")
        .bind(code.as_str())
        .bind(user)
        .execute(&mut *tx)
        .await?;
    if removed.rows_affected() == 0 {
        return Ok(Decision::NoRequest);
    }
    if accept {
        sqlx::query("INSERT INTO room_members (room_code, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(code.as_str())
            .bind(user)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Decision::Done)
}

/// Removes a member if `host` owns the room. Returns whether anything was removed.
pub async fn remove_member(pool: &PgPool, code: &RoomCode, host: UserId, user: UserId) -> Res<bool> {
    let done = sqlx::query(
        "DELETE FROM room_members m USING rooms r \
         WHERE m.room_code = $1 AND m.user_id = $2 AND r.code = m.room_code AND r.host_id = $3",
    )
    .bind(code.as_str())
    .bind(user)
    .bind(host)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}
