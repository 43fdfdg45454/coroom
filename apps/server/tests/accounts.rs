//! Cuentas con el servidor real (puertos efímeros, SQLite temporal): primer
//! administrador, login con desafío de dispositivo (firma válida e inválida), límite de intentos,
//! invitaciones (un solo uso, caducidad, revocación, modo `admin`), amigos y presencia,
//! invitaciones a salas solo entre amigos, señalización que rechaza tokens de otro dispositivo,
//! cuota mensual del relay con `QuotaExceeded`, administración, `/health` y `backup`.

mod common;

use std::time::{Duration, Instant};

use bytes::Bytes;
use common::{ADMIN, PASSWORD, TIMEOUT, next, next_matching, start};
use coroom_server::config::Registration;
use coroom_transport::api::ApiClient;
use coroom_transport::crypto::Identity;
use coroom_transport::proto::api::{
    CreateInvitation, LoginChallenge, LoginComplete, LoginRequest, UpdateUser, challenge_message,
};
use coroom_transport::proto::control::Role;
use coroom_transport::proto::hex;
use coroom_transport::proto::media::{ObjectHeader, Track};
use coroom_transport::proto::signal::{ErrorCode, SignalMessage};
use coroom_transport::{RelayInfo, SignalClient, TransportError, connect};
use http::Method;
use serde_json::{Value, json};

/// Código de error de la API.
fn api_code(result: Result<impl std::fmt::Debug, TransportError>) -> (u16, String) {
    match result {
        Err(TransportError::Api { status, code, .. }) => (status, code),
        other => panic!("se esperaba un error de la API: {other:?}"),
    }
}

async fn raw(api: &ApiClient, method: Method, path: &str, body: Value) -> (u16, Value) {
    let response = api
        .raw(method, path, Some(serde_json::to_vec(&body).expect("JSON")))
        .await
        .expect("respuesta");
    (
        response.status.as_u16(),
        serde_json::from_slice(&response.body).unwrap_or(Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_admin_is_created_once_from_setup() {
    let server = start(|_| {}).await;
    let api = server.api();
    assert!(api.setup_status().await.expect("estado").needed);
    assert_eq!(
        api_code(api.setup(ADMIN, "corta").await),
        (400, "bad_request".to_owned()),
        "contraseña de menos de 12"
    );
    let admin = api
        .setup(ADMIN, PASSWORD)
        .await
        .expect("primer administrador");
    assert!(admin.is_admin);
    assert_eq!(
        api_code(api.setup("otro", PASSWORD).await),
        (409, "already_setup".to_owned())
    );
    assert!(!api.setup_status().await.expect("estado").needed);

    // Por HTTP/3 y por el respaldo TCP con TLS responde lo mismo.
    let tcp = server.api().tcp_only();
    assert!(!tcp.setup_status().await.expect("estado por TCP").needed);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_binds_the_device_only_with_a_valid_signature() {
    let server = start(|_| {}).await;
    let admin = server.admin().await;
    let (identity, token) = server.user(&admin, "ana").await;
    assert_eq!(token.user.username, "ana");
    let me = server.api_with(&token.token).me().await.expect("/me");
    assert_eq!(
        me.device.as_ref().map(|device| device.fingerprint.clone()),
        Some(identity.fingerprint())
    );
    assert_eq!(
        me.device.as_ref().map(|d| d.name.as_str()),
        Some("equipo de prueba")
    );

    // Firma de otra identidad: rechazada; el desafío no se puede reutilizar.
    let api = server.api();
    let impostor = Identity::generate().expect("identidad");
    let challenge: LoginChallenge = api
        .json(
            Method::POST,
            "/auth/login",
            Some(&LoginRequest {
                username: "ana".to_owned(),
                password: PASSWORD.to_owned(),
                public_key: Some(hex::encode(&identity.public_key())),
                device_name: Some("robado".to_owned()),
            }),
        )
        .await
        .expect("desafío");
    assert_eq!(challenge.server_host, "127.0.0.1");
    assert_eq!(challenge.expires_in, 60);
    let bytes = hex::decode(&challenge.challenge).expect("hex");
    let forged = impostor.sign(&challenge_message(&challenge.server_host, &bytes));
    let complete = LoginComplete {
        challenge_id: challenge.challenge_id.clone(),
        signature: hex::encode(&forged),
    };
    assert_eq!(
        api_code(
            api.json::<Value>(Method::POST, "/auth/login/complete", Some(&complete))
                .await
        ),
        (401, "invalid_signature".to_owned())
    );
    let good = LoginComplete {
        challenge_id: challenge.challenge_id,
        signature: hex::encode(&identity.sign(&challenge_message("127.0.0.1", &bytes))),
    };
    assert_eq!(
        api_code(
            api.json::<Value>(Method::POST, "/auth/login/complete", Some(&good))
                .await
        ),
        (401, "challenge_expired".to_owned()),
        "un desafío solo sirve una vez"
    );

    // Una identidad vinculada a ana no sirve para otra cuenta.
    admin
        .create_user(&coroom_transport::proto::api::CreateUser {
            username: "bea".to_owned(),
            password: PASSWORD.to_owned(),
            display_name: None,
            is_admin: false,
        })
        .await
        .expect("bea");
    assert_eq!(
        api_code(api.login("bea", PASSWORD, &identity, "mismo").await),
        (409, "device_in_use".to_owned())
    );

    // La interfaz web es solo para administradores; credenciales malas, respuesta uniforme.
    assert_eq!(
        api_code(api.admin_login("ana", PASSWORD).await),
        (403, "device_required".to_owned())
    );
    assert_eq!(
        api_code(api.admin_login("ana", "contraseña-mala").await),
        (401, "invalid_credentials".to_owned())
    );
    assert_eq!(
        api_code(api.admin_login("nadie", PASSWORD).await),
        (401, "invalid_credentials".to_owned())
    );

    // Un token de dispositivo nuevo sustituye al anterior de ese dispositivo; logout lo revoca.
    let again = api
        .login("ana", PASSWORD, &identity, "equipo de prueba")
        .await
        .expect("segundo login");
    assert_eq!(
        api_code(server.api_with(&token.token).me().await),
        (401, "unauthorized".to_owned()),
        "uno por dispositivo"
    );
    server
        .api_with(&again.token)
        .logout()
        .await
        .expect("logout");
    assert_eq!(
        api_code(server.api_with(&again.token).me().await),
        (401, "unauthorized".to_owned())
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_failures_per_minute_lock_the_account_and_the_ip() {
    let server = start(|_| {}).await;
    let admin = server.admin().await;
    server.user(&admin, "carla").await;
    let api = server.api();
    for _ in 0..5 {
        assert_eq!(
            api_code(api.admin_login("carla", "contraseña-mala").await),
            (401, "invalid_credentials".to_owned())
        );
    }
    let identity = Identity::generate().expect("identidad");
    assert_eq!(
        api_code(api.login("carla", PASSWORD, &identity, "equipo").await),
        (429, "too_many_attempts".to_owned()),
        "ni con la contraseña buena"
    );
    assert_eq!(
        api_code(api.admin_login(ADMIN, PASSWORD).await),
        (429, "too_many_attempts".to_owned()),
        "la IP también queda bloqueada"
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invitations_are_single_use_expire_and_can_be_revoked() {
    let server = start(|_| {}).await;
    let admin = server.admin().await;
    let api = server.api();
    let created = admin
        .create_invitation(&CreateInvitation {
            note: "para Ana".to_owned(),
            hours: None,
        })
        .await
        .expect("invitación");
    assert_eq!(created.invitation.state, "pending");
    assert_eq!(
        created.invitation.expires_at - created.invitation.created_at,
        72 * 3600
    );
    let ana = api
        .redeem_invitation(&created.code.to_lowercase(), "ana", PASSWORD, Some("Ana"))
        .await
        .expect("canje");
    assert_eq!(
        (ana.username.as_str(), ana.display_name.as_str()),
        ("ana", "Ana")
    );
    assert_eq!(
        api_code(
            api.redeem_invitation(&created.code, "bea", PASSWORD, None)
                .await
        ),
        (404, "invalid_invitation".to_owned()),
        "un solo uso"
    );
    let identity = Identity::generate().expect("identidad");
    api.login("ana", PASSWORD, &identity, "portátil")
        .await
        .expect("la cuenta canjeada inicia sesión");

    // Caducada (se adelanta su caducidad en la base) y revocada.
    let expired = admin
        .create_invitation(&CreateInvitation::default())
        .await
        .expect("invitación");
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        server.dir.path().join("coroom.db").display()
    ))
    .await
    .expect("base de datos");
    sqlx::query("UPDATE invitations SET expires_at = 1 WHERE id = ?")
        .bind(i64::try_from(expired.invitation.id).expect("id"))
        .execute(&pool)
        .await
        .expect("caducar");
    pool.close().await;
    assert_eq!(
        api_code(
            api.redeem_invitation(&expired.code, "bea", PASSWORD, None)
                .await
        ),
        (404, "invalid_invitation".to_owned()),
        "caducada"
    );
    let revoked = admin
        .create_invitation(&CreateInvitation::default())
        .await
        .expect("invitación");
    let (status, _) = raw(
        &admin,
        Method::DELETE,
        &format!("/admin/invitations/{}", revoked.invitation.id),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        api_code(
            api.redeem_invitation(&revoked.code, "bea", PASSWORD, None)
                .await
        ),
        (404, "invalid_invitation".to_owned()),
        "revocada"
    );
    let (_, list) = raw(&admin, Method::GET, "/admin/invitations", Value::Null).await;
    let states: Vec<&str> = list["invitations"]
        .as_array()
        .expect("lista")
        .iter()
        .filter_map(|invitation| invitation["state"].as_str())
        .collect();
    assert_eq!(states, ["revoked", "expired", "used"]);
    server.stop().await;

    // Con COROOM_REGISTRATION=admin no hay invitaciones.
    let server = start(|config| config.registration = Registration::Admin).await;
    let admin = server.admin().await;
    assert_eq!(
        api_code(admin.create_invitation(&CreateInvitation::default()).await),
        (403, "invitations_disabled".to_owned())
    );
    assert_eq!(
        api_code(
            server
                .api()
                .redeem_invitation("ABCD-EFGH-JKLM", "ana", PASSWORD, None)
                .await
        ),
        (403, "invitations_disabled".to_owned())
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn friends_see_each_other_and_only_friends_can_be_invited() {
    let server = start(|_| {}).await;
    let admin = server.admin().await;
    let (ana_id, ana) = server.user(&admin, "ana").await;
    let (bea_id, bea) = server.user(&admin, "bea").await;
    let (_, carla) = server.user(&admin, "carla").await;
    let ana_api = server.api_with(&ana.token);
    let bea_api = server.api_with(&bea.token);

    // Solicitud → aceptación.
    let requested = ana_api.add_friend("BEA").await.expect("solicitud");
    assert!(!requested.accepted);
    assert_eq!(
        api_code(ana_api.add_friend("nadie").await),
        (404, "user_not_found".to_owned())
    );
    assert_eq!(
        api_code(ana_api.add_friend("ana").await),
        (400, "bad_request".to_owned())
    );
    let incoming = bea_api.friends().await.expect("amigos").incoming;
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].user.username, "ana");
    assert_eq!(
        ana_api.friends().await.expect("amigos").outgoing[0]
            .user
            .username,
        "bea"
    );
    let accepted = bea_api
        .accept_friend(incoming[0].id)
        .await
        .expect("aceptar");
    assert!(accepted.accepted);
    let list = ana_api.friends().await.expect("amigos");
    assert_eq!(list.friends.len(), 1);
    assert!(!list.friends[0].online, "bea no está conectada");

    // Presencia por la señalización.
    let (_ana_endpoint, mut ana_signal) = server.signal(&ana_id, &ana.token).await;
    let (_bea_endpoint, mut bea_signal) = server.signal(&bea_id, &bea.token).await;
    let bea_user = bea.user.id;
    let ana_user = ana.user.id;
    next_matching(&mut ana_signal, |message| match message {
        SignalMessage::Presence { user, online: true } if user == bea_user => Some(()),
        _ => None,
    })
    .await;
    next_matching(&mut bea_signal, |message| match message {
        SignalMessage::Presence { user, online: true } if user == ana_user => Some(()),
        _ => None,
    })
    .await;
    assert!(ana_api.friends().await.expect("amigos").friends[0].online);

    // Sala de ana: la identidad debe ser la del dispositivo.
    ana_signal
        .send(&SignalMessage::CreateRoom {
            identity_pk: bea_id.public_key(),
            name: "Sala".to_owned(),
        })
        .await
        .expect("CreateRoom");
    assert!(matches!(
        next(&mut ana_signal).await,
        SignalMessage::Error {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    ana_signal
        .send(&SignalMessage::CreateRoom {
            identity_pk: ana_id.public_key(),
            name: "Sala".to_owned(),
        })
        .await
        .expect("CreateRoom");
    let code = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::RoomCreated { code } => Some(code),
        _ => None,
    })
    .await;
    ana_signal
        .send(&SignalMessage::Invite {
            to_user: bea_user,
            code: code.clone(),
        })
        .await
        .expect("Invite");
    let invited = next_matching(&mut bea_signal, |message| match message {
        SignalMessage::Invited {
            from_user,
            from_name,
            code,
        } => Some((from_user, from_name, code)),
        _ => None,
    })
    .await;
    assert_eq!(invited, (ana_user, "ana (prueba)".to_owned(), code.clone()));

    // A quien no es amigo, no.
    ana_signal
        .send(&SignalMessage::Invite {
            to_user: carla.user.id,
            code: code.clone(),
        })
        .await
        .expect("Invite");
    let error = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::Error { code, .. } => Some(code),
        _ => None,
    })
    .await;
    assert_eq!(error, ErrorCode::NotFriend);

    // bea entra con la invitación y el anfitrión ve su usuario y su nombre.
    bea_signal
        .send(&SignalMessage::JoinRoom {
            code: code.clone(),
            identity_pk: bea_id.public_key(),
            key_package: Vec::new(),
        })
        .await
        .expect("JoinRoom");
    let joined = next_matching(&mut bea_signal, |message| match message {
        SignalMessage::Joined {
            host_user,
            host_name,
            ..
        } => Some((host_user, host_name)),
        _ => None,
    })
    .await;
    assert_eq!(joined, (ana_user, "ana (prueba)".to_owned()));
    let peer = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::PeerJoined { user, name, .. } => Some((user, name)),
        _ => None,
    })
    .await;
    assert_eq!(peer, (bea_user, "bea (prueba)".to_owned()));

    // Eliminada la amistad, ya no se le puede invitar; al salir, ana lo ve.
    ana_api.remove_friend(bea_user).await.expect("eliminar");
    assert!(ana_api.friends().await.expect("amigos").friends.is_empty());
    assert_eq!(
        api_code(ana_api.remove_friend(bea_user).await),
        (404, "not_friends".to_owned())
    );
    ana_signal
        .send(&SignalMessage::Invite {
            to_user: bea_user,
            code,
        })
        .await
        .expect("Invite");
    let error = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::Error { code, .. } => Some(code),
        _ => None,
    })
    .await;
    assert_eq!(error, ErrorCode::NotFriend);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signaling_rejects_tokens_of_another_device() {
    let server = start(|_| {}).await;
    let admin = server.admin().await;
    let (_, ana) = server.user(&admin, "ana").await;
    let other = Identity::generate().expect("identidad");
    let endpoint = coroom_transport::Endpoint::bind(
        &other,
        Some("127.0.0.1:0".parse().expect("dirección")),
        coroom_transport::EndpointOptions::default(),
    )
    .expect("endpoint");
    let rejected = |result: Result<SignalClient, TransportError>| match result {
        Err(TransportError::Rejected {
            code: ErrorCode::Unauthorized,
            message,
        }) => message,
        other => panic!("se esperaba Unauthorized: {other:?}"),
    };
    let message =
        rejected(SignalClient::connect(&server.url, &ana.token, &endpoint, &server.trust).await);
    assert!(message.contains("dispositivo"), "{message}");
    // Ni el token web del administrador ni uno inventado (sin modo desarrollo).
    let web = server
        .api()
        .admin_login(ADMIN, PASSWORD)
        .await
        .expect("login web");
    rejected(SignalClient::connect(&server.url, &web.token, &endpoint, &server.trust).await);
    rejected(SignalClient::connect(&server.url, "change-me", &endpoint, &server.trust).await);

    // Una cuenta deshabilitada no entra y su sesión abierta se cierra.
    let (ana_id, ana2) = server.login("ana").await;
    let (_ana_endpoint, mut signal) = server.signal(&ana_id, &ana2.token).await;
    let update = UpdateUser {
        disabled: Some(true),
        ..UpdateUser::default()
    };
    let (status, user) = raw(
        &admin,
        Method::PATCH,
        &format!("/admin/users/{}", ana2.user.id),
        serde_json::to_value(&update).expect("JSON"),
    )
    .await;
    assert_eq!((status, user["disabled"].clone()), (200, json!(true)));
    let closed = tokio::time::timeout(TIMEOUT, async {
        loop {
            match signal.next_event().await {
                Ok(Some(SignalMessage::Error { code, .. })) => break Some(code),
                Ok(Some(_)) => continue,
                _ => break None,
            }
        }
    })
    .await
    .expect("cierre a tiempo");
    assert_eq!(closed, Some(ErrorCode::Unauthorized));
    assert_eq!(
        api_code(server.api().admin_login("ana", PASSWORD).await),
        (401, "invalid_credentials".to_owned())
    );
    server.stop().await;
}

/// Datagrama de vídeo de `len` bytes de carga.
fn object(group: u32, index: u16, count: u16, len: usize) -> Bytes {
    let header = ObjectHeader {
        track: Track::Video,
        flags: if index + 1 == count {
            ObjectHeader::LAST_OBJECT
        } else {
            0
        },
        group,
        object: index,
        object_count: count,
        payload_len: len as u16,
        epoch_hint: 0,
    };
    let mut bytes = header.to_bytes().to_vec();
    bytes.extend(std::iter::repeat_n(0xa5, len));
    Bytes::from(bytes)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_monthly_quota_cuts_the_relay_and_new_rooms_go_direct() {
    // 20 kB al mes.
    let server = start(|config| config.relay_quota_gb_per_month = 0.000_02).await;
    let admin = server.admin().await;
    let (ana_id, ana) = server.user(&admin, "ana").await;
    let (bea_id, bea) = server.user(&admin, "bea").await;
    let (ana_endpoint, mut ana_signal) = server.signal(&ana_id, &ana.token).await;
    let (bea_endpoint, mut bea_signal) = server.signal(&bea_id, &bea.token).await;

    ana_signal
        .send(&SignalMessage::CreateRoom {
            identity_pk: ana_id.public_key(),
            name: "Sala".to_owned(),
        })
        .await
        .expect("CreateRoom");
    let code = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::RoomCreated { code } => Some(code),
        _ => None,
    })
    .await;
    let relay_info = |message: SignalMessage, role: Role| match message {
        SignalMessage::RelayToken { token, addr } => Some(RelayInfo {
            addr,
            server_name: "127.0.0.1".to_owned(),
            trust: server.trust.clone(),
            token,
            role,
        }),
        _ => None,
    };
    let host_relay = next_matching(&mut ana_signal, |m| relay_info(m, Role::Host)).await;
    bea_signal
        .send(&SignalMessage::JoinRoom {
            code: code.clone(),
            identity_pk: bea_id.public_key(),
            key_package: Vec::new(),
        })
        .await
        .expect("JoinRoom");
    let guest_relay = next_matching(&mut bea_signal, |m| relay_info(m, Role::Guest)).await;

    let host = connect::connect_relay(&ana_endpoint, &host_relay)
        .await
        .expect("anfitrión en el relay");
    let guest = connect::connect_relay(&bea_endpoint, &guest_relay)
        .await
        .expect("invitado en el relay");

    // El anfitrión envía hasta que el relay corta (a los ~20 kB reenviados).
    let received = tokio::spawn(async move {
        let mut bytes = 0usize;
        while let Ok(Ok(datagram)) =
            tokio::time::timeout(Duration::from_secs(5), guest.read_datagram()).await
        {
            bytes += datagram.len();
        }
        bytes
    });
    let deadline = Instant::now() + TIMEOUT;
    let mut group = 0u32;
    while host.quinn().close_reason().is_none() && Instant::now() < deadline {
        for index in 0..4 {
            let _ = host.send_datagram(object(group, index, 4, 900));
        }
        group += 1;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        host.quinn().close_reason().is_some(),
        "el relay debe cortar al agotar la cuota"
    );
    let message = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::QuotaExceeded { message } => Some(message),
        _ => None,
    })
    .await;
    assert!(message.contains("cuota"), "{message}");
    next_matching(&mut bea_signal, |message| match message {
        SignalMessage::QuotaExceeded { .. } => Some(()),
        _ => None,
    })
    .await;
    let forwarded = received.await.expect("invitado");
    assert!(
        (15_000..40_000).contains(&forwarded),
        "reenviados {forwarded} bytes con una cuota de 20 kB"
    );

    // El uso queda contado (propio y del administrador) y una sala nueva va sin relay.
    let usage = server.api_with(&ana.token).usage().await.expect("uso");
    assert!(usage.bytes >= 20_000, "{usage:?}");
    assert_eq!(usage.quota_bytes, Some(20_000));
    let (_, admin_usage) = raw(&admin, Method::GET, "/admin/usage", Value::Null).await;
    assert_eq!(admin_usage["users"][0]["username"], "ana");
    drop(ana_signal);
    let (_endpoint, mut ana_signal) = server.signal(&ana_id, &ana.token).await;
    ana_signal
        .send(&SignalMessage::CreateRoom {
            identity_pk: ana_id.public_key(),
            name: "Otra".to_owned(),
        })
        .await
        .expect("CreateRoom");
    let relay = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::RelayToken { .. } => Some(true),
        SignalMessage::QuotaExceeded { .. } => Some(false),
        _ => None,
    })
    .await;
    assert!(!relay, "sala nueva sin relay");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn administration_health_and_backup() {
    let server = start(|config| config.relay_max_kbps = 5_000).await;
    let admin = server.admin().await;
    let (_, ana) = server.user(&admin, "ana").await;

    // Usuarios: lista, contraseña nueva (cierra sesiones), último administrador.
    let (_, users) = raw(&admin, Method::GET, "/admin/users", Value::Null).await;
    let names: Vec<&str> = users["users"]
        .as_array()
        .expect("lista")
        .iter()
        .filter_map(|user| user["username"].as_str())
        .collect();
    assert_eq!(names, ["admin", "ana"]);
    assert_eq!(users["users"][1]["devices"], 1);
    let (status, _) = raw(
        &admin,
        Method::POST,
        &format!("/admin/users/{}/password", ana.user.id),
        json!({ "password": "otra-contraseña-larga" }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        api_code(server.api_with(&ana.token).me().await),
        (401, "unauthorized".to_owned())
    );
    let admin_id = users["users"][0]["id"].as_u64().expect("id");
    let (status, body) = raw(
        &admin,
        Method::PATCH,
        &format!("/admin/users/{admin_id}"),
        json!({ "is_admin": false }),
    )
    .await;
    assert_eq!((status, body["error"].clone()), (409, json!("last_admin")));
    let (status, body) = raw(
        &server.api_with(&ana.token),
        Method::GET,
        "/admin/users",
        Value::Null,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (401, json!("unauthorized"))
    );

    // Ajustes de solo lectura y salas.
    let (_, settings) = raw(&admin, Method::GET, "/admin/settings", Value::Null).await;
    assert_eq!(settings["tls_mode"], "self-signed");
    assert_eq!(settings["registration"], "invite");
    assert_eq!(
        settings["source_url"],
        "https://github.com/43fdfdg45454/coroom"
    );
    assert_eq!(settings["relay_max_kbps"], 5_000);
    assert_eq!(
        settings["certificate_fingerprint"].as_str().map(str::len),
        Some(64)
    );
    let (_, rooms) = raw(&admin, Method::GET, "/admin/rooms", Value::Null).await;
    assert_eq!(rooms, json!({ "rooms": [] }));

    // `/health` con los campos nuevos; la autocomprobación de UDP llega a sí misma.
    let deadline = Instant::now() + TIMEOUT;
    let health = loop {
        let health = health_over_tcp(&server).await;
        if !health["udp_checked_at"].is_null() || Instant::now() > deadline {
            break health;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(health["users"], 2);
    assert_eq!(health["udp_reachable"], true, "{health}");
    assert!(
        health["udp_checked_at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z'))
    );
    assert_eq!(health["relay_bytes_today"], 0);

    // Copia en línea restaurable.
    let copy = server.dir.path().join("copia.db");
    coroom_server::backup(&server.config, &copy)
        .await
        .expect("copia");
    assert!(
        coroom_server::backup(&server.config, &copy).await.is_err(),
        "no sobrescribe"
    );
    let dir = server.stop().await;
    let restored = tempfile::tempdir().expect("directorio temporal");
    std::fs::copy(&copy, restored.path().join("coroom.db")).expect("restaurar");
    let server = common::start_with(common::config(restored.path()), restored).await;
    assert!(!server.api().setup_status().await.expect("estado").needed);
    server
        .api()
        .admin_login(ADMIN, PASSWORD)
        .await
        .expect("el administrador sigue");
    server
        .api()
        .login(
            "ana",
            "otra-contraseña-larga",
            &Identity::generate().expect("identidad"),
            "nuevo",
        )
        .await
        .expect("ana con su contraseña nueva");
    server.stop().await;
    drop(dir);
}

/// `/health` (fuera de `/api/v1`) por HTTP/2 con TLS.
async fn health_over_tcp(server: &common::Running) -> Value {
    use http_body_util::BodyExt;
    let roots = {
        let mut roots = rustls::RootCertStore::empty();
        let path = coroom_server::tls::self_signed_cert_path(server.dir.path());
        for cert in rustls::pki_types::pem::PemObject::pem_file_iter(&path).expect("certificado") {
            let cert: rustls::pki_types::CertificateDer<'static> = cert.expect("certificado");
            roots.add(cert).expect("raíz");
        }
        roots
    };
    let mut config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS")
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let stream = tokio::net::TcpStream::connect(server.tcp)
        .await
        .expect("TCP");
    let tls = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config))
        .connect(
            rustls::pki_types::ServerName::try_from("localhost").expect("nombre"),
            stream,
        )
        .await
        .expect("TLS");
    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(tls),
    )
    .await
    .expect("HTTP/2");
    tokio::spawn(connection);
    let request = http::Request::get("https://localhost/health")
        .body(http_body_util::Empty::<Bytes>::new())
        .expect("petición");
    let response = sender.send_request(request).await.expect("respuesta");
    let body = response
        .into_body()
        .collect()
        .await
        .expect("cuerpo")
        .to_bytes();
    serde_json::from_slice(&body).expect("JSON")
}
