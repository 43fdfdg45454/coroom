//! `coroom doctor`: el estado del sistema (protocolo, dónde se guardan la identidad y las
//! credenciales, captura, adaptador gráfico, entrada, audio, códecs y la ruta sin copias), como
//! líneas de texto para la terminal y para Ajustes → Diagnóstico de la interfaz. No
//! contiene secretos ni identifica al usuario: se puede pegar al pedir ayuda.

use coroom_render::wgpu::{probe_adapter, probe_zero_copy};
use coroom_session::capture::{self as capture, compiled_backends, probe_screen_capture};
use coroom_session::codec::gpu::{GpuInput, zero_copy_disabled};
use coroom_session::codec::select::gpu_input_for;
use coroom_session::codec::{Backend, Capabilities};
use coroom_transport::crypto::{FileStore, KeyringStore};
use coroom_transport::proto::PROTOCOL_VERSION;

/// Las líneas del diagnóstico (tarda unos segundos: sondea los códecs de verdad).
pub fn report() -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    out.push(format!("coroom {}", env!("CARGO_PKG_VERSION")));
    out.push(format!(
        "Sistema operativo: {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    out.push(format!("Versión del protocolo: {PROTOCOL_VERSION}"));
    let data_dir = crate::remote::default_data_dir();
    out.push(format!("Directorio de datos: {}", data_dir.display()));
    out.push(format!(
        "Registro: {} (la ejecución anterior, en coroom.1.log; mándalo al pedir ayuda)",
        crate::logging::log_path().display()
    ));
    if !crate::portal::in_flatpak() {
        out.push(if KeyringStore::is_available() {
            "Llavero del sistema: disponible (identidad y credenciales de cada servidor)".to_owned()
        } else {
            format!(
                "Llavero del sistema: no disponible; la identidad va en {} (permisos 0600)",
                FileStore::new(&data_dir).path().display()
            )
        });
    }
    let backends = compiled_backends()
        .iter()
        .map(|backend| backend.name())
        .collect::<Vec<_>>()
        .join(", ");
    out.push(format!("Backends de captura compilados: {backends}"));
    match probe_screen_capture() {
        Ok(Some(status)) => out.push(format!("Captura de pantalla: {status}")),
        Ok(None) => out.push("Captura de pantalla: no compilada en este binario".to_owned()),
        Err(err) => out.push(format!("Captura de pantalla: no disponible ({err})")),
    }
    match probe_adapter() {
        Ok(adapter) => out.push(format!("Adaptador gráfico: {adapter}")),
        Err(err) => out.push(format!("Adaptador gráfico: ninguno ({err})")),
    }
    out.push("Entrada del anfitrión:".to_owned());
    for line in coroom_session::input::inject::status() {
        out.push(format!("  {line}"));
    }
    out.push(format!(
        "  ranuras de mando: {} (COROOM_MAX_GAMEPADS)",
        coroom_session::input::max_gamepads()
    ));
    out.push("Audio:".to_owned());
    for line in coroom_session::audio::status() {
        out.push(format!("  {line}"));
    }
    match coroom_session::audio::list_devices() {
        Ok(devices) if devices.is_empty() => out.push("  dispositivos: ninguno".to_owned()),
        Ok(devices) => {
            for device in devices {
                let kind = match device.kind {
                    coroom_session::audio::DeviceKind::Source => "micrófono",
                    coroom_session::audio::DeviceKind::Sink => "salida",
                    coroom_session::audio::DeviceKind::Application => "aplicación",
                };
                out.push(format!(
                    "  {kind}: «{}» — {}{}",
                    device.name,
                    device.description,
                    if device.default { " (por defecto)" } else { "" }
                ));
            }
        }
        Err(err) => out.push(format!("  dispositivos: no disponibles ({err})")),
    }
    out.push("Códecs de vídeo (sondeo real de cada combinación):".to_owned());
    let caps = Capabilities::probe();
    for line in caps.to_string().lines() {
        out.push(format!("  {line}"));
    }
    out.push("Cero copias:".to_owned());
    for line in zero_copy_report(&caps) {
        out.push(format!("  {line}"));
    }
    out.push("4:4:4 y HDR10 (según la tabla de cada backend):".to_owned());
    let support = crate::ui::backend::video_support(None);
    let list = |items: Vec<String>| {
        if items.is_empty() {
            "ninguno".to_owned()
        } else {
            items.join(", ")
        }
    };
    let names = |codecs: &[coroom_session::codec::Codec]| -> Vec<String> {
        codecs.iter().map(ToString::to_string).collect()
    };
    out.push(format!(
        "  codifica 4:4:4: {}",
        list(support.yuv444.into_iter().map(|(_, label)| label).collect())
    ));
    out.push(format!(
        "  codifica HDR10: {}",
        list(support.hdr10.into_iter().map(|(_, label)| label).collect())
    ));
    out.push(format!(
        "  decodifica 4:4:4: {}; HDR10: {}",
        list(names(&support.decode_yuv444)),
        list(names(&support.decode_hdr10))
    ));
    if crate::portal::in_flatpak() {
        out.push(format!(
            "Flatpak: llavero por el portal Secret ({})",
            if crate::portal::seal_key().is_some() {
                "responde"
            } else {
                "no responde: se usan ficheros sin sellar"
            }
        ));
    }
    Ok(out)
}

/// Qué parte de la ruta sin copias hay en este equipo: captura, codificadores de GPU y
/// presentación.
fn zero_copy_report(caps: &Capabilities) -> Vec<String> {
    if zero_copy_disabled() {
        return vec!["desactivada con COROOM_ZERO_COPY=0: todo pasa por memoria de CPU".to_owned()];
    }
    let mut lines = Vec::new();
    let capture = if compiled_backends().contains(&capture::Backend::PipeWire) {
        "captura: DMA-BUF de PipeWire con modificadores explícitos si el codificador lo importa \
         (si no, memoria compartida)"
    } else if compiled_backends().contains(&capture::Backend::Wgc) {
        "captura: texturas D3D11 de WGC si el codificador las recibe (si no, staging)"
    } else {
        "captura: solo la sintética, en memoria de CPU"
    };
    lines.push(capture.to_owned());
    let mut backends: Vec<Backend> = caps
        .encoders
        .iter()
        .map(|(_, backend)| *backend)
        .filter(|backend| backend.is_hardware())
        .collect();
    backends.sort();
    backends.dedup();
    if backends.is_empty() {
        lines.push(
            "codificadores: ninguno de GPU (solo software): la captura irá en memoria de CPU"
                .to_owned(),
        );
    }
    for backend in backends {
        let input = match gpu_input_for(backend) {
            GpuInput::None => "sin importación: copia a CPU y subida a la GPU".to_owned(),
            GpuInput::D3D11 => "importa texturas D3D11 (copia en la GPU, no en CPU)".to_owned(),
            GpuInput::DmaBuf { modifiers } => {
                format!("importa DMA-BUF ({} modificadores)", modifiers.len())
            }
        };
        lines.push(format!("codificador {backend}: {input}"));
    }
    lines.push(format!("presentación: {}", probe_zero_copy()));
    lines
}
