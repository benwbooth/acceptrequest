-- Machines running the client daemon. Tokens are stored as SHA-256 hex.
CREATE TABLE clients (
    id INTEGER PRIMARY KEY,
    hostname TEXT NOT NULL UNIQUE,
    token_hash TEXT NOT NULL UNIQUE,
    last_seen TEXT
);

CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE
);

-- weekday 0 = Monday. A missing row means no limit that day.
CREATE TABLE daily_allowances (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    weekday INTEGER NOT NULL CHECK (weekday BETWEEN 0 AND 6),
    minutes INTEGER NOT NULL CHECK (minutes >= 0),
    PRIMARY KEY (user_id, weekday)
);

-- days is a bitmask of the days the window starts on, bit 0 = Monday.
-- A window whose end is not after its start runs past midnight.
CREATE TABLE downtime_windows (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    days INTEGER NOT NULL,
    start_time TEXT NOT NULL,
    end_time TEXT NOT NULL
);

CREATE TABLE bonus_grants (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    day TEXT NOT NULL,
    minutes INTEGER NOT NULL,
    source TEXT NOT NULL,
    granted_at TEXT NOT NULL
);
CREATE INDEX bonus_grants_user_day ON bonus_grants (user_id, day);

-- action_token_hash authorizes the one-tap links in the phone notification for this request only.
CREATE TABLE time_requests (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    host TEXT NOT NULL,
    requested_minutes INTEGER,
    reason TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'granted', 'denied')),
    granted_minutes INTEGER,
    action_token_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    resolved_at TEXT
);

CREATE TABLE usage (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    host TEXT NOT NULL,
    day TEXT NOT NULL,
    active_minutes INTEGER NOT NULL,
    local_bonus_minutes INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, host, day)
);

-- Admin web sessions. Tokens are stored as SHA-256 hex.
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
