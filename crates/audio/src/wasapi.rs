//! Audio por **WASAPI** en Windows (feature `wasapi`) con la crate `wasapi` 0.26 (HEnquist, MIT,
//! mantenida: publicada en octubre de 2026, sobre `windows` 0.62 como el resto del proyecto).
//!
//! Todo en **modo compartido por eventos**. El micrófono y la reproducción se abren en el **modo
//! compartido de baja latencia** de `IAudioClient3` ([`open_shared`]): se pregunta al motor sus
//! periodos para el formato (`GetSharedModeEnginePeriod`) y se inicializa con el **mínimo**
//! (`InitializeSharedAudioStream`; 2-3 ms en vez de los 10 ms del periodo por defecto), primero
//! con nuestro `S16LE` a 48 kHz y, si el motor no lo acepta sin convertir (ese modo no admite
//! `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`), con su **formato de mezcla** (coma flotante de 32 bits
//! con los canales del dispositivo, casi siempre) y convertimos nosotros
//! ([`crate::convert::DeviceFormat`]; solo si mezcla a 48 kHz: no remuestreamos). Si tampoco (un
//! Windows sin `IAudioClient3`, una mezcla a 44,1 kHz), el modo compartido clásico de
//! `IAudioClient::Initialize` con conversión automática y el periodo por defecto, como antes. El
//! periodo obtenido se registra al abrir. El *loopback* (del dispositivo o por proceso) necesita
//! las banderas de `Initialize` y sigue en el modo clásico. Cada flujo vive en su hilo
//! (`wasapi-audio`), que inicializa COM (MTA), espera los eventos del dispositivo y copia entre su
//! búfer y el anillo compartido con el hilo de audio de la sesión (como el backend de PipeWire).
//!
//! La reproducción va **a demanda** por defecto ([`AudioSink::start_pull`]): en cada evento se
//! rellena el búfer del dispositivo hasta el nivel de [`crate::pacing::Pacing`] (un periodo del
//! motor, el que se obtuvo al abrir, más el margen que se mide en los propios eventos) pidiendo
//! las tramas a la sesión en ese momento ([`Pull`] reparte cada trama de 10 ms entre los periodos
//! que hagan falta). El motor no consume siempre 480 tramas por pasada (periodos de 128 o 144
//! tramas en baja latencia, mezcla a 44,1 kHz, periodos de 512 tramas o de 20 ms en USB y
//! Bluetooth, despertares tardíos que funden dos eventos): escribir una trama de 10 ms por evento
//! lo dejaba sin audio a trozos y el sonido era robótico.
//!
//! **Cuándo suena cada trama**: con la posición del dispositivo (`IAudioClock::GetPosition`,
//! convertida con `GetFrequency`), lo escrito y aún no reproducido es «tramas escritas − posición»,
//! que incluye el búfer y la latencia del motor hasta el dispositivo. Si el cliente no da reloj, el
//! relleno del búfer (`GetCurrentPadding`) más la latencia del flujo que dice el motor
//! (`IAudioClient::GetStreamLatency`); nunca una constante. **Cuándo se capturó**: la marca
//! QPC de cada paquete (`GetBuffer`), pasada al reloj local con una lectura del reloj del
//! dispositivo (`GetPosition` da también el QPC del momento en que se llama); sin ella, la llegada.
//!
//! - [`LoopbackSource`]: *loopback* del dispositivo de salida por defecto (lo que suena).
//! - [`ProcessLoopbackSource`]: **loopback por proceso** (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`
//!   por `ActivateAudioInterfaceAsync`, que hace `wasapi::AudioClient::new_application_loopback_client`):
//!   solo el audio del proceso `--audio process PID` y de sus hijos. Necesita Windows 10 20348 o
//!   posterior; [`process_loopback_status`] lo comprueba para `coroom doctor`.
//! - [`MicSource`] y [`PlaybackSink`]: el dispositivo por defecto o el que se nombre (`--mic`,
//!   `--speaker`, por su nombre visible). A demanda, cada trama sonará «ahora + el relleno del búfer
//!   del dispositivo» (`GetCurrentPadding`); con [`AudioSink::write`], además después de lo que
//!   haya en el anillo.
//!
//! En *loopback* WASAPI no da eventos mientras no suena nada: el anillo entrega silencio si en
//! 40 ms no llega audio (ver [`crate::ring`]). No se puede ejecutar en la CI de Linux; se comprueba
//! con `cargo check --target x86_64-pc-windows-msvc` y en Windows.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wasapi::{
    AudioCaptureClient, AudioClient, AudioClock, AudioRenderClient, DeviceEnumerator, Direction,
    Handle, SampleType, StreamMode, TimingMode, WaveFormat,
};

use crate::convert::{DeviceFormat, Sample as DeviceSample};
use crate::pacing::{Pacing, hns_to_frames};
use crate::ring::{Pull, Shared};
use crate::{
    AudioConfig, AudioError, AudioFormat, AudioSink, AudioSource, Captured, DeviceCounters,
    DeviceInfo, DeviceKind, Played, Render, SAMPLE_RATE_HZ, SAMPLES_PER_CHANNEL,
};

/// Espera máxima a que el hilo abra el dispositivo.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Duración del búfer compartido que se pide para capturar (50 ms, en unidades de 100 ns). No
/// añade latencia: en cada evento se leen todos los paquetes que haya, así que solo es el margen
/// para un despertar tardío del hilo. Con 10 ms (el periodo del motor), cualquier retraso del
/// hilo perdía audio (`AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`).
const CAPTURE_BUFFER_HNS: i64 = 500_000;

/// Duración del búfer que se pide para reproducir (50 ms): tiene que caber el nivel objetivo con
/// periodos de 20 ms. No añade latencia: se rellena solo hasta el objetivo.
const RENDER_BUFFER_HNS: i64 = 500_000;

fn backend(err: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(format!("WASAPI: {err}"))
}

/// Qué abrir.
#[derive(Debug, Clone)]
enum Endpoint {
    /// Loopback de un proceso y de todos sus descendientes. Windows no captura un proceso sin
    /// sus hijos: `new_application_loopback_client(pid, false)` no es «sin el árbol» sino «todo el
    /// sistema salvo ese árbol» (`PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`).
    ProcessTree(u32),
    /// Todo lo que suena salvo coroom (este proceso y sus hijos): las voces que reproduce la sala
    /// no vuelven como audio del juego. Windows 10 20348+; antes, toda la salida por defecto.
    AllButSelf,
    /// Captura de un micrófono.
    Capture(Option<String>),
    /// Reproducción.
    Render(Option<String>),
}

enum Io {
    Capture(AudioCaptureClient),
    Render(AudioRenderClient, Pacing),
}

/// Reloj del dispositivo (`IAudioClock`): posición en tramas a 48 kHz y el QPC del momento.
struct DeviceClock {
    clock: AudioClock,
    /// Unidades de posición por segundo (`GetFrequency`).
    frequency: u64,
}

impl DeviceClock {
    fn new(client: &AudioClient) -> Option<Self> {
        let clock = client.get_audioclock().ok()?;
        let frequency = clock.get_frequency().ok().filter(|f| *f > 0)?;
        Some(Self { clock, frequency })
    }

    /// Posición del dispositivo en tramas, el QPC (en unidades de 100 ns) en que se leyó y el
    /// instante local de la lectura (el medio de la llamada).
    fn read(&self) -> Option<(u64, u64, Instant)> {
        let before = Instant::now();
        let (position, qpc) = self.clock.get_position().ok()?;
        let after = Instant::now();
        let frames = u128::from(position) * u128::from(SAMPLE_RATE_HZ) / u128::from(self.frequency);
        Some((frames as u64, qpc, before + (after - before) / 2))
    }
}

/// Latencia del flujo que dice el motor (`IAudioClient::GetStreamLatency`), si la da.
#[allow(unsafe_code)] // `wasapi` no envuelve `GetStreamLatency`.
fn stream_latency(client: &AudioClient) -> Option<Duration> {
    // SAFETY: `as_iaudioclient` es la interfaz COM viva de `client`, ya inicializada (solo se llama
    // después de `initialize_client`); `GetStreamLatency` solo escribe su resultado.
    let hns = unsafe { client.as_iaudioclient().GetStreamLatency() }.ok()?;
    u64::try_from(hns)
        .ok()
        .filter(|hns| *hns > 0)
        .map(|hns| Duration::from_nanos(hns * 100))
}

/// Instante local de una marca QPC en unidades de 100 ns, con `reference` = (QPC, instante) de
/// una lectura del reloj del dispositivo.
fn qpc_to_instant(qpc: u64, reference: (u64, Instant)) -> Option<Instant> {
    let (ref_qpc, ref_at) = reference;
    if qpc <= ref_qpc {
        ref_at.checked_sub(Duration::from_nanos((ref_qpc - qpc).saturating_mul(100)))
    } else {
        ref_at.checked_add(Duration::from_nanos((qpc - ref_qpc).saturating_mul(100)))
    }
}

/// Tramas que se perdieron antes de un paquete de captura: con
/// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`, lo que salta su posición del dispositivo (`index`, en
/// tramas desde que arrancó el flujo) respecto a donde acabó el anterior (`expected`). Sin la
/// bandera, sin paquete anterior o sin salto hacia delante, ninguna.
fn capture_gap(expected: Option<u64>, index: u64, discontinuity: bool) -> Option<u64> {
    let expected = expected.filter(|_| discontinuity)?;
    index.checked_sub(expected).filter(|gap| *gap > 0)
}

/// Tramas a 48 kHz que dura `duration`.
fn duration_frames(duration: Duration) -> usize {
    (duration.as_micros() * u128::from(SAMPLE_RATE_HZ) / 1_000_000) as usize
}

/// Duración de `frames` tramas a 48 kHz.
fn frames_duration(frames: u64) -> Duration {
    Duration::from_micros(frames * 1_000_000 / u64::from(SAMPLE_RATE_HZ))
}

fn device(
    enumerator: &DeviceEnumerator,
    direction: Direction,
    name: Option<&str>,
) -> Result<wasapi::Device, AudioError> {
    match name {
        None => enumerator.get_default_device(&direction).map_err(backend),
        Some(name) => enumerator
            .get_device_collection(&direction)
            .map_err(backend)?
            .get_device_with_name(name)
            .map_err(|_| AudioError::NoSuchDevice(name.to_owned())),
    }
}

/// Nuestro formato en el cable: `S16LE` a 48 kHz con los canales de `format`.
fn pcm16_wave(format: AudioFormat) -> WaveFormat {
    WaveFormat::new(
        16,
        16,
        &SampleType::Int,
        SAMPLE_RATE_HZ as usize,
        usize::from(format.channels),
        None,
    )
}

/// Lo que devuelve [`open`]: el cliente inicializado y arrancado, su evento, el cliente de
/// captura o de reproducción (con su ritmo), las tramas de silencio escritas antes de arrancar y
/// el formato de las tramas del dispositivo (el nuestro, o el de mezcla si así se consiguió la
/// baja latencia).
struct Opened {
    client: AudioClient,
    handle: Handle,
    io: Io,
    prefilled: u64,
    device: DeviceFormat,
}

/// Abre el flujo.
fn open(endpoint: &Endpoint, format: AudioFormat) -> Result<Opened, AudioError> {
    let enumerator = DeviceEnumerator::new().map_err(backend)?;
    let wave = pcm16_wave(format);
    let ours = DeviceFormat::pcm16(usize::from(format.channels));
    let direction = match endpoint {
        Endpoint::Render(_) => Direction::Render,
        _ => Direction::Capture,
    };
    // El modo compartido clásico: `Initialize` con conversión automática y el periodo por defecto.
    let classic = |mut client: AudioClient| -> Result<AudioClient, AudioError> {
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: match direction {
                Direction::Render => RENDER_BUFFER_HNS,
                Direction::Capture => CAPTURE_BUFFER_HNS,
            },
        };
        client
            .initialize_client(&wave, &direction, &mode)
            .map_err(backend)?;
        Ok(client)
    };
    let (client, device, engine_period) = match endpoint {
        // El loopback (del proceso o del dispositivo) necesita la bandera de `Initialize`:
        // `InitializeSharedAudioStream` no lo admite, así que va en el modo clásico.
        Endpoint::AllButSelf => {
            // `false`: EXCLUDE_TARGET_PROCESS_TREE de este proceso.
            let client = match AudioClient::new_application_loopback_client(
                std::process::id(),
                false,
            ) {
                Ok(client) => client,
                Err(err) => {
                    tracing::warn!(
                        %err,
                        "sin loopback por proceso: se captura toda la salida, también las voces \
                         que reproduce coroom"
                    );
                    device(&enumerator, Direction::Render, None)?
                        .get_iaudioclient()
                        .map_err(backend)?
                }
            };
            (classic(client)?, ours, None)
        }
        Endpoint::ProcessTree(pid) => {
            // `true`: INCLUDE_TARGET_PROCESS_TREE (con `false` sería todo menos el proceso).
            let client = AudioClient::new_application_loopback_client(*pid, true).map_err(|err| {
                AudioError::Unsupported(format!(
                    "no se pudo abrir el loopback del proceso {pid} ({err}); hace falta Windows 10 \
                     20348 o posterior y que el proceso exista"
                ))
            })?;
            (classic(client)?, ours, None)
        }
        Endpoint::Capture(name) => open_shared(
            &device(&enumerator, Direction::Capture, name.as_deref())?,
            &wave,
            format,
            direction,
            classic,
        )?,
        Endpoint::Render(name) => open_shared(
            &device(&enumerator, Direction::Render, name.as_deref())?,
            &wave,
            format,
            direction,
            classic,
        )?,
    };
    let handle = client.set_get_eventhandle().map_err(backend)?;
    let mut prefilled = 0;
    let io = match direction {
        Direction::Render => {
            let render = client.get_audiorenderclient().map_err(backend)?;
            let mut pacing = render_pacing(&client, engine_period, device)?;
            // Un periodo de silencio antes de arrancar: la primera pasada del motor tiene qué leer
            // y el primer evento ya rellena hasta el objetivo.
            let prefill = pacing.prefill();
            let silence = vec![0u8; prefill * device.bytes_per_frame()];
            render
                .write_to_device(prefill, &silence, None)
                .map_err(backend)?;
            prefilled = prefill as u64;
            Io::Render(render, pacing)
        }
        Direction::Capture => {
            match engine_period {
                Some(period) => tracing::info!(
                    period_frames = period,
                    period_ms = f64::from(period) * 1000.0 / f64::from(SAMPLE_RATE_HZ),
                    ?device,
                    "captura WASAPI en modo compartido de baja latencia (IAudioClient3)"
                ),
                None => tracing::info!(?device, "captura WASAPI en modo compartido clásico"),
            }
            Io::Capture(client.get_audiocaptureclient().map_err(backend)?)
        }
    };
    client.start_stream().map_err(backend)?;
    Ok(Opened {
        client,
        handle,
        io,
        prefilled,
        device,
    })
}

/// Abre un flujo compartido en `device`: en el modo de baja latencia de `IAudioClient3` (el
/// periodo mínimo del motor, por eventos) con nuestro formato o, si el motor no lo admite sin
/// convertir, con su formato de mezcla (convertimos nosotros); si tampoco, en el modo compartido
/// clásico (`classic`) con la conversión automática del motor. Cada intento parte de un
/// `IAudioClient` nuevo: uno al que le falló `Initialize` no se reutiliza. Devuelve el cliente,
/// el formato de las tramas del dispositivo y el periodo del motor en tramas si hay baja latencia.
fn open_shared(
    device: &wasapi::Device,
    wave: &WaveFormat,
    format: AudioFormat,
    direction: Direction,
    classic: impl Fn(AudioClient) -> Result<AudioClient, AudioError>,
) -> Result<(AudioClient, DeviceFormat, Option<u32>), AudioError> {
    let ours = DeviceFormat::pcm16(usize::from(format.channels));
    match low_latency(device, wave) {
        Ok((client, period)) => return Ok((client, ours, Some(period))),
        Err(reason) => tracing::debug!(
            ?direction,
            %reason,
            "sin baja latencia con S16 a 48 kHz: se prueba el formato de mezcla"
        ),
    }
    let mix = device
        .get_iaudioclient()
        .map_err(|err| err.to_string())
        .and_then(|client| client.get_mixformat().map_err(|err| err.to_string()))
        .and_then(|mix| mix_device_format(&mix).map(|device| (mix, device)));
    match mix {
        Ok((mix, mix_device)) => match low_latency(device, &mix) {
            Ok((client, period)) => return Ok((client, mix_device, Some(period))),
            Err(reason) => tracing::debug!(
                ?direction,
                %reason,
                ?mix_device,
                "sin baja latencia con el formato de mezcla: modo compartido clásico"
            ),
        },
        Err(reason) => tracing::debug!(
            ?direction,
            %reason,
            "el formato de mezcla no se puede convertir aquí: modo compartido clásico"
        ),
    }
    let client = classic(device.get_iaudioclient().map_err(backend)?)?;
    Ok((client, ours, None))
}

/// Abre un `IAudioClient` nuevo de `device` en el modo compartido de baja latencia para `wave`
/// (`IAudioClient3`, por eventos): con el periodo mínimo del motor y, si otro proceso tiene el
/// periodo del motor fijado (`AUDCLNT_E_ENGINE_PERIODICITY_LOCKED`: un navegador o un cliente de
/// voz ya con baja latencia), con el periodo que el motor tiene ahora, que sigue siendo menor que
/// los 10 ms del modo clásico. Cada intento parte de un cliente nuevo (uno al que le falló
/// `Initialize` no se reutiliza). Devuelve el cliente y el periodo que quedó, en tramas.
fn low_latency(device: &wasapi::Device, wave: &WaveFormat) -> Result<(AudioClient, u32), String> {
    let mut client = device
        .get_iaudioclient()
        .map_err(|err| format!("IAudioClient: {err}"))?;
    let periods = client
        .get_shared_mode_engine_period(wave)
        .map_err(|err| format!("GetSharedModeEnginePeriod: {err}"))?;
    if periods.min == 0 {
        return Err("el motor no dice su periodo mínimo".to_owned());
    }
    let locked = client
        .get_current_shared_mode_engine_period()
        .ok()
        .map(|(_, period)| period)
        .filter(|period| *period > 0 && *period != periods.min);
    let mut attempts = vec![("mínimo", periods.min)];
    if let Some(current) = locked {
        attempts.push(("actual", current));
    }
    let mut failures = Vec::new();
    for (which, requested) in attempts {
        match client.initialize_shared_audio_stream(wave, TimingMode::Events, requested) {
            Ok(()) => {
                let current = client
                    .get_current_shared_mode_engine_period()
                    .ok()
                    .map(|(_, period)| period)
                    .filter(|period| *period > 0)
                    .unwrap_or(requested);
                tracing::debug!(
                    default = periods.default,
                    fundamental = periods.fundamental,
                    min = periods.min,
                    max = periods.max,
                    requested,
                    which,
                    period = current,
                    "periodos del motor de audio (tramas)"
                );
                return Ok((client, current));
            }
            Err(err) => {
                failures.push(format!(
                    "InitializeSharedAudioStream con {requested} tramas ({which}): {err}"
                ));
                client = device
                    .get_iaudioclient()
                    .map_err(|err| format!("IAudioClient: {err}"))?;
            }
        }
    }
    Err(failures.join("; "))
}

/// El formato de mezcla del motor como formato de las tramas del dispositivo, si podemos
/// convertirlo nosotros: a 48 kHz (no remuestreamos) con muestras en coma flotante de 32 bits o
/// enteras de 16 o 32 bits.
fn mix_device_format(mix: &WaveFormat) -> Result<DeviceFormat, String> {
    if mix.get_samplespersec() != SAMPLE_RATE_HZ {
        return Err(format!("la mezcla va a {} Hz", mix.get_samplespersec()));
    }
    let channels = usize::from(mix.get_nchannels());
    if channels == 0 {
        return Err("la mezcla no tiene canales".to_owned());
    }
    let kind = mix.get_subformat().map_err(|err| err.to_string())?;
    let sample = match (kind, mix.get_bitspersample()) {
        (SampleType::Float, 32) => DeviceSample::Float32,
        (SampleType::Int, 16) => DeviceSample::Int16,
        (SampleType::Int, 32) => DeviceSample::Int32 {
            valid_bits: mix.get_validbitspersample(),
        },
        (kind, bits) => return Err(format!("muestras {kind:?} de {bits} bits")),
    };
    Ok(DeviceFormat { sample, channels })
}

/// Búfer y periodo de un flujo de reproducción ya inicializado, en tramas a 48 kHz (las del
/// dispositivo coinciden con las nuestras: el formato de mezcla solo se usa a 48 kHz, y con la
/// conversión automática el búfer y el relleno se cuentan en nuestro formato). `engine_period` es
/// el periodo obtenido en baja latencia; sin él, el que dice `GetDevicePeriod`.
fn render_pacing(
    client: &AudioClient,
    engine_period: Option<u32>,
    device: DeviceFormat,
) -> Result<Pacing, AudioError> {
    let buffer = client.get_buffer_size().map_err(backend)? as usize;
    let (period_hns, min_period_hns) = client.get_device_period().unwrap_or((0, 0));
    let period = match engine_period {
        Some(frames) => frames as usize,
        None => hns_to_frames(period_hns),
    };
    let pacing = Pacing::new(buffer, period);
    let latency_hns = stream_latency(client).map_or(0, |latency| latency.as_nanos() / 100);
    tracing::info!(
        buffer_frames = buffer,
        period_frames = pacing.period,
        period_ms = pacing.period as f64 * 1000.0 / f64::from(SAMPLE_RATE_HZ),
        target_frames = pacing.target,
        low_latency = engine_period.is_some(),
        ?device,
        latency_hns,
        "salida WASAPI ({})",
        if engine_period.is_some() {
            "modo compartido de baja latencia, IAudioClient3"
        } else {
            "modo compartido clásico"
        }
    );
    match client.get_mixformat() {
        Ok(mix) => tracing::debug!(
            period_hns,
            min_period_hns,
            mix_rate_hz = mix.get_samplespersec(),
            mix_channels = mix.get_nchannels(),
            mix_bits = mix.get_bitspersample(),
            "formato de mezcla del motor"
        ),
        Err(err) => {
            tracing::debug!(period_hns, min_period_hns, error = %err, "sin formato de mezcla")
        }
    }
    Ok(pacing)
}

/// Cuerpo del hilo `wasapi-audio`.
fn run(
    endpoint: Endpoint,
    format: AudioFormat,
    shared: Arc<Shared>,
    pull: Option<Render>,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<(), AudioError>>,
) {
    // El hilo que mueve el dispositivo, con MMCSS «Pro Audio» como el motor de audio de Windows.
    match crate::realtime::promote_current_thread() {
        Ok(via) => tracing::debug!(via, "wasapi-audio en tiempo real"),
        Err(reason) => tracing::debug!(%reason, "wasapi-audio sin tiempo real"),
    }
    let _ = wasapi::initialize_mta();
    let Opened {
        client,
        handle,
        mut io,
        prefilled,
        device,
    } = match open(&endpoint, format) {
        Ok(opened) => opened,
        Err(err) => {
            shared.fail(err.clone());
            let _ = ready.send(Err(err));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let channels = usize::from(format.channels);
    // Las tramas del dispositivo (su formato) y las nuestras: la misma cuenta, a 48 kHz las dos.
    let bytes_per_frame = device.bytes_per_frame();
    // Búferes reservados una vez: 100 ms o el búfer del dispositivo, lo que sea más.
    let frames_max = match &io {
        Io::Render(_, pacing) => pacing.buffer.max(SAMPLES_PER_CHANNEL * 10),
        Io::Capture(_) => SAMPLES_PER_CHANNEL * 10,
    };
    let mut bytes = vec![0u8; frames_max * bytes_per_frame];
    let mut samples = vec![0i16; frames_max * channels];
    // Captura: lo leído del dispositivo, ya en nuestro formato.
    let mut decoded = Vec::with_capacity(frames_max * channels);
    let mut pull = pull.map(Pull::new);
    let clock = DeviceClock::new(&client);
    let latency = stream_latency(&client);
    // Tramas escritas al dispositivo desde que arrancó (con el silencio inicial).
    let mut written = prefilled;
    // Último evento atendido: lo que pasó desde entonces es lo que pudo llevarse el motor.
    let mut last_event = Instant::now();
    // El primer evento llega tras consumir el relleno inicial: vacío entonces no es una falta.
    let mut rendered = false;
    // Captura: posición del dispositivo (en tramas) que sigue al último paquete leído.
    let mut next_position: Option<u64> = None;
    while !stop.load(Ordering::Acquire) {
        let event = handle.wait_for_event(100);
        match &mut io {
            Io::Capture(capture) => {
                if event.is_err() {
                    continue;
                }
                // QPC ↔ reloj local, para fechar los paquetes con su marca del dispositivo.
                let reference = clock
                    .as_ref()
                    .and_then(DeviceClock::read)
                    .map(|(_, qpc, at)| (qpc, at));
                loop {
                    match capture.get_next_packet_size() {
                        Ok(Some(0)) | Ok(None) => break,
                        Ok(Some(_)) => {}
                        Err(err) => {
                            shared.fail(backend(err));
                            return;
                        }
                    }
                    match capture.read_from_device(&mut bytes) {
                        Ok((0, _)) => break,
                        Ok((frames, info)) => {
                            let len = frames as usize * bytes_per_frame;
                            let silent = info.flags.silent;
                            // Se perdió audio antes de este paquete (el hilo no leyó a tiempo):
                            // el hueco, medido por la posición del dispositivo, va en silencio.
                            if let Some(gap) = capture_gap(
                                next_position,
                                info.index,
                                info.flags.data_discontinuity,
                            ) {
                                tracing::debug!(gap_frames = gap, "captura WASAPI con un hueco");
                                shared.push_gap(format, gap);
                            }
                            next_position = Some(info.index.saturating_add(u64::from(frames)));
                            let captured_at = reference
                                .filter(|_| !info.flags.timestamp_error && info.timestamp > 0)
                                .and_then(|reference| qpc_to_instant(info.timestamp, reference));
                            device.decode(&bytes[..len], channels, &mut decoded);
                            shared.push(
                                format,
                                decoded
                                    .iter()
                                    .map(|&sample| if silent { 0 } else { sample }),
                                captured_at,
                            );
                        }
                        Err(err) => {
                            shared.fail(backend(err));
                            return;
                        }
                    }
                }
            }
            Io::Render(render, pacing) => {
                let padding = match client.get_current_padding() {
                    Ok(frames) => frames as usize,
                    Err(err) => {
                        shared.fail(backend(err));
                        return;
                    }
                };
                if padding == 0 && rendered {
                    // El motor se lo llevó todo: en su pasada siguiente sonaría silencio.
                    shared.note_underrun();
                }
                // Hasta el objetivo, no el búfer entero: llenarlo de más añadiría latencia. El
                // tiempo desde el evento anterior le dice al ritmo cuánto se llevó el motor si lo
                // vació.
                let now = Instant::now();
                let elapsed = duration_frames(now - last_event);
                last_event = now;
                let frames = pacing
                    .frames_to_write(padding, elapsed)
                    .min(samples.len() / channels);
                if frames == 0 {
                    continue;
                }
                let wanted = frames * channels;
                // Cuándo suena lo que se escribe ahora: lo escrito que el dispositivo aún no ha
                // reproducido, por su reloj; sin reloj, el relleno y la latencia del flujo.
                let ahead = |written: u64| match clock.as_ref().and_then(DeviceClock::read) {
                    Some((position, _, at)) => {
                        let pending = frames_duration(written.saturating_sub(position));
                        (at + pending).saturating_duration_since(Instant::now())
                    }
                    None => {
                        let queued = client
                            .get_current_padding()
                            .map_or(padding as u64, u64::from);
                        frames_duration(queued) + latency.unwrap_or_default()
                    }
                };
                match &mut pull {
                    Some(pull) => {
                        let delay = ahead(written);
                        pull.fill(&mut samples[..wanted], channels, delay);
                    }
                    None => shared.pop_into(&mut samples[..wanted]),
                }
                let len = device.encode(&samples[..wanted], channels, &mut bytes);
                if let Err(err) = render.write_to_device(frames, &bytes[..len], None) {
                    shared.fail(backend(err));
                    return;
                }
                written += frames as u64;
                rendered = true;
                // Con la cola (`write`): lo que se encole después sonará tras todo lo escrito.
                if pull.is_none() {
                    let us = ahead(written).as_micros() as u64;
                    shared.delay_us.store(us, Ordering::Relaxed);
                }
            }
        }
    }
    let _ = client.stop_stream();
}

/// Flujo en marcha en su hilo.
struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    fn start(
        endpoint: Endpoint,
        format: AudioFormat,
        shared: &Arc<Shared>,
        pull: Option<Render>,
    ) -> Result<Self, AudioError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = {
            let (stop, shared) = (Arc::clone(&stop), Arc::clone(shared));
            thread::Builder::new()
                .name("wasapi-audio".into())
                .spawn(move || run(endpoint, format, shared, pull, stop, ready_tx))
                .map_err(|err| backend(format!("no se pudo crear el hilo: {err}")))?
        };
        let mut running = Self {
            stop,
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => Ok(running),
            Ok(Err(err)) => {
                running.halt();
                Err(err)
            }
            Err(_) => {
                running.halt();
                Err(backend("el dispositivo no se abrió a tiempo"))
            }
        }
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Fuente de captura genérica sobre un [`Endpoint`].
struct Capture {
    endpoint: Endpoint,
    format: AudioFormat,
    shared: Option<Arc<Shared>>,
    running: Option<Running>,
}

impl Capture {
    fn new(endpoint: Endpoint, format: AudioFormat) -> Self {
        Self {
            endpoint,
            format,
            shared: None,
            running: None,
        }
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        if let (Endpoint::Capture(name), Some(device)) = (&mut self.endpoint, &config.device) {
            *name = Some(device.clone());
        }
        let shared = Shared::new(self.format);
        self.running = Some(Running::start(
            self.endpoint.clone(),
            self.format,
            &shared,
            None,
        )?);
        self.shared = Some(shared);
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        shared.read_frame(self.format, out)
    }

    fn stop(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.halt();
        }
        self.shared = None;
    }
}

macro_rules! capture_source {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $name(Capture);

        impl AudioSource for $name {
            fn format(&self) -> AudioFormat {
                self.0.format
            }

            fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
                self.0.start(config)
            }

            fn read(&mut self, out: &mut Vec<i16>) -> Result<Captured, AudioError> {
                self.0.read(out)
            }

            fn stop(&mut self) {
                self.0.stop();
            }

            fn describe(&self) -> String {
                match &self.0.endpoint {
                    Endpoint::AllButSelf => "todo lo que suena salvo coroom (WASAPI)".to_owned(),
                    Endpoint::ProcessTree(pid) => {
                        format!("loopback del proceso {pid} y sus hijos (WASAPI)")
                    }
                    Endpoint::Capture(Some(name)) => format!("micrófono «{name}» (WASAPI)"),
                    Endpoint::Capture(None) => "micrófono por defecto (WASAPI)".to_owned(),
                    Endpoint::Render(_) => "salida (WASAPI)".to_owned(),
                }
            }
        }
    };
}

capture_source!(LoopbackSource, "Loopback de la salida por defecto.");
capture_source!(
    ProcessLoopbackSource,
    "Loopback de un proceso y sus hijos (Windows 10 20348+)."
);
capture_source!(MicSource, "Micrófono.");

impl LoopbackSource {
    /// Lo que suena, salvo coroom (en Windows anterior a 10 20348, toda la salida por defecto).
    pub fn new() -> Self {
        Self(Capture::new(Endpoint::AllButSelf, AudioFormat::GAME))
    }
}

impl Default for LoopbackSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessLoopbackSource {
    /// El proceso `pid` y sus descendientes.
    pub fn new(pid: u32) -> Self {
        Self(Capture::new(Endpoint::ProcessTree(pid), AudioFormat::GAME))
    }
}

impl MicSource {
    /// Micrófono `device` por su nombre visible (`None`: el del sistema).
    pub fn new(device: Option<String>) -> Self {
        Self(Capture::new(Endpoint::Capture(device), AudioFormat::VOICE))
    }
}

/// Reproducción.
pub struct PlaybackSink {
    device: Option<String>,
    format: AudioFormat,
    shared: Option<Arc<Shared>>,
    running: Option<Running>,
    latency: Duration,
}

impl PlaybackSink {
    /// Salida `device` (`None`: la del sistema) en `format`.
    pub fn new(device: Option<String>, format: AudioFormat) -> Self {
        Self {
            device,
            format,
            shared: None,
            running: None,
            latency: crate::DEVICE_LATENCY,
        }
    }
}

impl AudioSink for PlaybackSink {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        self.latency = config.latency;
        let device = config.device.clone().or_else(|| self.device.clone());
        let shared = Shared::new(self.format);
        self.running = Some(Running::start(
            Endpoint::Render(device),
            self.format,
            &shared,
            None,
        )?);
        self.shared = Some(shared);
        Ok(())
    }

    fn supports_pull(&self) -> bool {
        true
    }

    fn start_pull(&mut self, config: &AudioConfig, render: Render) -> Result<(), AudioError> {
        self.stop();
        self.format = config.format;
        self.latency = config.latency;
        let device = config.device.clone().or_else(|| self.device.clone());
        let shared = Shared::new(self.format);
        self.running = Some(Running::start(
            Endpoint::Render(device),
            self.format,
            &shared,
            Some(render),
        )?);
        self.shared = Some(shared);
        Ok(())
    }

    fn check(&mut self) -> Result<(), AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        match shared.failure() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn device_counters(&self) -> DeviceCounters {
        self.shared
            .as_ref()
            .map(|shared| shared.counters())
            .unwrap_or_default()
    }

    fn wait_ready(&mut self) -> Result<(), AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        shared.wait_ready(self.format)
    }

    fn write(&mut self, samples: &[i16]) -> Result<Played, AudioError> {
        let shared = self.shared.as_ref().ok_or(AudioError::Stopped)?;
        shared.write_frame(self.format, samples, self.latency)
    }

    fn stop(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.halt();
        }
        self.shared = None;
    }

    fn describe(&self) -> String {
        match &self.device {
            Some(device) => format!("salida «{device}» (WASAPI)"),
            None => "salida por defecto (WASAPI)".to_owned(),
        }
    }
}

/// Dispositivos de entrada y salida.
pub fn list_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    let _ = wasapi::initialize_mta();
    let enumerator = DeviceEnumerator::new().map_err(backend)?;
    let mut out = Vec::new();
    for (direction, kind) in [
        (Direction::Render, DeviceKind::Sink),
        (Direction::Capture, DeviceKind::Source),
    ] {
        let default_id = enumerator
            .get_default_device(&direction)
            .and_then(|device| device.get_id())
            .ok();
        let collection = enumerator
            .get_device_collection(&direction)
            .map_err(backend)?;
        for device in &collection {
            let Ok(device) = device else {
                continue;
            };
            // El nombre amigable lleva el producto («Micrófono (Auriculares USB)»); la
            // descripción del dispositivo es solo la clase («Micrófono»), igual en todos.
            let name = device.get_friendlyname().unwrap_or_default();
            let id = device.get_id().ok();
            let description = if name.is_empty() {
                device.get_description().unwrap_or_default()
            } else {
                name.clone()
            };
            out.push(DeviceInfo {
                description,
                name,
                kind,
                default: id.is_some() && id == default_id,
            });
        }
    }
    Ok(out)
}

/// Si este Windows tiene loopback por proceso (se prueba activándolo para este mismo proceso).
pub fn process_loopback_status() -> String {
    let _ = wasapi::initialize_mta();
    match AudioClient::new_application_loopback_client(std::process::id(), false) {
        Ok(_) => "loopback por proceso: disponible (--audio process PID)".to_owned(),
        Err(err) => format!(
            "loopback por proceso: no disponible ({err}); hace falta Windows 10 20348 o posterior"
        ),
    }
}

/// Clave de aplicación del proceso `pid`: el nombre de su ejecutable en minúsculas
/// (`eldenring.exe`). La captura de ventanas calcula la misma para el proceso de la ventana, así
/// que audio y vídeo de una aplicación se emparejan por ella. `None` si el proceso no existe o no
/// se puede consultar.
pub fn app_key_for_pid(pid: u32) -> Option<String> {
    let path = process_image(pid)?;
    exe_file_name(&path).map(str::to_lowercase)
}

/// Nombre de fichero de una ruta de Windows (`C:\\Juegos\\X.exe` → `X.exe`).
fn exe_file_name(path: &str) -> Option<&str> {
    let name = path.rsplit(['\\', '/']).next()?.trim();
    (!name.is_empty()).then_some(name)
}

/// Nombre visible a partir de la clave: sin la extensión `.exe`.
fn exe_stem(key: &str) -> &str {
    key.len()
        .checked_sub(4)
        .filter(|&cut| key.is_char_boundary(cut) && key[cut..].eq_ignore_ascii_case(".exe"))
        .map_or(key, |cut| &key[..cut])
}

/// Nombre que se muestra: el de la sesión si lo puso la aplicación y no es una referencia a un
/// recurso (`@%SystemRoot%\\…,-123`); si no, el del ejecutable sin extensión.
fn display_name(session_name: &str, key: &str) -> String {
    let name = session_name.trim();
    if name.is_empty() || name.starts_with('@') {
        exe_stem(key).to_owned()
    } else {
        name.to_owned()
    }
}

/// Ruta completa del ejecutable del proceso `pid`.
#[allow(unsafe_code)] // Win32 sin envoltorio seguro; cada bloque lleva su `SAFETY:`.
fn process_image(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    // SAFETY: `OpenProcess` no toma punteros; el handle devuelto se cierra abajo en todos los
    // caminos.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = vec![0u16; 32_768];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` es válido y tiene `len` u16 de capacidad; la función escribe como mucho
    // `len` caracteres y deja en `len` los escritos (sin el nulo).
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: `process` es un handle abierto por `OpenProcess` y no se usa después.
    let _ = unsafe { CloseHandle(process) };
    queried.ok()?;
    let len = (len as usize).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// Sesiones de audio por aplicación de la salida por defecto; cada una se captura con el
/// loopback de su proceso.
pub(crate) struct AppSessions;

impl AppSessions {
    /// (clave, nombre, pid) de cada sesión viva de la salida por defecto, salvo la de sonidos del
    /// sistema y la de coroom.
    #[allow(unsafe_code)] // COM sin envoltorio en `wasapi` (`IsSystemSoundsSession`).
    fn sessions() -> Result<Vec<(String, String, String)>, AudioError> {
        use windows::Win32::Media::Audio::{
            AudioSessionStateExpired, IAudioSessionControl2, IAudioSessionManager2,
            IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
        };
        use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
        use windows::core::{HRESULT, Interface};

        // Hilo de la UI o `audio-apps`: COM en MTA (si ya estaba en STA, se usa ese).
        let _ = wasapi::initialize_mta();
        let own = std::process::id();
        // SAFETY: llamadas COM con COM inicializado en este hilo; las interfaces devueltas son
        // punteros contados que `windows` libera al soltarlos, y la cadena de `GetDisplayName`
        // se libera con `CoTaskMemFree` tras copiarla.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(backend)?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(backend)?;
            let manager: IAudioSessionManager2 =
                device.Activate(CLSCTX_ALL, None).map_err(backend)?;
            let sessions = manager.GetSessionEnumerator().map_err(backend)?;
            let count = sessions.GetCount().map_err(backend)?;
            let mut out = Vec::new();
            for index in 0..count {
                let Ok(control) = sessions.GetSession(index) else {
                    continue;
                };
                let Ok(control) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                // S_OK (0) solo para la sesión de sonidos del sistema; S_FALSE para el resto.
                if control.IsSystemSoundsSession() == HRESULT(0) {
                    continue;
                }
                let Ok(pid) = control.GetProcessId() else {
                    continue;
                };
                if pid == 0 || pid == own {
                    continue;
                }
                match control.GetState() {
                    Ok(state) if state != AudioSessionStateExpired => {}
                    _ => continue,
                }
                let Some(key) = app_key_for_pid(pid) else {
                    continue;
                };
                let session_name = match control.GetDisplayName() {
                    Ok(name) if !name.is_null() => {
                        let text = name.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(name.0 as *const _));
                        text
                    }
                    _ => String::new(),
                };
                let name = display_name(&session_name, &key);
                out.push((key, name, pid.to_string()));
            }
            Ok(out)
        }
    }
}

impl crate::apps::AppBackend for AppSessions {
    fn playing(&self) -> Result<Vec<crate::apps::PlayingApp>, AudioError> {
        // Sin loopback por proceso (antes de Windows 10 20348) no hay audio por aplicación: se
        // dice aquí para que la interfaz pase al audio de todo el sistema.
        static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let supported = *SUPPORTED.get_or_init(|| {
            let _ = wasapi::initialize_mta();
            AudioClient::new_application_loopback_client(std::process::id(), false).is_ok()
        });
        if !supported {
            return Err(AudioError::Unsupported(
                "el audio por aplicación necesita Windows 10 20348 o posterior".to_owned(),
            ));
        }
        Ok(crate::apps::group(Self::sessions()?))
    }

    fn open(&self, stream: &str, format: AudioFormat) -> Result<crate::apps::Tap, AudioError> {
        let pid: u32 = stream
            .parse()
            .map_err(|_| backend(format!("flujo de aplicación no válido: «{stream}»")))?;
        // Con su árbol (Windows no captura un proceso solo); `covering` ya quitó los procesos
        // cuyo ancestro también se captura, que si no saldrían dos veces.
        let shared = Shared::new(format);
        let running = Running::start(Endpoint::ProcessTree(pid), format, &shared, None)?;
        Ok(crate::apps::Tap {
            shared,
            _guard: Box::new(running),
        })
    }

    fn label(&self) -> &'static str {
        "WASAPI"
    }

    fn covering(&self, streams: Vec<String>) -> Vec<String> {
        let parents = parent_pids();
        let wanted: std::collections::HashSet<u32> =
            streams.iter().filter_map(|s| s.parse().ok()).collect();
        streams
            .into_iter()
            .filter(|stream| {
                stream
                    .parse::<u32>()
                    .map_or(true, |pid| !has_ancestor_in(pid, &parents, &wanted))
            })
            .collect()
    }
}

/// Padre de cada proceso vivo (`CreateToolhelp32Snapshot`). Vacío si no se puede consultar.
#[allow(unsafe_code)] // Win32 sin envoltorio seguro; cada bloque lleva su `SAFETY:`.
fn parent_pids() -> std::collections::HashMap<u32, u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut out = std::collections::HashMap::new();
    // SAFETY: sin punteros de entrada; el handle se cierra abajo.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return out;
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `entry` es local con `dwSize` puesto, como piden las dos funciones, que escriben en
    // él; `snapshot` es válido hasta `CloseHandle`.
    unsafe {
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            out.insert(entry.th32ProcessID, entry.th32ParentProcessID);
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

/// Si algún ancestro de `pid` (como mucho 32 niveles: los PID se reutilizan y podría haber un
/// ciclo) está en `set`.
fn has_ancestor_in(
    pid: u32,
    parents: &std::collections::HashMap<u32, u32>,
    set: &std::collections::HashSet<u32>,
) -> bool {
    let mut current = pid;
    for _ in 0..32 {
        match parents.get(&current) {
            Some(&parent) if parent != 0 && parent != current => {
                if set.contains(&parent) {
                    return true;
                }
                current = parent;
            }
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_discontinuity_is_measured_by_the_device_position() {
        assert_eq!(capture_gap(Some(960), 1_440, true), Some(480));
        assert_eq!(capture_gap(Some(960), 1_440, false), None, "sin la bandera");
        assert_eq!(capture_gap(None, 1_440, true), None, "primer paquete");
        assert_eq!(capture_gap(Some(960), 960, true), None, "sin salto");
        assert_eq!(capture_gap(Some(960), 0, true), None, "hacia atrás");
    }

    #[test]
    fn keys_and_names_come_from_the_executable() {
        assert_eq!(
            exe_file_name(r"C:\Juegos\Elden Ring\EldenRing.exe"),
            Some("EldenRing.exe")
        );
        assert_eq!(exe_file_name(r"C:\Juegos\"), None);
        assert_eq!(exe_stem("eldenring.exe"), "eldenring");
        assert_eq!(exe_stem("herramienta"), "herramienta");
        assert_eq!(display_name("", "eldenring.exe"), "eldenring");
        assert_eq!(
            display_name("@%SystemRoot%\\System32\\x.dll,-1", "x.exe"),
            "x"
        );
        assert_eq!(display_name("Spotify", "spotify.exe"), "Spotify");
    }

    #[test]
    fn this_process_has_a_key() {
        let key = app_key_for_pid(std::process::id()).expect("clave de este proceso");
        assert!(key.ends_with(".exe"));
        assert_eq!(key, key.to_lowercase());
    }

    #[test]
    fn a_process_whose_ancestor_is_captured_is_left_to_it() {
        let parents = std::collections::HashMap::from([(30, 20), (20, 10), (10, 0), (40, 10)]);
        let set = std::collections::HashSet::from([10, 30]);
        assert!(
            has_ancestor_in(30, &parents, &set),
            "su abuelo 10 ya lo cubre"
        );
        assert!(!has_ancestor_in(10, &parents, &set));
        let only_child = std::collections::HashSet::from([30]);
        assert!(!has_ancestor_in(30, &parents, &only_child));
        let cycle = std::collections::HashMap::from([(1, 2), (2, 1)]);
        assert!(!has_ancestor_in(
            1,
            &cycle,
            &std::collections::HashSet::from([3])
        ));
    }

    #[test]
    fn coroom_never_lists_itself() {
        use crate::apps::AppBackend;
        if let Ok(apps) = AppSessions.playing() {
            let own = std::process::id().to_string();
            assert!(apps.iter().all(|app| !app.streams.contains(&own)));
        }
    }
}
