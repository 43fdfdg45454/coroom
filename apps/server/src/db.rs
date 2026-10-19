//! Base de datos SQLite del servidor: pool, migraciones y repositorios.
//!
//! `sqlx` con consultas en tiempo de ejecución (sin macros que necesiten `DATABASE_URL`), modo
//! `WAL`, claves foráneas activadas y el fichero `COROOM_DATA_DIR/coroom.db`. Las migraciones de
//! `apps/server/migrations/` van embebidas en el binario (`include_str!`) y se aplican con el
//! `Migrator` de `sqlx`, que guarda las aplicadas en `_sqlx_migrations`: arrancar dos veces no
//! repite nada. Las copias de seguridad usan `VACUUM INTO`, la copia en línea y consistente de
//! SQLite (válida con el servidor en marcha).

use std::borrow::Cow;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
    SqliteSynchronous,
};
use sqlx::{AssertSqlSafe, Row, SqlSafeStr};

use crate::clock;

/// Nombre del fichero de la base dentro de `COROOM_DATA_DIR`.
pub const DATABASE_FILE: &str = "coroom.db";

/// Migraciones embebidas, en orden.
const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "accounts",
        include_str!("../migrations/0001_accounts.sql"),
    ),
    (2, "avatars", include_str!("../migrations/0002_avatars.sql")),
];

/// Error de la base de datos.
#[derive(Debug)]
pub enum DbError {
    /// Ya existe (nombre de usuario, clave de dispositivo…).
    Conflict(&'static str),
    /// No existe o no se puede usar.
    NotFound(&'static str),
    /// Error de SQLite.
    Sqlx(sqlx::Error),
    /// Error de las migraciones.
    Migrate(sqlx::migrate::MigrateError),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Conflict(what) => write!(f, "ya existe: {what}"),
            DbError::NotFound(what) => write!(f, "no existe: {what}"),
            DbError::Sqlx(err) => write!(f, "error de la base de datos: {err}"),
            DbError::Migrate(err) => write!(f, "error en las migraciones: {err}"),
        }
    }
}

impl std::error::Error for DbError {}

impl From<sqlx::Error> for DbError {
    fn from(err: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &err
            && db.is_unique_violation()
        {
            return DbError::Conflict("registro duplicado");
        }
        DbError::Sqlx(err)
    }
}

/// Resultado de la base de datos.
pub type DbResult<T> = Result<T, DbError>;

/// Usuario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    /// Identificador.
    pub id: i64,
    /// Nombre de usuario.
    pub username: String,
    /// Nombre visible.
    pub display_name: String,
    /// Hash Argon2id en formato PHC.
    pub password_hash: String,
    /// Administrador.
    pub is_admin: bool,
    /// Deshabilitado.
    pub disabled: bool,
    /// Alta.
    pub created_at: i64,
    /// Versión del avatar (`None`: no tiene).
    pub avatar: Option<u64>,
}

/// Usuario con sus dispositivos, para la administración.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminUserRow {
    /// El usuario.
    pub user: UserRow,
    /// Dispositivos vinculados.
    pub devices: i64,
    /// Último uso de un dispositivo.
    pub last_seen_at: Option<i64>,
}

/// Dispositivo vinculado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// Identificador.
    pub id: i64,
    /// Usuario.
    pub user_id: i64,
    /// Clave pública Ed25519.
    pub public_key: [u8; 32],
    /// Nombre.
    pub name: String,
    /// Alta.
    pub created_at: i64,
    /// Último uso.
    pub last_seen_at: i64,
}

/// Sesión válida con su usuario y su dispositivo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Identificador.
    pub id: i64,
    /// Usuario.
    pub user: UserRow,
    /// Dispositivo (`None` = interfaz web).
    pub device: Option<DeviceRow>,
    /// Caducidad.
    pub expires_at: i64,
}

/// Invitación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitationRow {
    /// Identificador.
    pub id: i64,
    /// Últimos caracteres.
    pub hint: String,
    /// Nota.
    pub note: String,
    /// Alta.
    pub created_at: i64,
    /// Caducidad.
    pub expires_at: i64,
    /// Cuándo se canjeó.
    pub used_at: Option<i64>,
    /// Quién la canjeó.
    pub used_by: Option<String>,
    /// Cuándo se revocó.
    pub revoked_at: Option<i64>,
}

impl InvitationRow {
    /// `pending`, `used`, `expired` o `revoked` en `now`.
    pub fn state(&self, now: i64) -> &'static str {
        if self.used_at.is_some() {
            "used"
        } else if self.revoked_at.is_some() {
            "revoked"
        } else if self.expires_at <= now {
            "expired"
        } else {
            "pending"
        }
    }
}

/// Solicitud de amistad con el otro usuario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendRequestRow {
    /// Identificador.
    pub id: i64,
    /// Quién la envió.
    pub from_user: i64,
    /// A quién.
    pub to_user: i64,
    /// El otro usuario (desde el punto de vista de quien pregunta).
    pub other: UserRow,
    /// Fecha.
    pub created_at: i64,
}

/// Resultado de pedir amistad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FriendOutcome {
    /// Solicitud enviada ahora.
    Requested,
    /// Ya estaba enviada y sin responder.
    AlreadyRequested,
    /// El otro ya la había pedido: ahora sois amigos.
    Accepted,
    /// Ya erais amigos.
    AlreadyFriends,
}

/// Base de datos (barata de clonar: comparte el pool).
#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

const USER_COLUMNS: &str = "u.id, u.username, u.display_name, u.password_hash, u.is_admin, \
                            u.disabled, u.created_at, u.avatar_version";

fn user_from(row: &SqliteRow) -> Result<UserRow, sqlx::Error> {
    Ok(UserRow {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        display_name: row.try_get("display_name")?,
        password_hash: row.try_get("password_hash")?,
        is_admin: row.try_get::<i64, _>("is_admin")? != 0,
        disabled: row.try_get::<i64, _>("disabled")? != 0,
        created_at: row.try_get("created_at")?,
        avatar: row
            .try_get::<Option<i64>, _>("avatar_version")?
            .and_then(|version| u64::try_from(version).ok()),
    })
}

fn key_from(bytes: Vec<u8>) -> Result<[u8; 32], sqlx::Error> {
    bytes
        .try_into()
        .map_err(|_| sqlx::Error::Decode("clave pública de dispositivo dañada".into()))
}

fn device_from(row: &SqliteRow, prefix: &str) -> Result<DeviceRow, sqlx::Error> {
    let col = |name: &str| format!("{prefix}{name}");
    Ok(DeviceRow {
        id: row.try_get(col("id").as_str())?,
        user_id: row.try_get(col("user_id").as_str())?,
        public_key: key_from(row.try_get(col("public_key").as_str())?)?,
        name: row.try_get(col("name").as_str())?,
        created_at: row.try_get(col("created_at").as_str())?,
        last_seen_at: row.try_get(col("last_seen_at").as_str())?,
    })
}

/// Consulta con texto construido en el propio código (nunca con datos del usuario: esos van en
/// `bind`).
fn sql(text: String) -> impl SqlSafeStr {
    AssertSqlSafe(text)
}

fn as_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

impl Db {
    /// Abre (o crea) la base en `path` y aplica las migraciones.
    pub async fn open(path: &Path) -> DbResult<Self> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).map_err(|err| DbError::Sqlx(sqlx::Error::Io(err)))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        let db = Self { pool };
        db.migrate().await?;
        Ok(db)
    }

    /// Aplica las migraciones pendientes (idempotente).
    pub async fn migrate(&self) -> DbResult<()> {
        let migrations = MIGRATIONS
            .iter()
            .map(|(version, description, text)| {
                Migration::new(
                    *version,
                    Cow::Borrowed(*description),
                    MigrationType::Simple,
                    (*text).into_sql_str(),
                    false,
                )
            })
            .collect();
        Migrator::with_migrations(migrations)
            .run(&self.pool)
            .await
            .map_err(DbError::Migrate)
    }

    /// Copia en línea y consistente en `path` (`VACUUM INTO`); `path` no debe existir.
    pub async fn backup(&self, path: &Path) -> DbResult<()> {
        if path.exists() {
            return Err(DbError::Conflict("el fichero de la copia ya existe"));
        }
        let target = path.to_string_lossy().into_owned();
        sqlx::query("VACUUM INTO ?")
            .bind(target)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Cierra el pool.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    // --- Usuarios ------------------------------------------------------------------------------

    /// Número de usuarios.
    pub async fn count_users(&self) -> DbResult<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(as_u64(count))
    }

    /// Hay al menos un administrador.
    pub async fn has_admin(&self) -> DbResult<bool> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_admin = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(count > 0)
    }

    /// Crea el primer administrador si no hay ninguno (atómico); `None` si ya lo había.
    pub async fn create_first_admin(
        &self,
        username: &str,
        display_name: &str,
        password_hash: &str,
    ) -> DbResult<Option<UserRow>> {
        let result = sqlx::query(
            "INSERT INTO users (username, display_name, password_hash, is_admin, disabled, \
             created_at) SELECT ?, ?, ?, 1, 0, ? WHERE NOT EXISTS (SELECT 1 FROM users WHERE \
             is_admin = 1)",
        )
        .bind(username)
        .bind(display_name)
        .bind(password_hash)
        .bind(clock::now())
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.user_by_id(result.last_insert_rowid()).await
    }

    /// Crea un usuario.
    pub async fn create_user(
        &self,
        username: &str,
        display_name: &str,
        password_hash: &str,
        is_admin: bool,
    ) -> DbResult<UserRow> {
        let result = sqlx::query(
            "INSERT INTO users (username, display_name, password_hash, is_admin, disabled, \
             created_at) VALUES (?, ?, ?, ?, 0, ?)",
        )
        .bind(username)
        .bind(display_name)
        .bind(password_hash)
        .bind(i64::from(is_admin))
        .bind(clock::now())
        .execute(&self.pool)
        .await
        .map_err(|err| match DbError::from(err) {
            DbError::Conflict(_) => DbError::Conflict("nombre de usuario"),
            other => other,
        })?;
        self.user_by_id(result.last_insert_rowid())
            .await?
            .ok_or(DbError::NotFound("usuario recién creado"))
    }

    /// Usuario por nombre (sin distinguir mayúsculas).
    pub async fn user_by_name(&self, username: &str) -> DbResult<Option<UserRow>> {
        let row = sqlx::query(sql(format!(
            "SELECT {USER_COLUMNS} FROM users u WHERE u.username = ?"
        )))
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(user_from).transpose()?)
    }

    /// Usuario por identificador.
    pub async fn user_by_id(&self, id: i64) -> DbResult<Option<UserRow>> {
        let row = sqlx::query(sql(format!(
            "SELECT {USER_COLUMNS} FROM users u WHERE u.id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(user_from).transpose()?)
    }

    /// Todos los usuarios con sus dispositivos.
    pub async fn list_users(&self) -> DbResult<Vec<AdminUserRow>> {
        let rows = sqlx::query(sql(format!(
            "SELECT {USER_COLUMNS}, COUNT(d.id) AS devices, MAX(d.last_seen_at) AS last_seen_at \
             FROM users u LEFT JOIN devices d ON d.user_id = u.id GROUP BY u.id \
             ORDER BY u.username COLLATE NOCASE"
        )))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(AdminUserRow {
                    user: user_from(row)?,
                    devices: row.try_get("devices")?,
                    last_seen_at: row.try_get("last_seen_at")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(DbError::from)
    }

    /// Cambia el nombre visible, el papel de administrador o el estado de un usuario.
    pub async fn update_user(
        &self,
        id: i64,
        display_name: Option<&str>,
        is_admin: Option<bool>,
        disabled: Option<bool>,
    ) -> DbResult<()> {
        let result = sqlx::query(
            "UPDATE users SET display_name = COALESCE(?, display_name), \
             is_admin = COALESCE(?, is_admin), disabled = COALESCE(?, disabled) WHERE id = ?",
        )
        .bind(display_name)
        .bind(is_admin.map(i64::from))
        .bind(disabled.map(i64::from))
        .bind(id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound("usuario"));
        }
        Ok(())
    }

    /// Borra al usuario con todo lo suyo (dispositivos, sesiones, amistades, solicitudes, avatar y
    /// uso del relay, en cascada; las invitaciones que creó o usó quedan sin autor). `false` si no
    /// existía.
    pub async fn delete_user(&self, id: i64) -> DbResult<bool> {
        let result = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Administradores habilitados.
    pub async fn count_active_admins(&self) -> DbResult<u64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_admin = 1 AND disabled = 0")
                .fetch_one(&self.pool)
                .await?;
        Ok(as_u64(count))
    }

    /// Pone otra contraseña y cierra todas las sesiones del usuario.
    pub async fn set_password(&self, id: i64, password_hash: &str) -> DbResult<()> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(password_hash)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound("usuario"));
        }
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    // --- Avatares -------------------------------------------------------------------------------

    /// Guarda el avatar de un usuario (el PNG ya validado) con su versión, sustituyendo el que
    /// tuviese.
    pub async fn set_avatar(&self, user: i64, png: &[u8], version: u64) -> DbResult<()> {
        let version = i64::try_from(version).map_err(|_| DbError::NotFound("versión de avatar"))?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query("UPDATE users SET avatar_version = ? WHERE id = ?")
            .bind(version)
            .bind(user)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound("usuario"));
        }
        sqlx::query(
            "INSERT INTO avatars (user_id, png, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (user_id) DO UPDATE SET png = excluded.png, \
             updated_at = excluded.updated_at",
        )
        .bind(user)
        .bind(png)
        .bind(clock::now())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Quita el avatar de un usuario; `false` si no tenía.
    pub async fn delete_avatar(&self, user: i64) -> DbResult<bool> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE users SET avatar_version = NULL WHERE id = ?")
            .bind(user)
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM avatars WHERE user_id = ?")
            .bind(user)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    /// El avatar de un usuario: su versión y el PNG (`None` si no tiene, no existe o está
    /// deshabilitado).
    pub async fn avatar(&self, user: i64) -> DbResult<Option<(u64, Vec<u8>)>> {
        let row = sqlx::query(
            "SELECT u.avatar_version, a.png FROM users u JOIN avatars a ON a.user_id = u.id \
             WHERE u.id = ? AND u.avatar_version IS NOT NULL AND u.disabled = 0",
        )
        .bind(user)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let version: i64 = row.try_get("avatar_version")?;
        Ok(Some((as_u64(version), row.try_get("png")?)))
    }

    // --- Dispositivos y sesiones ---------------------------------------------------------------

    /// Vincula la clave `public_key` a `user_id` (o actualiza su nombre si ya lo estaba).
    pub async fn upsert_device(
        &self,
        user_id: i64,
        public_key: &[u8; 32],
        name: &str,
    ) -> DbResult<DeviceRow> {
        let now = clock::now();
        let existing = sqlx::query("SELECT * FROM devices WHERE public_key = ?")
            .bind(public_key.as_slice())
            .fetch_optional(&self.pool)
            .await?;
        if let Some(row) = existing {
            let device = device_from(&row, "")?;
            if device.user_id != user_id {
                return Err(DbError::Conflict("clave de dispositivo de otra cuenta"));
            }
            sqlx::query("UPDATE devices SET name = ?, last_seen_at = ? WHERE id = ?")
                .bind(name)
                .bind(now)
                .bind(device.id)
                .execute(&self.pool)
                .await?;
            return Ok(DeviceRow {
                name: name.to_owned(),
                last_seen_at: now,
                ..device
            });
        }
        let result = sqlx::query(
            "INSERT INTO devices (user_id, public_key, name, created_at, last_seen_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(public_key.as_slice())
        .bind(name)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|err| match DbError::from(err) {
            DbError::Conflict(_) => DbError::Conflict("clave de dispositivo de otra cuenta"),
            other => other,
        })?;
        Ok(DeviceRow {
            id: result.last_insert_rowid(),
            user_id,
            public_key: *public_key,
            name: name.to_owned(),
            created_at: now,
            last_seen_at: now,
        })
    }

    /// Dispositivos de un usuario, del último usado al primero.
    pub async fn devices(&self, user_id: i64) -> DbResult<Vec<DeviceRow>> {
        let rows = sqlx::query(
            "SELECT * FROM devices WHERE user_id = ? ORDER BY last_seen_at DESC, id DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| device_from(row, "").map_err(DbError::from))
            .collect()
    }

    /// Desvincula un dispositivo de su usuario: borra la fila y, en cascada, su sesión (la clave
    /// ya no abre la señalización ni el relay). `false` si no es de ese usuario.
    pub async fn delete_device(&self, user_id: i64, device_id: i64) -> DbResult<bool> {
        let result = sqlx::query("DELETE FROM devices WHERE id = ? AND user_id = ?")
            .bind(device_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Marca el uso de un dispositivo.
    pub async fn touch_device(&self, id: i64) -> DbResult<()> {
        sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?")
            .bind(clock::now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Crea una sesión. Con dispositivo, sustituye la que tuviera (una por dispositivo).
    pub async fn create_session(
        &self,
        token_hash: &[u8; 32],
        user_id: i64,
        device_id: Option<i64>,
        expires_at: i64,
    ) -> DbResult<()> {
        let mut tx = self.pool.begin().await?;
        if let Some(device) = device_id {
            sqlx::query("DELETE FROM sessions WHERE device_id = ?")
                .bind(device)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, device_id, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(token_hash.as_slice())
        .bind(user_id)
        .bind(device_id)
        .bind(clock::now())
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Sesión vigente de un token (por su hash), con su usuario y su dispositivo.
    pub async fn session(&self, token_hash: &[u8; 32]) -> DbResult<Option<SessionRow>> {
        let row = sqlx::query(sql(format!(
            "SELECT s.id AS session_id, s.expires_at AS session_expires_at, {USER_COLUMNS}, \
             d.id AS d_id, d.user_id AS d_user_id, d.public_key AS d_public_key, d.name AS d_name, \
             d.created_at AS d_created_at, d.last_seen_at AS d_last_seen_at \
             FROM sessions s JOIN users u ON u.id = s.user_id \
             LEFT JOIN devices d ON d.id = s.device_id \
             WHERE s.token_hash = ? AND s.expires_at > ?"
        )))
        .bind(token_hash.as_slice())
        .bind(clock::now())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let device_id: Option<i64> = row.try_get("d_id")?;
        let device = match device_id {
            Some(_) => Some(device_from(&row, "d_")?),
            None => None,
        };
        Ok(Some(SessionRow {
            id: row.try_get("session_id")?,
            user: user_from(&row)?,
            device,
            expires_at: row.try_get("session_expires_at")?,
        }))
    }

    /// Revoca una sesión.
    pub async fn delete_session(&self, id: i64) -> DbResult<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Revoca todas las sesiones de un usuario.
    pub async fn delete_user_sessions(&self, user_id: i64) -> DbResult<()> {
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Borra las sesiones caducadas.
    pub async fn purge_expired(&self) -> DbResult<u64> {
        let result = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(clock::now())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    // --- Invitaciones --------------------------------------------------------------------------

    /// Crea una invitación.
    pub async fn create_invitation(
        &self,
        code_hash: &[u8; 32],
        hint: &str,
        note: &str,
        created_by: i64,
        expires_at: i64,
    ) -> DbResult<InvitationRow> {
        let now = clock::now();
        let result = sqlx::query(
            "INSERT INTO invitations (code_hash, hint, note, created_by, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(code_hash.as_slice())
        .bind(hint)
        .bind(note)
        .bind(created_by)
        .bind(now)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(InvitationRow {
            id: result.last_insert_rowid(),
            hint: hint.to_owned(),
            note: note.to_owned(),
            created_at: now,
            expires_at,
            used_at: None,
            used_by: None,
            revoked_at: None,
        })
    }

    /// Todas las invitaciones, de la más nueva a la más antigua.
    pub async fn list_invitations(&self) -> DbResult<Vec<InvitationRow>> {
        let rows = sqlx::query(
            "SELECT i.id, i.hint, i.note, i.created_at, i.expires_at, i.used_at, i.revoked_at, \
             u.username AS used_by FROM invitations i LEFT JOIN users u ON u.id = i.used_by \
             ORDER BY i.id DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(InvitationRow {
                    id: row.try_get("id")?,
                    hint: row.try_get("hint")?,
                    note: row.try_get("note")?,
                    created_at: row.try_get("created_at")?,
                    expires_at: row.try_get("expires_at")?,
                    used_at: row.try_get("used_at")?,
                    used_by: row.try_get("used_by")?,
                    revoked_at: row.try_get("revoked_at")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(DbError::from)
    }

    /// Revoca una invitación sin usar; `false` si no existe o ya se usó.
    pub async fn revoke_invitation(&self, id: i64) -> DbResult<bool> {
        let result = sqlx::query(
            "UPDATE invitations SET revoked_at = ? WHERE id = ? AND used_at IS NULL AND \
             revoked_at IS NULL",
        )
        .bind(clock::now())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Canjea una invitación vigente creando el usuario, todo o nada.
    pub async fn redeem_invitation(
        &self,
        code_hash: &[u8; 32],
        username: &str,
        display_name: &str,
        password_hash: &str,
    ) -> DbResult<UserRow> {
        let now = clock::now();
        let mut tx = self.pool.begin().await?;
        let invitation: Option<i64> = sqlx::query_scalar(
            "UPDATE invitations SET used_at = ? WHERE code_hash = ? AND used_at IS NULL AND \
             revoked_at IS NULL AND expires_at > ? RETURNING id",
        )
        .bind(now)
        .bind(code_hash.as_slice())
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(invitation) = invitation else {
            return Err(DbError::NotFound("invitación"));
        };
        let result = sqlx::query(
            "INSERT INTO users (username, display_name, password_hash, is_admin, disabled, \
             created_at) VALUES (?, ?, ?, 0, 0, ?)",
        )
        .bind(username)
        .bind(display_name)
        .bind(password_hash)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|err| match DbError::from(err) {
            DbError::Conflict(_) => DbError::Conflict("nombre de usuario"),
            other => other,
        })?;
        let user_id = result.last_insert_rowid();
        sqlx::query("UPDATE invitations SET used_by = ? WHERE id = ?")
            .bind(user_id)
            .bind(invitation)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.user_by_id(user_id)
            .await?
            .ok_or(DbError::NotFound("usuario recién creado"))
    }

    // --- Amigos --------------------------------------------------------------------------------

    fn pair(a: i64, b: i64) -> (i64, i64) {
        if a < b { (a, b) } else { (b, a) }
    }

    /// Si `a` y `b` son amigos.
    pub async fn are_friends(&self, a: i64, b: i64) -> DbResult<bool> {
        let (x, y) = Self::pair(a, b);
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM friendships WHERE user_a = ? AND user_b = ?")
                .bind(x)
                .bind(y)
                .fetch_one(&self.pool)
                .await?;
        Ok(count > 0)
    }

    /// Amigos de `user`.
    pub async fn friends(&self, user: i64) -> DbResult<Vec<UserRow>> {
        let rows = sqlx::query(sql(format!(
            "SELECT {USER_COLUMNS} FROM friendships f JOIN users u ON u.id = \
             CASE WHEN f.user_a = ? THEN f.user_b ELSE f.user_a END \
             WHERE f.user_a = ? OR f.user_b = ? ORDER BY u.username COLLATE NOCASE"
        )))
        .bind(user)
        .bind(user)
        .bind(user)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(user_from)
            .collect::<Result<_, _>>()
            .map_err(DbError::from)
    }

    /// Identificadores de los amigos de `user`.
    pub async fn friend_ids(&self, user: i64) -> DbResult<Vec<i64>> {
        let ids = sqlx::query_scalar(
            "SELECT CASE WHEN user_a = ? THEN user_b ELSE user_a END FROM friendships \
             WHERE user_a = ? OR user_b = ?",
        )
        .bind(user)
        .bind(user)
        .bind(user)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }

    /// Solicitudes recibidas (`incoming`) o enviadas por `user`.
    pub async fn friend_requests(
        &self,
        user: i64,
        incoming: bool,
    ) -> DbResult<Vec<FriendRequestRow>> {
        let (mine, other) = if incoming {
            ("to_user", "from_user")
        } else {
            ("from_user", "to_user")
        };
        let rows = sqlx::query(sql(format!(
            "SELECT r.id AS request_id, r.from_user, r.to_user, r.created_at AS request_created_at, \
             {USER_COLUMNS} FROM friend_requests r JOIN users u ON u.id = r.{other} \
             WHERE r.{mine} = ? ORDER BY r.id"
        )))
        .bind(user)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(FriendRequestRow {
                    id: row.try_get("request_id")?,
                    from_user: row.try_get("from_user")?,
                    to_user: row.try_get("to_user")?,
                    other: user_from(row)?,
                    created_at: row.try_get("request_created_at")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(DbError::from)
    }

    async fn befriend(tx: &mut sqlx::SqliteConnection, a: i64, b: i64) -> Result<(), sqlx::Error> {
        let (x, y) = Self::pair(a, b);
        sqlx::query(
            "DELETE FROM friend_requests WHERE (from_user = ? AND to_user = ?) OR \
                     (from_user = ? AND to_user = ?)",
        )
        .bind(a)
        .bind(b)
        .bind(b)
        .bind(a)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO friendships (user_a, user_b, created_at) VALUES (?, ?, ?)",
        )
        .bind(x)
        .bind(y)
        .bind(clock::now())
        .execute(&mut *tx)
        .await?;
        Ok(())
    }

    /// `from` pide amistad a `to`; si `to` ya se la había pedido, quedan como amigos.
    pub async fn request_friend(&self, from: i64, to: i64) -> DbResult<FriendOutcome> {
        if self.are_friends(from, to).await? {
            return Ok(FriendOutcome::AlreadyFriends);
        }
        let mut tx = self.pool.begin().await?;
        let reverse: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM friend_requests WHERE from_user = ? AND to_user = ?",
        )
        .bind(to)
        .bind(from)
        .fetch_optional(&mut *tx)
        .await?;
        if reverse.is_some() {
            Self::befriend(&mut tx, from, to).await?;
            tx.commit().await?;
            return Ok(FriendOutcome::Accepted);
        }
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO friend_requests (from_user, to_user, created_at) \
             VALUES (?, ?, ?)",
        )
        .bind(from)
        .bind(to)
        .bind(clock::now())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(if inserted == 1 {
            FriendOutcome::Requested
        } else {
            FriendOutcome::AlreadyRequested
        })
    }

    /// `user` acepta la solicitud `request`; devuelve quién la había enviado.
    pub async fn accept_friend(&self, request: i64, user: i64) -> DbResult<i64> {
        let mut tx = self.pool.begin().await?;
        let from: Option<i64> = sqlx::query_scalar(
            "SELECT from_user FROM friend_requests WHERE id = ? AND to_user = ?",
        )
        .bind(request)
        .bind(user)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(from) = from else {
            return Err(DbError::NotFound("solicitud de amistad"));
        };
        Self::befriend(&mut tx, from, user).await?;
        tx.commit().await?;
        Ok(from)
    }

    /// Rechaza (destinatario) o retira (remitente) una solicitud.
    pub async fn delete_friend_request(&self, request: i64, user: i64) -> DbResult<bool> {
        let result = sqlx::query(
            "DELETE FROM friend_requests WHERE id = ? AND (from_user = ? OR to_user = ?)",
        )
        .bind(request)
        .bind(user)
        .bind(user)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Deja de ser amigos.
    pub async fn remove_friend(&self, a: i64, b: i64) -> DbResult<bool> {
        let (x, y) = Self::pair(a, b);
        let result = sqlx::query("DELETE FROM friendships WHERE user_a = ? AND user_b = ?")
            .bind(x)
            .bind(y)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    // --- Uso del relay -------------------------------------------------------------------------

    /// Suma `bytes` al día `day` de `user`.
    pub async fn add_usage(&self, user: i64, day: i64, bytes: u64) -> DbResult<()> {
        let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT INTO relay_usage (user_id, day, bytes) VALUES (?, ?, ?) \
             ON CONFLICT (user_id, day) DO UPDATE SET bytes = bytes + excluded.bytes",
        )
        .bind(user)
        .bind(day)
        .bind(bytes)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Bytes por día de `user` entre `first` y `last` (incluidos).
    pub async fn usage_days(&self, user: i64, first: i64, last: i64) -> DbResult<Vec<(i64, u64)>> {
        let rows = sqlx::query(
            "SELECT day, bytes FROM relay_usage WHERE user_id = ? AND day BETWEEN ? AND ? \
             ORDER BY day",
        )
        .bind(user)
        .bind(first)
        .bind(last)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("day")?, as_u64(row.try_get("bytes")?))))
            .collect::<Result<_, sqlx::Error>>()
            .map_err(DbError::from)
    }

    /// Bytes de `user` entre `first` y `last` (incluidos).
    pub async fn usage_sum(&self, user: i64, first: i64, last: i64) -> DbResult<u64> {
        let sum: Option<i64> = sqlx::query_scalar(
            "SELECT SUM(bytes) FROM relay_usage WHERE user_id = ? AND day BETWEEN ? AND ?",
        )
        .bind(user)
        .bind(first)
        .bind(last)
        .fetch_one(&self.pool)
        .await?;
        Ok(as_u64(sum.unwrap_or(0)))
    }

    /// Bytes por usuario entre `first` y `last`, de más a menos.
    pub async fn usage_by_user(&self, first: i64, last: i64) -> DbResult<Vec<(i64, String, u64)>> {
        let rows = sqlx::query(
            "SELECT u.id, u.username, SUM(r.bytes) AS bytes FROM relay_usage r \
             JOIN users u ON u.id = r.user_id WHERE r.day BETWEEN ? AND ? \
             GROUP BY u.id ORDER BY bytes DESC, u.username",
        )
        .bind(first)
        .bind(last)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get("id")?,
                    row.try_get("username")?,
                    as_u64(row.try_get("bytes")?),
                ))
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(DbError::from)
    }

    /// Bytes de todos los usuarios en `day`.
    pub async fn usage_total_day(&self, day: i64) -> DbResult<u64> {
        let sum: Option<i64> =
            sqlx::query_scalar("SELECT SUM(bytes) FROM relay_usage WHERE day = ?")
                .bind(day)
                .fetch_one(&self.pool)
                .await?;
        Ok(as_u64(sum.unwrap_or(0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let db = Db::open(&dir.path().join(DATABASE_FILE))
            .await
            .expect("base de datos");
        (db, dir)
    }

    #[tokio::test]
    async fn migrations_are_idempotent_and_wal_is_on() {
        let (db, dir) = db().await;
        db.migrate().await.expect("segunda migración");
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&db.pool)
            .await
            .expect("pragma");
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        db.close().await;
        let again = Db::open(&dir.path().join(DATABASE_FILE))
            .await
            .expect("reapertura");
        assert_eq!(again.count_users().await.expect("usuarios"), 0);
    }

    #[tokio::test]
    async fn the_first_admin_is_created_once() {
        let (db, _dir) = db().await;
        let admin = db
            .create_first_admin("admin", "Admin", "hash")
            .await
            .expect("alta")
            .expect("primer administrador");
        assert!(admin.is_admin);
        assert!(
            db.create_first_admin("otro", "Otro", "hash")
                .await
                .expect("alta")
                .is_none()
        );
        assert!(matches!(
            db.create_user("ADMIN", "Admin", "hash", false).await,
            Err(DbError::Conflict("nombre de usuario"))
        ));
        assert_eq!(
            db.user_by_name("Admin").await.expect("busca").map(|u| u.id),
            Some(admin.id)
        );
    }

    #[tokio::test]
    async fn devices_belong_to_one_account_and_sessions_are_one_per_device() {
        let (db, _dir) = db().await;
        let ana = db.create_user("ana", "Ana", "h", false).await.expect("ana");
        let bea = db.create_user("bea", "Bea", "h", false).await.expect("bea");
        let device = db
            .upsert_device(ana.id, &[1; 32], "portátil")
            .await
            .expect("disp");
        let again = db
            .upsert_device(ana.id, &[1; 32], "sobremesa")
            .await
            .expect("disp");
        assert_eq!(device.id, again.id);
        assert!(matches!(
            db.upsert_device(bea.id, &[1; 32], "robado").await,
            Err(DbError::Conflict(_))
        ));
        db.create_session(&[7; 32], ana.id, Some(device.id), clock::now() + 60)
            .await
            .expect("sesión");
        db.create_session(&[8; 32], ana.id, Some(device.id), clock::now() + 60)
            .await
            .expect("sesión nueva");
        assert!(db.session(&[7; 32]).await.expect("sesión").is_none());
        let session = db
            .session(&[8; 32])
            .await
            .expect("sesión")
            .expect("vigente");
        assert_eq!(session.user.id, ana.id);
        assert_eq!(session.device.map(|d| d.public_key), Some([1; 32]));
        db.create_session(&[9; 32], ana.id, None, clock::now() - 1)
            .await
            .expect("caducada");
        assert!(db.session(&[9; 32]).await.expect("sesión").is_none());
        assert_eq!(db.purge_expired().await.expect("purga"), 1);
    }

    #[tokio::test]
    async fn invitations_are_single_use_and_expire() {
        let (db, _dir) = db().await;
        let admin = db
            .create_user("admin", "Admin", "h", true)
            .await
            .expect("admin");
        db.create_invitation(&[1; 32], "ABCD", "para Ana", admin.id, clock::now() + 60)
            .await
            .expect("invitación");
        let ana = db
            .redeem_invitation(&[1; 32], "ana", "Ana", "h")
            .await
            .expect("canje");
        assert_eq!(ana.username, "ana");
        assert!(matches!(
            db.redeem_invitation(&[1; 32], "bea", "Bea", "h").await,
            Err(DbError::NotFound(_))
        ));
        db.create_invitation(&[2; 32], "EFGH", "", admin.id, clock::now() - 1)
            .await
            .expect("caducada");
        assert!(matches!(
            db.redeem_invitation(&[2; 32], "bea", "Bea", "h").await,
            Err(DbError::NotFound(_))
        ));
        // Un nombre ocupado no gasta la invitación.
        let third = db
            .create_invitation(&[3; 32], "IJKL", "", admin.id, clock::now() + 60)
            .await
            .expect("tercera");
        assert!(matches!(
            db.redeem_invitation(&[3; 32], "ana", "Ana", "h").await,
            Err(DbError::Conflict(_))
        ));
        let list = db.list_invitations().await.expect("lista");
        let state = |id: i64| {
            list.iter()
                .find(|row| row.id == id)
                .map(|row| row.state(clock::now()))
        };
        assert_eq!(state(third.id), Some("pending"));
        assert!(db.revoke_invitation(third.id).await.expect("revocar"));
        assert!(
            !db.revoke_invitation(third.id)
                .await
                .expect("revocar dos veces")
        );
        let used = list.iter().find(|row| row.hint == "ABCD").expect("usada");
        assert_eq!(used.used_by.as_deref(), Some("ana"));
        assert_eq!(used.state(clock::now()), "used");
    }

    #[tokio::test]
    async fn friendship_needs_both_sides() {
        let (db, _dir) = db().await;
        let ana = db.create_user("ana", "Ana", "h", false).await.expect("ana");
        let bea = db.create_user("bea", "Bea", "h", false).await.expect("bea");
        assert_eq!(
            db.request_friend(ana.id, bea.id).await.expect("pide"),
            FriendOutcome::Requested
        );
        assert_eq!(
            db.request_friend(ana.id, bea.id).await.expect("repite"),
            FriendOutcome::AlreadyRequested
        );
        assert!(!db.are_friends(ana.id, bea.id).await.expect("amigos"));
        let incoming = db.friend_requests(bea.id, true).await.expect("recibidas");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].other.username, "ana");
        assert_eq!(
            db.accept_friend(incoming[0].id, bea.id)
                .await
                .expect("acepta"),
            ana.id
        );
        assert!(db.are_friends(bea.id, ana.id).await.expect("amigos"));
        assert_eq!(db.friend_ids(ana.id).await.expect("ids"), vec![bea.id]);
        assert_eq!(
            db.request_friend(bea.id, ana.id).await.expect("ya"),
            FriendOutcome::AlreadyFriends
        );
        assert!(db.remove_friend(bea.id, ana.id).await.expect("elimina"));
        assert!(!db.are_friends(ana.id, bea.id).await.expect("amigos"));
        // Pedirse a la vez es aceptarse.
        db.request_friend(ana.id, bea.id).await.expect("pide");
        assert_eq!(
            db.request_friend(bea.id, ana.id).await.expect("cruzada"),
            FriendOutcome::Accepted
        );
        assert!(
            db.friend_requests(ana.id, false)
                .await
                .expect("enviadas")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn usage_accumulates_per_day_and_backup_restores() {
        let (db, dir) = db().await;
        let ana = db.create_user("ana", "Ana", "h", false).await.expect("ana");
        db.add_usage(ana.id, 100, 10).await.expect("uso");
        db.add_usage(ana.id, 100, 5).await.expect("uso");
        db.add_usage(ana.id, 101, 7).await.expect("uso");
        assert_eq!(db.usage_sum(ana.id, 100, 101).await.expect("suma"), 22);
        assert_eq!(
            db.usage_days(ana.id, 100, 100).await.expect("días"),
            vec![(100, 15)]
        );
        assert_eq!(
            db.usage_by_user(0, 1000).await.expect("por usuario"),
            vec![(ana.id, "ana".to_owned(), 22)]
        );
        assert_eq!(db.usage_total_day(101).await.expect("día"), 7);
        let copy = dir.path().join("copia.db");
        db.backup(&copy).await.expect("copia");
        assert!(matches!(db.backup(&copy).await, Err(DbError::Conflict(_))));
        let restored = Db::open(&copy).await.expect("copia restaurada");
        assert_eq!(
            restored
                .user_by_name("ana")
                .await
                .expect("busca")
                .map(|u| u.id),
            Some(ana.id)
        );
        assert_eq!(restored.usage_sum(ana.id, 0, 1000).await.expect("suma"), 22);
    }
}
