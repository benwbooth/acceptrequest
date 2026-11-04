-- A parent's lock on a kid: host '*' means every computer, otherwise one computer's hostname.
CREATE TABLE locks (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    host TEXT NOT NULL COLLATE NOCASE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, host)
);
