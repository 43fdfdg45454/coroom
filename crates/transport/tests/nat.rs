//! Apertura del puerto en el router con routers falsos en localhost: uno PCP, uno que solo habla
//! NAT-PMP, uno UPnP (SSDP + HTTP) y ninguno.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;

use coroom_transport::nat::{
    Mapper, MappingOutcome, MappingProtocol, NATPMP_EXTERNAL_REQUEST, PCP_OP_MAP, PCP_VERSION,
    parse_natpmp, parse_pcp_map,
};

const EXTERNAL: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 9);
const INTERNAL_PORT: u16 = 47_000;
/// Plazo de lo que tiene que llegar: el router falso contesta enseguida, así que solo se agota si
/// algo está roto.
const TIMEOUT: Duration = Duration::from_secs(20);

fn v4(addr: SocketAddr) -> SocketAddrV4 {
    match addr {
        SocketAddr::V4(addr) => addr,
        SocketAddr::V6(_) => panic!("se esperaba IPv4"),
    }
}

/// Un puerto UDP de localhost en el que no escucha nadie.
fn closed_udp_port() -> SocketAddrV4 {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("socket");
    v4(socket.local_addr().expect("dirección"))
}

/// Servidor UDP falso: `reply` decide la respuesta a cada datagrama y cada petición se anota.
async fn udp_server(
    reply: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + 'static,
) -> (SocketAddrV4, mpsc::UnboundedReceiver<Vec<u8>>) {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("socket del router");
    let addr = v4(socket.local_addr().expect("dirección"));
    let (log, requests) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut buf = [0u8; 256];
        while let Ok((n, from)) = socket.recv_from(&mut buf).await {
            let request = buf[..n].to_vec();
            if let Some(answer) = reply(&request) {
                let _ = socket.send_to(&answer, from).await;
            }
            let _ = log.send(request);
        }
    });
    (addr, requests)
}

/// Respuesta MAP de PCP concedida: el puerto externo es el interno y la IP externa, [`EXTERNAL`].
fn pcp_granted(request: &[u8]) -> Vec<u8> {
    let mut reply = request.to_vec();
    reply[1] = PCP_OP_MAP | 0x80;
    reply[2] = 0;
    reply[3] = 0;
    reply[8..12].copy_from_slice(&1234u32.to_be_bytes());
    reply[12..24].fill(0);
    let internal = [reply[40], reply[41]];
    reply[42..44].copy_from_slice(&internal);
    reply[44..60].copy_from_slice(&EXTERNAL.to_ipv6_mapped().octets());
    reply
}

fn natpmp_reply(request: &[u8]) -> Option<Vec<u8>> {
    match request {
        [0, 0] => {
            let mut reply = vec![0u8, 128, 0, 0, 0, 0, 0, 10];
            reply.extend_from_slice(&EXTERNAL.octets());
            Some(reply)
        }
        [0, 1, _, _, internal @ .., _, _, _, _, _, _] if internal.len() >= 2 => {
            let mut reply = vec![0u8, 129, 0, 0, 0, 0, 0, 10];
            reply.extend_from_slice(&internal[..2]);
            reply.extend_from_slice(&[0xC0, 0x00]);
            reply.extend_from_slice(&3600u32.to_be_bytes());
            Some(reply)
        }
        // Un router NAT-PMP contesta a PCP con «versión no admitida» en su formato.
        _ if request.first() == Some(&PCP_VERSION) => Some(vec![0u8, 0x81, 0, 1, 0, 0, 0, 10]),
        _ => None,
    }
}

async fn wait_for(
    requests: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    what: &str,
    accept: impl Fn(&[u8]) -> bool,
) -> Vec<u8> {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let request = requests.recv().await.expect("servidor vivo");
            if accept(&request) {
                return request;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no llegó {what}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pcp_gateway_maps_the_port_and_the_mapping_is_deleted_on_drop() {
    let (gateway, mut requests) =
        udp_server(|request| (request.first() == Some(&PCP_VERSION)).then(|| pcp_granted(request)))
            .await;
    let mapper = Mapper::new()
        .with_gateway(gateway)
        .with_ssdp(closed_udp_port())
        .with_timeout(TIMEOUT);
    let mapping = mapper.map(INTERNAL_PORT).await;
    let report = mapping.report().clone();
    let granted = report.mapping().unwrap_or_else(|| panic!("{report}"));
    assert_eq!(granted.protocol, MappingProtocol::Pcp);
    assert_eq!(granted.external, SocketAddrV4::new(EXTERNAL, INTERNAL_PORT));
    assert_eq!(granted.lifetime, Some(Duration::from_secs(7200)));
    assert_eq!(report.gateway, Some(Ipv4Addr::LOCALHOST));
    assert_eq!(
        report.internal,
        Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, INTERNAL_PORT))
    );
    let request = wait_for(&mut requests, "la petición MAP", |r| r.len() == 60).await;
    assert_eq!(
        &request[8..24],
        &Ipv4Addr::LOCALHOST.to_ipv6_mapped().octets()
    );

    drop(mapping);
    let deletion = wait_for(&mut requests, "el borrado", |r| {
        r.len() == 60 && r[4..8] == [0, 0, 0, 0]
    })
    .await;
    assert_eq!(&deletion[24..36], &request[24..36], "mismo nonce");
    assert_eq!(&deletion[40..42], &INTERNAL_PORT.to_be_bytes());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b_a_nat_pmp_only_gateway_answers_the_fallback() {
    let (gateway, mut requests) = udp_server(natpmp_reply).await;
    let mapper = Mapper::new()
        .with_gateway(gateway)
        .with_ssdp(closed_udp_port())
        .with_timeout(TIMEOUT);
    let mapping = mapper.map(INTERNAL_PORT).await;
    let report = mapping.report().clone();
    let granted = report.mapping().unwrap_or_else(|| panic!("{report}"));
    assert_eq!(granted.protocol, MappingProtocol::NatPmp);
    assert_eq!(granted.external, SocketAddrV4::new(EXTERNAL, 0xC000));
    assert_eq!(granted.lifetime, Some(Duration::from_secs(3600)));
    // Primero PCP, después las dos peticiones NAT-PMP.
    let first = tokio::time::timeout(TIMEOUT, requests.recv())
        .await
        .expect("primera a tiempo")
        .expect("primera");
    assert!(
        parse_pcp_map(&pcp_granted(&first)).is_ok(),
        "primero se prueba PCP"
    );
    wait_for(&mut requests, "la dirección externa", |r| {
        r == NATPMP_EXTERNAL_REQUEST
    })
    .await;
    let map = wait_for(&mut requests, "la asignación NAT-PMP", |r| {
        r.len() == 12 && r[1] == 1
    })
    .await;
    assert_eq!(&map[4..6], &INTERNAL_PORT.to_be_bytes());
    assert!(parse_natpmp(&natpmp_reply(&map).expect("respuesta")).is_ok());

    drop(mapping);
    let deletion = wait_for(&mut requests, "el borrado", |r| {
        r.len() == 12 && r[1] == 1 && r[8..12] == [0, 0, 0, 0]
    })
    .await;
    assert_eq!(&deletion[6..8], &[0, 0], "puerto externo 0 al borrar");
}

/// Router UPnP falso: SSDP en UDP y la descripción y el control SOAP en HTTP. Anota cada acción
/// SOAP con su cuerpo. El primer `AddPortMapping` responde «puerto ocupado» (718).
async fn upnp_server() -> (SocketAddrV4, mpsc::UnboundedReceiver<(String, String)>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("HTTP del router");
    let http = v4(listener.local_addr().expect("dirección"));
    let (log, actions) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut adds = 0;
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let request = loop {
                let n = stream.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break String::from_utf8_lossy(&buf).into_owned();
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + length {
                        break text;
                    }
                }
            };
            let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
            let action = request
                .lines()
                .find_map(|line| line.strip_prefix("SOAPACTION: "))
                .and_then(|value| value.split('#').nth(1))
                .map(|value| value.trim_matches('"').to_owned());
            let body = request
                .split_once("\r\n\r\n")
                .map(|(_, body)| body.to_owned())
                .unwrap_or_default();
            let (status, payload) = match (path.as_str(), action.as_deref()) {
                ("/rootDesc.xml", None) => (
                    "200 OK",
                    "<?xml version=\"1.0\"?><root><device><deviceList><device><serviceList>\
                     <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>\
                     <controlURL>/ctl/IPConn</controlURL></service></serviceList></device>\
                     </deviceList></device></root>"
                        .to_owned(),
                ),
                ("/ctl/IPConn", Some("GetExternalIPAddress")) => (
                    "200 OK",
                    format!(
                        "<s:Envelope><s:Body><u:GetExternalIPAddressResponse>\
                         <NewExternalIPAddress>{EXTERNAL}</NewExternalIPAddress>\
                         </u:GetExternalIPAddressResponse></s:Body></s:Envelope>"
                    ),
                ),
                ("/ctl/IPConn", Some("AddPortMapping")) => {
                    adds += 1;
                    if adds == 1 {
                        (
                            "500 Internal Server Error",
                            "<s:Envelope><s:Body><s:Fault><detail><UPnPError>\
                             <errorCode>718</errorCode><errorDescription>ConflictInMappingEntry\
                             </errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"
                                .to_owned(),
                        )
                    } else {
                        (
                            "200 OK",
                            "<s:Envelope><s:Body><u:AddPortMappingResponse/></s:Body></s:Envelope>"
                                .to_owned(),
                        )
                    }
                }
                ("/ctl/IPConn", Some("DeletePortMapping")) => (
                    "200 OK",
                    "<s:Envelope><s:Body><u:DeletePortMappingResponse/></s:Body></s:Envelope>"
                        .to_owned(),
                ),
                _ => ("404 Not Found", String::new()),
            };
            if let Some(action) = action {
                let _ = log.send((action, body));
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    let (ssdp, _) = udp_server(move |request| {
        let text = String::from_utf8_lossy(request);
        text.starts_with("M-SEARCH").then(|| {
            format!(
                "HTTP/1.1 200 OK\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
                 LOCATION: http://{http}/rootDesc.xml\r\n\r\n"
            )
            .into_bytes()
        })
    })
    .await;
    (ssdp, actions)
}

async fn next_action(actions: &mut mpsc::UnboundedReceiver<(String, String)>) -> (String, String) {
    tokio::time::timeout(TIMEOUT, actions.recv())
        .await
        .expect("acción a tiempo")
        .expect("acción")
}

fn xml_value(body: &str, tag: &str) -> String {
    coroom_transport::nat::xml_text(body, tag)
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_upnp_maps_when_the_gateway_has_no_pcp() {
    let (ssdp, mut actions) = upnp_server().await;
    let mapper = Mapper::new()
        .with_gateway(closed_udp_port())
        .with_ssdp(ssdp)
        .with_timeout(TIMEOUT);
    let mapping = mapper.map(INTERNAL_PORT).await;
    let report = mapping.report().clone();
    let granted = report.mapping().unwrap_or_else(|| panic!("{report}"));
    assert_eq!(granted.protocol, MappingProtocol::Upnp);
    assert_eq!(*granted.external.ip(), EXTERNAL);
    assert!(
        granted.external.port() != INTERNAL_PORT && granted.external.port() >= 49_152,
        "tras el 718 se pide otro puerto: {}",
        granted.external
    );
    assert_eq!(granted.lifetime, Some(Duration::from_secs(7200)));

    let (first, _) = next_action(&mut actions).await;
    assert_eq!(first, "GetExternalIPAddress");
    let (second, body) = next_action(&mut actions).await;
    assert_eq!(second, "AddPortMapping");
    assert_eq!(
        xml_value(&body, "NewExternalPort"),
        INTERNAL_PORT.to_string()
    );
    assert_eq!(
        xml_value(&body, "NewInternalPort"),
        INTERNAL_PORT.to_string()
    );
    assert_eq!(xml_value(&body, "NewInternalClient"), "127.0.0.1");
    assert_eq!(xml_value(&body, "NewProtocol"), "UDP");
    let (third, body) = next_action(&mut actions).await;
    assert_eq!(third, "AddPortMapping");
    assert_eq!(
        xml_value(&body, "NewExternalPort"),
        granted.external.port().to_string()
    );

    drop(mapping);
    let (deleted, body) = next_action(&mut actions).await;
    assert_eq!(deleted, "DeletePortMapping");
    assert_eq!(
        xml_value(&body, "NewExternalPort"),
        granted.external.port().to_string()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d_without_any_answer_the_report_says_so() {
    let mapper = Mapper::new()
        .with_gateway(closed_udp_port())
        .with_ssdp(closed_udp_port())
        .with_timeout(Duration::from_millis(400));
    let started = std::time::Instant::now();
    let mapping = mapper.map(INTERNAL_PORT).await;
    // Se rinde al plazo de 400 ms: muy por encima, es que alguna espera no lo respeta.
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    coroom_testkit::timing_assert!(
        elapsed < Duration::from_secs(2),
        "sin respuesta tardó {elapsed:?}"
    );
    assert_eq!(mapping.report().outcome, MappingOutcome::NoResponse);
    assert!(mapping.mapping().is_none());
    assert!(
        mapping
            .report()
            .to_string()
            .contains("no respondió a UPnP, NAT-PMP ni PCP"),
        "{}",
        mapping.report()
    );
}
