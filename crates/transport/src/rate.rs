//! Control de tasa de los medios y controlador de congestión de `quinn`.
//!
//! [`RateController`] está inspirado en **SCReAM** (RFC 8298): el receptor envía un `Feedback`
//! cada 50 ms con sus marcas de llegada, los bytes recibidos, los objetos perdidos y las marcas
//! ECN-CE; el emisor estima la cola del camino como el retardo de ida (OWD) por encima de su mínimo
//! de los últimos 10 s y ajusta el bitrate objetivo:
//!
//! - **por retardo**: sin cola, crecimiento multiplicativo suave, +5 % cada 100 ms, hasta
//!   `max_kbps`, pero nunca por encima de [`DELIVERED_HEADROOM`] veces lo que el receptor dice haber
//!   **recibido** en los últimos [`RATE_WINDOW`]: lo que llega es la única medida del enlace, y
//!   también de si el emisor usa el objetivo (con la pantalla quieta el codificador manda poco y el
//!   objetivo se queda donde está; antes crecía sin tope y el primer intra tras un cambio de escena
//!   salía a un bitrate que el camino no pasaba). Cola por encima de [`QUEUE_THRESHOLD`] y
//!   creciendo: reducción proporcional al exceso y, si el emisor usaba el objetivo
//!   ([`UTILIZATION`]), hasta por debajo de lo recibido ([`DELIVERED_CUT`]); tras un recorte no se
//!   vuelve a crecer hasta que la cola se vacía y pasa [`HOLD_AFTER_CUT`] (antes la subida empezaba
//!   100 ms después del recorte, con la cola aún llena, y se encadenaban los recortes). Marcas
//!   ECN-CE sin cola: reducción del 10 % (reacción tipo L4S);
//! - **por pérdida**, una segunda estimación como la del controlador GCC
//!   (`draft-ietf-rmcat-gcc`, §6): con la pérdida de los informes del último segundo
//!   ([`LOSS_WINDOW`]) por debajo del 2 % crece (con el mismo tope de lo recibido), entre el 2 y
//!   el 10 % se mantiene y por encima del 10 % se recorta en `pérdida / 2` cada 300 ms;
//! - el objetivo es el **mínimo** de las dos. La pérdida aleatoria de un enlace malo (sin cola) la
//!   cubren la FEC y el `Nack`: con una sola estimación, recortar con cada 2 % la hundía hasta el
//!   mínimo, y con el 5 % de pérdida del perfil `mala` ya no se recuperaba de ningún recorte por
//!   cola (lo destapó la matriz de red).
//!
//! La **cola** es el mínimo del OWD de los informes de los últimos 500 ms ([`QUEUE_WINDOW`]) por
//! encima del mínimo de los últimos 10 s: una cola de verdad sube también el mínimo, el jitter no
//! (con 60 ± 20 ms y ~15 muestras en la ventana, el mínimo pasa de los 10 ms del umbral con una
//! probabilidad del 1 %; una cola que crece se detecta con 500 ms de retraso como mucho).
//! (Antes era la mediana de cada informe, y con 30 ± 10 ms de jitter la «cola» rondaba
//! los 10 ms del umbral sin que hubiera ninguna: el bitrate caía al mínimo.) Además, dos informes
//! seguidos cuyo mínimo supere en [`FAST_QUEUE`] el retardo base son una cola segura (con ± 30 ms
//! de jitter y tres muestras por informe, un informe solo lo hace por azar cada ~10 s; dos
//! seguidos, cada ~40 min) y recortan sin esperar a la ventana: una cola grande, la de un intra que
//! no cabe, se detecta en 100 ms y no en 500.
//!
//! **Techo de ráfaga** ([`RateController::burst_cap_kbps`]): un enlace con limitador (la subida de
//! muchos operadores, algunos routers) tira lo que llega por encima de su capacidad sin hacer cola,
//! así que el retardo no avisa y la pérdida que causa (2-10 %) quedaba en la franja que se tomaba
//! por aleatoria: el objetivo no bajaba y se perdían fotogramas sin parar (se pedían intras, que
//! volvían a desbordar). Y lo que desborda no es la media sino el ritmo al que sale cada fotograma
//! (el pacing lo comprime hasta en el 30 % del intervalo: unas 3 veces la media). Con pérdida por
//! encima de la mitad de [`LOSS_GROW`] y el objetivo en uso, el techo baja a lo que pasó del ritmo
//! al que salían los fotogramas ([`RateController::note_pace`]), con un 5 % de margen y al menos a
//! [`BURST_CUT`], cada [`BURST_HOLD`] mientras siga (la pérdida GCC espera a esa comprobación); el
//! objetivo, a [`BURST_TARGET_SHARE`] del techo (que quepa en un intervalo); y el pacing reparte
//! a ese techo aunque tarde más del 30 % del intervalo. Sin pérdida durante [`BURST_HOLD`] × 2 el
//! techo sube un [`BURST_GROW`] cada [`BURST_HOLD`] y, cuando supera el ritmo natural con holgura,
//! desaparece. Cada recorte se comprueba con lo que predice: si la pérdida `p` viene de desbordar
//! el enlace, bajar el ritmo a una proporción `r` la deja en `1 − (1 − p) / r` (casi siempre en
//! nada); si la pérdida de después del recorte queda más de un punto por encima de esa predicción
//! [`BURST_CHECKS`] veces sin bajar ni subir de forma significativa (una sola puede ser un intra;
//! si sube, son los intras que la pérdida hace pedir), no depende del ritmo (la de un wifi malo,
//! que cubren la FEC y el `Nack`): el recorte se deshace y esa pérdida se anota como propia del
//! enlace, que ya no recorta mientras no la pase con margen (el de una binomial: con pocos
//! objetos por segundo, la medida es ruidosa) y se olvida despacio si el enlace mejora.
//!
//! La salida [`RateController::target_kbps`] lleva histéresis: si la variación es < 10 % no cambia
//! más de una vez por segundo, para no reconfigurar el codificador a cada informe.
//!
//! [`MediaControllerFactory`] construye el controlador de congestión propio para `quinn`: fija
//! la ventana a `target_kbps × (RTT + 25 ms de retraso de los ACK) × 1,5` (nunca por debajo de
//! 64 KB) para que QUIC no frene los
//! datagramas por su cuenta pero tampoco deje salir ráfagas muy por encima del objetivo (el pacer de
//! `quinn` reparte la ventana en un RTT: con ×2 las ráfagas iban a 2,5 veces el objetivo y llenaban
//! la cola del camino), y cuenta los eventos de congestión por ECN que `quinn` le notifica
//! (`on_congestion_event` con `lost_bytes == 0`). Se compara con el BBR
//! de `quinn` ([`Congestion::Bbr`]) en el spike `tests/spike.rs`.
//!
//! ECN: `quinn` 0.11 marca ECT(0) y procesa los contadores ECN de los ACK, pero no expone al
//! receptor cuántos paquetes llegaron con CE. Por eso `Feedback::ecn_ce` va a 0 desde el receptor y
//! el emisor usa en su lugar los eventos ECN que ve su controlador de congestión
//! ([`CongestionShared::ecn_events`]).

use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use quinn::congestion::{BbrConfig, Controller, ControllerFactory, CubicConfig};
// `quinn` no reexporta el estimador de RTT que recibe `Controller::on_ack`.
use quinn_proto::RttEstimator;

/// Ventana mínima del controlador de congestión de medios.
pub const MIN_WINDOW: u64 = 64 * 1024;

/// Factor de la ventana sobre el producto bitrate × RTT.
pub const WINDOW_RTT_FACTOR: f64 = 1.5;

/// Corte de la red que la ventana aguanta sin llenarse: lo enviado durante un corte se pierde,
/// pero `quinn` lo cuenta en vuelo hasta que un ACK posterior lo da por perdido. Con una ventana de
/// solo 1,5 × BDP, un corte de 40 ms la llenaba, la conexión dejaba de enviar y volvía con el
/// temporizador de pérdida (PTO) de QUIC, que se duplica en cada intento sin respuesta: tras un
/// corte de 600 ms, 600 ms más sin vídeo. El ritmo lo siguen poniendo el control de tasa y el
/// pacing de los medios; esta ventana nunca fue para frenarlos.
pub const OUTAGE_ALLOWANCE: Duration = Duration::from_secs(2);

/// Ventana que usa el controlador de medios: la de [`media_window`] o la que aguanta un corte de
/// [`OUTAGE_ALLOWANCE`] al bitrate objetivo, la mayor.
pub fn controller_window(target_kbps: u32, rtt: Duration) -> u64 {
    let outage = (u64::from(target_kbps) * 1000 / 8) as f64 * OUTAGE_ALLOWANCE.as_secs_f64();
    media_window(target_kbps, rtt).max(outage as u64)
}

/// Cola a partir de la cual el emisor reduce el bitrate.
pub const QUEUE_THRESHOLD: Duration = Duration::from_millis(10);

/// Cola que dos informes seguidos bastan para dar por segura ([`RateController::queue_delay_now`]):
/// recorta sin esperar a [`QUEUE_WINDOW`]. Cinco veces el umbral: el jitter de los perfiles de la
/// matriz de red (hasta ± 20 ms) no la alcanza ni en un informe.
pub const FAST_QUEUE: Duration = Duration::from_millis(50);

/// Ventana del mínimo de OWD (retardo base del camino).
const BASE_WINDOW: Duration = Duration::from_secs(10);

/// Ventana corta del mínimo de OWD con la que se estima la cola (filtra el jitter).
pub const QUEUE_WINDOW: Duration = Duration::from_millis(500);

/// Ventana en la que se miden lo enviado y lo recibido ([`RateController::delivered_kbps`]).
pub const RATE_WINDOW: Duration = Duration::from_millis(500);

/// Tiempo mínimo que tiene que cubrir [`RATE_WINDOW`] para que sus tasas valgan (con menos, un
/// informe adelantado o retrasado por el jitter las desvía a la mitad o al doble).
const MIN_RATE_SPAN: Duration = Duration::from_millis(200);

/// Tope del crecimiento sobre lo recibido: la estimación por retardo no pasa de este factor por lo
/// que el receptor recibió en [`RATE_WINDOW`]. Con el objetivo en uso y sin pérdida, lo recibido
/// en la ventana queda algo por debajo del objetivo (que ya subió mientras tanto) y el tope deja
/// pasar el +5 %; con el codificador mandando menos del ~80 % del objetivo, lo frena.
pub const DELIVERED_HEADROOM: f64 = 1.25;

/// Parte del objetivo que tiene que estar enviando el emisor para que un recorte por cola baje
/// también por debajo de lo recibido: si mandaba mucho menos (pantalla quieta), lo recibido no
/// dice nada del enlace y solo vale el recorte proporcional.
pub const UTILIZATION: f64 = 0.75;

/// Al recortar por cola con el objetivo en uso, la estimación baja al menos hasta esta parte de lo
/// recibido: lo que llegaba ya era más de lo que el camino pasaba sin hacer cola.
pub const DELIVERED_CUT: f64 = 0.90;

/// Tras un recorte por cola o ECN no se crece durante este tiempo (y, además, hasta que la cola
/// medida baje de la mitad del umbral): lo recortado tiene que vaciar la cola antes de volver a
/// probar.
pub const HOLD_AFTER_CUT: Duration = Duration::from_secs(1);

/// Parte de lo que midió la sonda del enlace ([`RateController::restart`]) con la que arranca el
/// objetivo: lo que la sonda mide es el cuello del camino con una ráfaga corta; el resto lo deja al
/// control de tasa, que sube un 5 % cada 100 ms si no hay cola.
pub const PROBE_START_SHARE: f64 = 0.7;

/// Pérdida por debajo de la cual se puede crecer (GCC).
pub const LOSS_GROW: f64 = 0.02;

/// Pérdida por encima de la cual se reduce (GCC).
pub const LOSS_CUT: f64 = 0.10;

/// Periodo de los incrementos y de las reducciones por cola o ECN.
const ADJUST_PERIOD: Duration = Duration::from_millis(100);

/// Periodo mínimo entre reducciones por pérdida.
const LOSS_PERIOD: Duration = Duration::from_millis(300);

/// Ventana de la pérdida: la fracción es la de los objetos de los informes del último segundo (como
/// el intervalo de RTCP de GCC). Un informe de 50 ms lleva ~10 objetos a 30 fps: con un 5 % de
/// pérdida, uno perdido ya es un 10 %, y decidir con eso recortaba al azar.
pub const LOSS_WINDOW: Duration = Duration::from_secs(1);

/// Informes en [`LOSS_WINDOW`] (y en [`RATE_WINDOW`]) como mucho.
const MAX_LOSS_REPORTS: usize = 256;

/// Histéresis de la salida.
const OUTPUT_PERIOD: Duration = Duration::from_secs(1);
const OUTPUT_STEP: f64 = 0.10;

/// Cuánto baja el techo de ráfaga con cada recorte por pérdida (del ritmo al que salían).
pub const BURST_CUT: f64 = 0.85;

/// Tiempo mínimo entre recortes del techo de ráfaga (lo que tarda en verse el efecto en la ventana
/// de pérdida) y paso de su subida.
pub const BURST_HOLD: Duration = Duration::from_secs(1);

/// Lo que sube el techo de ráfaga por paso sin pérdida.
pub const BURST_GROW: f64 = 0.05;

/// Parte del techo de ráfaga a la que se acota el objetivo: un fotograma medio cabe en el 80 % del
/// intervalo y queda margen para la reparación y el audio.
pub const BURST_TARGET_SHARE: f64 = 0.8;

/// Límites del bitrate objetivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateConfig {
    /// Mínimo, en kbit/s.
    pub min_kbps: u32,
    /// Máximo, en kbit/s (el del usuario).
    pub max_kbps: u32,
    /// Valor inicial, en kbit/s.
    pub start_kbps: u32,
}

impl Default for RateConfig {
    /// 1080p60 AV1/HEVC "buena" de `docs/ARQUITECTURA.md` §2.1: arranca en 8 Mbit/s, entre 1 y 50.
    fn default() -> Self {
        Self {
            min_kbps: 1_000,
            max_kbps: 50_000,
            start_kbps: 8_000,
        }
    }
}

/// Datos de un `Feedback` ya traducidos por el emisor.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateInput<'a> {
    /// Retardos de ida en µs (con un desfase constante desconocido: solo importa su variación).
    pub owd_us: &'a [i64],
    /// Fracción de objetos de origen perdidos en la red desde el último informe (0..1).
    pub loss_fraction: f64,
    /// Objetos de origen que cubre `loss_fraction` (su peso en [`LOSS_WINDOW`]); con 0, el
    /// informe no dice nada de la pérdida.
    pub sent_objects: u64,
    /// Marcas ECN-CE nuevas desde el último informe.
    pub ecn_ce: u32,
    /// Bytes de medios que el receptor recibió desde el informe anterior (cabeceras y reparación
    /// incluidas).
    pub received_bytes: u64,
    /// Bytes de medios que el emisor envió entre lo que cubría el informe anterior y este.
    pub sent_bytes: u64,
}

/// Un recorte del techo de ráfaga pendiente de comprobar ([`RateController`]).
#[derive(Debug, Clone, Copy)]
struct BurstCut {
    /// Pérdida antes del recorte y objetos con que se midió.
    loss: f64,
    objects: u64,
    /// Proporción del ritmo que deja el recorte (techo nuevo / ritmo de antes).
    ratio: f64,
    /// Objetos perdidos y enviados desde el recorte: lo que lo comprueba.
    lost_since: f64,
    sent_since: u64,
}

/// Lo que había antes de que los recortes del techo empezaran a fallar ([`RateController`]): a lo
/// que se vuelve si la pérdida resulta propia del enlace.
#[derive(Debug, Clone, Copy)]
struct BurstSnapshot {
    /// Objetos perdidos y enviados desde que empezó: la pérdida media de la serie.
    lost: f64,
    sent: u64,
    cap: Option<f64>,
    estimate: f64,
    loss_estimate: f64,
}

/// Puntuación de comprobaciones de recorte fallidas (+1 cada fallo, −½ cada acierto) a la que la
/// pérdida se da por propia del enlace: un fallo suelto puede ser un intra o la cola que aún se
/// vacía, y una pérdida aleatoria pequeña a veces mide menos de un punto durante un segundo.
pub const BURST_CHECKS: f64 = 2.0;

/// Control de tasa tipo SCReAM del emisor.
#[derive(Debug, Clone)]
pub struct RateController {
    config: RateConfig,
    /// Estimación por retardo (cola y ECN), la de SCReAM.
    estimate: f64,
    /// Estimación por pérdida, la de GCC: el objetivo es el mínimo de las dos.
    loss_estimate: f64,
    /// Informes de [`LOSS_WINDOW`]: instante, objetos perdidos y objetos enviados.
    loss_window: VecDeque<(Instant, f64, u64)>,
    /// Informes de [`RATE_WINDOW`]: instante, bytes recibidos y bytes enviados desde el anterior.
    throughput: VecDeque<(Instant, u64, u64)>,
    output: u32,
    last_output_change: Option<Instant>,
    base: VecDeque<(Instant, i64)>,
    recent: VecDeque<(Instant, i64)>,
    queue_ms: f64,
    previous_queue_ms: f64,
    /// Cola del último informe sin filtrar (su mínimo sobre el retardo base) y la del anterior.
    instant_queue_ms: f64,
    previous_instant_queue_ms: f64,
    loss: f64,
    last_adjust: Option<Instant>,
    last_loss_cut: Option<Instant>,
    last_loss_grow: Option<Instant>,
    /// Hasta cuándo no se crece tras un recorte ([`HOLD_AFTER_CUT`]).
    hold_until: Option<Instant>,
    /// Techo de ráfaga en kbit/s (ver el módulo), si lo hay.
    burst_cap: Option<f64>,
    /// Ritmo natural del pacing (sin el techo) que informó el emisor, en kbit/s.
    natural_pace: f64,
    /// Último recorte del techo de ráfaga.
    burst_cut_at: Option<Instant>,
    /// Último paso de subida del techo.
    burst_grow_at: Option<Instant>,
    /// El último recorte del techo, para comprobarlo: la pérdida de antes y lo que había antes
    /// (techo y estimaciones), por si hay que deshacerlo.
    last_cut: Option<BurstCut>,
    /// Puntuación de fallos ([`BURST_CHECKS`]) y lo que había cuando empezó a subir.
    burst_failures: f64,
    burst_snapshot: Option<BurstSnapshot>,
    /// Objetos que cubre la pérdida de la ventana ([`RateController::loss`]).
    loss_objects: u64,
    /// Pérdida propia del enlace (aleatoria, no depende del ritmo), que no recorta.
    inherent_loss: f64,
}

impl RateController {
    /// Controlador que arranca en `config.start_kbps` (acotado a `[min, max]`).
    pub fn new(config: RateConfig) -> Self {
        let max = config.max_kbps.max(config.min_kbps);
        let config = RateConfig {
            max_kbps: max,
            start_kbps: config.start_kbps.clamp(config.min_kbps, max),
            ..config
        };
        Self {
            config,
            estimate: f64::from(config.start_kbps),
            loss_estimate: f64::from(config.start_kbps),
            loss_window: VecDeque::new(),
            throughput: VecDeque::new(),
            output: config.start_kbps,
            last_output_change: None,
            base: VecDeque::new(),
            recent: VecDeque::new(),
            queue_ms: 0.0,
            previous_queue_ms: 0.0,
            instant_queue_ms: 0.0,
            previous_instant_queue_ms: 0.0,
            loss: 0.0,
            last_adjust: None,
            last_loss_cut: None,
            last_loss_grow: None,
            hold_until: None,
            burst_cap: None,
            natural_pace: 0.0,
            burst_cut_at: None,
            burst_grow_at: None,
            last_cut: None,
            burst_failures: 0.0,
            burst_snapshot: None,
            loss_objects: 0,
            inherent_loss: 0.0,
        }
    }

    /// El emisor informa del ritmo natural de su pacing (sin el techo de ráfaga), en kbit/s: es la
    /// referencia del primer recorte del techo.
    pub fn note_pace(&mut self, kbps: u32) {
        self.natural_pace = f64::from(kbps);
    }

    /// Techo de ráfaga en kbit/s, si el enlace ha mostrado pérdida por desbordarlo (ver el módulo).
    pub fn burst_cap_kbps(&self) -> Option<u32> {
        self.burst_cap.map(|cap| cap.round().max(1.0) as u32)
    }

    /// Pérdida propia del enlace que se ha dado por aleatoria (0..1).
    pub fn inherent_loss(&self) -> f64 {
        self.inherent_loss
    }

    /// Techo de ráfaga: recorta mientras haya pérdida que depende del ritmo, deshace el recorte que
    /// no la baja y lo sube sin ella.
    fn update_burst(&mut self, now: Instant, utilized: bool) {
        let since = |at: Option<Instant>| at.map(|at| now.saturating_duration_since(at));
        if since(self.burst_cut_at).is_some_and(|elapsed| elapsed < BURST_HOLD) {
            return;
        }
        // Tras un recorte cuenta solo la pérdida de después: la de la ventana aún arrastra la de
        // antes.
        let (loss, objects) = match &self.last_cut {
            Some(cut) if cut.sent_since > 0 => {
                (cut.lost_since / cut.sent_since as f64, cut.sent_since)
            }
            _ => (self.loss, self.loss_objects),
        };
        // Lo que pasa de la pérdida propia del enlace, menos el ruido de medirla con `objects`
        // objetos (dos desviaciones típicas de una binomial): con pocos objetos por segundo, una
        // pérdida aleatoria del 2 % mide un 4 % muchos segundos.
        let noise =
            2.0 * (self.inherent_loss * (1.0 - self.inherent_loss) / objects.max(1) as f64).sqrt();
        let excess = loss - self.inherent_loss - noise;
        // Basta la mitad de [`LOSS_GROW`]: entre el 1 y el 2 % GCC aún sube el objetivo, y un
        // limitador que tira un 1-2 % de los paquetes tira varios fotogramas por segundo.
        let cut_at = LOSS_GROW / 2.0;
        if excess > cut_at && utilized {
            if let Some(cut) = self.last_cut.take() {
                // Cumple si la pérdida se fue (lo que predice desbordar el enlace, casi siempre
                // nada, más un punto) o si bajó de forma significativa (más de dos desviaciones
                // típicas de la diferencia de dos proporciones: un recorte que aún no basta).
                let predicted = (1.0 - (1.0 - cut.loss) / cut.ratio).max(0.0);
                let pooled = (cut.loss + loss) / 2.0;
                let spread = 2.0
                    * (pooled
                        * (1.0 - pooled)
                        * (1.0 / cut.objects.max(1) as f64 + 1.0 / objects.max(1) as f64))
                        .sqrt();
                let dropped = cut.loss - loss > spread;
                // Si subió no es aleatoria (esa no cambia): son los intras que la propia pérdida
                // hace pedir, o el contenido que pesa más. Ni cumple ni falla: se recorta otra vez.
                let rose = loss - cut.loss > spread;
                if excess > predicted + LOSS_GROW / 2.0 && !dropped && !rose {
                    self.burst_failures += 1.0;
                    if self.burst_failures >= BURST_CHECKS {
                        // La de toda la serie: la de un segundo es ruidosa.
                        let mean = self
                            .burst_snapshot
                            .map_or(loss, |snapshot| snapshot.lost / snapshot.sent.max(1) as f64);
                        self.inherent_loss = self.inherent_loss.max(mean.max(loss).min(LOSS_CUT));
                        if let Some(snapshot) = self.burst_snapshot.take() {
                            self.burst_cap = snapshot.cap;
                            self.estimate = self.estimate.max(snapshot.estimate);
                            self.loss_estimate = self.loss_estimate.max(snapshot.loss_estimate);
                        }
                        self.burst_failures = 0.0;
                        self.burst_cut_at = Some(now);
                        return;
                    }
                } else {
                    self.note_burst_success();
                }
            }
            if self.burst_snapshot.is_none() {
                self.burst_snapshot = Some(BurstSnapshot {
                    lost: 0.0,
                    sent: 0,
                    cap: self.burst_cap,
                    estimate: self.estimate,
                    loss_estimate: self.loss_estimate,
                });
            }
            let natural = if self.natural_pace > 0.0 {
                self.natural_pace
            } else {
                self.estimate.min(self.loss_estimate) / BURST_TARGET_SHARE
            };
            let base = self.burst_cap.map_or(natural, |cap| cap.min(natural));
            // Lo que pasó del ritmo de antes es lo que cabe: el recorte va hasta ahí (con un 5 %
            // de margen), y al menos un [`BURST_CUT`].
            let ratio = BURST_CUT.min((1.0 - loss) * 0.95);
            let floor = f64::from(self.config.min_kbps) / BURST_TARGET_SHARE;
            let cap = (base * ratio).max(floor);
            self.last_cut = Some(BurstCut {
                loss,
                objects,
                ratio: (cap / base).min(1.0),
                lost_since: 0.0,
                sent_since: 0,
            });
            self.burst_cap = Some(cap);
            self.loss_estimate = self.loss_estimate.min(cap * BURST_TARGET_SHARE);
            self.estimate = self.estimate.min(cap * BURST_TARGET_SHARE);
            self.burst_cut_at = Some(now);
            self.burst_grow_at = Some(now);
            return;
        }
        if excess > LOSS_GROW / 2.0 {
            return;
        }
        // La pérdida se fue: el último recorte cumplió. La propia del enlace se olvida despacio
        // (media vida de unos 17 s) si ahora pierde menos de la mitad (un wifi que mejora).
        if self.last_cut.take().is_some() {
            self.note_burst_success();
        }
        if loss < self.inherent_loss / 2.0 {
            self.inherent_loss = self.inherent_loss * 0.998 + loss * 0.002;
        }
        let Some(cap) = self.burst_cap else {
            return;
        };
        // Sin pérdida: tras dos esperas, sube un paso por espera; muy por encima del ritmo natural,
        // ya no hace falta.
        if since(self.burst_cut_at).is_some_and(|elapsed| elapsed < BURST_HOLD * 2)
            || since(self.burst_grow_at).is_some_and(|elapsed| elapsed < BURST_HOLD)
        {
            return;
        }
        let grown = cap * (1.0 + BURST_GROW);
        self.burst_grow_at = Some(now);
        self.burst_cap = if self.natural_pace > 0.0 && grown >= self.natural_pace * 1.5 {
            None
        } else {
            Some(grown)
        };
    }

    /// Un recorte que bajó la pérdida: resta medio fallo y, sin fallos, olvida la instantánea.
    fn note_burst_success(&mut self) {
        self.burst_failures = (self.burst_failures - 0.5).max(0.0);
        if self.burst_failures == 0.0 {
            self.burst_snapshot = None;
        }
    }

    /// Límites en uso.
    pub fn config(&self) -> RateConfig {
        self.config
    }

    /// Cambia el máximo en marcha (el anfitrión baja o sube la calidad con la sala abierta): al
    /// bajarlo, el objetivo baja ya; al subirlo, crece desde donde está como sin cola.
    pub fn set_max_kbps(&mut self, max_kbps: u32) {
        let max = max_kbps.max(1);
        self.config.min_kbps = self.config.min_kbps.min(max);
        self.config.max_kbps = max;
        self.config.start_kbps = self.config.start_kbps.clamp(self.config.min_kbps, max);
        let cap = f64::from(max);
        self.estimate = self.estimate.min(cap);
        self.loss_estimate = self.loss_estimate.min(cap);
        self.output = self.output.clamp(self.config.min_kbps, max);
    }

    /// Vuelve a arrancar en `kbps` (acotado a `[min, max]`), sin histéresis: con lo que midió la
    /// sonda del enlace antes del primer fotograma, en lugar de `start_kbps`.
    pub fn restart(&mut self, kbps: u32) {
        let kbps = kbps.clamp(self.config.min_kbps, self.config.max_kbps);
        self.estimate = f64::from(kbps);
        self.loss_estimate = f64::from(kbps);
        self.output = kbps;
        self.last_output_change = None;
    }

    /// Bitrate objetivo para el codificador, con histéresis.
    pub fn target_kbps(&self) -> u32 {
        self.output
    }

    /// Estimación interna sin histéresis, en kbit/s.
    pub fn estimate_kbps(&self) -> f64 {
        self.estimate.min(self.loss_estimate)
    }

    /// Retardo de ida base (el mínimo de la ventana larga de OWD, en µs del reloj del emisor
    /// estimado por el receptor): sin cola, ~RTT/2 con el receptor. `None` sin muestras.
    pub fn base_owd_us(&self) -> Option<i64> {
        self.base.iter().map(|(_, owd)| *owd).min()
    }

    /// Cola estimada (suavizada).
    pub fn queue_delay(&self) -> Duration {
        Duration::from_secs_f64(self.queue_ms.max(0.0) / 1000.0)
    }

    /// Cola del último informe, sin filtrar: lo que su OWD mínimo supera al retardo base. Sube en
    /// cuanto el camino hace cola; con jitter puede marcar unos milisegundos que no son cola.
    pub fn queue_delay_now(&self) -> Duration {
        Duration::from_secs_f64(self.instant_queue_ms.max(0.0) / 1000.0)
    }

    /// Pérdida de [`LOSS_WINDOW`] (0..1).
    pub fn loss(&self) -> f64 {
        self.loss
    }

    /// Lo que el receptor recibió en los últimos [`RATE_WINDOW`], en kbit/s; `None` hasta que la
    /// ventana cubre [`MIN_RATE_SPAN`].
    pub fn delivered_kbps(&self) -> Option<f64> {
        self.window_rates().map(|(_, delivered)| delivered)
    }

    /// Lo que el emisor envió en los últimos [`RATE_WINDOW`], en kbit/s; `None` como
    /// [`RateController::delivered_kbps`].
    pub fn sent_kbps(&self) -> Option<f64> {
        self.window_rates().map(|(sent, _)| sent)
    }

    /// `(enviado, recibido)` en kbit/s sobre [`RATE_WINDOW`]. Cada informe trae lo que pasó desde
    /// el anterior, así que el primero de la ventana solo pone el instante inicial.
    fn window_rates(&self) -> Option<(f64, f64)> {
        let (&(first, _, _), &(last, _, _)) = (self.throughput.front()?, self.throughput.back()?);
        let span = last.saturating_duration_since(first);
        if span < MIN_RATE_SPAN {
            return None;
        }
        let (received, sent) = self
            .throughput
            .iter()
            .skip(1)
            .fold((0u64, 0u64), |(r, s), &(_, received, sent)| {
                (r + received, s + sent)
            });
        let kbps = |bytes: u64| bytes as f64 * 8.0 / 1000.0 / span.as_secs_f64();
        Some((kbps(sent), kbps(received)))
    }

    /// Procesa un informe y devuelve el bitrate objetivo resultante.
    pub fn update(&mut self, now: Instant, input: RateInput<'_>) -> u32 {
        self.update_queue(now, input.owd_us);
        self.update_loss(now, input.loss_fraction, input.sent_objects);
        self.update_throughput(now, input.received_bytes, input.sent_bytes);

        let threshold = QUEUE_THRESHOLD.as_secs_f64() * 1000.0;
        let fast_ms = FAST_QUEUE.as_secs_f64() * 1000.0;
        let fast = self.instant_queue_ms > fast_ms && self.previous_instant_queue_ms > fast_ms;
        let growing = self.queue_ms > self.previous_queue_ms;
        let queue =
            fast || (self.queue_ms > threshold && (growing || self.queue_ms > 2.0 * threshold));
        let due = |last: Option<Instant>, period: Duration| {
            last.is_none_or(|at| now.saturating_duration_since(at) >= period)
        };
        let rates = self.window_rates();
        let delivered = rates.map(|(_, delivered)| delivered);
        // Con el objetivo en uso, lo recibido mide el enlace; si no, solo mide al codificador.
        let utilized = |target: f64| rates.is_none_or(|(sent, _)| sent >= UTILIZATION * target);
        // Lo más alto a lo que se puede subir `from`: [`DELIVERED_HEADROOM`] veces lo recibido, sin
        // bajar por ello de donde está.
        let burst_limit = self
            .burst_cap
            .map_or(f64::INFINITY, |cap| cap * BURST_TARGET_SHARE);
        let grow_cap = |from: f64| {
            delivered
                .map_or(f64::INFINITY, |delivered| {
                    (delivered * DELIVERED_HEADROOM).max(from)
                })
                .min(burst_limit.max(from))
        };
        let held = self.hold_until.is_some_and(|until| now < until);

        // Retardo: la cola y ECN recortan; sin cola (y pasada la espera tras un recorte), crece.
        if queue {
            if due(self.last_adjust, ADJUST_PERIOD) {
                let queue_ms = if fast {
                    self.queue_ms
                        .max(self.instant_queue_ms.min(self.previous_instant_queue_ms))
                } else {
                    self.queue_ms
                };
                let excess = (queue_ms - threshold) / threshold;
                let cut = (0.05 + 0.10 * excess).clamp(0.05, 0.40);
                let mut next = self.estimate * (1.0 - cut);
                if utilized(self.estimate)
                    && let Some(delivered) = delivered
                {
                    next = next.min(delivered * DELIVERED_CUT);
                }
                self.estimate = next;
                self.last_adjust = Some(now);
                self.hold_until = Some(now + HOLD_AFTER_CUT);
            }
        } else if input.ecn_ce > 0 {
            if due(self.last_adjust, ADJUST_PERIOD) {
                self.estimate *= 0.90;
                self.last_adjust = Some(now);
                self.hold_until = Some(now + HOLD_AFTER_CUT);
            }
        } else if self.queue_ms < threshold / 2.0 && due(self.last_adjust, ADJUST_PERIOD) {
            if !held {
                self.estimate = (self.estimate * 1.05).min(grow_cap(self.estimate));
            }
            self.last_adjust = Some(now);
        }
        // El techo de ráfaga va antes: mientras comprueba un recorte suyo, la pérdida de antes del
        // recorte no vuelve a recortar.
        let in_use = utilized(self.estimate.min(self.loss_estimate));
        self.update_burst(now, in_use);
        let burst_pending = self.last_cut.is_some()
            && self
                .burst_cut_at
                .is_some_and(|at| now.saturating_duration_since(at) < BURST_HOLD);
        // Pérdida (GCC): por encima del 10 %, recorte desde el objetivo actual; por debajo del
        // 2 %, crece; entre medias se mantiene (y manda la estimación por retardo).
        if self.loss > LOSS_CUT && !burst_pending {
            if due(self.last_loss_cut, LOSS_PERIOD) {
                let current = self.estimate.min(self.loss_estimate);
                self.loss_estimate = current * (1.0 - 0.5 * self.loss);
                self.last_loss_cut = Some(now);
            }
        } else if self.loss < LOSS_GROW && due(self.last_loss_grow, ADJUST_PERIOD) {
            self.loss_estimate = (self.loss_estimate * 1.05).min(grow_cap(self.loss_estimate));
            self.last_loss_grow = Some(now);
        }
        let (min, max) = (
            f64::from(self.config.min_kbps),
            f64::from(self.config.max_kbps),
        );
        self.estimate = self.estimate.clamp(min, max);
        self.loss_estimate = self.loss_estimate.clamp(min, max);
        self.previous_queue_ms = self.queue_ms;
        self.update_output(now);
        self.output
    }

    fn update_loss(&mut self, now: Instant, fraction: f64, sent: u64) {
        if sent > 0 {
            let lost = fraction.clamp(0.0, 1.0) * sent as f64;
            self.loss_window.push_back((now, lost, sent));
            if let Some(cut) = &mut self.last_cut {
                cut.lost_since += lost;
                cut.sent_since += sent;
            }
            if let Some(snapshot) = &mut self.burst_snapshot {
                snapshot.lost += lost;
                snapshot.sent += sent;
            }
        }
        // Un informe cada 50 ms son 20 en la ventana; la cota es solo contra un par que inunde.
        while self.loss_window.len() > MAX_LOSS_REPORTS {
            self.loss_window.pop_front();
        }
        while self
            .loss_window
            .front()
            .is_some_and(|&(at, _, _)| now.saturating_duration_since(at) > LOSS_WINDOW)
        {
            self.loss_window.pop_front();
        }
        let (lost, sent) = self
            .loss_window
            .iter()
            .fold((0.0, 0u64), |(lost, sent), &(_, l, s)| (lost + l, sent + s));
        if sent > 0 {
            self.loss = lost / sent as f64;
        }
        self.loss_objects = sent;
    }

    fn update_throughput(&mut self, now: Instant, received: u64, sent: u64) {
        self.throughput.push_back((now, received, sent));
        while self.throughput.len() > MAX_LOSS_REPORTS {
            self.throughput.pop_front();
        }
        // Se conserva el primero fuera de la ventana: pone el instante inicial del intervalo que
        // cubre el siguiente.
        while self.throughput.len() > 1
            && self
                .throughput
                .get(1)
                .is_some_and(|&(at, _, _)| now.saturating_duration_since(at) > RATE_WINDOW)
        {
            self.throughput.pop_front();
        }
    }

    fn update_queue(&mut self, now: Instant, owd_us: &[i64]) {
        let Some(minimum) = owd_us.iter().copied().min() else {
            return;
        };
        for (window, keep) in [
            (&mut self.base, BASE_WINDOW),
            (&mut self.recent, QUEUE_WINDOW),
        ] {
            window.push_back((now, minimum));
            while window
                .front()
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) > keep)
            {
                window.pop_front();
            }
        }
        let floor = |window: &VecDeque<(Instant, i64)>| {
            window.iter().map(|(_, owd)| *owd).min().unwrap_or(minimum)
        };
        let base = floor(&self.base);
        let queue_ms = (floor(&self.recent) - base).max(0) as f64 / 1000.0;
        self.queue_ms = 0.7 * self.queue_ms + 0.3 * queue_ms;
        self.previous_instant_queue_ms = self.instant_queue_ms;
        self.instant_queue_ms = (minimum - base).max(0) as f64 / 1000.0;
    }

    fn update_output(&mut self, now: Instant) {
        let candidate = self.estimate_kbps().round() as u32;
        if candidate == self.output {
            return;
        }
        let change =
            (f64::from(candidate) - f64::from(self.output)).abs() / f64::from(self.output.max(1));
        let elapsed = self
            .last_output_change
            .is_none_or(|at| now.saturating_duration_since(at) >= OUTPUT_PERIOD);
        if change >= OUTPUT_STEP || elapsed {
            self.output = candidate;
            self.last_output_change = Some(now);
        }
    }
}

/// Estado compartido entre una conexión y su controlador de congestión.
#[derive(Debug)]
pub struct CongestionShared {
    target_kbps: AtomicU32,
    ecn_events: AtomicU64,
    loss_events: AtomicU64,
    window: AtomicU64,
}

impl CongestionShared {
    /// Estado con un bitrate objetivo inicial.
    pub fn new(target_kbps: u32) -> Arc<Self> {
        Arc::new(Self {
            target_kbps: AtomicU32::new(target_kbps),
            ecn_events: AtomicU64::new(0),
            loss_events: AtomicU64::new(0),
            window: AtomicU64::new(MIN_WINDOW),
        })
    }

    /// Fija el bitrate objetivo del que depende la ventana.
    pub fn set_target_kbps(&self, kbps: u32) {
        self.target_kbps.store(kbps, Ordering::Relaxed);
    }

    /// Bitrate objetivo actual.
    pub fn target_kbps(&self) -> u32 {
        self.target_kbps.load(Ordering::Relaxed)
    }

    /// Eventos de congestión por ECN-CE notificados por `quinn` (acumulado).
    pub fn ecn_events(&self) -> u64 {
        self.ecn_events.load(Ordering::Relaxed)
    }

    /// Eventos de congestión por pérdida notificados por `quinn` (acumulado).
    pub fn loss_events(&self) -> u64 {
        self.loss_events.load(Ordering::Relaxed)
    }

    /// Última ventana calculada, en bytes.
    pub fn window(&self) -> u64 {
        self.window.load(Ordering::Relaxed)
    }
}

/// Retraso de los ACK que se suma al RTT en la ventana: el `max_ack_delay` por defecto de QUIC
/// (RFC 9000 §18.2). Los bytes siguen en vuelo hasta que llega su ACK, y el RTT que da `quinn` ya
/// descuenta ese retraso: con una ventana de `target × RTT` en una LAN (1 ms) cualquier ACK
/// retrasado (un receptor ocupado, un temporizador de Windows) dejaba la conexión limitada a la
/// ventana mínima por cada espera del ACK (64 KB cada 25 ms son ~20 Mbit/s).
pub const ACK_DELAY_ALLOWANCE: Duration = Duration::from_millis(25);

/// Ventana del controlador de medios para un bitrate y un RTT:
/// `target × (RTT + ACK_DELAY_ALLOWANCE) × WINDOW_RTT_FACTOR`, nunca menos de [`MIN_WINDOW`].
pub fn media_window(target_kbps: u32, rtt: Duration) -> u64 {
    let bytes_per_second = u64::from(target_kbps) * 1000 / 8;
    let bdp = bytes_per_second as f64 * (rtt + ACK_DELAY_ALLOWANCE).as_secs_f64();
    ((bdp * WINDOW_RTT_FACTOR) as u64).max(MIN_WINDOW)
}

/// Controlador de congestión de `quinn` que no frena los medios por debajo de su bitrate objetivo.
#[derive(Debug, Clone)]
pub struct MediaController {
    shared: Arc<CongestionShared>,
    rtt: Duration,
}

impl MediaController {
    /// Estado compartido de este controlador.
    pub fn shared(&self) -> &Arc<CongestionShared> {
        &self.shared
    }
}

impl Controller for MediaController {
    fn on_ack(
        &mut self,
        _now: Instant,
        _sent: Instant,
        _bytes: u64,
        _app_limited: bool,
        rtt: &RttEstimator,
    ) {
        self.rtt = rtt.get();
    }

    fn on_congestion_event(
        &mut self,
        _now: Instant,
        _sent: Instant,
        _is_persistent_congestion: bool,
        lost_bytes: u64,
    ) {
        // `quinn` notifica las marcas CE de los ACK como congestión sin bytes perdidos.
        if lost_bytes == 0 {
            self.shared.ecn_events.fetch_add(1, Ordering::Relaxed);
        } else {
            self.shared.loss_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn on_mtu_update(&mut self, _new_mtu: u16) {}

    fn window(&self) -> u64 {
        let window = controller_window(self.shared.target_kbps(), self.rtt);
        self.shared.window.store(window, Ordering::Relaxed);
        window
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }

    fn initial_window(&self) -> u64 {
        controller_window(self.shared.target_kbps(), self.rtt)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// Construye un [`MediaController`] por camino de la conexión, todos con el mismo estado
/// compartido (también tras una migración de ruta).
#[derive(Debug)]
pub struct MediaControllerFactory {
    shared: Arc<CongestionShared>,
}

impl MediaControllerFactory {
    /// Fábrica atada a `shared`.
    pub fn new(shared: Arc<CongestionShared>) -> Arc<Self> {
        Arc::new(Self { shared })
    }
}

impl ControllerFactory for MediaControllerFactory {
    fn build(self: Arc<Self>, _now: Instant, _current_mtu: u16) -> Box<dyn Controller> {
        Box::new(MediaController {
            shared: Arc::clone(&self.shared),
            // RTT inicial de RFC 9002 hasta el primer ACK.
            rtt: Duration::from_millis(333),
        })
    }
}

/// Controlador de congestión de las conexiones de medios.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Congestion {
    /// [`MediaController`]: ventana `target × (RTT + 25 ms) × 1,5`, nunca < 64 KB (por defecto).
    #[default]
    Media,
    /// BBR de `quinn` (referencia del spike).
    Bbr,
    /// El controlador por defecto de `quinn` (CUBIC): para conexiones sin medios, como la
    /// señalización, que no deben depender del bitrate de vídeo.
    Standard,
}

impl Congestion {
    /// Fábrica de `quinn` para esta elección.
    pub(crate) fn factory(
        self,
        shared: &Arc<CongestionShared>,
    ) -> Arc<dyn ControllerFactory + Send + Sync> {
        match self {
            Congestion::Media => MediaControllerFactory::new(Arc::clone(shared)),
            Congestion::Bbr => Arc::new(BbrConfig::default()),
            Congestion::Standard => Arc::new(CubicConfig::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(50);

    /// Bytes que salen en un `TICK` a `kbps`.
    fn bytes_per_tick(kbps: u32) -> u64 {
        u64::from(kbps) * 1000 / 8 * TICK.as_millis() as u64 / 1000
    }

    /// Un informe por `TICK` con el emisor usando el objetivo: envía `usage` veces lo que marca
    /// el objetivo y el receptor recibe lo enviado menos la pérdida.
    fn run_with_usage(
        controller: &mut RateController,
        start: Instant,
        ticks: std::ops::Range<u32>,
        owd_ms: impl Fn(u32) -> i64,
        loss: f64,
        usage: f64,
    ) -> Vec<u32> {
        ticks
            .map(|tick| {
                let owd = owd_ms(tick) * 1000;
                let sent = (bytes_per_tick(controller.target_kbps()) as f64 * usage) as u64;
                let received = (sent as f64 * (1.0 - loss)) as u64;
                controller.update(
                    start + TICK * tick,
                    RateInput {
                        owd_us: &[owd, owd + 300, owd - 200],
                        loss_fraction: loss,
                        sent_objects: 10,
                        ecn_ce: 0,
                        received_bytes: received,
                        sent_bytes: sent,
                    },
                )
            })
            .collect()
    }

    fn run(
        controller: &mut RateController,
        start: Instant,
        ticks: std::ops::Range<u32>,
        owd_ms: impl Fn(u32) -> i64,
        loss: f64,
    ) -> Vec<u32> {
        run_with_usage(controller, start, ticks, owd_ms, loss, 1.0)
    }

    /// Con la pantalla quieta el codificador manda una fracción del objetivo: el objetivo no
    /// crece (antes subía sin tope y el primer intra salía a un bitrate que el camino no pasaba).
    #[test]
    fn the_target_does_not_grow_while_the_sender_barely_uses_it() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig {
            min_kbps: 1_000,
            max_kbps: 50_000,
            start_kbps: 8_000,
        });
        let idle = run_with_usage(&mut controller, start, 0..100, |_| 20, 0.0, 0.2);
        // Los dos primeros ajustes, antes de tener ventana, crecen a ciegas; después, nada.
        let blind = (8_000.0 * 1.05 * 1.05) as u32 + 1;
        assert!(
            idle.iter().all(|&kbps| kbps <= blind),
            "sin usar el objetivo no se crece: {idle:?}"
        );
        assert_eq!(idle[20], idle[99], "{idle:?}");
        // Vuelve el movimiento: se usa el objetivo y crece.
        let busy = run(&mut controller, start, 100..160, |_| 20, 0.0);
        assert!(*busy.last().expect("muestras") > 10_000, "{busy:?}");
    }

    /// El crecimiento no pasa de [`DELIVERED_HEADROOM`] veces lo que el receptor recibe aunque la
    /// pérdida esté entre los umbrales de GCC (mantener).
    #[test]
    fn growth_is_bounded_by_what_the_receiver_gets() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig {
            min_kbps: 1_000,
            max_kbps: 50_000,
            start_kbps: 8_000,
        });
        // Llega el 60 % de lo enviado sin que la pérdida medida llegue al 10 % que recorta.
        let mut peak = 0;
        for tick in 0..100u32 {
            let sent = bytes_per_tick(controller.target_kbps());
            let kbps = controller.update(
                start + TICK * tick,
                RateInput {
                    owd_us: &[20_000],
                    loss_fraction: 0.05,
                    sent_objects: 10,
                    ecn_ce: 0,
                    received_bytes: sent * 6 / 10,
                    sent_bytes: sent,
                },
            );
            peak = peak.max(kbps);
        }
        let delivered = controller.delivered_kbps().expect("hay ventana");
        assert!(
            f64::from(peak) <= 8_000.0 * 1.06,
            "no crece por encima de 1,25 × lo recibido: {peak} (recibido {delivered:.0})"
        );
        assert!((delivered - 0.6 * f64::from(controller.target_kbps())).abs() < 500.0);
    }

    /// Tras un recorte por cola no se vuelve a crecer hasta pasado [`HOLD_AFTER_CUT`], aunque la
    /// cola ya no se vea (lo recortado tiene que vaciarla).
    #[test]
    fn after_a_cut_the_target_holds_before_growing_again() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig {
            min_kbps: 1_000,
            max_kbps: 50_000,
            start_kbps: 10_000,
        });
        run(&mut controller, start, 0..19, |_| 20, 0.0);
        // Dos informes con 60 ms de cola: recorta en el segundo (vía rápida), sin esperar a los
        // 500 ms de la ventana.
        let before = controller.target_kbps();
        let cut = run(&mut controller, start, 19..21, |_| 80, 0.0);
        assert_eq!(cut[0], before, "un informe solo no basta");
        assert!(cut[1] < before, "recorta al segundo: {before} → {}", cut[1]);
        let floor = controller.estimate_kbps();
        // Sin cola durante HOLD_AFTER_CUT: no sube.
        let hold_ticks = (HOLD_AFTER_CUT.as_millis() / TICK.as_millis()) as u32;
        run(&mut controller, start, 21..20 + hold_ticks, |_| 20, 0.0);
        assert!(
            controller.estimate_kbps() <= floor + 1.0,
            "no crece durante la espera: {floor} → {}",
            controller.estimate_kbps()
        );
        // Pasada la espera, crece.
        run(
            &mut controller,
            start,
            20 + hold_ticks..40 + hold_ticks,
            |_| 20,
            0.0,
        );
        assert!(controller.estimate_kbps() > floor * 1.2);
    }

    #[test]
    fn restart_moves_both_estimates_within_the_limits() {
        let mut controller = RateController::new(RateConfig::default());
        controller.restart(30_000);
        assert_eq!(controller.target_kbps(), 30_000);
        assert_eq!(controller.estimate_kbps(), 30_000.0);
        controller.restart(90_000);
        assert_eq!(controller.target_kbps(), 50_000, "no pasa del máximo");
        controller.restart(10);
        assert_eq!(controller.target_kbps(), 1_000, "ni baja del mínimo");
    }

    /// Las tasas de la ventana: lo enviado y lo recibido en kbit/s.
    #[test]
    fn the_rate_window_measures_sent_and_delivered() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig::default());
        assert_eq!(controller.delivered_kbps(), None, "sin ventana aún");
        for tick in 0..10u32 {
            controller.update(
                start + TICK * tick,
                RateInput {
                    owd_us: &[20_000],
                    sent_objects: 10,
                    // 50 ms × 8 Mbit/s = 50 000 B enviados; llegan 40 000.
                    received_bytes: 40_000,
                    sent_bytes: 50_000,
                    ..RateInput::default()
                },
            );
        }
        let sent = controller.sent_kbps().expect("ventana");
        let delivered = controller.delivered_kbps().expect("ventana");
        assert!((sent - 8_000.0).abs() < 1.0, "{sent}");
        assert!((delivered - 6_400.0).abs() < 1.0, "{delivered}");
    }

    #[test]
    fn the_maximum_changes_while_running() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig {
            min_kbps: 1_000,
            max_kbps: 10_000,
            start_kbps: 5_000,
        });
        let calm = run(&mut controller, start, 0..40, |_| 20, 0.0);
        let peak = *calm.last().expect("muestras");
        assert!(peak > 9_000, "llega cerca del máximo: {peak}");
        controller.set_max_kbps(4_000);
        assert!(controller.target_kbps() <= 4_000, "bajar el máximo baja ya");
        let lowered = run(&mut controller, start, 40..60, |_| 20, 0.0);
        assert!(lowered.iter().all(|&kbps| kbps <= 4_000), "{lowered:?}");
        controller.set_max_kbps(20_000);
        let raised = run(&mut controller, start, 60..120, |_| 20, 0.0);
        assert!(
            *raised.last().expect("muestras") > 10_000.max(peak),
            "subirlo deja crecer por encima del de antes: {raised:?}"
        );
        controller.set_max_kbps(500);
        assert_eq!(
            controller.config().min_kbps,
            500,
            "el mínimo no pasa del máximo"
        );
        assert_eq!(controller.target_kbps(), 500);
    }

    #[test]
    fn rising_owd_lowers_the_bitrate_and_it_recovers_afterwards() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig {
            min_kbps: 500,
            max_kbps: 20_000,
            start_kbps: 6_000,
        });
        // 2 s sin cola: crece.
        let calm = run(&mut controller, start, 0..40, |_| 20, 0.0);
        let peak = *calm.last().expect("muestras");
        assert!(peak > 6_000, "debe crecer sin cola: {peak}");
        // 1,5 s con OWD creciendo 2 ms por informe (60 ms de cola al final): baja.
        let congested = run(
            &mut controller,
            start,
            40..70,
            |t| 20 + 2 * i64::from(t - 39),
            0.0,
        );
        let bottom = *congested.last().expect("muestras");
        assert!(
            f64::from(bottom) < f64::from(peak) * 0.6,
            "debe bajar con la cola: {peak} → {bottom}"
        );
        // 5 s con la cola vaciada: se recupera.
        let recovered = run(&mut controller, start, 70..170, |_| 20, 0.0);
        let end = *recovered.last().expect("muestras");
        assert!(
            f64::from(end) > f64::from(bottom) * 1.5,
            "debe recuperarse: {bottom} → {end}"
        );
        assert!(end <= 20_000);
    }

    #[test]
    fn heavy_loss_without_queue_cuts_like_gcc() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig {
            min_kbps: 500,
            max_kbps: 20_000,
            start_kbps: 10_000,
        });
        // 20 % de pérdida: −10 % cada 300 ms (la pérdida suavizada tarda unos informes en
        // pasar del 10 %).
        let rates = run(&mut controller, start, 0..40, |_| 15, 0.20);
        assert!(*rates.last().expect("muestras") < 7_000, "{rates:?}");
        assert!(rates.iter().all(|&rate| rate >= 500));
    }

    /// Un enlace con policía (sin cola, 10 Mbit/s) y un emisor que manda cada fotograma en un 30 %
    /// del intervalo (ráfagas a 3,3× la media): el techo de ráfaga baja hasta que las ráfagas caben,
    /// la pérdida desaparece en un segundo y el bitrate se queda cerca de lo que cabe.
    #[test]
    fn policed_link_converges_with_the_burst_cap() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig {
            min_kbps: 500,
            max_kbps: 50_000,
            start_kbps: 6_000,
        });
        let link = 10_000.0;
        let mut target = 6_000;
        let mut lost_after_5s = 0.0;
        for tick in 0..400u32 {
            let natural = f64::from(target) / 0.3;
            controller.note_pace(natural as u32);
            let pace = controller
                .burst_cap_kbps()
                .map_or(natural, |cap| natural.min(f64::from(cap)));
            let loss = (1.0 - link / pace).max(0.0);
            // Lo enviado en 50 ms sigue al objetivo; lo recibido, lo que deja la pérdida.
            let sent = target * 50 / 8;
            target = controller.update(
                start + TICK * tick,
                RateInput {
                    owd_us: &[20_000],
                    loss_fraction: loss,
                    sent_objects: 10,
                    ecn_ce: 0,
                    received_bytes: (f64::from(sent) * (1.0 - loss)) as u64,
                    sent_bytes: u64::from(sent),
                },
            );
            if tick >= 100 {
                lost_after_5s += loss;
            }
        }
        // Tras 5 s solo pierde al tantear (el techo sube un 5 % y vuelve a recortar): menos del
        // 1 % de media.
        let mean = lost_after_5s / 300.0;
        assert!(mean < 0.01, "pérdida media tras 5 s: {mean:.4}");
        assert!(controller.burst_cap_kbps().is_some());
        assert!(
            (5_000..=10_000).contains(&target),
            "bitrate final {target} con un enlace de 10 Mbit/s"
        );
        assert!(controller.inherent_loss() < 0.01);
    }

    /// Matriz de red: con la pérdida aleatoria y el jitter de los perfiles `media`
    /// (2 %, 30 ± 10 ms) y `mala` (5 %, 60 ± 20 ms) y sin cola, el bitrate no se hunde: la pérdida de
    /// cada informe es ruidosa (10 objetos, cada uno perdido con probabilidad `p`), y una cola
    /// pasajera (1 s con 40 ms más, como un intra grande) recorta pero se recupera aunque la pérdida
    /// esté entre el 2 y el 10 % (ahí la pérdida mantiene y manda la estimación por retardo).
    #[test]
    fn random_loss_and_jitter_without_queue_hold_the_bitrate() {
        for (loss, jitter_ms) in [(0.02, 10i64), (0.05, 20)] {
            let start = Instant::now();
            let mut controller = RateController::new(RateConfig {
                min_kbps: 150,
                max_kbps: 1_500,
                start_kbps: 1_500,
            });
            // Generador determinista (xorshift).
            let mut seed = 0x5EED_u64;
            let mut next = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            };
            let mut lowest = u32::MAX;
            let mut last = 0;
            for tick in 0..600u32 {
                let queue = if (200..220).contains(&tick) {
                    40_000
                } else {
                    0
                };
                let mut jitter =
                    || (next() % (2 * jitter_ms as u64 * 1000 + 1)) as i64 - jitter_ms * 1000;
                let owd = [30_000 + queue + jitter(), 30_000 + queue + jitter()];
                let samples = if tick % 2 == 0 { &owd[..1] } else { &owd[..] };
                let lost = (0..10)
                    .filter(|_| (next() % 10_000) < (loss * 10_000.0) as u64)
                    .count();
                last = controller.update(
                    start + TICK * tick,
                    RateInput {
                        owd_us: samples,
                        loss_fraction: lost as f64 / 10.0,
                        sent_objects: 10,
                        ecn_ce: 0,
                        received_bytes: 10_000,
                        sent_bytes: 10_000,
                    },
                );
                if tick == 199 {
                    assert!(
                        last >= 1_200,
                        "{loss} de pérdida y ±{jitter_ms} ms: {last} kbit/s antes de la cola, \
                         pérdida medida {:.3}",
                        controller.loss()
                    );
                }
                lowest = lowest.min(last);
            }
            assert!(lowest < 1_200, "la cola recorta: {lowest}");
            assert!(
                last >= 1_200,
                "{loss} de pérdida y ±{jitter_ms} ms: {last} kbit/s al final, cola {:?}",
                controller.queue_delay()
            );
        }
    }

    /// Un informe suelto con mucha pérdida es ruido; la ventana de 1 s lo diluye.
    #[test]
    fn the_loss_is_measured_over_a_window_of_reports() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig::default());
        for tick in 0..20u32 {
            let lost = if tick == 10 { 0.3 } else { 0.0 };
            controller.update(
                start + TICK * tick,
                RateInput {
                    owd_us: &[20_000],
                    loss_fraction: lost,
                    sent_objects: 10,
                    ..RateInput::default()
                },
            );
        }
        // 3 objetos de 200 en la ventana.
        assert!(
            (controller.loss() - 0.015).abs() < 1e-9,
            "{}",
            controller.loss()
        );
        // Pasado 1 s sin pérdida, la ventana se vacía de ella.
        controller.update(
            start + TICK * 40,
            RateInput {
                owd_us: &[20_000],
                sent_objects: 10,
                ..RateInput::default()
            },
        );
        assert_eq!(controller.loss(), 0.0);
    }

    #[test]
    fn output_hysteresis_limits_small_changes_to_once_per_second() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig {
            min_kbps: 500,
            max_kbps: 100_000,
            start_kbps: 10_000,
        });
        let rates = run(&mut controller, start, 0..60, |_| 20, 0.0);
        let mut changes = Vec::new();
        for (tick, pair) in rates.windows(2).enumerate() {
            if pair[0] != pair[1] {
                changes.push((tick as u32 + 1, pair[0], pair[1]));
            }
        }
        for window in changes.windows(2) {
            let (t0, _, _) = window[0];
            let (t1, from, to) = window[1];
            let step = (f64::from(to) - f64::from(from)).abs() / f64::from(from);
            assert!(
                step >= OUTPUT_STEP || TICK * (t1 - t0) >= OUTPUT_PERIOD,
                "cambio pequeño antes de 1 s: {changes:?}"
            );
        }
        assert!(!changes.is_empty());
    }

    #[test]
    fn bitrate_stays_within_limits() {
        let start = Instant::now();
        let mut controller = RateController::new(RateConfig {
            min_kbps: 1_000,
            max_kbps: 4_000,
            start_kbps: 50_000,
        });
        assert_eq!(controller.target_kbps(), 4_000);
        let rates = run(
            &mut controller,
            start,
            0..400,
            |t| i64::from(t % 50) * 4,
            0.3,
        );
        assert!(rates.iter().all(|&rate| (1_000..=4_000).contains(&rate)));
    }

    #[test]
    fn media_window_follows_the_contract() {
        assert_eq!(media_window(1_000, Duration::from_millis(1)), MIN_WINDOW);
        // 8 Mbit/s = 1 MB/s; × (100 + 25) ms × 1,5 = 187,5 KB.
        assert_eq!(media_window(8_000, Duration::from_millis(100)), 187_500);
        // 80 Mbit/s en una LAN: el retraso de los ACK cuenta (10 MB/s × 26 ms × 1,5 = 390 KB).
        assert_eq!(media_window(80_000, Duration::from_millis(1)), 390_000);
        // La del controlador aguanta 2 s de corte al bitrate objetivo.
        assert_eq!(
            controller_window(80_000, Duration::from_millis(1)),
            20_000_000
        );
        assert_eq!(controller_window(100, Duration::from_millis(1)), MIN_WINDOW);
    }

    #[test]
    fn controller_counts_ecn_and_loss_events_separately() {
        let shared = CongestionShared::new(8_000);
        let factory = MediaControllerFactory::new(Arc::clone(&shared));
        let now = Instant::now();
        let mut controller = factory.build(now, 1200);
        controller.on_congestion_event(now, now, false, 0);
        controller.on_congestion_event(now, now, false, 1200);
        controller.on_congestion_event(now, now, false, 0);
        assert_eq!(shared.ecn_events(), 2);
        assert_eq!(shared.loss_events(), 1);
        // La ventana no se reduce por la congestión: la fija el bitrate objetivo.
        let before = controller.window();
        shared.set_target_kbps(80_000);
        assert!(controller.window() > before);
    }
}
