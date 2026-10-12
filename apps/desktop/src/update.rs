//! Aviso de versión nueva.
//!
//! La aplicación pregunta a la API pública de GitHub por la última publicación del proyecto
//! (`GET https://api.github.com/repos/<propietario>/<repositorio>/releases/latest`, sin token: el
//! límite anónimo de 60 peticiones por hora y por IP sobra para una al día), compara su etiqueta
//! (`vX.Y.Z`) con la versión de este binario (`CARGO_PKG_VERSION`) según las reglas de
//! precedencia de SemVer 2.0 y, si es más nueva, Inicio y Acerca de lo dicen con el enlace a la
//! publicación.
//!
//! Va por el cliente HTTP de siempre ([`coroom_transport::api::ApiClient`]): HTTP/2 por TCP con
//! las raíces de confianza del sistema. No envía nada del usuario: solo `User-Agent: coroom/X.Y.Z`
//! (GitHub lo exige). Se desactiva en Ajustes (Acerca de) o con la variable
//! `COROOM_NO_UPDATE_CHECK` (cualquier valor salvo `0`), por ejemplo en paquetes que se actualizan
//! por su propio canal.

use std::cmp::Ordering;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coroom_transport::ServerTrust;
use coroom_transport::api::ApiClient;
use serde::Deserialize;

/// Repositorio del proyecto en GitHub (`propietario/nombre`).
pub const REPOSITORY: &str = "43fdfdg45454/coroom";

/// Servidor de la API de GitHub.
pub const API_URL: &str = "https://api.github.com";

/// Página de las publicaciones, para cuando no hay un enlace concreto.
pub const RELEASES_PAGE: &str = "https://github.com/43fdfdg45454/coroom/releases";

/// Variable de entorno que desactiva la comprobación.
pub const DISABLE_ENV: &str = "COROOM_NO_UPDATE_CHECK";

/// Intervalos que se ofrecen en Ajustes, en horas (un día, una semana).
pub const INTERVAL_CHOICES: [u32; 2] = [24, 168];

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

/// Una publicación más nueva que esta versión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// Versión publicada.
    pub version: String,
    /// Página de la publicación (con las notas y los paquetes).
    pub url: String,
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
}

/// Lee la respuesta de `releases/latest` y dice si es más nueva que `current`.
pub fn newer_than(current: &str, body: &[u8]) -> Result<Option<Update>, String> {
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
    Ok(Some(Update {
        version: latest.to_string(),
        url,
    }))
}

/// Si `COROOM_NO_UPDATE_CHECK` desactiva la comprobación.
pub fn disabled_by_env() -> bool {
    std::env::var_os(DISABLE_ENV).is_some_and(|value| value != "0" && !value.is_empty())
}

/// Segundos desde 1970 de ahora.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Si toca comprobar: nunca se comprobó, o la última vez fue hace `interval_hours` o más (o en el
/// futuro: el reloj cambió).
pub fn due(last_check: Option<u64>, now: u64, interval_hours: u32) -> bool {
    match last_check {
        None => true,
        Some(last) => last > now || now - last >= u64::from(interval_hours.max(1)) * 3600,
    }
}

/// Pregunta a GitHub (bloquea hasta [`TIMEOUT`]): `Ok(None)` si esta es la última versión.
pub fn check(current: &str) -> Result<Option<Update>, String> {
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
                    .with_header("user-agent", &format!("coroom/{current}"))
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

/// Abre `url` en el navegador: el portal `OpenURI` en Linux (funciona dentro de Flatpak), con
/// `xdg-open` de respaldo; el explorador en Windows.
pub fn open_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("solo se abren enlaces https".to_owned());
    }
    #[cfg(target_os = "linux")]
    {
        let portal = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| err.to_string())
            .and_then(|runtime| {
                runtime.block_on(async {
                    let uri = ashpd::Uri::parse(url).map_err(|err| err.to_string())?;
                    ashpd::desktop::open_uri::OpenFileRequest::default()
                        .send_uri(&uri)
                        .await
                        .map(|_| ())
                        .map_err(|err| err.to_string())
                })
            });
        match portal {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::debug!(error = %err, "OpenURI no respondió; se prueba xdg-open");
                std::process::Command::new("xdg-open")
                    .arg(url)
                    .spawn()
                    .map(|_| ())
                    .map_err(|err| format!("no se pudo abrir el navegador: {err}"))
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("no se pudo abrir el navegador: {err}"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(format!("abre {url} en el navegador"))
    }
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

    #[test]
    fn only_newer_published_releases_count() {
        let body = |tag: &str, pre: bool| {
            format!(
                r#"{{"tag_name":"{tag}","html_url":"https://github.com/43fdfdg45454/coroom/releases/tag/{tag}","draft":false,"prerelease":{pre},"assets":[]}}"#
            )
        };
        let update = newer_than("0.1.0", body("v0.2.0", false).as_bytes())
            .unwrap()
            .expect("hay una nueva");
        assert_eq!(update.version, "0.2.0");
        assert!(update.url.ends_with("/tag/v0.2.0"));
        assert_eq!(
            newer_than("0.1.0", body("v0.1.0", false).as_bytes()),
            Ok(None)
        );
        assert_eq!(
            newer_than("0.2.0", body("v0.1.9", false).as_bytes()),
            Ok(None)
        );
        assert_eq!(
            newer_than("0.1.0", body("v0.2.0", true).as_bytes()),
            Ok(None),
            "las pre-publicaciones no avisan"
        );
        assert!(newer_than("0.1.0", b"no es json").is_err());
        assert!(newer_than("0.1.0", body("latest", false).as_bytes()).is_err());
        // Un enlace fuera de GitHub se sustituye por la página de publicaciones.
        let odd = r#"{"tag_name":"v9.0.0","html_url":"https://example.com/x"}"#;
        assert_eq!(
            newer_than("0.1.0", odd.as_bytes()).unwrap().map(|u| u.url),
            Some(RELEASES_PAGE.to_owned())
        );
    }

    #[test]
    fn checks_are_due_once_per_interval() {
        let day = 24 * 3600;
        assert!(due(None, 1_000, 24));
        assert!(!due(Some(1_000), 1_000 + day - 1, 24));
        assert!(due(Some(1_000), 1_000 + day, 24));
        assert!(!due(Some(1_000), 1_000 + 2 * day, 168));
        assert!(due(Some(5_000), 1_000, 24), "reloj hacia atrás");
        assert!(open_url("http://example.com").is_err());
    }
}
