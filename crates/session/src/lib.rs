//! Sesiones de coroom: anfitrión e invitado.
//!
//! Define el trait [`Session`], la máquina de estados que orquesta captura, códec, audio, entrada y
//! transporte (`docs/ARQUITECTURA.md` §5). Una sala tiene un anfitrión y N invitados; cada invitado tiene
//! [`Permissions`] propios (mando, teclado y ratón, o solo mirar) que el anfitrión aprueba, pausa o
//! revoca. La capa de sesión también gobierna el control de tasa y el reloj compartido para las
//! marcas de tiempo por etapa.
//!
//! La sesión local ([`LocalSession`], sobre [`loopback::LoopbackPipeline`]) y la
//! remota ([`remote::RemoteSession`]) implementan este trait. Las marcas de tiempo por
//! etapa y su resumen viven en [`latency`].
//!
//! [`account`] tiene la lógica de cuentas que comparten las aplicaciones (sesión de cada
//! servidor y llamadas a su API), con el almacén de credenciales tras
//! [`account::AccountStorage`], que implementa cada plataforma.
//!
//! [`update`] busca versiones nuevas en las publicaciones de GitHub, descarga y comprueba los
//! paquetes y decide cuándo se descarga y se reinicia (nunca con una sala abierta).
//!
//! Reexporta [`coroom_audio`], [`coroom_capture`], [`coroom_codec`], [`coroom_input`] y
//! [`coroom_transport`] como [`audio`], [`capture`], [`codec`], [`input`] y [`transport`] para que
//! las aplicaciones de `apps/` no dependan directamente de ellas.

use std::fmt;
use std::sync::Arc;

pub use coroom_audio as audio;
pub use coroom_capture as capture;
pub use coroom_codec as codec;
pub use coroom_input as input;
pub use coroom_transport as transport;

pub mod account;
pub mod latency;
pub mod lobby;
pub mod loopback;
pub mod preview;
pub mod remote;
pub mod stats;
pub mod update;

use latency::{LatencyLedger, LatencySummary};
use loopback::{FrameSink, LoopbackConfig, LoopbackPipeline};

/// Papel del par local en la sesión.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Comparte su pantalla y recibe la entrada.
    Host,
    /// Ve la pantalla del anfitrión y envía entrada.
    Guest,
}

/// Estado de la sesión.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// Creada, sin conectar.
    Idle,
    /// Negociando la conexión (directa o por relay).
    Connecting,
    /// Medios fluyendo.
    Active,
    /// El anfitrión pausó la entrada de todos los invitados.
    Paused,
    /// Terminada; no se puede reanudar.
    Closed,
}

impl SessionState {
    /// Indica si se puede pasar de `self` a `next`.
    pub const fn can_transition_to(self, next: SessionState) -> bool {
        use SessionState::{Active, Closed, Connecting, Idle, Paused};
        matches!(
            (self, next),
            (Idle, Connecting)
                | (Connecting, Active)
                | (Active, Paused)
                | (Paused, Active)
                | (Idle | Connecting | Active | Paused, Closed)
        )
    }
}

/// Identificador de un invitado dentro de la sala (el mismo que usa la inyección de entrada).
pub use coroom_input::GuestId;

/// Permisos de un invitado (el anfitrión los aplica antes de inyectar su entrada).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Permissions {
    /// Puede usar teclado y ratón.
    pub keyboard_mouse: bool,
    /// Ranura de mando asignada (desde 1, como mucho `COROOM_MAX_GAMEPADS`), si tiene.
    pub gamepad_slot: Option<u8>,
    /// Puede hablar por voz.
    pub voice: bool,
    /// Su mando va tal cual (con su identidad y sus botones) si se puede; si no, convertido a
    /// Xbox 360.
    pub gamepad_raw: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self::VIEW_ONLY
    }
}

impl Permissions {
    /// Solo mirar: sin entrada ni voz (con el mando tal cual para cuando se le dé ranura). Es con
    /// lo que entra un invitado si la sala pide que entren callados ([`Permissions::on_entry`]).
    pub const VIEW_ONLY: Permissions = Permissions {
        keyboard_mouse: false,
        gamepad_slot: None,
        voice: false,
        gamepad_raw: true,
    };

    /// Con lo que entra un invitado: mirar y hablar, salvo que la sala pida que los invitados
    /// entren callados (`muted`); entonces solo mirar, y el anfitrión da la voz a quien quiera.
    pub const fn on_entry(muted: bool) -> Permissions {
        Permissions {
            voice: !muted,
            ..Self::VIEW_ONLY
        }
    }
}

/// Error de la sesión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// Transición de estado no válida.
    InvalidTransition {
        /// Estado de partida.
        from: SessionState,
        /// Estado pedido.
        to: SessionState,
    },
    /// La operación solo la puede hacer el anfitrión.
    HostOnly,
    /// La operación solo la puede hacer un invitado.
    GuestOnly,
    /// El invitado no está en la sala.
    UnknownGuest(GuestId),
    /// El pipeline de medios no pudo arrancar o se detuvo con un error.
    Media(String),
    /// Falló la red: el servidor, la señalización o la conexión con el par.
    Network(String),
    /// El servidor rechazó la petición (token, sala inexistente o llena…); el texto es el suyo.
    Rejected(String),
    /// Falló la criptografía de la sala (MLS, SFrame o HPKE).
    Crypto(String),
    /// El anfitrión nos expulsó de la sala.
    Removed,
    /// El anfitrión cerró la sala.
    RoomClosed,
    /// Permisos que no se pueden conceder (ranura fuera de rango o ya ocupada).
    InvalidPermissions(String),
    /// El mensaje del chat no se envió: el texto es el motivo, para enseñarlo tal cual
    /// («Vas muy rápido…», «El mensaje está vacío.»).
    Chat(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::InvalidTransition { from, to } => {
                write!(f, "transición de sesión no válida: {from:?} → {to:?}")
            }
            SessionError::HostOnly => f.write_str("solo el anfitrión puede hacer esto"),
            SessionError::GuestOnly => f.write_str("solo un invitado puede hacer esto"),
            SessionError::UnknownGuest(id) => write!(f, "el invitado {} no está en la sala", id.0),
            SessionError::Media(what) => write!(f, "error de medios: {what}"),
            SessionError::Network(what) => write!(f, "error de red: {what}"),
            SessionError::Rejected(what) => write!(f, "el servidor lo rechazó: {what}"),
            SessionError::Crypto(what) => write!(f, "error de cifrado: {what}"),
            SessionError::Removed => f.write_str("el anfitrión te ha expulsado de la sala"),
            SessionError::RoomClosed => f.write_str("el anfitrión ha cerrado la sala"),
            SessionError::InvalidPermissions(what) => write!(f, "permisos no válidos: {what}"),
            SessionError::Chat(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SessionError {}

/// Sesión de coroom vista desde el par local.
///
/// Es `Send` porque la conduce la tarea de red mientras la interfaz la consulta.
pub trait Session: Send {
    /// Papel del par local.
    fn role(&self) -> Role;

    /// Estado actual.
    fn state(&self) -> SessionState;

    /// Empieza a conectar con la sala.
    fn start(&mut self) -> Result<(), SessionError>;

    /// Pausa o reanuda la entrada de todos los invitados (solo anfitrión).
    fn set_paused(&mut self, paused: bool) -> Result<(), SessionError>;

    /// Cambia los permisos de un invitado (solo anfitrión).
    fn set_permissions(
        &mut self,
        guest: GuestId,
        permissions: Permissions,
    ) -> Result<(), SessionError>;

    /// Expulsa a un invitado (solo anfitrión).
    fn kick(&mut self, guest: GuestId) -> Result<(), SessionError>;

    /// Cierra la sesión.
    fn close(&mut self);
}

/// Piezas del pipeline a la espera de [`Session::start`].
struct PipelineParts {
    capturer: Box<dyn capture::Capturer>,
    encoder: Box<dyn codec::Encoder>,
    decoder: Box<dyn codec::Decoder>,
    sink: Box<dyn FrameSink>,
    config: LoopbackConfig,
}

/// Sesión de anfitrión sin red: el pipeline local detrás del trait [`Session`].
///
/// `start` pasa por `Connecting` y llega a `Active` cuando el pipeline arranca; `close` lo detiene,
/// guarda el resumen final y deja la sesión en `Closed`. No hay invitados, así que sus operaciones
/// devuelven [`SessionError::UnknownGuest`].
pub struct LocalSession {
    state: SessionState,
    parts: Option<PipelineParts>,
    pipeline: Option<LoopbackPipeline>,
    ledger: Arc<LatencyLedger>,
    summary: Option<LatencySummary>,
}

impl LocalSession {
    /// Prepara una sesión local con sus piezas; no arranca nada hasta [`Session::start`].
    pub fn new(
        capturer: Box<dyn capture::Capturer>,
        encoder: Box<dyn codec::Encoder>,
        decoder: Box<dyn codec::Decoder>,
        sink: Box<dyn FrameSink>,
        ledger: Arc<LatencyLedger>,
        config: LoopbackConfig,
    ) -> Self {
        Self {
            state: SessionState::Idle,
            parts: Some(PipelineParts {
                capturer,
                encoder,
                decoder,
                sink,
                config,
            }),
            pipeline: None,
            ledger,
            summary: None,
        }
    }

    /// Registro de marcas de tiempo de la sesión.
    pub fn ledger(&self) -> &Arc<LatencyLedger> {
        &self.ledger
    }

    /// Pipeline en marcha, mientras la sesión no esté cerrada.
    pub fn pipeline(&self) -> Option<&LoopbackPipeline> {
        self.pipeline.as_ref()
    }

    /// Resumen final, disponible tras [`Session::close`].
    pub fn summary(&self) -> Option<&LatencySummary> {
        self.summary.as_ref()
    }

    fn transition(&mut self, to: SessionState) -> Result<(), SessionError> {
        if !self.state.can_transition_to(to) {
            return Err(SessionError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }
}

impl Session for LocalSession {
    fn role(&self) -> Role {
        Role::Host
    }

    fn state(&self) -> SessionState {
        self.state
    }

    fn start(&mut self) -> Result<(), SessionError> {
        self.transition(SessionState::Connecting)?;
        let Some(parts) = self.parts.take() else {
            self.state = SessionState::Closed;
            return Err(SessionError::Media("el pipeline ya se usó".into()));
        };
        match LoopbackPipeline::start(
            parts.capturer,
            parts.encoder,
            parts.decoder,
            parts.sink,
            Arc::clone(&self.ledger),
            parts.config,
        ) {
            Ok(pipeline) => {
                self.pipeline = Some(pipeline);
                self.transition(SessionState::Active)
            }
            Err(err) => {
                self.state = SessionState::Closed;
                Err(SessionError::Media(err.to_string()))
            }
        }
    }

    fn set_paused(&mut self, paused: bool) -> Result<(), SessionError> {
        // Sin invitados no hay entrada que pausar; solo cambia el estado.
        self.transition(if paused {
            SessionState::Paused
        } else {
            SessionState::Active
        })
    }

    fn set_permissions(
        &mut self,
        guest: GuestId,
        _permissions: Permissions,
    ) -> Result<(), SessionError> {
        Err(SessionError::UnknownGuest(guest))
    }

    fn kick(&mut self, guest: GuestId) -> Result<(), SessionError> {
        Err(SessionError::UnknownGuest(guest))
    }

    fn close(&mut self) {
        if let Some(pipeline) = self.pipeline.take() {
            self.summary = Some(pipeline.stop());
        }
        self.parts = None;
        self.state = SessionState::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct GuestSession {
        state: SessionState,
    }

    impl GuestSession {
        fn transition(&mut self, to: SessionState) -> Result<(), SessionError> {
            if !self.state.can_transition_to(to) {
                return Err(SessionError::InvalidTransition {
                    from: self.state,
                    to,
                });
            }
            self.state = to;
            Ok(())
        }
    }

    impl Session for GuestSession {
        fn role(&self) -> Role {
            Role::Guest
        }

        fn state(&self) -> SessionState {
            self.state
        }

        fn start(&mut self) -> Result<(), SessionError> {
            self.transition(SessionState::Connecting)
        }

        fn set_paused(&mut self, _paused: bool) -> Result<(), SessionError> {
            Err(SessionError::HostOnly)
        }

        fn set_permissions(&mut self, _: GuestId, _: Permissions) -> Result<(), SessionError> {
            Err(SessionError::HostOnly)
        }

        fn kick(&mut self, _guest: GuestId) -> Result<(), SessionError> {
            Err(SessionError::HostOnly)
        }

        fn close(&mut self) {
            self.state = SessionState::Closed;
        }
    }

    fn assert_send<T: Send + ?Sized>() {}

    #[test]
    fn session_is_send_and_object_safe() {
        assert_send::<dyn Session>();
        let mut session: Box<dyn Session> = Box::new(GuestSession {
            state: SessionState::Idle,
        });
        assert_eq!(session.start(), Ok(()));
        assert_eq!(session.state(), SessionState::Connecting);
        assert_eq!(session.kick(GuestId(1)), Err(SessionError::HostOnly));
        session.close();
        assert_eq!(session.state(), SessionState::Closed);
    }

    #[test]
    fn closed_sessions_cannot_restart() {
        assert!(!SessionState::Closed.can_transition_to(SessionState::Connecting));
        assert!(SessionState::Paused.can_transition_to(SessionState::Active));
        assert!(!SessionState::Idle.can_transition_to(SessionState::Active));
    }

    #[test]
    fn guests_join_with_voice_unless_the_room_mutes_them() {
        assert_eq!(Permissions::VIEW_ONLY, Permissions::default());
        let entry = Permissions::on_entry(false);
        assert!(entry.voice && !entry.keyboard_mouse && entry.gamepad_slot.is_none());
        assert_eq!(Permissions::on_entry(true), Permissions::VIEW_ONLY);
    }

    fn local_session(frames: u64) -> LocalSession {
        LocalSession::new(
            Box::new(capture::synthetic::SyntheticCapturer::new(160, 90, 60)),
            Box::new(codec::openh264::OpenH264Encoder::new()),
            Box::new(codec::openh264::OpenH264Decoder::new().unwrap()),
            Box::new(loopback::NullSink),
            Arc::new(LatencyLedger::new()),
            LoopbackConfig {
                width: 160,
                height: 90,
                fps: 60,
                bitrate_kbps: 500,
                frames: Some(frames),
                yuv444: false,
                hdr: false,
            },
        )
    }

    #[test]
    fn local_session_runs_the_pipeline_as_host() {
        assert_send::<LocalSession>();
        let mut session = local_session(10);
        assert_eq!(session.role(), Role::Host);
        assert_eq!(session.state(), SessionState::Idle);
        session.start().unwrap();
        assert_eq!(session.state(), SessionState::Active);
        assert_eq!(
            session.kick(GuestId(3)),
            Err(SessionError::UnknownGuest(GuestId(3)))
        );
        assert_eq!(
            session.set_permissions(GuestId(4), Permissions::VIEW_ONLY),
            Err(SessionError::UnknownGuest(GuestId(4)))
        );
        session.set_paused(true).unwrap();
        assert_eq!(session.state(), SessionState::Paused);
        session.set_paused(false).unwrap();
        let pipeline = session.pipeline().expect("pipeline en marcha");
        assert!(pipeline.wait(std::time::Duration::from_secs(20)));
        session.close();
        assert_eq!(session.state(), SessionState::Closed);
        assert!(session.summary().expect("resumen").frames_presented >= 10);
        assert!(session.start().is_err(), "una sesión cerrada no se reanuda");
    }

    #[test]
    fn local_session_reports_start_failures() {
        let mut session = LocalSession::new(
            Box::new(capture::synthetic::SyntheticCapturer::new(160, 90, 60)),
            Box::new(codec::openh264::OpenH264Encoder::new()),
            Box::new(codec::openh264::OpenH264Decoder::new().unwrap()),
            Box::new(loopback::NullSink),
            Arc::new(LatencyLedger::new()),
            LoopbackConfig {
                width: 0,
                ..LoopbackConfig::default()
            },
        );
        assert!(matches!(session.start(), Err(SessionError::Media(_))));
        assert_eq!(session.state(), SessionState::Closed);
    }
}
