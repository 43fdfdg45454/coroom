//! Avatares con el servidor real: subir (el servidor valida y vuelve a codificar), límite de
//! tamaño, imágenes no válidas, descarga con `ETag`, la versión en `/me`, en los amigos, en el
//! aviso `Avatar` y en `Joined`/`PeerJoined` (también entre quienes no son amigos y comparten
//! sala, también si cambia con la sala abierta), y quitarlo; los avisos acotados (la misma imagen
//! no avisa, tope de cambios por minuto) y la moderación desde la administración.

mod common;

use common::{next_matching, start};
use coroom_transport::TransportError;
use coroom_transport::api::ApiClient;
use coroom_transport::proto::api::{
    AVATAR_MAX_BYTES, AVATAR_SIDE_MAX, User, avatar_path, avatar_version,
};
use coroom_transport::proto::signal::SignalMessage;
use http::{Method, StatusCode};
use sha2::{Digest, Sha256};

/// PNG RGB de `side`×`side` con un degradado que depende de `seed`.
fn png(side: u32, seed: u8) -> Vec<u8> {
    let data: Vec<u8> = (0..side * side)
        .flat_map(|i| {
            let x = (i % side) as u8;
            let y = (i / side) as u8;
            [x.wrapping_add(seed), y, seed.wrapping_mul(3)]
        })
        .collect();
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, side, side);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("cabecera");
    writer.write_image_data(&data).expect("píxeles");
    writer.finish().expect("fin");
    out
}

fn api_code<T: std::fmt::Debug>(result: Result<T, TransportError>) -> (u16, String) {
    match result {
        Err(TransportError::Api { status, code, .. }) => (status, code),
        other => panic!("se esperaba un error de la API: {other:?}"),
    }
}

/// Petición JSON en bruto: estado y cuerpo.
async fn call(
    api: &ApiClient,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let body = body.map(|value| serde_json::to_vec(&value).expect("JSON"));
    let response = api.raw(method, path, body).await.expect("respuesta");
    let value = if response.body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&response.body).expect("JSON")
    };
    (response.status, value)
}

async fn put_raw(api: &ApiClient, body: Vec<u8>) -> u16 {
    api.raw(Method::PUT, "/me/avatar", Some(body))
        .await
        .expect("respuesta")
        .status
        .as_u16()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn avatars_are_validated_served_versioned_and_removed() {
    let server = start(|_| {}).await;
    let admin = server.admin().await;
    let (ana_id, ana) = server.user(&admin, "ana").await;
    let (bea_id, bea) = server.user(&admin, "bea").await;
    let (carla_id, carla) = server.user(&admin, "carla").await;
    let (_dani_id, dani) = server.user(&admin, "dani").await;
    let dani_api = server.api_with(&dani.token);
    let ana_api = server.api_with(&ana.token);
    let bea_api = server.api_with(&bea.token);
    let carla_api = server.api_with(&carla.token);
    assert_eq!(ana.user.avatar, None);
    assert_eq!(ana_api.avatar(ana.user.id).await.expect("sin avatar"), None);

    // Sin sesión no se ve ni se sube.
    let anonymous = server.api();
    let response = anonymous
        .raw(Method::GET, &avatar_path(ana.user.id), None)
        .await
        .expect("respuesta");
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(put_raw(&anonymous, png(64, 1)).await, 401);
    // Sin token, ni siquiera se lee un cuerpo del tamaño de un avatar.
    let mut big = png(64, 1);
    big.resize(128 * 1024, 0);
    assert_eq!(put_raw(&anonymous, big.clone()).await, 413);
    assert_eq!(put_raw(&server.api().tcp_only(), big).await, 413);

    // Lo que no es un PNG cuadrado del tamaño permitido se rechaza (lo prueba dani, para no
    // gastar los cambios por minuto de ana).
    assert_eq!(
        api_code(dani_api.set_avatar(b"no es una imagen".to_vec()).await),
        (400, "invalid_avatar".to_owned())
    );
    assert_eq!(
        api_code(dani_api.set_avatar(png(8, 1)).await),
        (400, "invalid_avatar".to_owned()),
        "demasiado pequeño"
    );
    assert_eq!(
        api_code(dani_api.set_avatar(png(AVATAR_SIDE_MAX * 2, 1)).await),
        (400, "invalid_avatar".to_owned()),
        "demasiado grande en píxeles"
    );
    let mut truncated = png(64, 1);
    truncated.truncate(truncated.len() / 2);
    assert_eq!(
        api_code(dani_api.set_avatar(truncated).await),
        (400, "invalid_avatar".to_owned())
    );
    // Más bytes de los permitidos: lo corta la pila HTTP (las dos) antes de leerlo entero.
    let mut huge = png(64, 1);
    huge.resize(AVATAR_MAX_BYTES + 1, 0);
    assert_eq!(put_raw(&ana_api, huge.clone()).await, 413);
    assert_eq!(
        put_raw(&server.api_with(&ana.token).tcp_only(), huge).await,
        413
    );
    assert_eq!(ana_api.me().await.expect("/me").user.avatar, None);

    // bea y carla, conectadas a la señalización; bea es amiga de ana, carla no.
    let request = ana_api.add_friend("bea").await.expect("solicitud");
    assert!(!request.accepted);
    let incoming = bea_api.friends().await.expect("amigos").incoming;
    bea_api
        .accept_friend(incoming[0].id)
        .await
        .expect("aceptar");
    let (_bea_endpoint, mut bea_signal) = server.signal(&bea_id, &bea.token).await;

    // Uno bueno: el servidor guarda su propio PNG y la versión es la de ese PNG.
    let first: User = ana_api.set_avatar(png(128, 1)).await.expect("avatar");
    let version = first.avatar.expect("versión");
    let stored = ana_api
        .avatar(ana.user.id)
        .await
        .expect("descarga")
        .expect("tiene avatar");
    assert_eq!(avatar_version(&Sha256::digest(&stored).into()), version);
    assert!(stored.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(ana_api.me().await.expect("/me").user.avatar, Some(version));
    // Cualquiera con sesión lo descarga, también quien no es amigo; por TCP, igual.
    assert_eq!(
        carla_api.avatar(ana.user.id).await.expect("descarga"),
        Some(stored.clone())
    );
    assert_eq!(
        server
            .api_with(&carla.token)
            .tcp_only()
            .avatar(ana.user.id)
            .await
            .expect("descarga por TCP"),
        Some(stored.clone())
    );
    // La amiga lo ve en la lista y por la señalización, en cuanto cambia.
    let seen = next_matching(&mut bea_signal, |message| match message {
        SignalMessage::Avatar { user, avatar } if user == ana.user.id => Some(avatar),
        _ => None,
    })
    .await;
    assert_eq!(seen, Some(version));
    assert_eq!(
        bea_api.friends().await.expect("amigos").friends[0].avatar,
        Some(version)
    );

    // Con `If-None-Match` igual al `ETag`, 304 sin cuerpo.
    let etag = format!("\"{version:014x}\"");
    let conditional = server
        .api_with(&carla.token)
        .with_header("if-none-match", &etag)
        .expect("cabecera");
    let response = conditional
        .raw(Method::GET, &avatar_path(ana.user.id), None)
        .await
        .expect("respuesta");
    assert_eq!(response.status, StatusCode::NOT_MODIFIED);
    assert!(response.body.is_empty());

    // Otra imagen: otra versión; la misma imagen, la misma versión.
    let second = ana_api.set_avatar(png(96, 2)).await.expect("avatar");
    let version_2 = second.avatar.expect("versión");
    assert_ne!(version_2, version);
    let seen = next_matching(&mut bea_signal, |message| match message {
        SignalMessage::Avatar { user, avatar }
            if user == ana.user.id && avatar != Some(version) =>
        {
            Some(avatar)
        }
        _ => None,
    })
    .await;
    assert_eq!(seen, Some(version_2));
    assert_eq!(
        ana_api.set_avatar(png(96, 2)).await.expect("avatar").avatar,
        Some(version_2),
        "la misma imagen, la misma versión (y sin aviso: lo comprueba el `None` de más abajo)"
    );
    let response = conditional
        .raw(Method::GET, &avatar_path(ana.user.id), None)
        .await
        .expect("respuesta");
    assert_eq!(
        response.status,
        StatusCode::OK,
        "la versión vieja ya no vale"
    );

    // En una sala, el anfitrión y los invitados ven la versión del otro aunque no sean amigos.
    let (_ana_endpoint, mut ana_signal) = server.signal(&ana_id, &ana.token).await;
    let (_carla_endpoint, mut carla_signal) = server.signal(&carla_id, &carla.token).await;
    carla_api
        .set_avatar(png(64, 9))
        .await
        .expect("avatar de carla");
    let carla_version = carla_api.me().await.expect("/me").user.avatar;
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
    carla_signal
        .send(&SignalMessage::JoinRoom {
            code,
            identity_pk: carla_id.public_key(),
            key_package: Vec::new(),
        })
        .await
        .expect("JoinRoom");
    let host_avatar = next_matching(&mut carla_signal, |message| match message {
        SignalMessage::Joined { host_avatar, .. } => Some(host_avatar),
        _ => None,
    })
    .await;
    assert_eq!(host_avatar, Some(version_2));
    let guest_avatar = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::PeerJoined { avatar, .. } => Some(avatar),
        _ => None,
    })
    .await;
    assert_eq!(guest_avatar, carla_version);
    // Mientras comparten sala, el cambio llega también a quien no es amigo.
    let carla_new = carla_api
        .set_avatar(png(48, 5))
        .await
        .expect("otro avatar de carla")
        .avatar;
    assert_ne!(carla_new, carla_version);
    let seen = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::Avatar { user, avatar } if user == carla.user.id => Some(avatar),
        _ => None,
    })
    .await;
    assert_eq!(seen, carla_new);

    // Quitarlo: sin versión, 404 al pedirlo y aviso a la amiga.
    assert_eq!(ana_api.delete_avatar().await.expect("quitar").avatar, None);
    assert_eq!(carla_api.avatar(ana.user.id).await.expect("descarga"), None);
    // El aviso siguiente de ana es este: volver a subir la misma imagen no avisó.
    let seen = next_matching(&mut bea_signal, |message| match message {
        SignalMessage::Avatar { user, avatar } if user == ana.user.id => Some(avatar),
        _ => None,
    })
    .await;
    assert_eq!(seen, None);
    assert_eq!(
        bea_api.friends().await.expect("amigos").friends[0].avatar,
        None
    );
    next_matching(&mut carla_signal, |message| match message {
        SignalMessage::Avatar { user, avatar: None } if user == ana.user.id => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(
        ana_api
            .delete_avatar()
            .await
            .expect("quitar otra vez")
            .avatar,
        None
    );

    // Moderación: el administrador quita el de carla y se avisa igual que si lo quitara ella.
    let (status, removed) = call(
        &admin,
        Method::DELETE,
        &format!("/admin/users/{}/avatar", carla.user.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(removed["avatar"], serde_json::Value::Null);
    assert_eq!(ana_api.avatar(carla.user.id).await.expect("descarga"), None);
    let seen = next_matching(&mut ana_signal, |message| match message {
        SignalMessage::Avatar { user, avatar: None } if user == carla.user.id => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(seen, ());
    // Una cuenta deshabilitada no enseña su avatar.
    let dani_version = dani_api
        .set_avatar(png(32, 7))
        .await
        .expect("avatar")
        .avatar;
    assert!(dani_version.is_some());
    let (status, _) = call(
        &admin,
        Method::PATCH,
        &format!("/admin/users/{}", dani.user.id),
        Some(serde_json::json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ana_api.avatar(dani.user.id).await.expect("descarga"), None);

    server.stop().await;
}

/// Cada cuenta cambia el avatar como mucho `MAX_AVATAR_CHANGES` veces por minuto; después, `429`
/// sin tocar nada (también para lo que no llega a ser válido, que igual cuesta decodificar).
#[tokio::test]
async fn avatar_changes_have_a_cap_per_minute() {
    let server = start(|_| {}).await;
    let admin = server.admin().await;
    let (_ana_id, ana) = server.user(&admin, "ana").await;
    let (_bea_id, bea) = server.user(&admin, "bea").await;
    let ana_api = server.api_with(&ana.token);
    let mut accepted = 0;
    let mut last = None;
    for seed in 0..64u8 {
        match ana_api.set_avatar(png(32, seed)).await {
            Ok(user) => {
                accepted += 1;
                last = user.avatar;
            }
            Err(err) => {
                assert_eq!(
                    api_code::<()>(Err(err)),
                    (429, "too_many_requests".to_owned())
                );
                break;
            }
        }
    }
    assert!(
        accepted > 0 && accepted < 64,
        "{accepted} cambios aceptados"
    );
    assert_eq!(ana_api.me().await.expect("/me").user.avatar, last);
    assert_eq!(
        api_code(ana_api.delete_avatar().await),
        (429, "too_many_requests".to_owned())
    );
    // Otra cuenta no hereda el tope.
    server
        .api_with(&bea.token)
        .set_avatar(png(32, 1))
        .await
        .expect("bea sí");
    server.stop().await;
}
