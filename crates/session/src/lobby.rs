//! Sala de espera: la señalización con la cuenta abierta mientras no se está en ninguna sala, para
//! recibir invitaciones, presencia y actividad de los amigos y pedir entrar en la sala de uno.
//!
//! La comparten la aplicación de escritorio y el invitado Android: cada una traduce los
//! [`LobbyEvent`] a su interfaz y pone los textos en su idioma.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use coroom_transport::crypto::Identity;
use coroom_transport::proto::signal::SignalMessage;
use coroom_transport::{Endpoint, EndpointOptions, SignalClient};

use crate::account::Session;

/// Cada cuánto se da el estado de la conexión con el servidor ([`LobbyEvent::Online`]).
pub const SERVER_STATUS_EVERY: Duration = Duration::from_secs(5);

/// Espera entre un corte de la señalización y el siguiente intento ([`run`]).
pub const RETRY: Duration = Duration::from_secs(5);

/// Cada cuánto se mira si hay que parar o enviar algo de la cola.
const TICK: Duration = Duration::from_millis(200);

/// Lo que pasa en la sala de espera.
#[derive(Debug, Clone, PartialEq)]
pub enum LobbyEvent {
    /// Conectado con el servidor, con el RTT de la señalización.
    Online {
        /// RTT en milisegundos.
        rtt_ms: f64,
    },
    /// Se cortó la señalización (se reintenta tras [`RETRY`]); por qué.
    Offline(String),
    /// Un mensaje del servidor (invitación, presencia, actividad, avatar, peticiones de entrar o
    /// de amistad, o el error con que responde a una petición).
    Signal(SignalMessage),
    /// Un mensaje de la cola no salió: sin conexión (`None`) o con este error.
    SendFailed(Option<String>),
}

/// Abre la sala de espera y la mantiene hasta `stop`, reconectando tras [`RETRY`] si se corta.
/// `session` se vuelve a pedir en cada intento (la cuenta puede cambiar de token). Lo que llegue
/// por `outbox` (una petición de entrar, por ejemplo) se envía al servidor.
pub fn run(
    session: &dyn Fn() -> Result<(Session, Identity), String>,
    stop: &AtomicBool,
    outbox: &mpsc::Receiver<SignalMessage>,
    on_event: &mut dyn FnMut(LobbyEvent),
) {
    while !stop.load(Ordering::Acquire) {
        let result = session()
            .and_then(|(session, identity)| run_once(&session, &identity, stop, outbox, on_event));
        if let Err(err) = result {
            tracing::debug!(error = %err, "sala de espera");
            if !stop.load(Ordering::Acquire) {
                on_event(LobbyEvent::Offline(err));
            }
        }
        let deadline = std::time::Instant::now() + RETRY;
        while std::time::Instant::now() < deadline {
            if stop.load(Ordering::Acquire) {
                return;
            }
            // Sin señalización no se puede enviar nada: lo encolado se descarta avisando.
            while outbox.try_recv().is_ok() {
                on_event(LobbyEvent::SendFailed(None));
            }
            std::thread::sleep(TICK);
        }
    }
}

/// Una conexión de la sala de espera, hasta que se corte (`Err`) o se pida parar (`Ok`).
pub fn run_once(
    session: &Session,
    identity: &Identity,
    stop: &AtomicBool,
    outbox: &mpsc::Receiver<SignalMessage>,
    on_event: &mut dyn FnMut(LobbyEvent),
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    runtime.block_on(async {
        let endpoint = Endpoint::bind(identity, None, EndpointOptions::default())
            .map_err(|err| err.to_string())?;
        let mut signal = SignalClient::connect(
            &session.credentials.server,
            &session.credentials.token,
            &endpoint,
            &session.trust,
        )
        .await
        .map_err(|err| err.to_string())?;
        let mut tick = tokio::time::interval(TICK);
        let mut status_tick = tokio::time::interval(SERVER_STATUS_EVERY);
        let result = loop {
            tokio::select! {
                _ = status_tick.tick() => {
                    on_event(LobbyEvent::Online { rtt_ms: signal.rtt().as_secs_f64() * 1000.0 });
                }
                event = signal.next_event() => match event {
                    Ok(Some(message)) => on_event(LobbyEvent::Signal(message)),
                    Ok(None) => break Err("el servidor cerró la señalización".to_owned()),
                    Err(err) => break Err(err.to_string()),
                },
                _ = tick.tick() => {
                    // La parada se mira antes de vaciar la cola: lo que se encoló antes de pedirla
                    // (una petición de entrar justo antes de cerrar la sala de espera) sale
                    // todavía, en vez de perderse en silencio.
                    let stopping = stop.load(Ordering::Acquire);
                    while let Ok(message) = outbox.try_recv() {
                        if let Err(err) = signal.send(&message).await {
                            on_event(LobbyEvent::SendFailed(Some(err.to_string())));
                        }
                    }
                    if stopping {
                        break Ok(());
                    }
                }
            }
        };
        signal.close();
        endpoint.close();
        result
    })
}
