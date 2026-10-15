//! API `/api/v1`: primer administrador, login con desafío de dispositivo, sesión,
//! invitaciones, amigos y uso del relay. Las rutas de administración están en [`crate::admin`].
//!
//! Errores uniformes `{ "error", "message" }` en español; `Authorization: Bearer <token>`. Ningún
//! registro incluye tokens, contraseñas ni claves.

use http::{Method, StatusCode};
use serde::de::DeserializeOwned;
use tracing::{info, warn};

use coroom_transport::crypto::{sha256_fingerprint, verify_signature};
use coroom_transport::proto::api::{
    AddFriend, AddFriendResult, DayUsage, Device, Devices, Friend, FriendRequest, Friends,
    LoginChallenge, LoginComplete, LoginRequest, Me, RedeemRequest, SessionToken, SetupRequest,
    SetupStatus, Usage, User, challenge_message,
};
use coroom_transport::proto::hex;
use coroom_transport::proto::signal::Activity;

use crate::ServerState;
use crate::auth::{self, CHALLENGE_TTL, SESSION_TTL, WEB_SESSION_TTL};
use crate::clock;
use crate::config::Registration;
use crate::db::{DbError, DeviceRow, FriendOutcome, SessionRow, UserRow};
use crate::routes::{Request, Response, SERVER_VERSION};

/// Resultado de un manejador: la respuesta buena o la de error.
pub(crate) type Handled = Result<Response, Response>;

pub(crate) fn ok<T: serde::Serialize + ?Sized>(body: &T) -> Handled {
    Ok(Response::json(StatusCode::OK, body))
}

pub(crate) fn created<T: serde::Serialize + ?Sized>(body: &T) -> Handled {
    Ok(Response::json(StatusCode::CREATED, body))
}

pub(crate) fn fail(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    Response::error(status, code, message)
}

pub(crate) fn bad_request(message: impl Into<String>) -> Response {
    fail(StatusCode::BAD_REQUEST, "bad_request", message)
}

pub(crate) fn internal(err: impl std::fmt::Display) -> Response {
    warn!(error = %err, "error interno de la API");
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "error interno del servidor; mira su registro",
    )
}

pub(crate) fn db_error(err: DbError) -> Response {
    match err {
        DbError::Conflict(what) => fail(
            StatusCode::CONFLICT,
            "conflict",
            format!("ya existe: {what}"),
        ),
        DbError::NotFound(what) => fail(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no existe: {what}"),
        ),
        other => internal(other),
    }
}

/// Deserializa el cuerpo JSON.
pub(crate) fn body<T: DeserializeOwned>(request: &Request) -> Result<T, Response> {
    serde_json::from_slice(&request.body)
        .map_err(|err| bad_request(format!("el cuerpo JSON no es válido: {err}")))
}

/// Identificador de la ruta.
pub(crate) fn id(segment: &str) -> Result<i64, Response> {
    segment
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| bad_request(format!("«{segment}» no es un identificador")))
}

pub(crate) fn as_u64(id: i64) -> u64 {
    u64::try_from(id).unwrap_or(0)
}

pub(crate) fn user_dto(user: &UserRow) -> User {
    User {
        id: as_u64(user.id),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        is_admin: user.is_admin,
    }
}

fn device_dto(device: &DeviceRow) -> Device {
    Device {
        id: as_u64(device.id),
        name: device.name.clone(),
        fingerprint: sha256_fingerprint(&device.public_key),
        created_at: device.created_at,
        last_seen_at: device.last_seen_at,
    }
}

fn friend_dto(state: &ServerState, user: &UserRow, presence: bool) -> Friend {
    Friend {
        id: as_u64(user.id),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        online: presence && state.hub.is_online(user.id),
        activity: if presence {
            state.hub.activity(user.id)
        } else {
            Activity::Idle
        },
    }
}

/// Sesión del token de `Authorization: Bearer`.
pub(crate) async fn session(
    state: &ServerState,
    request: &Request,
) -> Result<SessionRow, Response> {
    let unauthorized = || {
        fail(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "falta la sesión o ha caducado: inicia sesión de nuevo",
        )
    };
    let token = request
        .authorization
        .as_deref()
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(unauthorized)?;
    let session = state
        .db
        .session(&auth::token_hash(token))
        .await
        .map_err(db_error)?
        .ok_or_else(unauthorized)?;
    if session.user.disabled {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "disabled",
            "tu cuenta está deshabilitada; habla con el administrador del servidor",
        ));
    }
    Ok(session)
}

async fn hash(password: String) -> Result<String, Response> {
    tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(internal)?
        .map_err(internal)
}

async fn verify(password: String, phc: Option<String>) -> Result<bool, Response> {
    tokio::task::spawn_blocking(move || match phc {
        Some(phc) => auth::verify_password(&password, &phc),
        None => {
            auth::dummy_verify(&password);
            false
        }
    })
    .await
    .map_err(internal)
}

/// Nombre, nombre visible y contraseña de un alta, validados.
pub(crate) fn validate_new_user(
    username: &str,
    display_name: Option<&str>,
    password: &str,
) -> Result<(String, String), Response> {
    let username = auth::validate_username(username).map_err(bad_request)?;
    let display_name = match display_name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => auth::validate_display_name(name).map_err(bad_request)?,
        None => username.clone(),
    };
    auth::validate_password(password).map_err(bad_request)?;
    Ok((username, display_name))
}

/// Crea un token de sesión.
async fn issue_session(
    state: &ServerState,
    user: &UserRow,
    device: Option<i64>,
    ttl: std::time::Duration,
) -> Handled {
    let (token, token_hash) = auth::new_token().ok_or_else(|| internal("sin aleatoriedad"))?;
    let expires_at = clock::now() + i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX / 2);
    state
        .db
        .create_session(&token_hash, user.id, device, expires_at)
        .await
        .map_err(db_error)?;
    ok(&SessionToken {
        token,
        expires_at,
        user: user_dto(user),
    })
}

/// Atiende `/api/v1{rest}`.
pub(crate) async fn handle(state: &ServerState, request: &Request, rest: &str) -> Response {
    let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    let result = match (&request.method, segments.as_slice()) {
        (&Method::GET, ["setup"]) => setup_status(state).await,
        (&Method::POST, ["setup"]) => setup(state, request).await,
        (&Method::POST, ["auth", "login"]) => login(state, request).await,
        (&Method::POST, ["auth", "login", "complete"]) => login_complete(state, request).await,
        (&Method::POST, ["auth", "logout"]) => logout(state, request).await,
        (&Method::GET, ["me"]) => me(state, request).await,
        (&Method::GET, ["me", "devices"]) => devices(state, request).await,
        (&Method::DELETE, ["me", "devices", id]) => delete_device(state, request, id).await,
        (&Method::POST, ["invitations", "redeem"]) => redeem(state, request).await,
        (&Method::GET, ["friends"]) => friends(state, request).await,
        (&Method::POST, ["friends"] | ["friends", "requests"]) => add_friend(state, request).await,
        (&Method::POST, ["friends", "requests", id, "accept"]) => {
            accept_friend(state, request, id).await
        }
        (&Method::DELETE, ["friends", "requests", id]) => decline_friend(state, request, id).await,
        (&Method::DELETE, ["friends", id]) => remove_friend(state, request, id).await,
        (&Method::GET, ["usage"]) => usage(state, request).await,
        (_, ["admin", ..]) => crate::admin::handle(state, request, &segments[1..]).await,
        _ => Err(Response::not_found()),
    };
    result.unwrap_or_else(|response| response)
}

async fn setup_status(state: &ServerState) -> Handled {
    ok(&SetupStatus {
        needed: !state.db.has_admin().await.map_err(db_error)?,
        server_host: state.config.public_host.clone(),
        version: SERVER_VERSION.to_owned(),
    })
}

async fn setup(state: &ServerState, request: &Request) -> Handled {
    let already = || {
        fail(
            StatusCode::CONFLICT,
            "already_setup",
            "el servidor ya tiene administrador: inicia sesión",
        )
    };
    if state.db.has_admin().await.map_err(db_error)? {
        return Err(already());
    }
    let setup: SetupRequest = body(request)?;
    let (username, display_name) = validate_new_user(
        &setup.username,
        setup.display_name.as_deref(),
        &setup.password,
    )?;
    let password_hash = hash(setup.password).await?;
    let user = state
        .db
        .create_first_admin(&username, &display_name, &password_hash)
        .await
        .map_err(db_error)?
        .ok_or_else(already)?;
    info!(user = user.id, "primer administrador creado");
    created(&user_dto(&user))
}

fn too_many() -> Response {
    fail(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many_attempts",
        "demasiados intentos fallidos; espera un minuto y vuelve a probar",
    )
}

fn invalid_credentials() -> Response {
    fail(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        "usuario o contraseña incorrectos",
    )
}

async fn login(state: &ServerState, request: &Request) -> Handled {
    let login: LoginRequest = body(request)?;
    if state.auth.limited(&login.username, request.remote) {
        return Err(too_many());
    }
    let user = state
        .db
        .user_by_name(login.username.trim())
        .await
        .map_err(db_error)?;
    let phc = user.as_ref().map(|user| user.password_hash.clone());
    let good = verify(login.password.clone(), phc).await?;
    let Some(user) = user.filter(|user| good && !user.disabled) else {
        state.auth.failed(&login.username, request.remote);
        return Err(invalid_credentials());
    };
    let Some(public_key) = login.public_key.as_deref() else {
        if !user.is_admin {
            return Err(fail(
                StatusCode::FORBIDDEN,
                "device_required",
                "inicia sesión desde la app coroom (`coroom login`): la interfaz web es solo \
                 para administradores",
            ));
        }
        info!(user = user.id, "sesión web de administración");
        return issue_session(state, &user, None, WEB_SESSION_TTL).await;
    };
    let public_key: [u8; 32] = hex::decode_array(public_key)
        .ok_or_else(|| bad_request("la clave pública debe ser Ed25519 en hexadecimal"))?;
    let device_name = auth::clean_device_name(login.device_name.as_deref());
    let (challenge_id, challenge) = state
        .auth
        .issue_challenge(user.id, public_key, device_name)
        .ok_or_else(|| internal("no se pudo crear el desafío"))?;
    ok(&LoginChallenge {
        challenge_id,
        challenge: hex::encode(&challenge),
        server_host: state.config.public_host.clone(),
        expires_in: CHALLENGE_TTL.as_secs(),
    })
}

async fn login_complete(state: &ServerState, request: &Request) -> Handled {
    let complete: LoginComplete = body(request)?;
    let pending = state
        .auth
        .take_challenge(&complete.challenge_id)
        .ok_or_else(|| {
            fail(
                StatusCode::UNAUTHORIZED,
                "challenge_expired",
                "el desafío de inicio de sesión no existe o ha caducado; vuelve a empezar",
            )
        })?;
    let signature: [u8; 64] = hex::decode_array(&complete.signature)
        .ok_or_else(|| bad_request("la firma debe ser Ed25519 en hexadecimal"))?;
    let message = challenge_message(&state.config.public_host, &pending.challenge);
    if verify_signature(&pending.public_key, &message, &signature).is_err() {
        warn!(user = pending.user_id, "firma de dispositivo no válida");
        return Err(fail(
            StatusCode::UNAUTHORIZED,
            "invalid_signature",
            "la firma del dispositivo no es válida",
        ));
    }
    let user = state
        .db
        .user_by_id(pending.user_id)
        .await
        .map_err(db_error)?
        .filter(|user| !user.disabled)
        .ok_or_else(invalid_credentials)?;
    let device = state
        .db
        .upsert_device(user.id, &pending.public_key, &pending.device_name)
        .await
        .map_err(|err| match err {
            DbError::Conflict(_) => fail(
                StatusCode::CONFLICT,
                "device_in_use",
                "esta identidad ya está vinculada a otra cuenta; usa otro directorio de datos \
                 (--data-dir) para esta cuenta",
            ),
            other => db_error(other),
        })?;
    info!(user = user.id, device = device.id, "dispositivo vinculado");
    issue_session(state, &user, Some(device.id), SESSION_TTL).await
}

async fn logout(state: &ServerState, request: &Request) -> Handled {
    let session = session(state, request).await?;
    state
        .db
        .delete_session(session.id)
        .await
        .map_err(db_error)?;
    ok(&serde_json::json!({}))
}

async fn me(state: &ServerState, request: &Request) -> Handled {
    let session = session(state, request).await?;
    ok(&Me {
        user: user_dto(&session.user),
        device: session.device.as_ref().map(device_dto),
        expires_at: session.expires_at,
    })
}

async fn devices(state: &ServerState, request: &Request) -> Handled {
    let session = session(state, request).await?;
    let devices = state.db.devices(session.user.id).await.map_err(db_error)?;
    ok(&Devices {
        current: session.device.as_ref().map(|device| as_u64(device.id)),
        devices: devices.iter().map(device_dto).collect(),
    })
}

async fn delete_device(state: &ServerState, request: &Request, segment: &str) -> Handled {
    let session = session(state, request).await?;
    let device = id(segment)?;
    if !state
        .db
        .delete_device(session.user.id, device)
        .await
        .map_err(db_error)?
    {
        return Err(fail(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "ese dispositivo no es de tu cuenta",
        ));
    }
    ok(&serde_json::json!({}))
}

async fn redeem(state: &ServerState, request: &Request) -> Handled {
    if state.config.registration == Registration::Admin {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "invitations_disabled",
            "este servidor no usa invitaciones: pide la cuenta al administrador",
        ));
    }
    let redeem: RedeemRequest = body(request)?;
    // Los códigos fallidos cuentan como intentos de la IP.
    if state.auth.limited_ip(request.remote) {
        return Err(too_many());
    }
    let (username, display_name) = validate_new_user(
        &redeem.username,
        redeem.display_name.as_deref(),
        &redeem.password,
    )?;
    let password_hash = hash(redeem.password).await?;
    let user = state
        .db
        .redeem_invitation(
            &auth::invitation_hash(&redeem.code),
            &username,
            &display_name,
            &password_hash,
        )
        .await
        .map_err(|err| match err {
            DbError::NotFound(_) => {
                state.auth.failed_ip(request.remote);
                fail(
                    StatusCode::NOT_FOUND,
                    "invalid_invitation",
                    "el código de invitación no es válido, ha caducado o ya se usó",
                )
            }
            DbError::Conflict(_) => fail(
                StatusCode::CONFLICT,
                "username_taken",
                format!("el nombre de usuario «{username}» ya existe; elige otro"),
            ),
            other => db_error(other),
        })?;
    info!(user = user.id, "invitación canjeada");
    created(&user_dto(&user))
}

async fn friends(state: &ServerState, request: &Request) -> Handled {
    let session = session(state, request).await?;
    let user = session.user.id;
    let friends = state.db.friends(user).await.map_err(db_error)?;
    let request_dto = |row: &crate::db::FriendRequestRow| FriendRequest {
        id: as_u64(row.id),
        user: friend_dto(state, &row.other, false),
        created_at: row.created_at,
    };
    let incoming = state
        .db
        .friend_requests(user, true)
        .await
        .map_err(db_error)?;
    let outgoing = state
        .db
        .friend_requests(user, false)
        .await
        .map_err(db_error)?;
    ok(&Friends {
        friends: friends
            .iter()
            .map(|friend| friend_dto(state, friend, true))
            .collect(),
        incoming: incoming.iter().map(request_dto).collect(),
        outgoing: outgoing.iter().map(request_dto).collect(),
    })
}

async fn add_friend(state: &ServerState, request: &Request) -> Handled {
    let session = session(state, request).await?;
    let add: AddFriend = body(request)?;
    let other = state
        .db
        .user_by_name(add.username.trim())
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            fail(
                StatusCode::NOT_FOUND,
                "user_not_found",
                format!("no existe el usuario «{}»", add.username.trim()),
            )
        })?;
    if other.id == session.user.id {
        return Err(bad_request("no puedes ser tu propio amigo"));
    }
    let outcome = state
        .db
        .request_friend(session.user.id, other.id)
        .await
        .map_err(db_error)?;
    let accepted = !matches!(outcome, FriendOutcome::Requested);
    created(&AddFriendResult {
        accepted,
        user: friend_dto(state, &other, accepted),
    })
}

async fn accept_friend(state: &ServerState, request: &Request, segment: &str) -> Handled {
    let session = session(state, request).await?;
    let request_id = id(segment)?;
    let from = state
        .db
        .accept_friend(request_id, session.user.id)
        .await
        .map_err(db_error)?;
    let other = state
        .db
        .user_by_id(from)
        .await
        .map_err(db_error)?
        .ok_or_else(Response::not_found)?;
    ok(&AddFriendResult {
        accepted: true,
        user: friend_dto(state, &other, true),
    })
}

async fn decline_friend(state: &ServerState, request: &Request, segment: &str) -> Handled {
    let session = session(state, request).await?;
    let request_id = id(segment)?;
    if !state
        .db
        .delete_friend_request(request_id, session.user.id)
        .await
        .map_err(db_error)?
    {
        return Err(fail(
            StatusCode::NOT_FOUND,
            "not_found",
            "no existe esa solicitud de amistad",
        ));
    }
    ok(&serde_json::json!({}))
}

async fn remove_friend(state: &ServerState, request: &Request, segment: &str) -> Handled {
    let session = session(state, request).await?;
    let other = id(segment)?;
    if !state
        .db
        .remove_friend(session.user.id, other)
        .await
        .map_err(db_error)?
    {
        return Err(fail(
            StatusCode::NOT_FOUND,
            "not_friends",
            "ese usuario no está en tu lista de amigos",
        ));
    }
    ok(&serde_json::json!({}))
}

async fn usage(state: &ServerState, request: &Request) -> Handled {
    let session = session(state, request).await?;
    let user = session.user.id;
    let today = clock::day_of(clock::now());
    let (first, last) = clock::month_range(today);
    let mut days: std::collections::BTreeMap<i64, u64> = state
        .db
        .usage_days(user, first, last)
        .await
        .map_err(db_error)?
        .into_iter()
        .collect();
    for (day, bytes) in state.quota.pending_of(user) {
        if (first..=last).contains(&day) {
            *days.entry(day).or_insert(0) += bytes;
        }
    }
    ok(&Usage {
        month: clock::month_label(today),
        bytes: days.values().sum(),
        quota_bytes: state.quota.quota_bytes(),
        relay_enabled: state.config.relay_enabled,
        days: days
            .into_iter()
            .map(|(day, bytes)| DayUsage {
                day: clock::day_label(day),
                bytes,
            })
            .collect(),
    })
}
