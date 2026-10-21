//! Versiones nuevas de la aplicación: lo que comparten el escritorio y Android.
//!
//! - Pregunta a la API pública de GitHub por la última publicación del proyecto
//!   (`GET https://api.github.com/repos/<propietario>/<repositorio>/releases/latest`, sin token)
//!   ([`check`]), compara su etiqueta (`vX.Y.Z`) con la versión propia según SemVer 2.0
//!   ([`Version`]) y lee sus paquetes ([`Asset`]: nombre, enlace, tamaño y el SHA-256 que GitHub
//!   publica en `digest`).
//! - Elige el paquete de cada plataforma por su nombre ([`Package`]): `coroom-X.Y.Z-windows-x64.zip`
//!   y `coroom-X.Y.Z-android.apk`.
//! - Lo descarga con progreso, pausa y reanudación ([`download`]) y comprueba su SHA-256.
//! - [`machine::Updater`] decide cuándo se descarga, se pausa y se reinicia: nunca con una sala
//!   abierta.
//! - [`protocol_gap`] dice cuál de los dos quedó atrás cuando el servidor rechaza la versión del
//!   protocolo.
//!
//! Sin internet no se encuentra nada y la aplicación sigue igual: cada error es un texto en español
//! para enseñarlo y se reintenta en la siguiente búsqueda.

use std::cmp::Ordering;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coroom_transport::ServerTrust;
use coroom_transport::api::ApiClient;
use coroom_transport::proto::PROTOCOL_VERSION;
use serde::Deserialize;

mod download;
pub mod machine;

pub use download::{DownloadControl, Downloaded, download, part_name, sha256_file, verify_sha256};
pub use machine::{
    Channel, Command, Failure, HomeNotice, NoticeOnly, Progress, Stage, Updater, Wait,
};

/// Repositorio del proyecto en GitHub (`propietario/nombre`).
pub const REPOSITORY: &str = "43fdfdg45454/coroom";

/// Servidor de la API de GitHub.
pub const API_URL: &str = "https://api.github.com";

/// Página de las publicaciones, para cuando no hay un enlace concreto.
pub const RELEASES_PAGE: &str = "https://github.com/43fdfdg45454/coroom/releases";

/// Prefijo de los enlaces de descarga que se aceptan (los de las publicaciones del proyecto).
pub const DOWNLOADS: &str = "https://github.com/43fdfdg45454/coroom/releases/download/";

/// Cuánto se espera la respuesta de GitHub.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Versión SemVer (`MAYOR.MENOR.PARCHE[-PRE][+BUILD]`; los metadatos de build no cuentan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Mayor.
    pub major: u64,
    /// Menor.
    pub minor: u64,
    /// Parche.
    pub patch: u64,
    /// Identificadores de pre-publicación (`beta.2` → `["beta", "2"]`); vacío en una publicación.
    pub pre: Vec<String>,
}

impl Version {
    /// Lee `1.2.3`, `v1.2.3`, `1.2.3-rc.1` o `1.2.3+abc`. `None` si no es SemVer.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut numbers = core.split('.');
        let mut number = || -> Option<u64> {
            let part = numbers.next()?;
            if part.is_empty()
                || !part.bytes().all(|b| b.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return None;
            }
            part.parse().ok()
        };
        let (major, minor, patch) = (number()?, number()?, number()?);
        if numbers.next().is_some() {
            return None;
        }
        let pre = match pre {
            Some(pre) => {
                let parts: Vec<String> = pre.split('.').map(str::to_owned).collect();
                let valid = parts.iter().all(|part| {
                    !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                });
                if !valid {
                    return None;
                }
                parts
            }
            None => Vec::new(),
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// Precedencia de SemVer 2.0 (§11): números, y una pre-publicación va antes que su publicación;
/// entre identificadores, los numéricos por valor y antes que los alfanuméricos.
impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(a), Ok(b)) => a.cmp(&b),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => a.cmp(b),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Los paquetes que se instalan solos, cada uno con su nombre fijo en la publicación.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Package {
    /// El zip portátil de Windows: `coroom-X.Y.Z-windows-x64.zip`.
    WindowsZip,
    /// El APK de Android: `coroom-X.Y.Z-android.apk`.
    AndroidApk,
}

impl Package {
    /// Nombre del paquete de la versión `version` (`2.1.0`) en la publicación.
    pub fn asset_name(self, version: &str) -> String {
        match self {
            Package::WindowsZip => format!("coroom-{version}-windows-x64.zip"),
            Package::AndroidApk => format!("coroom-{version}-android.apk"),
        }
    }
}

/// Un paquete de una publicación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// Nombre (`coroom-2.1.0-windows-x64.zip`).
    pub name: String,
    /// Enlace de descarga (`browser_download_url`, siempre de las publicaciones del proyecto).
    pub url: String,
    /// Tamaño en bytes.
    pub size: u64,
    /// SHA-256 que publica GitHub (`digest`: `sha256:<hex>`), si lo publica.
    pub sha256: Option<[u8; 32]>,
}

/// Una publicación más nueva que esta versión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Versión publicada (`2.1.0`).
    pub version: String,
    /// Página de la publicación (con las notas y los paquetes).
    pub url: String,
    /// Sus paquetes.
    pub assets: Vec<Asset>,
}

impl Release {
    /// Una publicación sin paquetes (las pruebas y las capturas).
    pub fn new(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            url: format!("{RELEASES_PAGE}/tag/v{version}"),
            assets: Vec::new(),
        }
    }

    /// El paquete `package` de esta versión, si la publicación lo trae.
    pub fn asset(&self, package: Package) -> Option<&Asset> {
        let name = package.asset_name(&self.version);
        self.assets.iter().find(|asset| asset.name == name)
    }
}

/// Lo que importa de `releases/latest`.
#[derive(Debug, Deserialize)]
struct LatestRelease {
    tag_name: String,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// Lee `sha256:<64 hex>` (el `digest` de un paquete de GitHub).
pub fn parse_digest(digest: &str) -> Option<[u8; 32]> {
    let hex = digest.trim().strip_prefix("sha256:")?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let text = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(text, 16).ok()?;
    }
    Some(out)
}

/// Lee la respuesta de `releases/latest` y la devuelve si es más nueva que `current`.
pub fn newer_than(current: &str, body: &[u8]) -> Result<Option<Release>, String> {
    let current =
        Version::parse(current).ok_or_else(|| format!("versión propia no válida: {current}"))?;
    let release: LatestRelease = serde_json::from_slice(body)
        .map_err(|err| format!("respuesta de GitHub no válida: {err}"))?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let Some(latest) = Version::parse(&release.tag_name) else {
        return Err(format!(
            "la última publicación tiene una etiqueta que no es una versión: {}",
            release.tag_name
        ));
    };
    if latest <= current {
        return Ok(None);
    }
    let url = release
        .html_url
        .filter(|url| url.starts_with("https://github.com/"))
        .unwrap_or_else(|| RELEASES_PAGE.to_owned());
    // Solo los enlaces de las publicaciones del proyecto: nada de descargar de otro sitio.
    let assets = release
        .assets
        .into_iter()
        .filter(|asset| asset.browser_download_url.starts_with(DOWNLOADS))
        .map(|asset| Asset {
            sha256: asset.digest.as_deref().and_then(parse_digest),
            name: asset.name,
            url: asset.browser_download_url,
            size: asset.size,
        })
        .collect();
    Ok(Some(Release {
        version: latest.to_string(),
        url,
        assets,
    }))
}

/// Segundos desde 1970 de ahora.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Si toca buscar: nunca se buscó, o la última vez fue hace `interval_hours` o más (o en el
/// futuro: el reloj cambió).
pub fn due(last_check: Option<u64>, now: u64, interval_hours: u32) -> bool {
    match last_check {
        None => true,
        Some(last) => last > now || now - last >= u64::from(interval_hours.max(1)) * 3600,
    }
}

/// Pregunta a GitHub (bloquea hasta [`TIMEOUT`]): `Ok(None)` si `current` es la última versión.
///
/// Va por el cliente HTTP de siempre ([`ApiClient`]): HTTP/2 por TCP con las raíces de confianza
/// del sistema. No envía nada del usuario: solo `User-Agent: coroom/X.Y.Z` (GitHub lo exige).
pub fn check(current: &str) -> Result<Option<Release>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("no se pudo preparar la red: {err}"))?;
    runtime.block_on(async {
        let trust = ServerTrust::system_roots().map_err(|err| err.to_string())?;
        let client = ApiClient::new(API_URL, trust)
            .and_then(|client| {
                client
                    .tcp_only()
                    .with_header("user-agent", &user_agent(current))
            })
            .and_then(|client| client.with_header("accept", "application/vnd.github+json"))
            .map_err(|err| err.to_string())?;
        let path = format!("/repos/{REPOSITORY}/releases/latest");
        let response = tokio::time::timeout(TIMEOUT, client.get(&path))
            .await
            .map_err(|_| "GitHub no respondió a tiempo".to_owned())?
            .map_err(|err| format!("no se pudo consultar GitHub: {err}"))?;
        match response.status.as_u16() {
            200 => newer_than(current, &response.body),
            // Sin publicaciones todavía.
            404 => Ok(None),
            403 | 429 => Err(
                "GitHub limita las consultas desde esta red; se reintentará más tarde".to_owned(),
            ),
            status => Err(format!("GitHub respondió {status}")),
        }
    })
}

/// `coroom/X.Y.Z`, el `User-Agent` de las peticiones a GitHub.
pub fn user_agent(current: &str) -> String {
    format!("coroom/{current}")
}

/// Cuál de los dos quedó atrás cuando el servidor rechaza la versión del protocolo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolGap {
    /// Esta aplicación es más vieja que el servidor: hay que actualizarla.
    ClientOlder {
        /// Protocolo del servidor.
        server: u16,
        /// Protocolo de esta aplicación.
        client: u16,
    },
    /// El servidor es más viejo: lo tiene que actualizar quien lo administra.
    ServerOlder {
        /// Protocolo del servidor.
        server: u16,
        /// Protocolo de esta aplicación.
        client: u16,
    },
}

/// Compara el protocolo que dice el servidor (`GET /version`, [`ServerProbe::protocol`]) con el
/// propio: `None` si coinciden (o el servidor no lo dijo: 0).
///
/// [`ServerProbe::protocol`]: coroom_transport::api::ServerProbe::protocol
pub fn protocol_gap(server: u16) -> Option<ProtocolGap> {
    protocol_gap_with(server, PROTOCOL_VERSION)
}

/// Como [`protocol_gap`], con el protocolo propio dado.
pub fn protocol_gap_with(server: u16, client: u16) -> Option<ProtocolGap> {
    match server.cmp(&client) {
        _ if server == 0 => None,
        Ordering::Equal => None,
        Ordering::Greater => Some(ProtocolGap::ClientOlder { server, client }),
        Ordering::Less => Some(ProtocolGap::ServerOlder { server, client }),
    }
}

/// Si el error del transporte es el rechazo del servidor por la versión del protocolo (el
/// `Error { code: ProtocolVersion }` de la señalización).
pub fn is_protocol_rejection(err: &coroom_transport::TransportError) -> bool {
    matches!(
        err,
        coroom_transport::TransportError::Rejected {
            code: coroom_transport::proto::signal::ErrorCode::ProtocolVersion,
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("versión válida")
    }

    #[test]
    fn versions_parse_like_semver() {
        assert_eq!(
            v("v1.2.3"),
            Version {
                major: 1,
                minor: 2,
                patch: 3,
                pre: Vec::new()
            }
        );
        assert_eq!(v("1.2.3-rc.1+abc").pre, vec!["rc", "1"]);
        assert_eq!(v("0.1.0").to_string(), "0.1.0");
        assert_eq!(v("2.0.0-beta.2").to_string(), "2.0.0-beta.2");
        for bad in [
            "",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.x.3",
            "1.2.3-",
            "1.2.3-a..b",
            "nightly",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    /// El orden del ejemplo de SemVer 2.0 §11.4.
    #[test]
    fn precedence_follows_the_spec() {
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("1.0.0+a").cmp(&v("1.0.0+b")), Ordering::Equal);
    }

    /// Una respuesta de `releases/latest` de ejemplo (ficticia), con los paquetes de verdad de una
    /// publicación: el zip y el MSIX de Windows, el APK y el Flatpak.
    fn sample(tag: &str, pre: bool) -> String {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let base = format!("{DOWNLOADS}{tag}");
        let version = tag.trim_start_matches('v');
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/43fdfdg45454/coroom/releases/tag/{tag}",
            "draft":false,"prerelease":{pre},"assets":[
              {{"name":"coroom-{version}-windows-x64.zip","browser_download_url":"{base}/coroom-{version}-windows-x64.zip","size":63963136,"digest":"{digest}"}},
              {{"name":"coroom-{version}-x64.msix","browser_download_url":"{base}/coroom-{version}-x64.msix","size":64000000,"digest":null}},
              {{"name":"coroom-{version}-android.apk","browser_download_url":"{base}/coroom-{version}-android.apk","size":31000000,"digest":"{digest}"}},
              {{"name":"coroom-{version}-android.apk.otro","browser_download_url":"https://example.com/x.apk","size":1}}
            ]}}"#
        )
    }

    #[test]
    fn only_newer_published_releases_count() {
        let update = newer_than("0.1.0", sample("v0.2.0", false).as_bytes())
            .expect("respuesta válida")
            .expect("hay una nueva");
        assert_eq!(update.version, "0.2.0");
        assert!(update.url.ends_with("/tag/v0.2.0"));
        assert_eq!(
            newer_than("0.1.0", sample("v0.1.0", false).as_bytes()),
            Ok(None)
        );
        assert_eq!(
            newer_than("0.2.0", sample("v0.1.9", false).as_bytes()),
            Ok(None)
        );
        assert_eq!(
            newer_than("0.1.0", sample("v0.2.0", true).as_bytes()),
            Ok(None),
            "las pre-publicaciones no avisan"
        );
        assert!(newer_than("0.1.0", b"no es json").is_err());
        assert!(newer_than("0.1.0", sample("latest", false).as_bytes()).is_err());
        // Un enlace fuera de GitHub se sustituye por la página de publicaciones.
        let odd = r#"{"tag_name":"v9.0.0","html_url":"https://example.com/x"}"#;
        let odd = newer_than("0.1.0", odd.as_bytes())
            .expect("válida")
            .expect("nueva");
        assert_eq!(odd.url, RELEASES_PAGE);
        assert!(odd.assets.is_empty(), "sin `assets` no hay paquetes");
    }

    #[test]
    fn packages_are_chosen_by_name_with_their_digest() {
        let release = newer_than("2.0.6", sample("v2.1.0", false).as_bytes())
            .expect("válida")
            .expect("nueva");
        let zip = release.asset(Package::WindowsZip).expect("zip");
        assert_eq!(zip.name, "coroom-2.1.0-windows-x64.zip");
        assert_eq!(zip.size, 63_963_136);
        assert_eq!(zip.sha256, Some([0xab; 32]));
        assert!(zip.url.starts_with(DOWNLOADS));
        let apk = release.asset(Package::AndroidApk).expect("apk");
        assert_eq!(apk.name, "coroom-2.1.0-android.apk");
        // Los enlaces de fuera del proyecto se descartan.
        assert!(release.assets.iter().all(|a| a.url.starts_with(DOWNLOADS)));
        assert_eq!(release.assets.len(), 3);
        assert_eq!(
            release
                .assets
                .iter()
                .find(|a| a.name.ends_with(".msix"))
                .map(|a| a.sha256),
            Some(None),
            "sin digest no hay SHA-256"
        );
        assert_eq!(
            Package::WindowsZip.asset_name("2.1.0"),
            "coroom-2.1.0-windows-x64.zip"
        );
        assert_eq!(
            Package::AndroidApk.asset_name("2.1.0"),
            "coroom-2.1.0-android.apk"
        );
        assert!(Release::new("2.1.0").asset(Package::WindowsZip).is_none());
    }

    #[test]
    fn digests_are_sha256_hex() {
        assert_eq!(
            parse_digest(&format!("sha256:{}", "0f".repeat(32))),
            Some([0x0f; 32])
        );
        assert_eq!(parse_digest(&format!("sha512:{}", "0f".repeat(32))), None);
        assert_eq!(parse_digest("sha256:abc"), None);
        assert_eq!(parse_digest(&format!("sha256:{}", "zz".repeat(32))), None);
    }

    #[test]
    fn checks_are_due_once_per_interval() {
        let day = 24 * 3600;
        assert!(due(None, 1_000, 24));
        assert!(!due(Some(1_000), 1_000 + day - 1, 24));
        assert!(due(Some(1_000), 1_000 + day, 24));
        assert!(!due(Some(1_000), 1_000 + 2 * day, 168));
        assert!(due(Some(5_000), 1_000, 24), "reloj hacia atrás");
    }

    #[test]
    fn protocol_gaps_say_who_is_behind() {
        assert_eq!(
            protocol_gap_with(23, 22),
            Some(ProtocolGap::ClientOlder {
                server: 23,
                client: 22
            })
        );
        assert_eq!(
            protocol_gap_with(21, 22),
            Some(ProtocolGap::ServerOlder {
                server: 21,
                client: 22
            })
        );
        assert_eq!(protocol_gap_with(22, 22), None);
        assert_eq!(protocol_gap_with(0, 22), None, "no lo dijo");
        assert!(is_protocol_rejection(
            &coroom_transport::TransportError::Rejected {
                code: coroom_transport::proto::signal::ErrorCode::ProtocolVersion,
                message: String::new(),
            }
        ));
        assert!(!is_protocol_rejection(
            &coroom_transport::TransportError::Closed
        ));
    }
}
