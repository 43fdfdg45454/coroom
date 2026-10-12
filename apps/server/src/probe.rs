//! Autocomprobación de UDP para `/health` (`udp_reachable`, `udp_checked_at`).
//!
//! Al arrancar y cada [`PROBE_PERIOD`], el servidor intenta un handshake QUIC (ALPN `h3`) consigo
//! mismo en `COROOM_PUBLIC_HOST:COROOM_PUBLIC_UDP_PORT`, con [`PROBE_TIMEOUT`] de límite, y solo
//! lo da por bueno si el certificado que responde es el suyo (por su huella): así no cuenta otro
//! servidor que conteste en esa dirección. Una NAT sin *hairpin* (sin bucle de vuelta a la propia
//! IP pública) puede dar `false` aunque desde fuera funcione.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quinn::crypto::rustls::QuicClientConfig;
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, aws_lc_rs};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::sync::watch;
use tracing::{debug, info};

use coroom_transport::crypto::sha256_fingerprint;
use coroom_transport::proto::Alpn;

use crate::clock;
use crate::tls::CertResolver;

/// Cada cuánto se repite la comprobación.
pub const PROBE_PERIOD: Duration = Duration::from_secs(600);

/// Espera máxima del handshake.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Último resultado.
#[derive(Debug, Default)]
pub struct UdpStatus {
    inner: Mutex<(Option<bool>, Option<i64>)>,
}

impl UdpStatus {
    /// `(udp_reachable, udp_checked_at)`.
    pub fn get(&self) -> (Option<bool>, Option<i64>) {
        self.inner.lock().map(|inner| *inner).unwrap_or_default()
    }

    fn set(&self, reachable: bool) {
        if let Ok(mut inner) = self.inner.lock() {
            *inner = (Some(reachable), Some(clock::now()));
        }
    }
}

/// Acepta solo el certificado del propio servidor.
#[derive(Debug)]
struct OwnCertificate {
    resolver: Arc<CertResolver>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for OwnCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if sha256_fingerprint(end_entity.as_ref()) == self.resolver.fingerprint() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Un intento: `true` si el servidor se alcanza a sí mismo por UDP en `host:port`.
pub async fn check(host: &str, port: u16, resolver: Arc<CertResolver>) -> bool {
    let mut endpoints = Endpoints::default();
    check_with(&mut endpoints, host, port, resolver).await
}

/// Endpoints cliente de la autocomprobación, uno por familia de direcciones, creados la primera
/// vez y reutilizados: cada `quinn::Endpoint` reserva sus búferes de recepción y crear uno por
/// comprobación dejaba la memoria residente del servidor unos MB más alta cada 10 minutos (lo
/// destapó la prueba de larga duración).
#[derive(Default)]
pub struct Endpoints {
    v4: Option<quinn::Endpoint>,
    v6: Option<quinn::Endpoint>,
}

impl Endpoints {
    fn for_addr(&mut self, addr: SocketAddr) -> Option<&quinn::Endpoint> {
        let (slot, local) = if addr.is_ipv6() {
            (
                &mut self.v6,
                SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)),
            )
        } else {
            (
                &mut self.v4,
                SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0)),
            )
        };
        if slot.is_none() {
            *slot = quinn::Endpoint::client(local).ok();
        }
        slot.as_ref()
    }
}

/// Como [`check`], con los endpoints de `endpoints`.
pub async fn check_with(
    endpoints: &mut Endpoints,
    host: &str,
    port: u16,
    resolver: Arc<CertResolver>,
) -> bool {
    let attempt = async {
        let addr: SocketAddr = tokio::net::lookup_host((host, port)).await.ok()?.next()?;
        let mut tls =
            rustls::ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .ok()?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(OwnCertificate {
                    resolver,
                    algorithms: aws_lc_rs::default_provider().signature_verification_algorithms,
                }))
                .with_no_client_auth();
        tls.alpn_protocols = vec![Alpn::H3.as_bytes().to_vec()];
        let crypto = QuicClientConfig::try_from(tls).ok()?;
        let endpoint = endpoints.for_addr(addr)?;
        let connection = endpoint
            .connect_with(quinn::ClientConfig::new(Arc::new(crypto)), addr, host)
            .ok()?
            .await
            .ok()?;
        connection.close(quinn::VarInt::from_u32(0), b"autocomprobacion");
        Some(())
    };
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, attempt).await,
        Ok(Some(()))
    )
}

/// Comprueba al arrancar y cada [`PROBE_PERIOD`] hasta que `stop` cambie.
pub async fn run(
    status: Arc<UdpStatus>,
    host: String,
    port: u16,
    resolver: Arc<CertResolver>,
    mut stop: watch::Receiver<bool>,
) {
    let mut endpoints = Endpoints::default();
    loop {
        let reachable = check_with(&mut endpoints, &host, port, Arc::clone(&resolver)).await;
        status.set(reachable);
        if reachable {
            debug!(%host, port, "autocomprobación de UDP correcta");
        } else {
            info!(
                %host,
                port,
                "autocomprobación de UDP fallida: el servidor no se alcanza a sí mismo por UDP \
                 (revisa el puerto UDP; una NAT sin hairpin también da este resultado)"
            );
        }
        tokio::select! {
            () = tokio::time::sleep(PROBE_PERIOD) => {}
            _ = stop.changed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prueba de larga duración: la autocomprobación reutiliza su endpoint cliente (antes
    /// creaba uno cada 10 minutos y la memoria residente subía unos MB cada vez; con 80
    /// comprobaciones seguidas, +27 MB frente a +2 MB ahora).
    #[tokio::test]
    async fn the_probe_reuses_one_client_endpoint_per_family() {
        let mut endpoints = Endpoints::default();
        let v4: SocketAddr = ([127, 0, 0, 1], 4433).into();
        let first = endpoints
            .for_addr(v4)
            .and_then(|endpoint| endpoint.local_addr().ok())
            .expect("endpoint IPv4");
        let again = endpoints
            .for_addr(v4)
            .and_then(|endpoint| endpoint.local_addr().ok())
            .expect("endpoint IPv4");
        assert_eq!(first, again, "el mismo socket");
        assert!(endpoints.v6.is_none(), "IPv6 solo cuando hace falta");
    }
}
