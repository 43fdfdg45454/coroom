//! Registro sin secretos (`docs/seguridad.md`, capa «Servidor»): con el servidor real, todo el
//! registro de coroom en `trace` (y el resto de crates en `debug`) mientras se crea el
//! administrador, se da de alta un usuario, se inicia sesión con el desafío de dispositivo, se
//! abre la señalización, se falla una contraseña y se cambia otra, y el texto registrado no
//! contiene ninguna contraseña, ningún token de sesión ni la firma del desafío.
//!
//! `COROOM_LOG_TEST_FILTER=trace` lo repite con todas las crates en `trace` (ver el informe de
//! `docs/seguridad.md`: `h3` y `rustls` en `trace` no registran cabeceras ni claves).
//!
//! El suscriptor es global (las tareas del servidor corren en otros hilos), así que este fichero
//! tiene un solo test.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::{ADMIN, PASSWORD, start};
use coroom_transport::proto::api::{CreateUser, SetPassword};
use http::Method;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// Todo lo que se registra, en memoria.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(|p| p.into_inner()).clone();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_log_never_contains_passwords_or_tokens() {
    let captured = Captured::default();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(
            std::env::var("COROOM_LOG_TEST_FILTER").unwrap_or_else(|_| {
                "debug,coroom_server=trace,coroom_transport=trace,coroom_proto=trace".to_owned()
            }),
        ))
        .with_ansi(false)
        .with_writer(captured.clone())
        .init();

    let server = start(|_| {}).await;
    let admin = server.admin().await;
    let admin_token = admin.token().map(str::to_owned).expect("token web");
    let secret_password = "otra-clave-de-prueba-larga";
    admin
        .create_user(&CreateUser {
            username: "bea".to_owned(),
            password: secret_password.to_owned(),
            display_name: Some("Bea".to_owned()),
            is_admin: false,
        })
        .await
        .expect("alta");
    let identity = coroom_transport::crypto::Identity::generate().expect("identidad");
    let session = server
        .api()
        .login("bea", secret_password, &identity, "portátil")
        .await
        .expect("login con desafío");
    // Señalización con el token del dispositivo.
    let (_endpoint, signal) = server.signal(&identity, &session.token).await;
    drop(signal);
    // Contraseña equivocada y cambio de contraseña por el administrador.
    let wrong = "contraseña-equivocada-123";
    assert!(
        server
            .api()
            .login("bea", wrong, &identity, "portátil")
            .await
            .is_err()
    );
    let new_password = "clave-nueva-del-administrador";
    let response = admin
        .raw(
            Method::POST,
            "/admin/users/2/password",
            Some(
                serde_json::to_vec(&SetPassword {
                    password: new_password.to_owned(),
                })
                .expect("JSON"),
            ),
        )
        .await
        .expect("cambio de contraseña");
    assert!(response.status.is_success(), "{:?}", response.status);
    server.stop().await;

    let log = captured.text();
    assert!(
        log.contains("usuario creado"),
        "el registro capturó el servidor:\n{log}"
    );
    for (what, secret) in [
        ("contraseña del administrador", PASSWORD),
        ("contraseña del usuario", secret_password),
        ("contraseña equivocada", wrong),
        ("contraseña nueva", new_password),
        ("token web", admin_token.as_str()),
        ("token del dispositivo", session.token.as_str()),
    ] {
        assert!(!log.contains(secret), "el registro contiene la {what}");
    }
    assert!(!log.contains(ADMIN) || !log.contains(&format!("{ADMIN}:{PASSWORD}")));
    std::io::stderr()
        .write_all(format!("{} bytes de registro revisados\n", log.len()).as_bytes())
        .ok();
}
