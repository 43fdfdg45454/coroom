//! UPnP IGD: búsqueda SSDP de la puerta de enlace (`InternetGatewayDevice`), descripción del
//! dispositivo por HTTP, y `AddPortMapping`, `GetExternalIPAddress` y `DeletePortMapping` por SOAP
//! sobre su servicio `WANIPConnection` (2 o 1) o `WANPPPConnection`. HTTP/1.1 mínimo propio: una
//! petición por conexión, con `Content-Length` o `chunked` en la respuesta.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddrV4, ToSocketAddrs};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::Instant;
use tracing::debug;

use super::{Failure, Lease, Mapping, MappingProtocol};

/// Dirección multicast de SSDP.
pub const SSDP_MULTICAST: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
/// Servicios que abren puertos, por preferencia.
const SERVICES: [&str; 3] = [
    "urn:schemas-upnp-org:service:WANIPConnection:2",
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
];
/// Descripción de la asignación en el router.
const DESCRIPTION: &str = "coroom";
/// Intentos de puerto externo cuando el pedido está ocupado.
const PORT_ATTEMPTS: usize = 3;
/// Espera de las operaciones bloqueantes (borrado al soltar la asignación).
const BLOCKING_TIMEOUT: Duration = Duration::from_secs(1);
/// Tamaño máximo de una respuesta HTTP que se acepta.
const MAX_RESPONSE: usize = 256 * 1024;

/// URL `http://host:puerto/ruta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// Nombre o IP.
    pub host: String,
    /// Puerto (80 si no se dice).
    pub port: u16,
    /// Ruta con `/` inicial (y la consulta, si la hay).
    pub path: String,
}

impl Url {
    /// Lee una URL `http://`.
    pub fn parse(text: &str) -> Result<Url, String> {
        let rest = text
            .trim()
            .strip_prefix("http://")
            .ok_or_else(|| format!("no es una URL http: {text}"))?;
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        let (host, port) = if let Some(end) = authority.strip_prefix('[').and_then(|a| a.find(']'))
        {
            let host = &authority[1..=end];
            let port = authority[end + 2..]
                .strip_prefix(':')
                .map(str::parse)
                .transpose()
                .map_err(|_| format!("puerto no válido en {text}"))?;
            (host.to_owned(), port.unwrap_or(80))
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (
                    host.to_owned(),
                    port.parse()
                        .map_err(|_| format!("puerto no válido en {text}"))?,
                ),
                None => (authority.to_owned(), 80),
            }
        };
        if host.is_empty() {
            return Err(format!("URL sin host: {text}"));
        }
        Ok(Url {
            host,
            port,
            path: path.to_owned(),
        })
    }

    /// `host:puerto` para la cabecera `HOST`.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Resuelve `reference` (absoluta, con `/` inicial o relativa) respecto a esta URL.
    pub fn join(&self, reference: &str) -> Result<Url, String> {
        let reference = reference.trim();
        if reference.starts_with("http://") {
            return Url::parse(reference);
        }
        let path = if reference.starts_with('/') {
            reference.to_owned()
        } else {
            let base = match self.path.rfind('/') {
                Some(index) => &self.path[..=index],
                None => "/",
            };
            format!("{base}{reference}")
        };
        Ok(Url {
            host: self.host.clone(),
            port: self.port,
            path,
        })
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http://{}:{}{}", self.host, self.port, self.path)
    }
}

/// El servicio del router que abre puertos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    /// URL de control (SOAP).
    pub url: Url,
    /// Tipo del servicio (`urn:schemas-upnp-org:service:WANIPConnection:1`…).
    pub service: String,
}

/// Petición `M-SEARCH` de SSDP para la puerta de enlace.
pub fn search_request(target: SocketAddrV4) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {target}\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\n\
         ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n"
    )
}

/// La cabecera `LOCATION` de una respuesta SSDP.
pub fn location_from_response(text: &str) -> Option<String> {
    header(text, "location").map(str::to_owned)
}

fn header<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// El texto del primer elemento `<tag>…</tag>` (sin atributos ni espacios de nombres).
pub fn xml_text<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim())
}

fn service_blocks(xml: &str) -> impl Iterator<Item = &str> {
    xml.split("<service>")
        .skip(1)
        .filter_map(|block| block.split("</service>").next())
}

/// El servicio de apertura de puertos de la descripción del dispositivo, con su URL de control
/// resuelta respecto a `location` (o a `URLBase`, si la descripción lo trae).
pub fn control_from_description(xml: &str, location: &Url) -> Result<Control, String> {
    let base = match xml_text(xml, "URLBase").filter(|base| !base.is_empty()) {
        Some(base) => Url::parse(base).unwrap_or_else(|_| location.clone()),
        None => location.clone(),
    };
    let mut found: Option<(usize, Control)> = None;
    for block in service_blocks(xml) {
        let Some(service) = xml_text(block, "serviceType") else {
            continue;
        };
        let Some(rank) = SERVICES.iter().position(|known| *known == service) else {
            continue;
        };
        let Some(control_url) = xml_text(block, "controlURL") else {
            continue;
        };
        if found.as_ref().is_none_or(|(best, _)| rank < *best) {
            found = Some((
                rank,
                Control {
                    url: base.join(control_url)?,
                    service: service.to_owned(),
                },
            ));
        }
    }
    found
        .map(|(_, control)| control)
        .ok_or_else(|| "el router no anuncia WANIPConnection ni WANPPPConnection".to_owned())
}

/// Respuesta HTTP: estado y cuerpo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// Código de estado.
    pub status: u16,
    /// Cuerpo como texto.
    pub body: String,
}

/// Bytes de una petición HTTP/1.1 de una sola respuesta (`Connection: close`).
pub fn request_bytes(method: &str, url: &Url, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "{method} {} HTTP/1.1\r\nHOST: {}\r\n",
        url.path,
        url.authority()
    );
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\nUser-Agent: coroom\r\n\r\n",
        body.len()
    ));
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Lee lo recibido hasta ahora: `Ok(None)` si la respuesta aún no está completa (`eof` dice si la
/// conexión ya se cerró, y entonces el cuerpo es lo que haya).
pub fn parse_http(buf: &[u8], eof: bool) -> Result<Option<HttpResponse>, String> {
    let Some(end) = find(buf, b"\r\n\r\n") else {
        return if eof {
            Err("respuesta HTTP incompleta".to_owned())
        } else {
            Ok(None)
        };
    };
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| "cabeceras HTTP no válidas")?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "línea de estado HTTP no válida".to_owned())?;
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if name == "content-length" {
            content_length = value.trim().parse().ok();
        } else if name == "transfer-encoding" {
            chunked = value.to_ascii_lowercase().contains("chunked");
        }
    }
    let body = &buf[end + 4..];
    let body: Vec<u8> = if chunked {
        match decode_chunked(body)? {
            Some(decoded) => decoded,
            None if eof => return Err("respuesta HTTP troceada incompleta".to_owned()),
            None => return Ok(None),
        }
    } else if let Some(length) = content_length {
        if body.len() >= length {
            body[..length].to_vec()
        } else if eof {
            return Err("cuerpo HTTP incompleto".to_owned());
        } else {
            return Ok(None);
        }
    } else if eof {
        body.to_vec()
    } else {
        return Ok(None);
    };
    Ok(Some(HttpResponse {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

fn decode_chunked(mut body: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = find(body, b"\r\n") else {
            return Ok(None);
        };
        let size_text = std::str::from_utf8(&body[..line_end]).map_err(|_| "trozo no válido")?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| "tamaño de trozo no válido")?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(Some(out));
        }
        if body.len() < size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// Envía `request` a `url` y espera la respuesta hasta `deadline`.
async fn exchange(url: &Url, request: &[u8], deadline: Instant) -> Result<HttpResponse, String> {
    let connect = TcpStream::connect((url.host.as_str(), url.port));
    let mut stream = tokio::time::timeout_at(deadline, connect)
        .await
        .map_err(|_| format!("sin conexión con {} a tiempo", url.authority()))?
        .map_err(|err| format!("{}: {err}", url.authority()))?;
    tokio::time::timeout_at(deadline, stream.write_all(request))
        .await
        .map_err(|_| "no se pudo enviar la petición a tiempo".to_owned())?
        .map_err(|err| err.to_string())?;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let read = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
            .await
            .map_err(|_| "el router no respondió a tiempo".to_owned())?
            .map_err(|err| err.to_string())?;
        buf.extend_from_slice(&chunk[..read]);
        if buf.len() > MAX_RESPONSE {
            return Err("respuesta demasiado grande".to_owned());
        }
        if let Some(response) = parse_http(&buf, read == 0)? {
            return Ok(response);
        }
    }
}

/// Como [`exchange`], bloqueante (para borrar la asignación al soltarla).
fn exchange_blocking(url: &Url, request: &[u8], timeout: Duration) -> Result<HttpResponse, String> {
    use std::io::{Read, Write};
    let addr = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(|err| err.to_string())?
        .next()
        .ok_or_else(|| format!("{} no resuelve", url.host))?;
    let mut stream =
        std::net::TcpStream::connect_timeout(&addr, timeout).map_err(|err| err.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|err| err.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|err| err.to_string())?;
    stream.write_all(request).map_err(|err| err.to_string())?;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).map_err(|err| err.to_string())?;
        buf.extend_from_slice(&chunk[..read]);
        if buf.len() > MAX_RESPONSE {
            return Err("respuesta demasiado grande".to_owned());
        }
        if let Some(response) = parse_http(&buf, read == 0)? {
            return Ok(response);
        }
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Petición SOAP `action` al servicio de `control` con sus argumentos.
pub fn soap_request(control: &Control, action: &str, args: &[(&str, String)]) -> Vec<u8> {
    let mut arguments = String::new();
    for (name, value) in args {
        arguments.push_str(&format!("<{name}>{}</{name}>", escape_xml(value)));
    }
    let body = format!(
        "<?xml version=\"1.0\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action} xmlns:u=\"{}\">{arguments}</u:{action}></s:Body></s:Envelope>",
        control.service
    );
    let soap_action = format!("\"{}#{action}\"", control.service);
    request_bytes(
        "POST",
        &control.url,
        &[
            ("Content-Type", "text/xml; charset=\"utf-8\""),
            ("SOAPACTION", soap_action.as_str()),
        ],
        body.as_bytes(),
    )
}

/// Error SOAP de UPnP: código (`errorCode`) y descripción.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoapError {
    /// Código UPnP (718: puerto ocupado, 725: solo arrendamientos permanentes…).
    pub code: Option<u16>,
    /// Texto para el usuario.
    pub text: String,
}

impl fmt::Display for SoapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(code) => write!(f, "{} (código UPnP {code})", self.text),
            None => f.write_str(&self.text),
        }
    }
}

/// El cuerpo de una respuesta SOAP correcta, o su error.
pub fn soap_result(response: &HttpResponse) -> Result<&str, SoapError> {
    if response.status == 200 {
        return Ok(&response.body);
    }
    let code = xml_text(&response.body, "errorCode").and_then(|code| code.parse().ok());
    let description = xml_text(&response.body, "errorDescription")
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let text = match (code, description) {
        (Some(718), _) => "el puerto externo ya está asignado a otro equipo".to_owned(),
        (Some(725), _) => "el router solo admite asignaciones permanentes".to_owned(),
        (Some(729), _) => "el puerto lo gestiona otro mecanismo del router".to_owned(),
        (Some(606), _) | (Some(501), None) => "el router no permite abrir puertos".to_owned(),
        (_, Some(description)) => description,
        (_, None) => format!("HTTP {}", response.status),
    };
    Err(SoapError { code, text })
}

fn random_high_port() -> u16 {
    let mut bytes = [0u8; 2];
    let _ = getrandom::fill(&mut bytes);
    49_152 + (u16::from_be_bytes(bytes) % 16_384)
}

/// Busca la puerta de enlace por SSDP y le pide la asignación de `internal` hasta `deadline`.
/// Busca el router por SSDP desde `local` y lee su descripción: el servicio de control.
async fn find_control(
    ssdp: SocketAddrV4,
    local: Ipv4Addr,
    deadline: Instant,
) -> Result<Control, Failure> {
    let location = discover(ssdp, local, deadline).await?;
    let location = Url::parse(&location).map_err(|err| refused(err, None))?;
    let description = exchange(
        &location,
        &request_bytes("GET", &location, &[], b""),
        deadline,
    )
    .await
    .map_err(|err| refused(format!("descripción del router: {err}"), None))?;
    let control =
        control_from_description(&description.body, &location).map_err(|err| refused(err, None))?;
    debug!(%control.url, service = %control.service, "servicio UPnP del router");
    Ok(control)
}

fn refused(error: String, external: Option<Ipv4Addr>) -> Failure {
    Failure::Refused {
        protocol: MappingProtocol::Upnp,
        error,
        external,
    }
}

/// La dirección externa del router (`GetExternalIPAddress`), sin abrir nada.
pub(super) async fn external(
    ssdp: SocketAddrV4,
    local: Ipv4Addr,
    deadline: Instant,
) -> Result<Ipv4Addr, Failure> {
    let control = find_control(ssdp, local, deadline).await?;
    external_ip(&control, deadline)
        .await
        .map_err(|err| refused(format!("dirección externa: {err}"), None))
}

pub(super) async fn map(
    ssdp: SocketAddrV4,
    internal: SocketAddrV4,
    lifetime: Duration,
    deadline: Instant,
) -> Result<(Mapping, Lease), Failure> {
    let control = find_control(ssdp, *internal.ip(), deadline).await?;
    let external_ip = external_ip(&control, deadline).await;
    let external_ip = match external_ip {
        Ok(ip) => ip,
        Err(err) => return Err(refused(format!("dirección externa: {err}"), None)),
    };
    let mut external_port = internal.port();
    let mut lease_s = lifetime.as_secs().min(u64::from(u32::MAX)) as u32;
    let mut last_error = None;
    for attempt in 0..PORT_ATTEMPTS {
        match add_mapping(&control, internal, external_port, lease_s, deadline).await {
            Ok(()) => {
                let lease = Lease::Upnp {
                    control,
                    client: *internal.ip(),
                    internal: internal.port(),
                    external: external_port,
                    lease_s,
                };
                let mapping = Mapping {
                    external: SocketAddrV4::new(external_ip, external_port),
                    protocol: MappingProtocol::Upnp,
                    lifetime: (lease_s > 0).then(|| Duration::from_secs(u64::from(lease_s))),
                };
                return Ok((mapping, lease));
            }
            Err(err) => {
                match err.code {
                    Some(725) if lease_s != 0 => lease_s = 0,
                    Some(718) if attempt + 1 < PORT_ATTEMPTS => external_port = random_high_port(),
                    _ => return Err(refused(err.to_string(), Some(external_ip))),
                }
                last_error = Some(err);
            }
        }
    }
    Err(refused(
        last_error.map_or_else(|| "sin respuesta".to_owned(), |err| err.to_string()),
        Some(external_ip),
    ))
}

/// Busca la puerta de enlace por SSDP: la `LOCATION` de la primera que responde.
async fn discover(
    ssdp: SocketAddrV4,
    local: Ipv4Addr,
    deadline: Instant,
) -> Result<String, Failure> {
    let socket = UdpSocket::bind((local, 0)).await.map_err(|err| {
        debug!(error = %err, "socket SSDP");
        Failure::NoResponse
    })?;
    if ssdp.ip().is_multicast() {
        let _ = socket.set_multicast_ttl_v4(2);
    }
    let request = search_request(ssdp);
    let mut buf = [0u8; 2048];
    let mut sent = 0;
    let mut resend_at = Instant::now();
    loop {
        if sent < 2 && Instant::now() >= resend_at {
            if let Err(err) = socket.send_to(request.as_bytes(), ssdp).await {
                debug!(error = %err, "no se pudo enviar la búsqueda SSDP");
                return Err(Failure::NoResponse);
            }
            sent += 1;
            resend_at = Instant::now() + Duration::from_millis(150);
        }
        let until = if sent < 2 {
            deadline.min(resend_at)
        } else {
            deadline
        };
        match tokio::time::timeout_at(until, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, from))) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                if let Some(location) = location_from_response(&text) {
                    debug!(%from, %location, "puerta de enlace UPnP");
                    return Ok(location);
                }
            }
            Ok(Err(err)) => {
                debug!(error = %err, "SSDP");
                return Err(Failure::NoResponse);
            }
            Err(_) if Instant::now() >= deadline => return Err(Failure::NoResponse),
            Err(_) => {}
        }
    }
}

async fn external_ip(control: &Control, deadline: Instant) -> Result<Ipv4Addr, String> {
    let request = soap_request(control, "GetExternalIPAddress", &[]);
    let response = exchange(&control.url, &request, deadline).await?;
    let body = soap_result(&response).map_err(|err| err.to_string())?;
    let text = xml_text(body, "NewExternalIPAddress").ok_or("sin NewExternalIPAddress")?;
    let ip: Ipv4Addr = text
        .parse()
        .map_err(|_| format!("dirección externa no válida: {text}"))?;
    if ip.is_unspecified() {
        return Err("el router no tiene conexión WAN".to_owned());
    }
    Ok(ip)
}

fn mapping_args(
    internal: SocketAddrV4,
    external: u16,
    lease_s: u32,
) -> Vec<(&'static str, String)> {
    vec![
        ("NewRemoteHost", String::new()),
        ("NewExternalPort", external.to_string()),
        ("NewProtocol", "UDP".to_owned()),
        ("NewInternalPort", internal.port().to_string()),
        ("NewInternalClient", internal.ip().to_string()),
        ("NewEnabled", "1".to_owned()),
        ("NewPortMappingDescription", DESCRIPTION.to_owned()),
        ("NewLeaseDuration", lease_s.to_string()),
    ]
}

async fn add_mapping(
    control: &Control,
    internal: SocketAddrV4,
    external: u16,
    lease_s: u32,
    deadline: Instant,
) -> Result<(), SoapError> {
    let request = soap_request(
        control,
        "AddPortMapping",
        &mapping_args(internal, external, lease_s),
    );
    let response = exchange(&control.url, &request, deadline)
        .await
        .map_err(|text| SoapError { code: None, text })?;
    soap_result(&response).map(|_| ())
}

/// Renueva la asignación (otro `AddPortMapping` igual).
pub(super) async fn renew(lease: &Lease, lifetime: Duration) -> Result<Option<Duration>, String> {
    let Lease::Upnp {
        control,
        client,
        internal,
        external,
        lease_s,
    } = lease
    else {
        return Err("no es UPnP".to_owned());
    };
    if *lease_s == 0 {
        return Ok(None);
    }
    let lease_s = lifetime.as_secs().min(u64::from(u32::MAX)) as u32;
    let deadline = Instant::now() + Duration::from_secs(5);
    add_mapping(
        control,
        SocketAddrV4::new(*client, *internal),
        *external,
        lease_s,
        deadline,
    )
    .await
    .map(|()| Some(Duration::from_secs(u64::from(lease_s))))
    .map_err(|err| err.to_string())
}

/// Borra la asignación (`DeletePortMapping`), bloqueante.
pub(super) fn delete_blocking(lease: &Lease) {
    let Lease::Upnp {
        control, external, ..
    } = lease
    else {
        return;
    };
    let request = soap_request(
        control,
        "DeletePortMapping",
        &[
            ("NewRemoteHost", String::new()),
            ("NewExternalPort", external.to_string()),
            ("NewProtocol", "UDP".to_owned()),
        ],
    );
    if let Err(err) = exchange_blocking(&control.url, &request, BLOCKING_TIMEOUT) {
        debug!(error = %err, "no se pudo borrar la asignación UPnP");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_parsed_and_joined() {
        let url = Url::parse("http://192.168.1.1:5000/rootDesc.xml").expect("url");
        assert_eq!(url.host, "192.168.1.1");
        assert_eq!(url.port, 5000);
        assert_eq!(url.path, "/rootDesc.xml");
        assert_eq!(Url::parse("http://router.lan").expect("url").port, 80);
        assert_eq!(Url::parse("http://router.lan").expect("url").path, "/");
        assert!(Url::parse("https://x/").is_err());
        assert_eq!(
            url.join("/ctl/IPConn").expect("abs").to_string(),
            "http://192.168.1.1:5000/ctl/IPConn"
        );
        assert_eq!(url.join("ctl/IPConn").expect("rel").path, "/ctl/IPConn");
        assert_eq!(
            url.join("http://192.168.1.1:49000/upnp/control")
                .expect("otra")
                .port,
            49000
        );
    }

    #[test]
    fn the_control_url_comes_from_the_best_service() {
        let xml = r#"<?xml version="1.0"?><root><URLBase>http://192.168.1.1:5000/</URLBase>
            <device><serviceList><service>
              <serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType>
              <controlURL>/ctl/L3F</controlURL></service></serviceList>
            <deviceList><device><deviceList><device><serviceList>
              <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
                <controlURL>/ctl/IPConn1</controlURL></service>
              <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:2</serviceType>
                <controlURL>ctl/IPConn2</controlURL></service>
            </serviceList></device></deviceList></device></deviceList></device></root>"#;
        let location = Url::parse("http://192.168.1.1:5000/rootDesc.xml").expect("url");
        let control = control_from_description(xml, &location).expect("control");
        assert_eq!(
            control.service,
            "urn:schemas-upnp-org:service:WANIPConnection:2"
        );
        assert_eq!(
            control.url.to_string(),
            "http://192.168.1.1:5000/ctl/IPConn2"
        );
        assert!(control_from_description("<root></root>", &location).is_err());
    }

    #[test]
    fn ssdp_and_http_are_parsed() {
        let response = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nST: urn:x\r\n\
                        Location: http://192.168.1.1:5000/rootDesc.xml\r\n\r\n";
        assert_eq!(
            location_from_response(response).as_deref(),
            Some("http://192.168.1.1:5000/rootDesc.xml")
        );
        assert!(
            search_request(SSDP_MULTICAST)
                .starts_with("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n")
        );

        let full = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\n\r\nhello";
        let parsed = parse_http(full, false).expect("ok").expect("completa");
        assert_eq!((parsed.status, parsed.body.as_str()), (500, "hello"));
        assert!(
            parse_http(&full[..full.len() - 1], false)
                .expect("ok")
                .is_none()
        );
        assert!(parse_http(&full[..full.len() - 1], true).is_err());
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;ext\r\nde\r\n0\r\n\r\n";
        assert_eq!(
            parse_http(chunked, false)
                .expect("ok")
                .expect("completa")
                .body,
            "abcde"
        );
        let until_close = b"HTTP/1.1 200 OK\r\n\r\n<x/>";
        assert!(parse_http(until_close, false).expect("ok").is_none());
        assert_eq!(
            parse_http(until_close, true)
                .expect("ok")
                .expect("fin")
                .body,
            "<x/>"
        );
    }

    #[test]
    fn soap_requests_and_errors() {
        let control = Control {
            url: Url::parse("http://192.168.1.1:5000/ctl/IPConn").expect("url"),
            service: "urn:schemas-upnp-org:service:WANIPConnection:1".to_owned(),
        };
        let request = soap_request(
            &control,
            "AddPortMapping",
            &mapping_args(
                SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 47_000),
                47_000,
                7200,
            ),
        );
        let text = String::from_utf8(request).expect("utf8");
        assert!(text.starts_with("POST /ctl/IPConn HTTP/1.1\r\nHOST: 192.168.1.1:5000\r\n"));
        assert!(text.contains(
            "SOAPACTION: \"urn:schemas-upnp-org:service:WANIPConnection:1#AddPortMapping\"\r\n"
        ));
        assert!(text.contains("<NewInternalClient>192.168.1.10</NewInternalClient>"));
        assert!(text.contains("<NewLeaseDuration>7200</NewLeaseDuration>"));
        let conflict = HttpResponse {
            status: 500,
            body: "<s:Fault><detail><UPnPError><errorCode>718</errorCode>\
                   <errorDescription>ConflictInMappingEntry</errorDescription></UPnPError></detail></s:Fault>"
                .to_owned(),
        };
        let err = soap_result(&conflict).expect_err("error");
        assert_eq!(err.code, Some(718));
        assert!(err.to_string().contains("ya está asignado"));
        let ok = HttpResponse {
            status: 200,
            body: "<NewExternalIPAddress>203.0.113.9</NewExternalIPAddress>".to_owned(),
        };
        assert_eq!(
            xml_text(soap_result(&ok).expect("ok"), "NewExternalIPAddress"),
            Some("203.0.113.9")
        );
    }
}
