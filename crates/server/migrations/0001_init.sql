CREATE TABLE clients (
    id INTEGER PRIMARY KEY,
    hostname TEXT NOT NULL UNIQUE,
    token_hash TEXT NOT NULL,
    last_seen TEXT
);

CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    daily_allowance_minutes INTEGER NOT NULL DEFAULT 120
);

-- days is a bitmask, bit 0 = Monday.
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
    granted_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE time_requests (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_id INTEGER NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
    requested_minutes INTEGER,
    reason TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    granted_minutes INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);

CREATE TABLE usage_samples (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_id INTEGER NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
    day TEXT NOT NULL,
    active_minutes INTEGER NOT NULL,
    UNIQUE (user_id, client_id, day)
);
