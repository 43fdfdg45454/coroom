//! Diagnóstico: por qué la sala va por el relay, los candidatos probados con su resultado, la
//! prueba del enlace por la ruta en uso, la latencia por etapa medida y «Copiar informe». Se abre
//! encima de cualquier pantalla (desde la insignia del servidor, la píldora de latencia o el panel
//! de conexión) sin cortar la sala.
//!
//! [`on_event`] recibe todos los eventos de las sesiones y de `coroom doctor`, también con el panel
//! cerrado, y guarda lo que hace falta: la ruta y los candidatos ([`SessionEvent::Connected`],
//! [`SessionEvent::Candidates`]), lo que dijo el router ([`SessionEvent::PortMapping`]), la sonda
//! del enlace ([`SessionEvent::LinkProbe`]), el resumen de latencia del invitado
//! ([`Event::GuestStats`]) y, como anfitrión, el estado de cada invitado ([`Event::HostGuests`]).
//! Nada se estima: lo que no se mide (el MTU útil, que `quinn` no expone a la sesión) se muestra
//! como «—».
//!
//! El informe copiado ([`report`]) es texto plano sin direcciones IP, rutas ni nombres: las
//! direcciones se cambian por su rango («[red local]», «[IPv4 pública]»…) y las personas por su
//! papel («anfitrión», «invitado 1»…), para pegarlo en un issue.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

use coroom_session::GuestId;
use coroom_session::latency::{InputLatency, LatencySummary, SpanStats, Stage};
use coroom_session::remote::{
    CandidateClass, CandidateOutcome, CandidateReport, GuestStatus, SessionEvent,
};
use coroom_transport::connect::{PUNCH_TIMEOUT, candidate_class};
use coroom_transport::nat::{MappingOutcome, MappingReport};
use coroom_transport::quic::Path;
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ContainerClass, SvgClass, TextClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{space as gap, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, Row, Space, column, container, row, rule, scrollable};

use crate::ui::app::{App, Effect, Event, Message};
use crate::ui::stats;

/// Ancho máximo del panel.
const PANEL_WIDTH: f32 = 860.0;

/// Percentil que enseñan las etapas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Percentile {
    /// Mediana.
    #[default]
    P50,
    /// Percentil 95.
    P95,
}

impl Percentile {
    fn label(self) -> &'static str {
        match self {
            Percentile::P50 => "p50",
            Percentile::P95 => "p95",
        }
    }

    fn of_span(self, span: &SpanStats) -> u64 {
        match self {
            Percentile::P50 => span.p50_us,
            Percentile::P95 => span.p95_us,
        }
    }

    fn of_input(self, input: &InputLatency) -> u64 {
        match self {
            Percentile::P50 => input.p50_us,
            Percentile::P95 => input.p95_us,
        }
    }
}

/// Lo que midió la sonda del enlace antes del vídeo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// kbit/s entregados.
    pub kbps: u32,
    /// Datagramas del tren que no llegaron.
    pub lost: u16,
    /// Datagramas del tren.
    pub sent: u16,
}

/// Ruta con un par: la del invitado con el anfitrión o la del anfitrión con un invitado.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Route {
    /// Directa o por el relay, cuando ya hay conexión.
    pub path: Option<Path>,
    /// Dirección del otro extremo (o del relay) vista desde aquí.
    pub remote: Option<SocketAddr>,
    /// Cada candidato probado con su resultado.
    pub candidates: Vec<CandidateReport>,
    /// Por qué se va por el relay, si se va y se probó la directa.
    pub relay_reason: Option<String>,
    /// Cuándo llegó el último intento de conexión directa.
    pub tried_at: Option<Instant>,
    /// La sonda del enlace.
    pub probe: Option<Probe>,
}

/// Reconexión del invitado en marcha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconnecting {
    /// Intento (desde 1).
    pub attempt: u32,
    /// Intentos como mucho.
    pub of: u32,
    /// Qué falló.
    pub reason: String,
}

/// Lo que se sabe de la sala en la que se está como invitado.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GuestSide {
    /// La ruta con el anfitrión.
    pub route: Route,
    /// Lo que respondió el router de este equipo.
    pub mapping: Option<MappingReport>,
    /// Reconectando.
    pub reconnecting: Option<Reconnecting>,
    /// La sala no tiene relay (con el motivo).
    pub relay_unavailable: Option<String>,
    /// Último resumen de latencia (con el enlace).
    pub summary: Option<LatencySummary>,
}

/// Lo que se sabe de la sala propia como anfitrión.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostSide {
    /// Lo que respondió el router de este equipo.
    pub mapping: Option<MappingReport>,
    /// La sala no tiene relay (con el motivo).
    pub relay_unavailable: Option<String>,
    /// La ruta con cada invitado (candidatos, motivo del relay y sonda).
    pub routes: BTreeMap<GuestId, Route>,
    /// Último estado de los invitados.
    pub statuses: Vec<GuestStatus>,
}

/// Estado del diagnóstico.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// Abierto encima de la pantalla.
    pub open: bool,
    /// Percentil de las etapas.
    pub percentile: Percentile,
    /// Líneas de `coroom doctor`, cuando llegan.
    pub doctor: Option<Vec<String>>,
    /// Las mismas, ya filtradas (sin lo privado ni los nombres) para enseñarlas: se calcula al
    /// llegar y cuando cambia lo que hay que ocultar, no en cada vista.
    pub doctor_text: Option<String>,
    /// `coroom doctor` pedido y sin contestar.
    pub doctor_pending: bool,
    /// Como invitado.
    pub guest: GuestSide,
    /// Como anfitrión.
    pub host: HostSide,
}

/// Mensajes del diagnóstico.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    /// Abrir.
    Open,
    /// Cerrar.
    Close,
    /// Reintentar la conexión directa ya (solo el invitado).
    Retry,
    /// Copiar el informe (sin direcciones ni nombres).
    CopyReport,
    /// Elegir el percentil de las etapas.
    Percentile(Percentile),
}

/// Papel de este equipo ahora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Guest,
    Host,
    Lobby,
}

fn side(app: &App) -> Side {
    if app.guest.as_ref().is_some_and(|guest| !guest.probe) {
        Side::Guest
    } else if app.host.is_open() {
        Side::Host
    } else {
        Side::Lobby
    }
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Open => {
            let state = &mut app.diagnostics;
            state.open = true;
            if state.doctor.is_none() {
                if let Some(lines) = app.settings_page.doctor.loaded() {
                    state.doctor = Some(lines.clone());
                } else if !state.doctor_pending && !app.settings_page.doctor.is_loading() {
                    state.doctor_pending = true;
                    return vec![Effect::RunDoctor];
                }
            }
            // Al abrir, con los nombres de ahora (amigos, invitados que llegaron después).
            refresh_doctor_text(app);
            Vec::new()
        }
        Msg::Close => {
            app.diagnostics.open = false;
            Vec::new()
        }
        Msg::Retry => {
            // Solo el invitado vuelve a entrar; el anfitrión no tiene a quién llamar.
            if side(app) == Side::Guest && app.diagnostics.guest.reconnecting.is_none() {
                vec![Effect::Reconnect]
            } else {
                Vec::new()
            }
        }
        Msg::CopyReport => {
            let text = report(app);
            app.toast(Tone::Success, t!("diagnostics-informe-copiado"));
            vec![Effect::Copy(text)]
        }
        Msg::Percentile(percentile) => {
            app.diagnostics.percentile = percentile;
            Vec::new()
        }
    }
}

/// Mira los eventos de las sesiones y de `coroom doctor` (se acumulan aunque esté cerrado).
pub fn on_event(app: &mut App, event: &Event) {
    let now = app.now;
    let state = &mut app.diagnostics;
    match event {
        Event::Doctor(lines) => {
            state.doctor = Some(lines.clone());
            state.doctor_pending = false;
            refresh_doctor_text(app);
        }
        Event::GuestStarted(_) | Event::GuestEnded(_) => state.guest = GuestSide::default(),
        Event::HostStarted(_) | Event::HostEnded(_) => state.host = HostSide::default(),
        Event::GuestStats(stats) => state.guest.summary = Some(stats.summary.clone()),
        Event::HostGuests(statuses, _) => state.host.statuses = statuses.clone(),
        Event::Guest(event) => guest_event(&mut state.guest, event, now),
        Event::Host(event) => host_event(&mut state.host, event, now),
        _ => {}
    }
}

fn guest_event(side: &mut GuestSide, event: &SessionEvent, now: Instant) {
    match event {
        SessionEvent::Connected { path, remote } => {
            side.route.path = Some(*path);
            side.route.remote = Some(*remote);
            side.reconnecting = None;
        }
        SessionEvent::Candidates {
            candidates,
            relay_reason,
            ..
        } => {
            side.route.candidates = candidates.clone();
            side.route.relay_reason = relay_reason.clone();
            side.route.tried_at = Some(now);
        }
        SessionEvent::PortMapping { report } => side.mapping = Some(report.clone()),
        SessionEvent::LinkProbe {
            kbps, lost, sent, ..
        } => {
            side.route.probe = Some(Probe {
                kbps: *kbps,
                lost: *lost,
                sent: *sent,
            });
        }
        SessionEvent::Reconnecting {
            attempt,
            of,
            reason,
            ..
        } => {
            side.reconnecting = Some(Reconnecting {
                attempt: *attempt,
                of: *of,
                reason: reason.clone(),
            });
        }
        SessionEvent::Reconnected { path } => {
            side.reconnecting = None;
            side.route.path = Some(*path);
        }
        SessionEvent::RelayUnavailable(reason) => side.relay_unavailable = Some(reason.clone()),
        _ => {}
    }
}

fn host_event(side: &mut HostSide, event: &SessionEvent, now: Instant) {
    match event {
        SessionEvent::Candidates {
            guest: Some(guest),
            candidates,
            relay_reason,
        } => {
            let route = side.routes.entry(*guest).or_default();
            route.candidates = candidates.clone();
            route.relay_reason = relay_reason.clone();
            route.tried_at = Some(now);
        }
        SessionEvent::GuestConnected {
            guest,
            path,
            remote,
        } => {
            let route = side.routes.entry(*guest).or_default();
            route.path = Some(*path);
            route.remote = Some(*remote);
        }
        SessionEvent::LinkProbe {
            guest: Some(guest),
            kbps,
            lost,
            sent,
        } => {
            side.routes.entry(*guest).or_default().probe = Some(Probe {
                kbps: *kbps,
                lost: *lost,
                sent: *sent,
            });
        }
        SessionEvent::GuestLeft { guest, .. } => {
            side.routes.remove(guest);
            side.statuses.retain(|status| status.guest != *guest);
        }
        SessionEvent::PortMapping { report } => side.mapping = Some(report.clone()),
        SessionEvent::RelayUnavailable(reason) => side.relay_unavailable = Some(reason.clone()),
        _ => {}
    }
}

// --- Lo que se enseña (lo mismo para la pantalla y para el informe) ------------------------------

/// Una fila de la lista de candidatos (también el router y el relay).
#[derive(Debug, Clone, PartialEq)]
struct Line {
    glyph: Icon,
    /// `None`: neutro.
    tone: Option<Tone>,
    title: String,
    explanation: String,
    /// Dirección (solo en pantalla, en una línea discreta; nunca en el informe).
    address: Option<String>,
    result: &'static str,
}

/// Una cifra de la prueba de enlace.
#[derive(Debug, Clone, PartialEq)]
struct Metric {
    label: &'static str,
    detail: Option<String>,
    value: String,
    /// Los valores más anchos que puede tomar `value`: la columna de valores tiene ese ancho fijo
    /// y la etiqueta de al lado no reparte sus líneas de otra forma con cada medida. Vacío en los
    /// que no cambian solos.
    widest: Vec<String>,
}

/// Las muestras de una ranura de ancho fijo, para [`Metric::widest`].
fn widest(samples: &[&str]) -> Vec<String> {
    samples.iter().map(|sample| (*sample).to_owned()).collect()
}

fn ok_line(title: String, explanation: String, result: &'static str) -> Line {
    Line {
        glyph: Icon::Check,
        tone: Some(Tone::Success),
        title,
        explanation,
        address: None,
        result,
    }
}

fn failed_line(title: String, explanation: String, result: &'static str) -> Line {
    Line {
        glyph: Icon::Close,
        tone: Some(Tone::Error),
        title,
        explanation,
        address: None,
        result,
    }
}

fn neutral_line(title: String, explanation: String, result: &'static str) -> Line {
    Line {
        glyph: Icon::CircleDashed,
        tone: None,
        title,
        explanation,
        address: None,
        result,
    }
}

fn class_title(class: CandidateClass) -> &'static str {
    match class {
        CandidateClass::Ipv6Global => t!("diagnostics-clase-ipv6"),
        CandidateClass::Ipv4Public => t!("diagnostics-clase-ipv4"),
        CandidateClass::CarrierNat => t!("diagnostics-clase-cgnat"),
        CandidateClass::Private => t!("diagnostics-clase-privada"),
    }
}

fn no_response(class: CandidateClass) -> String {
    let timeout = PUNCH_TIMEOUT.as_secs();
    match class {
        CandidateClass::Ipv6Global => t!("diagnostics-sin-respuesta-ipv6", timeout = timeout),
        CandidateClass::Ipv4Public => t!("diagnostics-sin-respuesta-ipv4", timeout = timeout),
        CandidateClass::CarrierNat => t!("diagnostics-sin-respuesta-cgnat", timeout = timeout),
        CandidateClass::Private => t!("diagnostics-sin-respuesta-privada", timeout = timeout),
    }
}

fn candidate_line(candidate: &CandidateReport) -> Line {
    let title = class_title(candidate.class).to_owned();
    let mut line = match &candidate.outcome {
        CandidateOutcome::Connected => ok_line(
            title,
            t!("diagnostics-por-aqui").to_owned(),
            t!("diagnostics-resultado-en-uso"),
        ),
        CandidateOutcome::Duplicate => ok_line(
            title,
            t!("diagnostics-duplicado").to_owned(),
            t!("diagnostics-resultado-respondio"),
        ),
        CandidateOutcome::Failed(err) => failed_line(
            title,
            t!(
                "diagnostics-fallo-err",
                err = scrub_addresses(err.trim_end_matches('.'))
            ),
            t!("diagnostics-resultado-fallo"),
        ),
        CandidateOutcome::NoResponse => failed_line(
            title,
            no_response(candidate.class),
            t!("diagnostics-resultado-sin-respuesta"),
        ),
        CandidateOutcome::Cancelled => neutral_line(
            title,
            t!("diagnostics-cancelado").to_owned(),
            t!("diagnostics-resultado-no-probado"),
        ),
    };
    line.address = Some(candidate.addr.to_string());
    line
}

fn router_line(report: &MappingReport) -> Line {
    let title = t!("diagnostics-router").to_owned();
    let mut line = match &report.outcome {
        MappingOutcome::Mapped(mapping) => ok_line(
            title,
            t!("diagnostics-router-abierto", protocol = mapping.protocol),
            t!("diagnostics-resultado-abierto"),
        ),
        MappingOutcome::ExternalOnly {
            protocol, error, ..
        } => failed_line(
            title,
            t!(
                "diagnostics-router-solo-externa",
                protocol = protocol,
                error = scrub_addresses(error)
            ),
            t!("diagnostics-resultado-fallo"),
        ),
        MappingOutcome::NoResponse => failed_line(
            title,
            t!("diagnostics-router-sin-respuesta").to_owned(),
            t!("diagnostics-resultado-sin-respuesta"),
        ),
        MappingOutcome::Refused(error) => failed_line(
            title,
            t!("diagnostics-router-rechazo", error = scrub_addresses(error)),
            t!("diagnostics-resultado-fallo"),
        ),
        MappingOutcome::NoGateway(error) => neutral_line(
            title,
            t!(
                "diagnostics-router-sin-puerta",
                error = scrub_addresses(error)
            ),
            t!("diagnostics-resultado-no-probado"),
        ),
    };
    line.address = match &report.outcome {
        MappingOutcome::Mapped(mapping) => Some(mapping.external.to_string()),
        MappingOutcome::ExternalOnly { external, .. } => Some(external.to_string()),
        _ => None,
    };
    line
}

/// El servidor para enseñarlo (`coroom.example.com`).
fn server_host(app: &App) -> String {
    app.account
        .as_ref()
        .map(|account| account.host())
        .unwrap_or_else(|| t!("diagnostics-anon-servidor").to_owned())
}

fn relay_line(
    app: &App,
    path: Option<Path>,
    rtt_ms: Option<f64>,
    unavailable: Option<&str>,
    remote: Option<SocketAddr>,
) -> Line {
    let title = t!("diagnostics-relay-de-host", host = server_host(app));
    let mut line = match (unavailable, path) {
        (Some(reason), _) => failed_line(
            title,
            reason.to_owned(),
            t!("diagnostics-resultado-no-disponible"),
        ),
        (None, Some(Path::Relayed)) => ok_line(
            title,
            match rtt_ms {
                Some(rtt) => t!("diagnostics-relay-en-uso-rtt", rtt = stats::ms(rtt)),
                None => t!("diagnostics-relay-en-uso").to_owned(),
            },
            t!("diagnostics-resultado-en-uso"),
        ),
        (None, Some(Path::Direct)) => neutral_line(
            title,
            t!("diagnostics-relay-respaldo").to_owned(),
            t!("diagnostics-resultado-no-hace-falta"),
        ),
        (None, None) => neutral_line(
            title,
            t!("diagnostics-relay-espera").to_owned(),
            t!("diagnostics-resultado-esperando"),
        ),
    };
    if path == Some(Path::Relayed) {
        line.address = remote.map(|remote| remote.to_string());
    }
    line
}

/// Candidatos, el router y el relay de una ruta.
fn route_lines(
    candidates: &[CandidateReport],
    mapping: Option<&MappingReport>,
    relay: Line,
) -> Vec<Line> {
    let mut lines: Vec<Line> = candidates.iter().map(candidate_line).collect();
    lines.extend(mapping.map(router_line));
    lines.push(relay);
    lines
}

/// RTT del invitado con el anfitrión: el de QUIC en las estadísticas o, antes del vídeo, el que
/// midió al entrar.
fn guest_rtt(app: &App) -> Option<f64> {
    app.diagnostics
        .guest
        .summary
        .as_ref()
        .and_then(|summary| summary.link.as_ref())
        .map(|link| link.rtt_ms)
        .filter(|rtt| *rtt > 0.0)
        .or_else(|| {
            app.guest
                .as_ref()
                .and_then(|guest| guest.estimate.map(|estimate| estimate.rtt_ms))
        })
}

fn guest_lines(app: &App) -> Vec<Line> {
    let side = &app.diagnostics.guest;
    let relay = relay_line(
        app,
        side.route.path,
        guest_rtt(app),
        side.relay_unavailable.as_deref(),
        side.route.remote,
    );
    route_lines(&side.route.candidates, side.mapping.as_ref(), relay)
}

fn mbps(kbps: f64) -> String {
    stats::number(kbps / 1000.0, 1)
}

fn probe_metric(probe: Option<Probe>, kbps: Option<u32>, detail: &'static str) -> Metric {
    let kbps = probe.map(|probe| probe.kbps).or(kbps);
    let mut detail = detail.to_owned();
    if let Some(probe) = probe.filter(|probe| probe.sent > 0) {
        detail = format!(
            "{detail} · {}",
            t!(
                "diagnostics-sonda-perdidos",
                lost = probe.lost,
                sent = probe.sent
            )
        );
    }
    Metric {
        label: t!("diagnostics-sonda"),
        detail: Some(detail),
        value: kbps.map_or_else(|| "—".to_owned(), |kbps| stats::kbps(f64::from(kbps))),
        widest: widest(&stats::KBPS_WIDEST),
    }
}

fn mtu_metric() -> Metric {
    Metric {
        label: t!("diagnostics-mtu"),
        detail: Some(t!("diagnostics-mtu-detalle").to_owned()),
        value: "—".to_owned(),
        widest: Vec::new(),
    }
}

fn input_value(input: Option<&InputLatency>) -> String {
    match input.filter(|input| input.samples > 0) {
        Some(input) => stats::p50_p95(input.p50_us, input.p95_us),
        None => "—".to_owned(),
    }
}

fn guest_metrics(app: &App) -> Vec<Metric> {
    let side = &app.diagnostics.guest;
    let summary = side.summary.as_ref();
    let link = summary.and_then(|summary| summary.link.as_ref());
    vec![
        Metric {
            label: t!("diagnostics-rtt-anfitrion"),
            detail: None,
            value: guest_rtt(app).map_or_else(|| "—".to_owned(), stats::ms),
            widest: widest(&stats::MS_WIDEST),
        },
        Metric {
            label: t!("diagnostics-perdida"),
            detail: Some(t!("diagnostics-perdida-detalle").to_owned()),
            value: link.map_or_else(|| "—".to_owned(), |link| stats::percent(link.loss_percent)),
            widest: widest(&stats::PERCENT_WIDEST),
        },
        mtu_metric(),
        probe_metric(side.route.probe, None, t!("diagnostics-sonda-hacia-ti")),
        Metric {
            label: t!("diagnostics-bitrate"),
            detail: Some(t!("diagnostics-bitrate-lo-limita-el-enlace").to_owned()),
            value: match (link, summary) {
                (Some(link), Some(summary)) => t!(
                    "diagnostics-bitrate-valor",
                    target = mbps(f64::from(link.target_kbps)),
                    real = mbps(summary.bitrate_recent_kbps)
                ),
                _ => "—".to_owned(),
            },
            widest: vec![t!(
                "diagnostics-bitrate-valor",
                target = "888,8",
                real = "888,8"
            )],
        },
    ]
}

fn host_guest_lines(app: &App, status: &GuestStatus, route: Option<&Route>) -> Vec<Line> {
    let host = &app.diagnostics.host;
    let empty = Route::default();
    let route = route.unwrap_or(&empty);
    let path = status.path.or(route.path);
    let relay = relay_line(
        app,
        path,
        status.rtt_ms,
        host.relay_unavailable.as_deref(),
        status.remote.or(route.remote),
    );
    // Los candidatos del evento (con su hora) o, si aún no llegó, los del estado.
    let candidates = if route.tried_at.is_some() {
        &route.candidates
    } else {
        &status.candidates
    };
    route_lines(candidates, None, relay)
}

fn host_guest_metrics(status: &GuestStatus, route: Option<&Route>) -> Vec<Metric> {
    vec![
        Metric {
            label: t!("diagnostics-rtt-invitado"),
            detail: (status.path == Some(Path::Relayed))
                .then(|| t!("diagnostics-rtt-tramo-relay").to_owned()),
            value: status.rtt_ms.map_or_else(|| "—".to_owned(), stats::ms),
            widest: widest(&stats::MS_WIDEST),
        },
        mtu_metric(),
        probe_metric(
            route.and_then(|route| route.probe),
            status.link_kbps,
            t!("diagnostics-sonda-hacia-invitado"),
        ),
        Metric {
            label: t!("diagnostics-bitrate-objetivo"),
            detail: Some(t!("diagnostics-bitrate-lo-limita-el-enlace").to_owned()),
            value: status
                .target_kbps
                .map_or_else(|| "—".to_owned(), |kbps| stats::kbps(f64::from(kbps))),
            widest: widest(&stats::KBPS_WIDEST),
        },
        Metric {
            label: t!("diagnostics-entrada"),
            detail: Some(t!("diagnostics-entrada-detalle").to_owned()),
            value: input_value(Some(&status.input)),
            widest: widest(&["88,8 ms / 88,8 ms", "888 ms / 888 ms"]),
        },
    ]
}

/// Los cinco tramos de la barra, de las etapas consecutivas del pipeline. `Captured` es el
/// instante del evento de captura (la captura no tiene marca de inicio): el primer tramo es
/// captura → codificado.
const GROUPS: [(Stage, Stage); 5] = [
    (Stage::Captured, Stage::Encoded),
    (Stage::Encoded, Stage::Sent),
    (Stage::Sent, Stage::Received),
    (Stage::Received, Stage::Decoded),
    (Stage::Decoded, Stage::Presented),
];

fn group_label(index: usize) -> &'static str {
    match index {
        0 => t!("diagnostics-etapa-codificacion"),
        1 => t!("diagnostics-etapa-envio"),
        2 => t!("diagnostics-etapa-red"),
        3 => t!("diagnostics-etapa-decodificacion"),
        _ => t!("diagnostics-etapa-presentacion"),
    }
}

/// El percentil de cada tramo en µs: la suma de los de sus etapas consecutivas (`None` sin
/// muestras).
fn groups(summary: &LatencySummary, percentile: Percentile) -> Option<[u64; 5]> {
    let mut out = [0u64; 5];
    for (slot, (from, to)) in out.iter_mut().zip(GROUPS) {
        let spans: Vec<&SpanStats> = summary
            .spans
            .iter()
            .filter(|span| span.from.index() >= from.index() && span.to.index() <= to.index())
            .collect();
        if spans.is_empty() || spans.iter().any(|span| span.samples == 0) {
            return None;
        }
        *slot = spans.iter().map(|span| percentile.of_span(span)).sum();
    }
    Some(out)
}

/// La etapa «dibujado de la interfaz»: lo que tarda la ventana en reconstruir, dibujar y entregar
/// la interfaz, con el percentil elegido; se mide en este equipo, con o sin vídeo.
fn ui_draws<'a>(app: &App) -> Element<'a, Message> {
    let text = match app.ui_draws.p50_p95() {
        Some((p50, p95)) => t!(
            "diagnostics-dibujado-de-la-interfaz",
            percentile = app.diagnostics.percentile.label(),
            value = ms_us(match app.diagnostics.percentile {
                Percentile::P50 => p50,
                Percentile::P95 => p95,
            }),
            draws = app.ui_draws.len()
        ),
        None => t!("diagnostics-dibujado-de-la-interfaz-vacio").to_owned(),
    };
    ui::caption(text).wrapping(Wrapping::WordOrGlyph).into()
}

fn ms_us(us: u64) -> String {
    stats::ms(us as f64 / 1000.0)
}

/// Vídeo total, entrada y lo añadido de extremo a extremo (la suma de los dos) con el percentil.
fn totals(summary: &LatencySummary, percentile: Percentile) -> (String, String, String) {
    let video = (summary.total.samples > 0).then(|| percentile.of_span(&summary.total));
    let input = summary
        .input
        .as_ref()
        .filter(|input| input.samples > 0)
        .map(|input| percentile.of_input(input));
    let show = |value: Option<u64>| value.map_or_else(|| "—".to_owned(), ms_us);
    let added = video.zip(input).map(|(video, input)| video + input);
    (show(video), show(input), show(added))
}

/// Las líneas de `coroom doctor` que van al diagnóstico: sin directorios, registro, llavero ni
/// los nombres de los dispositivos de audio (las direcciones se quitan después).
fn doctor_lines(lines: &[String]) -> Vec<String> {
    const PRIVATE: [&str; 3] = ["Directorio de datos", "Registro", "Llavero del sistema"];
    let mut out = Vec::new();
    let mut block = "";
    for line in lines {
        let nested = line.starts_with(' ');
        if !nested {
            block = line.as_str();
        }
        let private = if nested {
            block.starts_with("Audio")
                && (line.contains('«') || line.trim_start().starts_with("dispositivos"))
        } else {
            PRIVATE.iter().any(|prefix| line.starts_with(prefix))
        };
        if !private {
            out.push(line.clone());
        }
    }
    out
}

// --- Informe sin direcciones ni nombres ----------------------------------------------------------

/// El informe en texto plano: candidatos y resultados, prueba de enlace, etapas, versión, sistema,
/// adaptador y códecs. Sin direcciones IP, rutas ni nombres.
pub fn report(app: &App) -> String {
    let mut out: Vec<String> = vec![t!("diagnostics-informe-titulo", version = app.version)];
    let side = side(app);
    let role = match side {
        Side::Guest => t!("diagnostics-anon-invitado"),
        Side::Host => t!("diagnostics-anon-anfitrion"),
        Side::Lobby => t!("diagnostics-sin-sala"),
    };
    out.push(format!("{}: {role}", t!("diagnostics-informe-papel")));
    out.push(format!(
        "{}: {}",
        t!("diagnostics-servidor"),
        server_state(app)
    ));
    match side {
        Side::Guest => guest_report(app, &mut out),
        Side::Host => host_report(app, &mut out),
        Side::Lobby => {}
    }
    if let Some(lines) = &app.diagnostics.doctor {
        out.push(String::new());
        out.push(format!("{}:", t!("diagnostics-equipo")));
        out.extend(
            doctor_lines(lines)
                .into_iter()
                .map(|line| format!("  {line}")),
        );
    }
    let mut text = out.join("\n");
    text.push('\n');
    scrub(&text, &private_names(app))
}

fn path_word(path: Option<Path>) -> &'static str {
    match path {
        Some(Path::Direct) => t!("diagnostics-informe-ruta-directa"),
        Some(Path::Relayed) => t!("diagnostics-informe-ruta-relay"),
        None => t!("diagnostics-informe-ruta-conectando"),
    }
}

fn push_lines(out: &mut Vec<String>, lines: &[Line], metrics: &[Metric], indent: &str) {
    out.push(format!("{indent}{}:", t!("diagnostics-candidatos-titulo")));
    for line in lines {
        out.push(format!(
            "{indent}  - {}: {}. {}",
            line.title, line.result, line.explanation
        ));
    }
    out.push(format!("{indent}{}:", t!("diagnostics-enlace-titulo")));
    for metric in metrics {
        let detail = metric
            .detail
            .as_ref()
            .map_or_else(String::new, |detail| format!(" ({detail})"));
        out.push(format!(
            "{indent}  - {}: {}{detail}",
            metric.label, metric.value
        ));
    }
}

fn guest_report(app: &App, out: &mut Vec<String>) {
    let side = &app.diagnostics.guest;
    out.push(format!(
        "{}: {}",
        t!("diagnostics-informe-ruta"),
        path_word(side.route.path)
    ));
    if side.route.path == Some(Path::Relayed) {
        let reason = side
            .route
            .relay_reason
            .clone()
            .unwrap_or_else(|| t!("diagnostics-relay-sin-probar").to_owned());
        out.push(format!("{}: {reason}", t!("diagnostics-informe-motivo")));
    }
    if let Some(reconnecting) = &side.reconnecting {
        out.push(format!(
            "{}: {}",
            t!(
                "diagnostics-reconectando-titulo",
                attempt = reconnecting.attempt,
                of = reconnecting.of
            ),
            reconnecting.reason
        ));
    }
    out.push(String::new());
    push_lines(out, &guest_lines(app), &guest_metrics(app), "");
    if let Some(summary) = &side.summary
        && let (Some(p50), Some(p95)) = (
            groups(summary, Percentile::P50),
            groups(summary, Percentile::P95),
        )
    {
        out.push(t!(
            "diagnostics-informe-etapas",
            frames = summary.frames_in_window
        ));
        for (index, (p50, p95)) in p50.iter().zip(p95).enumerate() {
            out.push(format!(
                "  - {}: {} / {}",
                group_label(index),
                ms_us(*p50),
                ms_us(p95)
            ));
        }
        let (video50, input50, added50) = totals(summary, Percentile::P50);
        let (video95, input95, added95) = totals(summary, Percentile::P95);
        for (label, p50, p95) in [
            (t!("diagnostics-video-total"), video50, video95),
            (t!("diagnostics-entrada"), input50, input95),
            (t!("diagnostics-anadido"), added50, added95),
        ] {
            out.push(format!("  - {label}: {p50} / {p95}"));
        }
        if let Some(video) = &summary.video {
            out.push(format!("  - {video}"));
        }
    }
}

fn host_report(app: &App, out: &mut Vec<String>) {
    let host = &app.diagnostics.host;
    if host.statuses.is_empty() {
        out.push(t!("diagnostics-informe-nadie").to_owned());
    }
    for (index, status) in host.statuses.iter().enumerate() {
        let route = host.routes.get(&status.guest);
        out.push(String::new());
        out.push(format!(
            "{}: {}",
            t!("diagnostics-anon-invitado-n", n = index + 1),
            path_word(status.path)
        ));
        if status.path == Some(Path::Relayed)
            && let Some(reason) = route.and_then(|route| route.relay_reason.as_ref())
        {
            out.push(format!("  {}: {reason}", t!("diagnostics-informe-motivo")));
        }
        push_lines(
            out,
            &host_guest_lines(app, status, route),
            &host_guest_metrics(status, route),
            "  ",
        );
    }
    out.push(t!("diagnostics-etapas-anfitrion").to_owned());
}

fn server_state(app: &App) -> String {
    match (app.server_status.online, app.server_status.rtt_ms) {
        (Some(true), Some(rtt)) => t!("diagnostics-servidor-responde", rtt = stats::ms(rtt)),
        (Some(true), None) => t!("diagnostics-servidor-responde-sin-rtt").to_owned(),
        (Some(false), _) => t!("diagnostics-servidor-no-responde").to_owned(),
        (None, _) => t!("diagnostics-servidor-sin-comprobar").to_owned(),
    }
}

/// Nombres de personas, del equipo y del servidor que no pueden salir en el informe, con lo que
/// los sustituye (los más largos primero).
fn private_names(app: &App) -> Vec<(String, String)> {
    let mut names: Vec<(String, String)> = Vec::new();
    let mut add = |name: &str, replacement: &str| {
        let name = name.trim();
        if name.chars().count() >= 2 {
            names.push((name.to_owned(), replacement.to_owned()));
        }
    };
    let me = match side(app) {
        Side::Host => t!("diagnostics-anon-anfitrion"),
        Side::Guest | Side::Lobby => t!("diagnostics-anon-invitado"),
    };
    let server = t!("diagnostics-anon-servidor");
    if let Some(account) = &app.account {
        add(&account.display_name, me);
        add(&account.username, me);
        add(&account.device, t!("diagnostics-anon-equipo"));
        let host = account.host();
        add(&host, server);
        add(
            host.rsplit_once(':')
                .map_or(host.as_str(), |(name, _)| name),
            server,
        );
    }
    if let Some(guest) = &app.guest {
        if let Some(name) = &guest.host_name {
            add(name, t!("diagnostics-anon-anfitrion"));
        }
        for member in &guest.members {
            add(&member.name, t!("diagnostics-anon-participante"));
        }
    }
    for (index, status) in app.diagnostics.host.statuses.iter().enumerate() {
        add(
            &status.name,
            &t!("diagnostics-anon-invitado-n", n = index + 1),
        );
    }
    if let Some(friends) = app.friends.loaded() {
        let users = friends
            .friends
            .iter()
            .chain(friends.incoming.iter().map(|request| &request.user))
            .chain(friends.outgoing.iter().map(|request| &request.user));
        for user in users {
            add(&user.display_name, t!("diagnostics-anon-amigo"));
            add(&user.username, t!("diagnostics-anon-amigo"));
        }
    }
    names.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    names
}

/// Quita del texto los nombres (como palabras enteras, sin distinguir mayúsculas), las rutas de
/// usuario y las direcciones IP.
pub fn scrub(text: &str, names: &[(String, String)]) -> String {
    let mut text = text.to_owned();
    for (name, replacement) in names {
        text = replace_word(&text, name, replacement);
    }
    let text = scrub_addresses(&text);
    text.split('\n')
        .map(|line| {
            line.split(' ')
                .map(|word| {
                    if ["/home/", "/Users/", "\\Users\\"]
                        .iter()
                        .any(|marker| word.contains(marker))
                    {
                        t!("diagnostics-anon-ruta")
                    } else {
                        word
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `name` como palabra entera (sin letras ni cifras pegadas), sin distinguir mayúsculas.
fn replace_word(text: &str, name: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut previous: Option<char> = None;
    while !rest.is_empty() {
        let boundary = previous.is_none_or(|c| !c.is_alphanumeric());
        if boundary {
            let mut matched = 0;
            let mut chars = rest.char_indices();
            let mut ok = true;
            for wanted in name.chars() {
                match chars.next() {
                    Some((at, got)) if got.to_lowercase().eq(wanted.to_lowercase()) => {
                        matched = at + got.len_utf8();
                    }
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            let after = rest[matched..].chars().next();
            if ok && matched > 0 && after.is_none_or(|c| !c.is_alphanumeric()) {
                out.push_str(replacement);
                previous = rest[..matched].chars().next_back();
                rest = &rest[matched..];
                continue;
            }
        }
        let Some(c) = rest.chars().next() else { break };
        out.push(c);
        previous = Some(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn address_label(ip: IpAddr) -> &'static str {
    match candidate_class(SocketAddr::new(ip, 0)) {
        CandidateClass::Ipv6Global => t!("diagnostics-anon-ipv6"),
        CandidateClass::Ipv4Public => t!("diagnostics-anon-ipv4"),
        CandidateClass::CarrierNat => t!("diagnostics-anon-cgnat"),
        CandidateClass::Private => t!("diagnostics-anon-privada"),
    }
}

fn parse_address(run: &str) -> Option<IpAddr> {
    if let Ok(addr) = run.parse::<SocketAddr>() {
        return Some(addr.ip());
    }
    run.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok()
}

/// Cambia cada dirección IP (con puerto o sin él, v4 o v6) por su rango.
pub fn scrub_addresses(text: &str) -> String {
    let is_address = |c: char| c.is_ascii_hexdigit() || matches!(c, '.' | ':' | '[' | ']');
    let mut out = String::with_capacity(text.len());
    let mut start: Option<usize> = None;
    let flush = |out: &mut String, run: &str| {
        // Sin los separadores de los extremos («:192.0.2.1.») y, si va pegada a una palabra
        // («de:192.0.2.1»), sin lo que hay antes del primer separador.
        let core = run.trim_matches(['.', ':']);
        let lead = run.len() - run.trim_start_matches(['.', ':']).len();
        let mut tries = vec![(lead, core)];
        if let Some(at) = core.find(':') {
            tries.push((lead + at + 1, core[at + 1..].trim_start_matches(':')));
        }
        let found = tries.into_iter().find_map(|(offset, candidate)| {
            let separators = candidate.matches(['.', ':']).count();
            (candidate.chars().any(|c| c.is_ascii_digit()) && separators >= 2)
                .then(|| parse_address(candidate))
                .flatten()
                .map(|ip| (offset, candidate.len(), ip))
        });
        match found {
            Some((offset, len, ip)) => {
                out.push_str(&run[..offset]);
                out.push_str(address_label(ip));
                out.push_str(&run[offset + len..]);
            }
            None => out.push_str(run),
        }
    };
    for (at, c) in text.char_indices() {
        if is_address(c) {
            start.get_or_insert(at);
            continue;
        }
        if let Some(from) = start.take() {
            flush(&mut out, &text[from..at]);
        }
        out.push(c);
    }
    if let Some(from) = start {
        flush(&mut out, &text[from..]);
    }
    out
}

// --- Vista ----------------------------------------------------------------------------------------

fn section_title<'a>(title: &'a str, detail: Option<String>) -> Element<'a, Message> {
    let mut line = row![ui::strong(title).size(15.0)]
        .spacing(gap::XS)
        .align_y(Alignment::Center);
    if let Some(detail) = detail {
        line = line.push(ui::caption(detail).wrapping(Wrapping::WordOrGlyph));
    }
    line.into()
}

fn line_view<'a>(line: Line) -> Element<'a, Message> {
    let (svg, class) = match line.tone {
        Some(tone) => (SvgClass::Tone(tone), TextClass::Tone(tone)),
        None => (SvgClass::Secondary, TextClass::Secondary),
    };
    let mut texts = column![
        ui::body(line.title).wrapping(Wrapping::WordOrGlyph),
        ui::secondary(line.explanation).wrapping(Wrapping::WordOrGlyph),
    ]
    .spacing(2)
    .width(Length::Fill);
    if let Some(address) = line.address {
        texts = texts.push(
            ui::caption(address)
                .size(11.0)
                .class(TextClass::Disabled)
                .wrapping(Wrapping::Glyph),
        );
    }
    container(
        row![
            icon_with(line.glyph, 18.0, svg),
            texts,
            ui::strong(line.result)
                .size(type_size::CAPTION)
                .class(class)
                .wrapping(Wrapping::None),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([8.0, 14.0]))
    .width(Length::Fill)
    .into()
}

fn metric_view<'a>(metric: Metric) -> Element<'a, Message> {
    let mut texts = column![ui::body(metric.label).wrapping(Wrapping::WordOrGlyph)]
        .spacing(2)
        .width(Length::Fill);
    if let Some(detail) = metric.detail {
        texts = texts.push(ui::caption(detail).wrapping(Wrapping::WordOrGlyph));
    }
    container(
        row![texts, {
            let widest: Vec<&str> = metric.widest.iter().map(String::as_str).collect();
            ui::figure(
                |value: String| ui::mono(value, type_size::BODY),
                metric.value,
                &widest,
            )
        }]
        .spacing(gap::S)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([8.0, 14.0]))
    .width(Length::Fill)
    .into()
}

fn tried_detail(app: &App, route: &Route) -> String {
    let timeout = PUNCH_TIMEOUT.as_secs();
    match route.tried_at {
        // Redondeado («ahora mismo», «hace 2 min»): junto al título no cambia cada segundo.
        Some(at) => t!(
            "diagnostics-candidatos-detalle",
            ago = super::settings::ago_secs(app.now.saturating_duration_since(at).as_secs()),
            timeout = timeout
        ),
        None => t!(
            "diagnostics-candidatos-detalle-sin-intento",
            timeout = timeout
        ),
    }
}

/// Candidatos a la izquierda y prueba de enlace a la derecha.
fn route_columns<'a>(
    detail: String,
    tried: bool,
    lines: Vec<Line>,
    metrics: Vec<Metric>,
) -> Element<'a, Message> {
    let mut rows: Vec<Element<'a, Message>> = Vec::new();
    if !tried {
        rows.push(
            container(ui::secondary(t!("diagnostics-candidatos-vacio")))
                .padding(Padding::from([10.0, 14.0]))
                .into(),
        );
    }
    rows.extend(lines.into_iter().map(line_view));
    let left = column![
        section_title(t!("diagnostics-candidatos-titulo"), Some(detail)),
        ui::boxed_list(rows),
    ]
    .spacing(gap::XS)
    .width(Length::FillPortion(5));
    let right = column![
        section_title(
            t!("diagnostics-enlace-titulo"),
            Some(t!("diagnostics-enlace-detalle").to_owned())
        ),
        ui::boxed_list(metrics.into_iter().map(metric_view).collect()),
    ]
    .spacing(gap::XS)
    .width(Length::FillPortion(4));
    row![left, right]
        .spacing(gap::S)
        .align_y(Alignment::Start)
        .into()
}

fn route_banner<'a>(tone: Tone, title: String, text: String, retry: bool) -> Element<'a, Message> {
    let glyph = match tone {
        Tone::Success => Icon::CircleCheck,
        Tone::Warning => Icon::Warning,
        Tone::Error => Icon::CircleAlert,
        Tone::Info | Tone::Neutral => Icon::Info,
    };
    let mut line = row![
        icon_with(glyph, 20.0, SvgClass::Tone(tone)),
        column![
            ui::strong(title).wrapping(Wrapping::WordOrGlyph),
            ui::body(text).wrapping(Wrapping::WordOrGlyph),
        ]
        .spacing(2)
        .width(Length::Fill),
    ]
    .spacing(gap::S)
    .align_y(Alignment::Center);
    if retry {
        line = line.push(ui::button_with(
            Kind::Normal,
            t!("diagnostics-reintentar-ahora"),
            Some(Icon::Refresh),
            Some(Message::Diagnostics(Msg::Retry)),
        ));
    }
    container(line)
        .padding(Padding::from([12.0, 14.0]))
        .width(Length::Fill)
        .class(ContainerClass::Banner(tone))
        .into()
}

fn guest_banner<'a>(app: &App) -> Element<'a, Message> {
    let side = &app.diagnostics.guest;
    if let Some(reconnecting) = &side.reconnecting {
        return route_banner(
            Tone::Warning,
            t!(
                "diagnostics-reconectando-titulo",
                attempt = reconnecting.attempt,
                of = reconnecting.of
            ),
            reconnecting.reason.clone(),
            false,
        );
    }
    match side.route.path {
        Some(Path::Relayed) => {
            let mut text = match &side.route.relay_reason {
                Some(reason) => t!("diagnostics-relay-motivo", reason = reason),
                None => t!("diagnostics-relay-sin-probar").to_owned(),
            };
            let router_closed = side.mapping.as_ref().is_some_and(|report| {
                matches!(
                    report.outcome,
                    MappingOutcome::NoResponse
                        | MappingOutcome::Refused(_)
                        | MappingOutcome::ExternalOnly { .. }
                )
            });
            if router_closed {
                text.push(' ');
                text.push_str(t!("diagnostics-relay-router"));
            }
            text.push(' ');
            text.push_str(t!("diagnostics-relay-reintento"));
            route_banner(
                Tone::Warning,
                t!("diagnostics-relay-titulo", host = server_host(app)),
                text,
                true,
            )
        }
        Some(Path::Direct) => route_banner(
            Tone::Success,
            match guest_rtt(app) {
                Some(rtt) => t!("diagnostics-directa-titulo", rtt = stats::ms(rtt)),
                None => t!("diagnostics-directa-titulo-sin-rtt").to_owned(),
            },
            t!("diagnostics-directa-texto").to_owned(),
            false,
        ),
        None => match &side.relay_unavailable {
            Some(reason) => route_banner(
                Tone::Error,
                t!("diagnostics-sin-relay-titulo").to_owned(),
                t!("diagnostics-sin-relay-texto", reason = reason),
                false,
            ),
            None => route_banner(
                Tone::Info,
                t!("diagnostics-conectando-titulo").to_owned(),
                t!("diagnostics-conectando-texto").to_owned(),
                false,
            ),
        },
    }
}

fn stages_card<'a>(app: &App, summary: Option<&LatencySummary>) -> Element<'a, Message> {
    let percentile = app.diagnostics.percentile;
    let toggle = ui::segmented(
        [Percentile::P50, Percentile::P95]
            .into_iter()
            .map(|option| {
                (
                    option.label().to_owned(),
                    Message::Diagnostics(Msg::Percentile(option)),
                    option == percentile,
                )
            })
            .collect(),
    );
    let values = summary.and_then(|summary| groups(summary, percentile).map(|v| (summary, v)));
    let detail = summary.map(|summary| {
        t!(
            "diagnostics-etapas-detalle",
            percentile = percentile.label(),
            frames = summary.frames_in_window
        )
    });
    let header = row![
        container(section_title(t!("diagnostics-etapas-titulo"), detail)).width(Length::Fill),
        toggle
    ]
    .spacing(gap::S)
    .align_y(Alignment::Center);
    let Some((summary, values)) = values else {
        return column![
            header,
            ui::card(
                column![ui::secondary(t!("diagnostics-etapas-vacio")), ui_draws(app)]
                    .spacing(gap::XS)
            )
        ]
        .spacing(gap::XS)
        .into();
    };
    let largest = values.iter().copied().max().unwrap_or(0).max(1);
    let bar = Row::with_children(values.iter().enumerate().map(|(index, us)| {
        // Proporcional, con un mínimo para que un tramo de 0 ms se siga viendo.
        let portion = ((*us * 1000 / largest) as u16).max(12);
        container(Space::new())
            .width(Length::FillPortion(portion))
            .height(Length::Fixed(22.0))
            .class(ContainerClass::BarSegment {
                alternate: index % 2 == 1,
            })
            .into()
    }))
    .spacing(2)
    .width(Length::Fill);
    let labels = Row::with_children(values.iter().enumerate().map(|(index, us)| {
        column![
            ui::strong(ms_us(*us)).size(13.0),
            ui::caption(group_label(index)).wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(2)
        .width(Length::FillPortion(1))
        .into()
    }))
    .spacing(gap::XS)
    .width(Length::Fill);
    let (video, input, added) = totals(summary, percentile);
    let hint = ui::secondary(t!(
        "diagnostics-etapas-resumen",
        video = video,
        input = input,
        added = added
    ))
    .wrapping(Wrapping::WordOrGlyph);
    column![
        header,
        container(column![bar, labels, hint, ui_draws(app)].spacing(gap::XS))
            .padding(Padding::from([12.0, 14.0]))
            .width(Length::Fill)
            .class(ContainerClass::Card),
    ]
    .spacing(gap::XS)
    .into()
}

/// Vuelve a filtrar la salida de `coroom doctor` con los nombres que hay que ocultar ahora
/// (al llegar y al abrir el diagnóstico, cuando puede haber nombres nuevos: amigos, invitados).
pub fn refresh_doctor_text(app: &mut App) {
    let text = app
        .diagnostics
        .doctor
        .as_ref()
        .map(|lines| scrub(&doctor_lines(lines).join("\n"), &private_names(app)));
    app.diagnostics.doctor_text = text;
}

fn equipment<'a>(app: &App) -> Element<'a, Message> {
    let state = &app.diagnostics;
    let content: Element<'a, Message> = match &state.doctor_text {
        Some(text) => {
            let text = text.clone();
            container(ui::mono(text, type_size::CAPTION).wrapping(Wrapping::WordOrGlyph))
                .padding(Padding::from([10.0, 14.0]))
                .width(Length::Fill)
                .class(ContainerClass::Code)
                .into()
        }
        None if state.doctor_pending || app.settings_page.doctor.is_loading() => {
            ui::secondary(t!("diagnostics-equipo-analizando")).into()
        }
        None => ui::secondary(t!("diagnostics-equipo-sin-datos")).into(),
    };
    column![
        section_title(
            t!("diagnostics-equipo"),
            Some(t!("diagnostics-equipo-detalle").to_owned())
        ),
        content
    ]
    .spacing(gap::XS)
    .into()
}

fn guest_body<'a>(app: &App) -> Vec<Element<'a, Message>> {
    let side = &app.diagnostics.guest;
    vec![
        guest_banner(app),
        route_columns(
            tried_detail(app, &side.route),
            !side.route.candidates.is_empty(),
            guest_lines(app),
            guest_metrics(app),
        ),
        stages_card(app, side.summary.as_ref()),
        equipment(app),
    ]
}

fn path_badge<'a>(path: Option<Path>, rtt_ms: Option<f64>) -> Element<'a, Message> {
    let (tone, text) = match (path, rtt_ms) {
        (Some(Path::Direct), Some(rtt)) => (
            Tone::Success,
            t!("diagnostics-insignia-directa", rtt = stats::ms(rtt)),
        ),
        (Some(Path::Direct), None) => (
            Tone::Success,
            t!("diagnostics-insignia-directa-sin-rtt").to_owned(),
        ),
        (Some(Path::Relayed), Some(rtt)) => (
            Tone::Warning,
            t!("diagnostics-insignia-relay", rtt = stats::ms(rtt)),
        ),
        (Some(Path::Relayed), None) => (
            Tone::Warning,
            t!("diagnostics-insignia-relay-sin-rtt").to_owned(),
        ),
        (None, _) => (
            Tone::Neutral,
            t!("diagnostics-insignia-conectando").to_owned(),
        ),
    };
    ui::status_badge(tone, text)
}

fn host_body<'a>(app: &'a App) -> Vec<Element<'a, Message>> {
    let host = &app.diagnostics.host;
    let total = host.statuses.len();
    let relayed = host
        .statuses
        .iter()
        .filter(|status| status.path == Some(Path::Relayed))
        .count();
    let banner = if total == 0 {
        route_banner(
            Tone::Info,
            t!("diagnostics-anfitrion-vacio-titulo").to_owned(),
            t!("diagnostics-anfitrion-vacio-texto").to_owned(),
            false,
        )
    } else if relayed > 0 {
        route_banner(
            Tone::Warning,
            t!(
                "diagnostics-anfitrion-relay-titulo",
                relayed = relayed,
                total = total,
                host = server_host(app)
            ),
            t!("diagnostics-anfitrion-relay-texto").to_owned(),
            false,
        )
    } else {
        route_banner(
            Tone::Success,
            t!("diagnostics-anfitrion-directa-titulo").to_owned(),
            t!("diagnostics-anfitrion-directa-texto").to_owned(),
            false,
        )
    };
    let mut body = vec![banner];
    if let Some(mapping) = &host.mapping {
        body.push(
            column![
                section_title(t!("diagnostics-tu-router"), None),
                ui::boxed_list(vec![line_view(router_line(mapping))]),
            ]
            .spacing(gap::XS)
            .into(),
        );
    }
    for status in &host.statuses {
        let route = host.routes.get(&status.guest);
        let empty = Route::default();
        let detail = tried_detail(app, route.unwrap_or(&empty));
        let heading = row![
            crate::ui::avatars::view(app, &status.name, status.avatar, 28.0),
            ui::heading(status.name.as_str()),
            ui::fill(),
            path_badge(status.path, status.rtt_ms),
        ]
        .spacing(gap::XS)
        .align_y(Alignment::Center);
        let mut block = column![heading].spacing(gap::XS);
        if status.path == Some(Path::Relayed)
            && let Some(reason) = route.and_then(|route| route.relay_reason.as_ref())
        {
            block = block.push(
                ui::secondary(t!("diagnostics-relay-motivo", reason = reason))
                    .wrapping(Wrapping::WordOrGlyph),
            );
        }
        let tried = route.is_some_and(|route| !route.candidates.is_empty())
            || !status.candidates.is_empty();
        block = block.push(route_columns(
            detail,
            tried,
            host_guest_lines(app, status, route),
            host_guest_metrics(status, route),
        ));
        body.push(block.into());
    }
    if total > 0 {
        body.push(
            ui::secondary(t!("diagnostics-etapas-anfitrion"))
                .wrapping(Wrapping::WordOrGlyph)
                .into(),
        );
    }
    body.push(equipment(app));
    body
}

fn lobby_body<'a>(app: &App) -> Vec<Element<'a, Message>> {
    let relay = match &app.usage {
        Some(usage) if usage.relay_enabled => t!("diagnostics-servidor-relay-si"),
        Some(_) => t!("diagnostics-servidor-relay-no"),
        None => "—",
    };
    let server = ui::boxed_list(vec![
        metric_view(Metric {
            label: t!("diagnostics-servidor"),
            detail: None,
            value: server_host(app),
            widest: Vec::new(),
        }),
        metric_view(Metric {
            label: t!("diagnostics-servidor-estado"),
            detail: None,
            value: server_state(app),
            widest: Vec::new(),
        }),
        metric_view(Metric {
            label: t!("diagnostics-servidor-relay"),
            detail: None,
            value: relay.to_owned(),
            widest: Vec::new(),
        }),
    ]);
    vec![
        route_banner(
            Tone::Info,
            t!("diagnostics-lobby-titulo").to_owned(),
            t!("diagnostics-lobby-texto").to_owned(),
            false,
        ),
        column![section_title(t!("diagnostics-servidor"), None), server]
            .spacing(gap::XS)
            .into(),
        equipment(app),
    ]
}

fn subtitle(app: &App, side: Side) -> String {
    match side {
        Side::Guest => match app
            .guest
            .as_ref()
            .and_then(|guest| guest.host_name.as_ref())
        {
            Some(host) if app.diagnostics.guest.reconnecting.is_some() => {
                t!("diagnostics-sala-de-host-reconectando", host = host)
            }
            Some(host) => t!("diagnostics-sala-de-host", host = host),
            None => t!("diagnostics-sala-del-anfitrion").to_owned(),
        },
        Side::Host => t!("diagnostics-tu-sala").to_owned(),
        Side::Lobby => t!("diagnostics-sin-sala").to_owned(),
    }
}

fn header_badge<'a>(app: &App, side: Side) -> Element<'a, Message> {
    match side {
        Side::Guest => path_badge(app.diagnostics.guest.route.path, guest_rtt(app)),
        Side::Host => {
            let statuses = &app.diagnostics.host.statuses;
            let tone = if statuses.is_empty() {
                Tone::Neutral
            } else if statuses
                .iter()
                .any(|status| status.path == Some(Path::Relayed))
            {
                Tone::Warning
            } else {
                Tone::Success
            };
            let text = if statuses.len() == 1 {
                t!("diagnostics-insignia-un-invitado").to_owned()
            } else {
                t!("diagnostics-insignia-invitados", count = statuses.len())
            };
            ui::status_badge(tone, text)
        }
        Side::Lobby => app.server_badge(),
    }
}

/// El diagnóstico (el panel que la aplicación pone encima de la pantalla).
pub fn view(app: &App) -> Element<'_, Message> {
    let side = side(app);
    let header = row![
        column![
            ui::heading(t!("diagnostics-titulo")),
            ui::caption(subtitle(app, side))
        ]
        .spacing(2)
        .width(Length::Fill),
        header_badge(app, side),
        ui::button_with(
            Kind::Normal,
            t!("diagnostics-copiar-informe"),
            Some(Icon::Copy),
            Some(Message::Diagnostics(Msg::CopyReport)),
        ),
        ui::icon_button(
            Icon::Close,
            t!("app-cerrar"),
            Some(Message::Diagnostics(Msg::Close))
        ),
    ]
    .spacing(gap::XS)
    .align_y(Alignment::Center);
    let body = match side {
        Side::Guest => guest_body(app),
        Side::Host => host_body(app),
        Side::Lobby => lobby_body(app),
    };
    container(column![
        container(header).padding(Padding {
            top: gap::S,
            right: gap::S,
            bottom: gap::S,
            left: gap::L,
        }),
        rule::horizontal(1),
        scrollable(
            container(Column::with_children(body).spacing(gap::M)).padding(Padding {
                top: gap::M,
                right: gap::L,
                bottom: gap::L,
                left: gap::L,
            })
        )
        .height(Length::Fill),
    ])
    .width(Length::Fill)
    .max_width(PANEL_WIDTH)
    .height(Length::Fill)
    .class(ContainerClass::Dialog)
    .into()
}

/// Escenas del diagnóstico para las capturas.
pub(crate) mod shots {
    use std::time::{Duration, Instant};

    use coroom_session::GuestId;
    use coroom_session::latency::{LatencyLedger, LinkSummary, Stage};
    use coroom_session::remote::{CandidateOutcome, CandidateReport, SessionEvent};
    use coroom_transport::DirectAttempt;
    use coroom_transport::nat::{Mapping, MappingOutcome, MappingProtocol, MappingReport};
    use coroom_transport::quic::Path;

    use super::Msg;
    use crate::ui::app::{App, Event, Message};
    use crate::ui::shots::{Builder, sample_doctor};

    pub(crate) fn candidate(addr: &str, outcome: CandidateOutcome) -> CandidateReport {
        let addr = addr.parse().expect("dirección");
        CandidateReport {
            addr,
            class: coroom_transport::connect::candidate_class(addr),
            outcome,
        }
    }

    /// Cuatro direcciones del anfitrión que no responden (direcciones de documentación).
    pub(crate) fn failed_candidates() -> Vec<CandidateReport> {
        vec![
            candidate("[2001:db8::10]:50432", CandidateOutcome::Cancelled),
            candidate("198.51.100.7:50432", CandidateOutcome::NoResponse),
            candidate("100.64.12.9:50432", CandidateOutcome::NoResponse),
            candidate(
                "192.168.1.20:50432",
                CandidateOutcome::Failed("red inalcanzable hacia 192.168.1.20".to_owned()),
            ),
        ]
    }

    /// Estadísticas del invitado con un registro de latencia propio (las de la sala, con `red`
    /// µs en el tramo de la red y algo de variación) y el enlace por `path`.
    pub(crate) fn stats(red: u64, path: &str, rtt_ms: f64) -> crate::ui::screens::guest::Stats {
        let ledger = LatencyLedger::new();
        ledger.set_video("AV1 1920×1080 a 60 fps (Vulkan Video)");
        let start = Instant::now();
        // Microsegundos de cada etapa, de captura → codificado a decodificado → presentado.
        let steps = [7_300u64, 150, 250, red, 60, 2_600, 6_000];
        for frame in 1..=120u64 {
            let jitter = (frame * 37) % 900;
            let mut at = start + Duration::from_micros(frame * 16_667);
            ledger.mark(frame, Stage::Captured, at);
            for (stage, step) in Stage::ALL[1..].iter().zip(steps) {
                let extra = if *stage == Stage::Received { jitter } else { 0 };
                at += Duration::from_micros(step + extra);
                ledger.mark(frame, *stage, at);
            }
            ledger.record_encoded_bytes_at(42_000, at);
        }
        let mut stats = crate::ui::screens::guest::shots::sample_stats();
        let link = stats.summary.link.take();
        let input = stats.summary.input.take();
        stats.summary = ledger.summary();
        stats.summary.fps = 60.0;
        stats.summary.bitrate_kbps = 18_400.0;
        stats.summary.fps_recent = 60.0;
        stats.summary.bitrate_recent_kbps = 18_400.0;
        stats.summary.input = input;
        stats.summary.link = link.map(|link| LinkSummary {
            path: path.to_owned(),
            remote: None,
            rtt_ms,
            target_kbps: 20_000,
            loss_percent: 0.8,
            ..link
        });
        stats
    }

    fn guest_event(app: &mut App, event: SessionEvent) {
        app.update(Message::Event(Event::Guest(event)));
    }

    fn back_in_time(app: &mut App, secs: u64) {
        if let Some(at) = app.now.checked_sub(Duration::from_secs(secs)) {
            app.diagnostics.guest.route.tried_at = Some(at);
        }
    }

    /// Como invitado por el relay: cuatro candidatos fallidos y el router sin UPnP.
    pub(crate) fn guest_relay(app: &mut App) {
        crate::ui::screens::guest::shots::video(app);
        let candidates = failed_candidates();
        let reason = DirectAttempt {
            connection: None,
            candidates: candidates.clone(),
        }
        .relay_reason();
        guest_event(
            app,
            SessionEvent::PortMapping {
                report: MappingReport {
                    gateway: Some("192.168.1.1".parse().expect("dirección")),
                    internal: Some("192.168.1.20:50432".parse().expect("dirección")),
                    outcome: MappingOutcome::NoResponse,
                },
            },
        );
        guest_event(
            app,
            SessionEvent::Candidates {
                guest: None,
                candidates,
                relay_reason: Some(reason),
            },
        );
        guest_event(
            app,
            SessionEvent::Connected {
                path: Path::Relayed,
                remote: "198.51.100.200:4433".parse().expect("dirección"),
            },
        );
        guest_event(
            app,
            SessionEvent::LinkProbe {
                guest: None,
                kbps: 94_000,
                lost: 1,
                sent: 64,
            },
        );
        app.update(Message::Event(Event::GuestStats(Box::new(stats(
            15_400, "relay", 41.0,
        )))));
        app.update(Message::Event(Event::Doctor(sample_doctor())));
        back_in_time(app, 12);
        app.update(Message::Diagnostics(Msg::Open));
        app.toasts.clear();
    }

    /// Como invitado en directo.
    pub(crate) fn guest_direct(app: &mut App) {
        crate::ui::screens::guest::shots::video(app);
        guest_event(
            app,
            SessionEvent::PortMapping {
                report: MappingReport {
                    gateway: Some("192.168.1.1".parse().expect("dirección")),
                    internal: Some("192.168.1.20:50432".parse().expect("dirección")),
                    outcome: MappingOutcome::Mapped(Mapping {
                        external: "203.0.113.5:50432".parse().expect("dirección"),
                        protocol: MappingProtocol::Pcp,
                        lifetime: Some(Duration::from_secs(7200)),
                    }),
                },
            },
        );
        guest_event(
            app,
            SessionEvent::Candidates {
                guest: None,
                candidates: vec![
                    candidate("[2001:db8::10]:50432", CandidateOutcome::Cancelled),
                    candidate("192.0.2.10:4433", CandidateOutcome::Connected),
                    candidate("192.168.1.20:50432", CandidateOutcome::NoResponse),
                ],
                relay_reason: None,
            },
        );
        guest_event(
            app,
            SessionEvent::LinkProbe {
                guest: None,
                kbps: 240_000,
                lost: 0,
                sent: 64,
            },
        );
        app.update(Message::Event(Event::GuestStats(Box::new(stats(
            5_200, "directo", 13.8,
        )))));
        app.update(Message::Event(Event::Doctor(sample_doctor())));
        back_in_time(app, 4);
        app.update(Message::Diagnostics(Msg::Open));
        app.toasts.clear();
    }

    /// En el lobby: solo el servidor y este equipo.
    pub(crate) fn lobby(app: &mut App) {
        crate::ui::screens::home::shots::home(app);
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: Some(12.4),
            online: true,
        }));
        app.update(Message::Event(Event::Doctor(sample_doctor())));
        app.update(Message::Diagnostics(Msg::Open));
        app.toasts.clear();
    }

    /// Como anfitrión con dos invitados: uno en directo y otro por el relay.
    pub(crate) fn host(app: &mut App) {
        crate::ui::screens::host::shots::host_room(app);
        let host_event = |app: &mut App, event: SessionEvent| {
            app.update(Message::Event(Event::Host(event)));
        };
        host_event(
            app,
            SessionEvent::PortMapping {
                report: MappingReport {
                    gateway: Some("192.168.1.1".parse().expect("dirección")),
                    internal: Some("192.168.1.30:50500".parse().expect("dirección")),
                    outcome: MappingOutcome::Mapped(Mapping {
                        external: "203.0.113.8:50500".parse().expect("dirección"),
                        protocol: MappingProtocol::Upnp,
                        lifetime: None,
                    }),
                },
            },
        );
        host_event(
            app,
            SessionEvent::Candidates {
                guest: Some(GuestId(1)),
                candidates: vec![candidate("192.0.2.24:50412", CandidateOutcome::Connected)],
                relay_reason: None,
            },
        );
        let failed = vec![
            candidate("198.51.100.9:50433", CandidateOutcome::NoResponse),
            candidate("100.64.3.4:50433", CandidateOutcome::NoResponse),
        ];
        let reason = DirectAttempt {
            connection: None,
            candidates: failed.clone(),
        }
        .relay_reason();
        host_event(
            app,
            SessionEvent::Candidates {
                guest: Some(GuestId(2)),
                candidates: failed,
                relay_reason: Some(reason),
            },
        );
        host_event(
            app,
            SessionEvent::LinkProbe {
                guest: Some(GuestId(2)),
                kbps: 21_000,
                lost: 3,
                sent: 64,
            },
        );
        if let Some(status) = app.diagnostics.host.statuses.first_mut() {
            status.input = coroom_session::latency::InputLatency {
                samples: 600,
                p50_us: 7_400,
                p95_us: 11_900,
                ..Default::default()
            };
        }
        app.update(Message::Event(Event::Doctor(sample_doctor())));
        app.update(Message::Diagnostics(Msg::Open));
        app.toasts.clear();
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[
        ("24-diagnostico-relay", guest_relay),
        ("24-diagnostico-directa", guest_direct),
        ("24-diagnostico-lobby", lobby),
        ("24-diagnostico-anfitrion", host),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;
    use crate::ui::shots::{base, signed_in};
    use coroom_ui::font::Fonts;
    use coroom_ui::theme::Mode;

    fn has_word(text: &str, word: &str) -> bool {
        text.split(|c: char| !c.is_alphanumeric())
            .any(|token| token.eq_ignore_ascii_case(word))
    }

    #[test]
    fn open_asks_for_the_doctor_once_and_close_closes() {
        let mut app = app();
        assert_eq!(
            app.update(Message::Diagnostics(Msg::Open)),
            [Effect::RunDoctor]
        );
        assert!(app.diagnostics.open);
        app.update(Message::Diagnostics(Msg::Close));
        assert!(!app.diagnostics.open);
        assert!(
            app.update(Message::Diagnostics(Msg::Open)).is_empty(),
            "ya pedido"
        );
        app.update(Message::Event(Event::Doctor(vec!["coroom 1.0.0".into()])));
        assert!(!app.diagnostics.doctor_pending);
        app.update(Message::Shortcut(crate::ui::app::Shortcut::Escape));
        assert!(!app.diagnostics.open, "Esc lo cierra");
        assert!(app.update(Message::Diagnostics(Msg::Open)).is_empty());
    }

    #[test]
    fn events_build_the_route_candidates_and_stages() {
        let mut app = base(Mode::Light, Fonts::default());
        shots::guest_relay(&mut app);
        let guest = &app.diagnostics.guest;
        assert_eq!(guest.route.path, Some(Path::Relayed));
        assert_eq!(guest.route.candidates.len(), 4);
        assert!(guest.route.relay_reason.is_some());
        assert!(guest.route.tried_at.is_some());
        assert_eq!(
            guest.route.probe,
            Some(Probe {
                kbps: 94_000,
                lost: 1,
                sent: 64
            })
        );
        assert!(matches!(
            guest.mapping.as_ref().map(|report| &report.outcome),
            Some(MappingOutcome::NoResponse)
        ));
        let summary = guest.summary.as_ref().expect("resumen");
        let p50 = groups(summary, Percentile::P50).expect("etapas");
        // Las etapas de `shots::stats`: 7,3 ms; 0,15 + 0,25; 15,4 + variación; 0,06 + 2,6; 6,0.
        assert_eq!(p50[0], 7_300);
        assert_eq!(p50[1], 400);
        assert!(p50[2] >= 15_400);
        assert_eq!(p50[3], 2_660);
        assert_eq!(p50[4], 6_000);
        // Cuatro candidatos, el router y el relay en uso.
        let lines = guest_lines(&app);
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[5].result, t!("diagnostics-resultado-en-uso"));
        assert!(
            lines[..5]
                .iter()
                .all(|line| line.tone != Some(Tone::Success))
        );
        // Al salir de la sala se olvida.
        app.update(Message::Event(Event::GuestEnded(None)));
        assert_eq!(app.diagnostics.guest, GuestSide::default());
    }

    #[test]
    fn the_host_keeps_one_route_per_guest() {
        let mut app = base(Mode::Light, Fonts::default());
        shots::host(&mut app);
        let host = &app.diagnostics.host;
        assert_eq!(host.statuses.len(), 2);
        assert_eq!(host.routes.len(), 2);
        assert!(host.routes[&GuestId(2)].relay_reason.is_some());
        app.update(Message::Event(Event::Host(SessionEvent::GuestLeft {
            guest: GuestId(2),
            kicked: false,
        })));
        assert_eq!(app.diagnostics.host.routes.len(), 1);
        assert_eq!(app.diagnostics.host.statuses.len(), 1);
    }

    #[test]
    fn retry_reconnects_only_as_guest() {
        let mut app = base(Mode::Light, Fonts::default());
        signed_in(&mut app, crate::ui::app::Page::Home);
        assert!(app.update(Message::Diagnostics(Msg::Retry)).is_empty());
        let mut host = base(Mode::Light, Fonts::default());
        shots::host(&mut host);
        assert!(host.update(Message::Diagnostics(Msg::Retry)).is_empty());
        let mut guest = base(Mode::Light, Fonts::default());
        shots::guest_relay(&mut guest);
        assert_eq!(
            guest.update(Message::Diagnostics(Msg::Retry)),
            [Effect::Reconnect]
        );
        // Ya reconectando: no se pide otra vez.
        guest.update(Message::Event(Event::Guest(SessionEvent::Reconnecting {
            attempt: 1,
            of: 5,
            retry_in: std::time::Duration::from_millis(500),
            reason: "prueba".to_owned(),
        })));
        assert!(guest.update(Message::Diagnostics(Msg::Retry)).is_empty());
        guest.update(Message::Event(Event::Guest(SessionEvent::Reconnected {
            path: Path::Direct,
        })));
        assert_eq!(guest.diagnostics.guest.reconnecting, None);
        assert_eq!(guest.diagnostics.guest.route.path, Some(Path::Direct));
    }

    #[test]
    fn the_percentile_toggle_changes_the_stages() {
        let mut app = base(Mode::Light, Fonts::default());
        shots::guest_relay(&mut app);
        app.update(Message::Diagnostics(Msg::Percentile(Percentile::P95)));
        assert_eq!(app.diagnostics.percentile, Percentile::P95);
        let summary = app.diagnostics.guest.summary.as_ref().expect("resumen");
        let p50 = groups(summary, Percentile::P50).expect("p50");
        let p95 = groups(summary, Percentile::P95).expect("p95");
        assert!(p95[2] > p50[2], "la red varía: el p95 es mayor");
    }

    /// Ninguna dirección ni nombre de los datos de ejemplo llega al informe copiado.
    #[test]
    fn the_copied_report_has_no_addresses_nor_names() {
        let forbidden = [
            "192.0.2.",
            "198.51.100.",
            "203.0.113.",
            "100.64.",
            "192.168.",
            "2001:db8",
            "coroom.example.com",
        ];
        let names = ["Ana", "ana", "Bea", "Carla", "Dani", "Fran", "Gael"];
        for build in [
            shots::guest_relay as fn(&mut App),
            shots::guest_direct,
            shots::lobby,
            shots::host,
        ] {
            let mut app = base(Mode::Light, Fonts::default());
            build(&mut app);
            app.toasts.clear();
            let effects = app.update(Message::Diagnostics(Msg::CopyReport));
            let [Effect::Copy(text)] = effects.as_slice() else {
                panic!("se esperaba copiar el informe: {effects:?}");
            };
            assert!(
                app.toasts
                    .iter()
                    .any(|toast| toast.text == t!("diagnostics-informe-copiado"))
            );
            for needle in forbidden {
                assert!(!text.contains(needle), "«{needle}» en el informe:\n{text}");
            }
            for name in names {
                assert!(!has_word(text, name), "«{name}» en el informe:\n{text}");
            }
            assert!(text.contains(app.version), "lleva la versión");
            assert!(text.contains("Adaptador"), "lleva el adaptador:\n{text}");
        }
        // Lo que sí lleva: candidatos y resultados, la sonda y las etapas.
        let mut app = base(Mode::Light, Fonts::default());
        shots::guest_relay(&mut app);
        let text = report(&app);
        assert!(text.contains(t!("diagnostics-clase-cgnat")));
        assert!(text.contains(t!("diagnostics-resultado-sin-respuesta")));
        assert!(text.contains(t!("diagnostics-anon-privada")), "{text}");
        assert!(text.contains(t!("diagnostics-etapa-red")));
        assert!(text.contains("94,0 Mbit/s"), "{text}");
        assert!(text.contains(t!("diagnostics-informe-ruta-relay")));
    }

    #[test]
    fn addresses_and_names_are_scrubbed() {
        assert_eq!(
            scrub_addresses("de 198.51.100.7:4433, 192.168.1.1 y [2001:db8::1]:9."),
            "de [IPv4 pública], [red local] y [IPv6 global]."
        );
        assert_eq!(scrub_addresses("100.64.0.9"), "[IPv4 de la operadora]");
        // Lo que no es una dirección se queda.
        let plain = "coroom 1.0.0 · AV1 1920×1080 · coroom_session::remote · 12:30 · cafe:beef";
        assert_eq!(scrub_addresses(plain), plain);
        let names = vec![("Ana".to_owned(), "invitado".to_owned())];
        assert_eq!(
            scrub("ana y Ana, pero no Anabel ni ventana", &names),
            "invitado y invitado, pero no Anabel ni ventana"
        );
        assert_eq!(
            scrub("en /home/alguien/.local/share/coroom", &[]),
            "en [ruta]"
        );
    }

    #[test]
    fn the_doctor_drops_paths_and_device_names() {
        let lines: Vec<String> = [
            "coroom 1.0.0",
            "Directorio de datos: /home/alguien/.local/share/coroom",
            "Registro: /home/alguien/.local/state/coroom/coroom.log",
            "Adaptador gráfico: GPU (Vulkan 1.4)",
            "Audio:",
            "  PipeWire: captura de aplicaciones",
            "  micrófono: «alsa_input.usb» — Auriculares de Ana (por defecto)",
            "  dispositivos: ninguno",
            "Router (abrir el puerto UDP para la conexión directa):",
            "  puerta de enlace: 192.168.1.1 (MTU del adaptador: 1500)",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let kept = doctor_lines(&lines);
        assert_eq!(kept.len(), 6, "{kept:?}");
        assert!(kept.iter().all(|line| !line.contains("/home/")));
        assert!(kept.iter().all(|line| !line.contains("Ana")));
        assert!(kept.iter().any(|line| line.contains("PipeWire")));
        let scrubbed = scrub(&kept.join("\n"), &[]);
        assert!(scrubbed.contains("puerta de enlace: [red local]"));
    }

    #[test]
    fn views_render_without_panicking() {
        for build in [
            shots::guest_relay as fn(&mut App),
            shots::guest_direct,
            shots::lobby,
            shots::host,
        ] {
            let mut app = base(Mode::Dark, Fonts::default());
            build(&mut app);
            let _ = view(&app);
            let _ = app.view();
        }
    }
}
