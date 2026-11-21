-- Devices paired for remote control. Only a hash of each device token is
-- stored, so a database copy cannot authenticate a client.
CREATE TABLE remote_device (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER
);
