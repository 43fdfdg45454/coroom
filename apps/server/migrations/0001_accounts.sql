-- Cuentas, dispositivos, sesiones, invitaciones, amigos y uso del relay.
-- Fechas en segundos Unix (UTC); días del relay en días desde 1970-01-01.

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT    NOT NULL,
    password_hash TEXT    NOT NULL,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    disabled      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

-- Claves públicas Ed25519 vinculadas por el desafío de dispositivo (una cuenta por clave).
CREATE TABLE devices (
    id           INTEGER PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    public_key   BLOB    NOT NULL UNIQUE,
    name         TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL
);
CREATE INDEX devices_user ON devices (user_id);

-- Tokens opacos guardados con SHA-256; uno por dispositivo (device_id NULL = interfaz web).
CREATE TABLE sessions (
    id         INTEGER PRIMARY KEY,
    token_hash BLOB    NOT NULL UNIQUE,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id  INTEGER REFERENCES devices (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX sessions_user ON sessions (user_id);
CREATE INDEX sessions_device ON sessions (device_id);

-- Códigos de invitación de un solo uso, guardados con SHA-256.
CREATE TABLE invitations (
    id         INTEGER PRIMARY KEY,
    code_hash  BLOB    NOT NULL UNIQUE,
    hint       TEXT    NOT NULL,
    note       TEXT    NOT NULL DEFAULT '',
    created_by INTEGER REFERENCES users (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER,
    used_by    INTEGER REFERENCES users (id) ON DELETE SET NULL,
    revoked_at INTEGER
);

CREATE TABLE friend_requests (
    id         INTEGER PRIMARY KEY,
    from_user  INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    to_user    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    UNIQUE (from_user, to_user),
    CHECK (from_user <> to_user)
);
CREATE INDEX friend_requests_to ON friend_requests (to_user);

-- Amistad simétrica: una fila por pareja con user_a < user_b.
CREATE TABLE friendships (
    user_a     INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    user_b     INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_a, user_b),
    CHECK (user_a < user_b)
);
CREATE INDEX friendships_b ON friendships (user_b);

-- Bytes reenviados por el relay en las salas de cada anfitrión, por día (sin contenido).
CREATE TABLE relay_usage (
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    day     INTEGER NOT NULL,
    bytes   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, day)
);
