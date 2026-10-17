-- Avatares: el PNG que guarda el servidor (ya validado y vuelto a codificar por él), uno por
-- usuario. La versión (53 bits del SHA-256 del PNG) va en `users` para leerla con el usuario.

ALTER TABLE users ADD COLUMN avatar_version INTEGER;

CREATE TABLE avatars (
    user_id    INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    png        BLOB    NOT NULL,
    updated_at INTEGER NOT NULL
);
