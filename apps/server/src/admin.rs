//! Administración: la interfaz `/admin` (HTML, CSS y JS embebidos en el binario, sin
//! dependencias externas) y las rutas `/api/v1/admin/...`, que exigen una sesión de un
//! administrador (la web de `/admin`, sin dispositivo, o la de la app).

use http::{Method, StatusCode};

use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::api::{
    AdminUsage, AdminUser, CreateInvitation, CreateUser, CreatedInvitation, Invitation, RoomInfo,
    Rooms, SetPassword, Settings, UpdateUser, UserUsage,
};

use crate::ServerState;
use crate::api::{
    Handled, as_u64, bad_request, body, created, db_error, fail, id, internal, ok, session,
    user_dto, validate_new_user,
};
use crate::auth::{self, INVITATION_MAX_HOURS, INVITATION_TTL_HOURS};
use crate::clock;
use crate::config::Registration;
use crate::db::{InvitationRow, SessionRow};
use crate::routes::{Request, Response, SERVER_VERSION};

const INDEX_HTML: &str = include_str!("../static/admin/index.html");
const ADMIN_CSS: &str = include_str!("../static/admin/admin.css");
const ADMIN_JS: &str = include_str!("../static/admin/admin.js");

/// Cabeceras de la interfaz: nada de fuera (CSP), sin marcos ni referer.
const STATIC_HEADERS: &[(&str, &str)] = &[
    (
        "content-security-policy",
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
         img-src 'self' data: blob:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    ),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-frame-options", "DENY"),
    ("cache-control", "no-cache"),
    crate::routes::HSTS,
];

/// Ficheros estáticos de `/admin`.
pub(crate) fn static_file(request: &Request) -> Response {
    if request.method != Method::GET && request.method != Method::HEAD {
        return Response::not_found();
    }
    let (content_type, body) = match request.path.as_str() {
        "/admin" | "/admin/" | "/admin/index.html" => ("text/html; charset=utf-8", INDEX_HTML),
        "/admin/admin.css" => ("text/css; charset=utf-8", ADMIN_CSS),
        "/admin/admin.js" => ("text/javascript; charset=utf-8", ADMIN_JS),
        _ => return Response::not_found(),
    };
    Response {
        status: StatusCode::OK,
        content_type,
        headers: STATIC_HEADERS.to_vec(),
        etag: None,
        body: body.as_bytes().to_vec(),
    }
}

async fn require_admin(state: &ServerState, request: &Request) -> Result<SessionRow, Response> {
    let session = session(state, request).await?;
    if !session.user.is_admin {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "admin_required",
            "solo un administrador puede hacer esto",
        ));
    }
    Ok(session)
}

/// Atiende `/api/v1/admin/{segments}`.
pub(crate) async fn handle(state: &ServerState, request: &Request, segments: &[&str]) -> Handled {
    let admin = require_admin(state, request).await?;
    match (&request.method, segments) {
        (&Method::GET, ["users"]) => users(state).await,
        (&Method::POST, ["users"]) => create_user(state, request).await,
        (&Method::PATCH, ["users", user]) => update_user(state, request, &admin, user).await,
        (&Method::POST, ["users", user, "password"]) => set_password(state, request, user).await,
        (&Method::DELETE, ["users", user, "avatar"]) => delete_avatar(state, user).await,
        (&Method::GET, ["invitations"]) => invitations(state).await,
        (&Method::POST, ["invitations"]) => create_invitation(state, request, &admin).await,
        (&Method::DELETE, ["invitations", invitation]) => {
            revoke_invitation(state, invitation).await
        }
        (&Method::GET, ["usage"]) => usage(state, request).await,
        (&Method::GET, ["rooms"]) => rooms(state),
        (&Method::GET, ["settings"]) => settings(state),
        _ => Err(Response::not_found()),
    }
}

async fn admin_user(state: &ServerState, id: i64) -> Result<AdminUser, Response> {
    let rows = state.db.list_users().await.map_err(db_error)?;
    rows.into_iter()
        .find(|row| row.user.id == id)
        .map(|row| admin_user_dto(state, row))
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "not_found", "no existe ese usuario"))
}

fn admin_user_dto(state: &ServerState, row: crate::db::AdminUserRow) -> AdminUser {
    AdminUser {
        id: as_u64(row.user.id),
        online: state.hub.is_online(row.user.id),
        username: row.user.username,
        display_name: row.user.display_name,
        is_admin: row.user.is_admin,
        disabled: row.user.disabled,
        created_at: row.user.created_at,
        devices: as_u64(row.devices),
        last_seen_at: row.last_seen_at,
        avatar: row.user.avatar,
    }
}

async fn users(state: &ServerState) -> Handled {
    let rows = state.db.list_users().await.map_err(db_error)?;
    let users: Vec<AdminUser> = rows
        .into_iter()
        .map(|row| admin_user_dto(state, row))
        .collect();
    ok(&serde_json::json!({ "users": users }))
}

async fn create_user(state: &ServerState, request: &Request) -> Handled {
    let create: CreateUser = body(request)?;
    let (username, display_name) = validate_new_user(
        &create.username,
        create.display_name.as_deref(),
        &create.password,
    )?;
    let password = create.password;
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    let user = state
        .db
        .create_user(&username, &display_name, &hash, create.is_admin)
        .await
        .map_err(|err| match err {
            crate::db::DbError::Conflict(_) => fail(
                StatusCode::CONFLICT,
                "username_taken",
                format!("el nombre de usuario «{username}» ya existe"),
            ),
            other => db_error(other),
        })?;
    tracing::info!(user = user.id, "usuario creado por el administrador");
    created(&user_dto(&user))
}

async fn update_user(
    state: &ServerState,
    request: &Request,
    admin: &SessionRow,
    segment: &str,
) -> Handled {
    let user_id = id(segment)?;
    let update: UpdateUser = body(request)?;
    let display_name = match update.display_name.as_deref() {
        Some(name) => Some(auth::validate_display_name(name).map_err(bad_request)?),
        None => None,
    };
    let target = state
        .db
        .user_by_id(user_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "not_found", "no existe ese usuario"))?;
    if user_id == admin.user.id && update.disabled == Some(true) {
        return Err(bad_request("no puedes deshabilitar tu propia cuenta"));
    }
    let loses_admin = target.is_admin
        && !target.disabled
        && (update.is_admin == Some(false) || update.disabled == Some(true));
    if loses_admin && state.db.count_active_admins().await.map_err(db_error)? <= 1 {
        return Err(fail(
            StatusCode::CONFLICT,
            "last_admin",
            "es el último administrador habilitado: nombra antes a otro",
        ));
    }
    state
        .db
        .update_user(
            user_id,
            display_name.as_deref(),
            update.is_admin,
            update.disabled,
        )
        .await
        .map_err(db_error)?;
    if update.disabled == Some(true) {
        state
            .db
            .delete_user_sessions(user_id)
            .await
            .map_err(db_error)?;
        state.hub.disconnect_user(user_id);
    }
    ok(&admin_user(state, user_id).await?)
}

async fn set_password(state: &ServerState, request: &Request, segment: &str) -> Handled {
    let user_id = id(segment)?;
    let set: SetPassword = body(request)?;
    auth::validate_password(&set.password).map_err(bad_request)?;
    let password = set.password;
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    state
        .db
        .set_password(user_id, &hash)
        .await
        .map_err(db_error)?;
    state.hub.disconnect_user(user_id);
    ok(&serde_json::json!({}))
}

/// Quita el avatar de otro usuario (moderación): lo dejan de ver todos en ese momento.
async fn delete_avatar(state: &ServerState, segment: &str) -> Handled {
    let user_id = id(segment)?;
    if state.db.delete_avatar(user_id).await.map_err(db_error)? {
        tracing::info!(user = user_id, "avatar quitado por un administrador");
        state.hub.avatar_changed(user_id).await;
    }
    ok(&admin_user(state, user_id).await?)
}

fn invitation_dto(row: &InvitationRow, now: i64) -> Invitation {
    Invitation {
        id: as_u64(row.id),
        hint: row.hint.clone(),
        note: row.note.clone(),
        created_at: row.created_at,
        expires_at: row.expires_at,
        state: row.state(now).to_owned(),
        used_by: row.used_by.clone(),
    }
}

async fn invitations(state: &ServerState) -> Handled {
    let now = clock::now();
    let rows = state.db.list_invitations().await.map_err(db_error)?;
    let invitations: Vec<Invitation> = rows.iter().map(|row| invitation_dto(row, now)).collect();
    ok(&serde_json::json!({ "invitations": invitations }))
}

async fn create_invitation(state: &ServerState, request: &Request, admin: &SessionRow) -> Handled {
    if state.config.registration == Registration::Admin {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "invitations_disabled",
            "las invitaciones están desactivadas (COROOM_REGISTRATION=admin): crea el usuario \
             directamente",
        ));
    }
    let create: CreateInvitation = if request.body.is_empty() {
        CreateInvitation::default()
    } else {
        body(request)?
    };
    let hours = create.hours.unwrap_or(INVITATION_TTL_HOURS);
    if !(1..=INVITATION_MAX_HOURS).contains(&hours) {
        return Err(bad_request(format!(
            "la validez debe estar entre 1 y {INVITATION_MAX_HOURS} horas"
        )));
    }
    let note: String = create
        .note
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    let code = auth::new_invitation_code().ok_or_else(|| internal("sin aleatoriedad"))?;
    let hint: String = code
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let expires_at = clock::now() + i64::from(hours) * 3600;
    let row = state
        .db
        .create_invitation(
            &auth::invitation_hash(&code),
            &hint,
            &note,
            admin.user.id,
            expires_at,
        )
        .await
        .map_err(db_error)?;
    tracing::info!(invitation = row.id, "invitación creada");
    created(&CreatedInvitation {
        code,
        invitation: invitation_dto(&row, clock::now()),
    })
}

async fn revoke_invitation(state: &ServerState, segment: &str) -> Handled {
    let invitation = id(segment)?;
    if !state
        .db
        .revoke_invitation(invitation)
        .await
        .map_err(db_error)?
    {
        return Err(fail(
            StatusCode::NOT_FOUND,
            "not_found",
            "no existe esa invitación o ya se usó",
        ));
    }
    ok(&serde_json::json!({}))
}

async fn usage(state: &ServerState, request: &Request) -> Handled {
    let today = clock::day_of(clock::now());
    let day = match request.query_param("month") {
        Some(month) => {
            clock::parse_month(month).ok_or_else(|| bad_request("el mes debe ir como AAAA-MM"))?
        }
        None => today,
    };
    let (first, last) = clock::month_range(day);
    let mut totals: std::collections::HashMap<i64, (String, u64)> = state
        .db
        .usage_by_user(first, last)
        .await
        .map_err(db_error)?
        .into_iter()
        .map(|(id, name, bytes)| (id, (name, bytes)))
        .collect();
    for (user, bytes) in state.quota.pending_between(first, last) {
        match totals.get_mut(&user) {
            Some((_, total)) => *total += bytes,
            None => {
                let name = state
                    .db
                    .user_by_id(user)
                    .await
                    .map_err(db_error)?
                    .map(|user| user.username)
                    .unwrap_or_default();
                totals.insert(user, (name, bytes));
            }
        }
    }
    let mut users: Vec<UserUsage> = totals
        .into_iter()
        .map(|(user_id, (username, bytes))| UserUsage {
            user_id: as_u64(user_id),
            username,
            bytes,
        })
        .collect();
    users.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.username.cmp(&b.username)));
    ok(&AdminUsage {
        month: clock::month_label(day),
        quota_bytes: state.quota.quota_bytes(),
        total: users.iter().map(|user| user.bytes).sum(),
        users,
    })
}

fn rooms(state: &ServerState) -> Handled {
    let counters = state.relay.counters();
    let rooms = state
        .hub
        .room_summaries()
        .into_iter()
        .map(|room| {
            let counters = counters.get(&room.id).copied().unwrap_or_default();
            RoomInfo {
                guests: room.guests as u64,
                relayed: room.relay,
                relay_bytes: counters.bytes,
                relay_dropped: counters.dropped,
            }
        })
        .collect();
    ok(&Rooms { rooms })
}

fn settings(state: &ServerState) -> Handled {
    let config = &state.config;
    let (udp_reachable, udp_checked_at) = state.udp.get();
    ok(&Settings {
        version: SERVER_VERSION.to_owned(),
        protocol: PROTOCOL_VERSION,
        source_url: config.source_url.clone(),
        public_host: config.public_host.clone(),
        public_udp_port: state.public_udp_port(),
        public_tcp_port: state.public_tcp_port(),
        tls_mode: config.tls_mode.to_string(),
        certificate_fingerprint: state.certs.fingerprint(),
        certificate_not_after: state
            .certs
            .validity()
            .map(|(_, not_after)| clock::rfc3339(not_after)),
        registration: config.registration.to_string(),
        relay_enabled: config.relay_enabled,
        relay_quota_gb_per_month: config.relay_quota_gb_per_month,
        relay_max_kbps: config.relay_max_kbps,
        max_guests_per_room: config.max_guests_per_room,
        udp_reachable,
        udp_checked_at: udp_checked_at.map(clock::rfc3339),
        dev_mode: config.dev_token.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_files_are_embedded_with_a_strict_policy() {
        let index = static_file(&Request::get("/admin/"));
        assert_eq!(index.status, StatusCode::OK);
        assert!(index.content_type.starts_with("text/html"));
        let html = String::from_utf8(index.body).expect("UTF-8");
        assert!(html.contains("lang=\"es\""), "en español");
        assert!(
            !html.contains("http://") && !html.contains("https://"),
            "sin CDN"
        );
        assert!(
            index
                .headers
                .iter()
                .any(|(name, value)| *name == "content-security-policy"
                    && value.contains("default-src 'none'"))
        );
        assert_eq!(
            static_file(&Request::get("/admin/admin.css")).status,
            StatusCode::OK
        );
        assert_eq!(
            static_file(&Request::get("/admin/admin.js")).status,
            StatusCode::OK
        );
        assert_eq!(
            static_file(&Request::get("/admin/../secreto")).status,
            StatusCode::NOT_FOUND
        );
        let css =
            String::from_utf8(static_file(&Request::get("/admin/admin.css")).body).expect("UTF-8");
        assert!(css.contains("color-scheme: light dark"));
        assert!(css.contains("system-ui"));
    }

    /// Revisión de `/admin` (`docs/seguridad.md`, capa «Administración»): cabeceras completas, sin
    /// cookies ni `innerHTML` en la interfaz y el token solo en `sessionStorage` y en
    /// `Authorization`, así que no hay CSRF que explotar.
    #[test]
    fn the_admin_page_has_every_security_header_and_no_cookies() {
        let index = static_file(&Request::get("/admin/"));
        for name in [
            "content-security-policy",
            "x-frame-options",
            "referrer-policy",
            "x-content-type-options",
            "strict-transport-security",
        ] {
            assert!(
                index.headers.iter().any(|(header, _)| *header == name),
                "falta {name}"
            );
        }
        let csp = index
            .headers
            .iter()
            .find(|(name, _)| *name == "content-security-policy")
            .map(|(_, value)| *value)
            .unwrap_or_default();
        for directive in [
            "script-src 'self'",
            "frame-ancestors 'none'",
            "form-action 'none'",
            "base-uri 'none'",
        ] {
            assert!(csp.contains(directive), "CSP sin {directive}: {csp}");
        }
        assert!(!csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"));
        assert!(
            !index
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        );
        assert!(!ADMIN_JS.contains("document.cookie"));
        assert!(!ADMIN_JS.contains("innerHTML") && !ADMIN_JS.contains("outerHTML"));
        assert!(
            !ADMIN_JS.contains("localStorage"),
            "el token no sobrevive a la pestaña"
        );
        assert!(ADMIN_JS.contains("sessionStorage"));
        assert!(ADMIN_JS.contains("Authorization"));
        assert!(!INDEX_HTML.contains("<script>") && !INDEX_HTML.contains("style="));
    }
}
