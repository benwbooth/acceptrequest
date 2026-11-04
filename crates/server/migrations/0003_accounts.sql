-- How a kid is shown in the web UI ("Ryan"); username stays the server's key ("ryan").
ALTER TABLE users ADD COLUMN display_name TEXT;

-- Local accounts each computer reports, and which kid (if any) each one belongs to.
-- decided = 0 until a parent has chosen, so new accounts can be flagged.
CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
    client_id INTEGER NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
    name TEXT NOT NULL COLLATE NOCASE,
    is_admin INTEGER NOT NULL DEFAULT 0,
    user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    decided INTEGER NOT NULL DEFAULT 0,
    first_seen TEXT NOT NULL,
    last_seen TEXT NOT NULL,
    UNIQUE (client_id, name)
);
