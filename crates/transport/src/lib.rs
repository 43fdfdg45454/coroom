//! Transporte de coroom sobre QUIC.
//!
//! Depende solo de [`coroom_proto`] y [`coroom_crypto`] (`docs/ARQUITECTURA.md` §5), que reexporta como
//! [`proto`] y [`crypto`] para que las aplicaciones de `apps/` no dependan directamente de ellas.
//!
//! - [`quic`]: el [`Endpoint`] (un socket UDP por proceso, cliente y servidor a la vez, TLS 1.3 con
//!   claves públicas crudas e intercambio X25519MLKEM768) y la [`PeerConnection`] con sus
//!   datagramas, su stream de control y el de entrada.
//! - [`media`]: [`MediaSender`] y [`MediaReceiver`] (objetos con FEC RaptorQ, pacing, NACK, plazos
//!   y reloj por `Ping`/`Pong`).
//! - [`rate`]: [`RateController`] tipo SCReAM y el controlador de congestión de `quinn` para medios.
//! - [`connect`]: [`Connector`] (apertura simultánea de NAT con candidatos y relay de respaldo).
//! - [`nat`]: [`Mapper`](nat::Mapper) (abre el puerto del `Endpoint` en el router por PCP, NAT-PMP o
//!   UPnP para que lleguen en directo).
//! - [`signal`]: [`SignalClient`] (señalización por WebTransport desde el mismo socket).
//! - [`api`]: [`ApiClient`] (API de cuentas, amigos y uso por HTTP/3 con respaldo HTTP/2 por TCP).
//! - [`download`]: descargas grandes por HTTPS con redirecciones y reanudación (`Range`), para
//!   las versiones nuevas de la aplicación.
//! - [`loopback`]: el bucle UDP de la prueba de latencia local.
//! - `lossy` (feature `test-util`): proxy UDP con pérdida y retardo para los tests.

pub use coroom_crypto as crypto;
pub use coroom_proto as proto;

pub mod api;
pub mod connect;
pub mod download;
mod error;
mod eyeballs;
pub mod fec;
pub mod loopback;
#[cfg(feature = "test-util")]
pub mod lossy;
pub mod media;
pub mod nat;
pub mod quic;
pub mod rate;
pub mod relay;
pub mod signal;
pub mod socket;
pub mod tls;
mod webtransport;

pub use api::ApiClient;
pub use connect::{
    CandidateClass, CandidateOutcome, CandidateReport, Connector, DirectAttempt, Established,
    RelayInfo,
};
pub use error::TransportError;
pub use eyeballs::remember_addresses_in;
pub use media::{
    CompleteFrame, MediaReceiver, MediaReceiverConfig, MediaSender, MediaSenderConfig,
};
pub use quic::{
    BiStream, ControlReceiver, ControlSender, ControlStream, Endpoint, EndpointOptions, Path,
    PeerConnection,
};
/// Mitades de un stream de QUIC (las del stream de entrada).
pub use quinn::{RecvStream, SendStream};
pub use rate::{Congestion, RateConfig, RateController};
pub use signal::{SignalClient, SignalSender};
pub use tls::{PublicKey, ServerTrust};
