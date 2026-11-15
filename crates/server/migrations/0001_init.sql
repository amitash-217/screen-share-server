CREATE TABLE users (
    id            BIGSERIAL PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Temporary rooms exist only in memory while live, but a row is written on
-- creation so the 3-room limit and code uniqueness survive a restart check.
CREATE TABLE rooms (
    code       TEXT PRIMARY KEY CHECK (code ~ '^[a-z]{5}$'),
    host_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    permanent  BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- At most one permanent room per user.
CREATE UNIQUE INDEX one_permanent_room_per_user ON rooms (host_id) WHERE permanent;

CREATE TABLE room_members (
    room_code TEXT   NOT NULL REFERENCES rooms(code) ON DELETE CASCADE,
    user_id   BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (room_code, user_id)
);

CREATE TABLE join_requests (
    room_code  TEXT   NOT NULL REFERENCES rooms(code) ON DELETE CASCADE,
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_code, user_id)
);
