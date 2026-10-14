//! Servidor real para los tests de extremo a extremo: `coroom-server` en puertos
//! efímeros de `127.0.0.1`, certificado autofirmado fijado por su huella, TLS por TCP y SQLite
//! temporal, **con cuentas**: al arrancar se crea el administrador (`setup`) y cada
//! [`TestServer::config`] da de alta un usuario nuevo, inicia sesión desde una identidad nueva
//! (desafío de dispositivo) y devuelve la identidad y la configuración con su token. El modo
//! `COROOM_DEV_TOKEN` queda para [`TestServer::start_dev`].

#![allow(dead_code)]

use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use coroom_server::{Config, Server};
use coroom_session::remote::RemoteConfig;
use coroom_transport::ServerTrust;
use coroom_transport::api::ApiClient;
use coroom_transport::crypto::Identity;
use coroom_transport::proto::api::CreateUser;

/// Token del modo desarrollo (ficticio).
pub const DEV_TOKEN: &str = "change-me";

/// Contraseña de las cuentas de prueba (ficticia).
pub const PASSWORD: &str = "change-me-please";

/// Una cuenta con su identidad vinculada y la configuración de la sesión remota con su token.
///
/// Se comporta como la [`RemoteConfig`] (`account.force_relay = true` funciona).
pub struct Account {
    /// Identidad del dispositivo (la vinculada a la cuenta).
    pub identity: Identity,
    /// Configuración con el token de sesión.
    pub config: RemoteConfig,
    /// Usuario.
    pub user: u64,
    /// Nombre de usuario.
    pub username: String,
}

impl Deref for Account {
    type Target = RemoteConfig;

    fn deref(&self) -> &RemoteConfig {
        &self.config
    }
}

impl DerefMut for Account {
    fn deref_mut(&mut self) -> &mut RemoteConfig {
        &mut self.config
    }
}

/// Servidor real en su propio runtime.
pub struct TestServer {
    /// `https://127.0.0.1:puerto`.
    pub url: String,
    /// Huella del certificado.
    pub trust: ServerTrust,
    /// Runtime del servidor (y de los proxies de los tests).
    pub runtime: tokio::runtime::Runtime,
    tcp_port: u16,
    admin: Option<ApiClient>,
    dev: bool,
    next: AtomicU32,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    /// Servidor con cuentas (y el administrador ya creado).
    pub fn start() -> Self {
        Self::launch(false)
    }

    /// Servidor en modo desarrollo: la señalización acepta [`DEV_TOKEN`] sin cuenta.
    pub fn start_dev() -> Self {
        Self::launch(true)
    }

    fn launch(dev: bool) -> Self {
        stall_watch();
        let dir = tempfile::tempdir().expect("directorio temporal");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime del servidor");
        let any: SocketAddr = "127.0.0.1:0".parse().expect("dirección");
        let mut config = Config::self_signed(any, any, dir.path().to_owned());
        config.public_host = "127.0.0.1".to_owned();
        if dev {
            config.dev_token = Some(DEV_TOKEN.to_owned());
        }
        let server = runtime
            .block_on(Server::bind(&config))
            .expect("arranque del servidor");
        let url = format!("https://127.0.0.1:{}", server.udp_addr().port());
        let trust = ServerTrust::Fingerprint(server.fingerprint());
        let tcp_port = server.tcp_addr().port();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        runtime.spawn(server.run(async move {
            let _ = stopped.await;
        }));
        let mut test = Self {
            url,
            trust,
            runtime,
            tcp_port,
            admin: None,
            dev,
            next: AtomicU32::new(1),
            stop: Some(stop),
            _dir: dir,
        };
        if !dev {
            let api = test.api();
            let admin = test.runtime.block_on(async {
                api.setup("admin", PASSWORD).await.expect("setup");
                api.admin_login("admin", PASSWORD)
                    .await
                    .expect("login del administrador")
            });
            test.admin = Some(test.api().with_token(admin.token));
        }
        test
    }

    /// Cliente de la API sin sesión.
    pub fn api(&self) -> ApiClient {
        ApiClient::new(&self.url, self.trust.clone())
            .expect("cliente")
            .with_tcp_port(self.tcp_port)
    }

    fn remote_config(&self, token: &str) -> RemoteConfig {
        let mut config = RemoteConfig::new(&self.url, token, self.trust.clone());
        config.bind = Some("127.0.0.1:0".parse().expect("dirección"));
        config
    }

    /// Cuenta nueva (`jugadorN`) con su identidad vinculada; en modo desarrollo, una identidad
    /// nueva con el token de desarrollo.
    pub fn config(&self) -> Account {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let identity = Identity::generate().expect("identidad");
        if self.dev {
            return Account {
                identity,
                config: self.remote_config(DEV_TOKEN),
                user: 0,
                username: "desarrollo".to_owned(),
            };
        }
        let username = format!("jugador{n}");
        let admin = self.admin.as_ref().expect("administrador");
        let api = self.api();
        let token = self.runtime.block_on(async {
            admin
                .create_user(&CreateUser {
                    username: username.clone(),
                    password: PASSWORD.to_owned(),
                    display_name: Some(format!("Jugador {n}")),
                    is_admin: false,
                })
                .await
                .expect("alta del usuario");
            api.login(&username, PASSWORD, &identity, "equipo de prueba")
                .await
                .expect("login del usuario")
        });
        Account {
            identity,
            config: self.remote_config(&token.token),
            user: token.user.id,
            username,
        }
    }

    /// Hace amigos a `a` y `b`.
    pub fn befriend(&self, a: &Account, b: &Account) {
        let api_a = self.api().with_token(a.config.token.clone());
        let api_b = self.api().with_token(b.config.token.clone());
        let b_name = b.username.clone();
        self.runtime.block_on(async {
            api_a.add_friend(&b_name).await.expect("solicitud");
            let incoming = api_b.friends().await.expect("amigos").incoming;
            api_b.accept_friend(incoming[0].id).await.expect("aceptar");
        });
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Lo que tarda el equipo de referencia en codificar [`CALIBRATION_FRAMES`] fotogramas de 640×360
/// con OpenH264 en debug (el perfil de los tests), sin carga: el de la máquina de desarrollo
/// de referencia.
pub const REFERENCE_ENCODE: Duration = Duration::from_millis(REFERENCE_ENCODE_MS);

/// Milisegundos de [`REFERENCE_ENCODE`].
pub const REFERENCE_ENCODE_MS: u64 = 120;

/// Fotogramas que se codifican para medir la carga.
pub const CALIBRATION_FRAMES: u64 = 30;

/// Factor de carga del entorno (≥ 1, como mucho 3): lo que tarda aquí codificar
/// [`CALIBRATION_FRAMES`] fotogramas frente a [`REFERENCE_ENCODE`], medido una vez por proceso.
///
/// Los tests sensibles a la carga multiplican por él las cotas de **tiempo de reloj** (huecos,
/// |A−V|), nunca las de corrección (fotogramas entregados, silencios): en el equipo de referencia
/// la exigencia es la misma que en su diseño, y en un runner compartido que va dos
/// veces más lento se admite el doble de hueco. Por encima de 3 el entorno no sirve para medir
/// tiempos y el factor se queda en 3 (el test puede fallar, y debe).
pub fn load_factor() -> f64 {
    static FACTOR: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *FACTOR.get_or_init(|| {
        let elapsed = encode_calibration();
        let factor = (elapsed.as_secs_f64() / REFERENCE_ENCODE.as_secs_f64()).clamp(1.0, 3.0);
        println!(
            "calibración: {CALIBRATION_FRAMES} fotogramas en {elapsed:?} (referencia \
             {REFERENCE_ENCODE:?}): factor de carga {factor:.2}"
        );
        factor
    })
}

/// Tiempo de codificar [`CALIBRATION_FRAMES`] fotogramas sintéticos de 640×360 (sin la captura).
pub fn encode_calibration() -> Duration {
    use coroom_session::capture::Capturer;
    use coroom_session::capture::synthetic::SyntheticCapturer;
    use coroom_session::codec::openh264::OpenH264Encoder;
    use coroom_session::codec::{Codec, Encoder, EncoderConfig, VideoFormat};

    let mut capturer = SyntheticCapturer::new(640, 360, 1_000);
    let target = capturer
        .targets()
        .ok()
        .and_then(|targets| targets.into_iter().next())
        .expect("fuente sintética");
    capturer.start(&target).expect("captura sintética");
    let frames: Vec<_> = (0..CALIBRATION_FRAMES)
        .map(|_| coroom_session::loopback::to_raw_frame(capturer.next_frame().expect("fotograma")))
        .collect();
    let mut encoder = OpenH264Encoder::new();
    encoder
        .configure(&EncoderConfig {
            codec: Codec::H264,
            width: 640,
            height: 360,
            fps: 30,
            bitrate_kbps: 1_500,
            slices: 1,
            format: VideoFormat::SDR_420,
        })
        .expect("OpenH264");
    let started = std::time::Instant::now();
    for frame in &frames {
        encoder.encode(frame).expect("codificar");
    }
    started.elapsed()
}

/// Paradas del sistema (no de coroom): un hilo duerme [`STALL_TICK`] en bucle y anota cuándo
/// despierta más de [`STALL_MIN`] tarde. En la máquina virtual de los tests (y en los runners de la
/// CI) todo el proceso se queda a veces parado cientos de milisegundos, captura del anfitrión
/// incluida; los tests de cuelgues descuentan ese tiempo ([`stalled_between`]).
pub struct StallWatch {
    stalls: Mutex<Vec<(Instant, Instant)>>,
}

/// Lo que duerme el hilo vigilante en cada vuelta.
const STALL_TICK: Duration = Duration::from_millis(2);

/// Retraso del vigilante a partir del cual se anota una parada.
const STALL_MIN: Duration = Duration::from_millis(20);

/// Arranca el vigilante (una vez por proceso) y lo devuelve.
pub fn stall_watch() -> &'static StallWatch {
    static WATCH: OnceLock<&'static StallWatch> = OnceLock::new();
    WATCH.get_or_init(|| {
        let watch: &'static StallWatch = Box::leak(Box::new(StallWatch {
            stalls: Mutex::new(Vec::new()),
        }));
        std::thread::Builder::new()
            .name("stall-watch".into())
            .spawn(move || {
                loop {
                    let before = Instant::now();
                    std::thread::sleep(STALL_TICK);
                    let after = Instant::now();
                    if after - before > STALL_TICK + STALL_MIN
                        && let Ok(mut stalls) = watch.stalls.lock()
                    {
                        stalls.push((before + STALL_TICK, after));
                    }
                }
            })
            .expect("hilo vigilante");
        watch
    })
}

/// Tiempo que el sistema estuvo parado entre `from` y `to`.
pub fn stalled_between(from: Instant, to: Instant) -> Duration {
    let Ok(stalls) = stall_watch().stalls.lock() else {
        return Duration::ZERO;
    };
    stalls
        .iter()
        .map(|&(start, end)| end.min(to).saturating_duration_since(start.max(from)))
        .sum()
}
