//! Capacidades de vídeo de esta máquina: qué codificadores y decodificadores funcionan de verdad.
//!
//! [`Capabilities::probe`] abre cada combinación de códec y backend con un contexto real (ver
//! `ffmpeg::probe`) y guarda el resultado para todo el proceso. OpenH264 (codificar y decodificar
//! H.264 por software) está siempre. Sin la feature `ffmpeg` no hay nada más.
//!
//! Con un fichero de caché ([`Capabilities::use_cache_file`]) el resultado se guarda en disco con
//! la huella de la máquina (`ffmpeg::fingerprint`: ejecutable, FFmpeg, GPU, drivers) y el
//! siguiente arranque lo reutiliza sin abrir nada mientras la huella sea la misma: el sondeo
//! cuesta unos 13 s en una GPU NVIDIA con Linux. Si un codificador o decodificador que el sondeo
//! dio por bueno no abre en una sesión, la caché se borra ([`Capabilities::forget_cache`]) y el
//! siguiente arranque vuelve a sondear.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::color::{Chroma, VideoFormat};
use crate::{Backend, Codec, DecoderBackend, decoder_label, encoder_label};

/// Si el codificador `codec` con `backend` admite `format` en esta versión.
///
/// Tabla fija, comprobada contra los formatos de píxel y los perfiles de cada codificador de
/// FFmpeg 8.1 (`ffmpeg -h encoder=…`; el test `encoder_pixel_formats_back_the_table` lo vuelve a
/// mirar en los codificadores de la build) y contra lo que hace el hardware de cada fabricante:
///
/// | | 4:4:4 (8 bits) | HDR10 (10 bits 4:2:0) |
/// |---|---|---|
/// | OpenH264 (H.264 software) | no | no |
/// | SVT-AV1 (AV1 software) | no (solo perfil *main*) | sí (`yuv420p10le`) |
/// | NVENC | H.264 *High 4:4:4* y HEVC *Rext* (`yuv444p`) | HEVC *Main10* y AV1 (`p010le`) |
/// | Vulkan Video | H.264 `high444p`, HEVC `rext`, AV1 `high` (NV24; según el driver) | HEVC `main10` y AV1 (P010) |
/// | QSV, VA-API, AMF | no (el 4:4:4 de Intel en HEVC pide formatos empaquetados que coroom aún no convierte) | HEVC y AV1 (P010) |
///
/// 4:4:4 y HDR a la vez no se negocian (el 4:4:4 de 10 bits apenas existe en hardware): la sesión
/// elige uno de los dos.
pub fn encoder_supports(codec: Codec, backend: Backend, format: VideoFormat) -> bool {
    if format.is_hdr() && format.chroma == Chroma::Yuv444 {
        return false;
    }
    if format.is_default() {
        return true;
    }
    match format.chroma {
        Chroma::Yuv444 => matches!(
            (backend, codec),
            (Backend::Nvenc, Codec::H264 | Codec::Hevc) | (Backend::VulkanVideo, _)
        ),
        Chroma::Yuv420 => match backend {
            Backend::Software => codec == Codec::Av1,
            _ => codec != Codec::H264,
        },
    }
}

/// Si el decodificador `codec` con `backend` admite `format`. Tabla fija, como
/// [`encoder_supports`]:
///
/// | | 4:4:4 (8 bits) | HDR10 (10 bits 4:2:0) |
/// |---|---|---|
/// | OpenH264 (H.264 software) | no | no |
/// | dav1d (AV1) y `hevc` de FFmpeg (software) | sí | sí |
/// | NVDEC | HEVC (Turing y posteriores) | HEVC y AV1 |
/// | Vulkan Video, VA-API, D3D11/D3D12 | no (Vulkan Video no tiene perfiles 4:4:4 de decodificación estándar) | HEVC y AV1 |
pub fn decoder_supports(codec: Codec, backend: DecoderBackend, format: VideoFormat) -> bool {
    if format.is_hdr() && format.chroma == Chroma::Yuv444 {
        return false;
    }
    if format.is_default() {
        return true;
    }
    if codec == Codec::H264 {
        // OpenH264 y los decodificadores de hardware de H.264: solo 4:2:0 de 8 bits.
        return false;
    }
    match (format.chroma, backend) {
        (_, DecoderBackend::Software) => true,
        (Chroma::Yuv444, DecoderBackend::Nvdec) => codec == Codec::Hevc,
        (Chroma::Yuv444, _) => false,
        (Chroma::Yuv420, _) => true,
    }
}

/// Lo que esta máquina codifica y decodifica, en orden de preferencia (por códec, AV1 → HEVC →
/// H.264; dentro de cada uno, del backend preferido al de respaldo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Codificadores que pasaron el sondeo.
    pub encoders: Vec<(Codec, Backend)>,
    /// Decodificadores que pasaron el sondeo.
    pub decoders: Vec<(Codec, DecoderBackend)>,
    /// Versión de FFmpeg cargada (`None` sin la feature `ffmpeg`).
    pub ffmpeg_version: Option<String>,
    /// Directorio de las bibliotecas de FFmpeg cargadas (en Linux; si no, el de compilación).
    pub ffmpeg_dir: Option<String>,
    /// Codificadores y decodificadores que abren pero fallaron la comprobación de imagen (su
    /// imagen no coincide con la original), con el motivo: `«codificar AV1 con Vulkan Video: …»`.
    pub rejected: Vec<String>,
    /// Codificadores por hardware que FFmpeg trae pero que no abren en esta máquina, con el
    /// motivo (lo normal si falta esa GPU o su driver): `«codificar HEVC con NVENC: …»`.
    pub unavailable: Vec<String>,
}

static PROBED: OnceLock<Capabilities> = OnceLock::new();

/// Fichero de la caché del sondeo, si la aplicación dio uno.
static CACHE_FILE: OnceLock<PathBuf> = OnceLock::new();

/// Si el sondeo de este proceso salió de la caché.
static FROM_CACHE: OnceLock<bool> = OnceLock::new();

/// Lo que se guarda en disco: la huella y las listas, con los identificadores estables de
/// códecs y backends (`av1`, `vulkan`…).
#[derive(Debug, Serialize, Deserialize)]
struct CachedProbe {
    fingerprint: String,
    encoders: Vec<(String, String)>,
    decoders: Vec<(String, String)>,
    ffmpeg_version: Option<String>,
    ffmpeg_dir: Option<String>,
    rejected: Vec<String>,
    unavailable: Vec<String>,
}

impl CachedProbe {
    fn new(fingerprint: String, caps: &Capabilities) -> Self {
        Self {
            fingerprint,
            encoders: caps
                .encoders
                .iter()
                .map(|(codec, backend)| (codec.id().to_owned(), backend.id().to_owned()))
                .collect(),
            decoders: caps
                .decoders
                .iter()
                .map(|(codec, backend)| (codec.id().to_owned(), backend.id().to_owned()))
                .collect(),
            ffmpeg_version: caps.ffmpeg_version.clone(),
            ffmpeg_dir: caps.ffmpeg_dir.clone(),
            rejected: caps.rejected.clone(),
            unavailable: caps.unavailable.clone(),
        }
    }

    /// Las capacidades guardadas, o `None` si algún identificador ya no existe en este binario.
    fn capabilities(&self) -> Option<Capabilities> {
        let encoders = self
            .encoders
            .iter()
            .map(|(codec, backend)| Some((Codec::from_id(codec)?, Backend::from_id(backend)?)))
            .collect::<Option<Vec<_>>>()?;
        let decoders = self
            .decoders
            .iter()
            .map(|(codec, backend)| {
                Some((Codec::from_id(codec)?, DecoderBackend::from_id(backend)?))
            })
            .collect::<Option<Vec<_>>>()?;
        let mut caps = Capabilities::with(encoders, decoders);
        caps.ffmpeg_version = self.ffmpeg_version.clone();
        caps.ffmpeg_dir = self.ffmpeg_dir.clone();
        caps.rejected = self.rejected.clone();
        caps.unavailable = self.unavailable.clone();
        Some(caps)
    }
}

/// `caps` en una línea de texto (el formato de la caché, sin huella): así las pasa el hijo del
/// sondeo aislado (`ffmpeg::probe_child`) a la aplicación.
#[cfg_attr(not(feature = "ffmpeg"), allow(dead_code))]
pub(crate) fn to_wire(caps: &Capabilities) -> String {
    serde_json::to_string(&CachedProbe::new(String::new(), caps)).unwrap_or_default()
}

/// Lo contrario de [`to_wire`] (`None` si el texto no se entiende).
#[cfg_attr(not(feature = "ffmpeg"), allow(dead_code))]
pub(crate) fn from_wire(text: &str) -> Option<Capabilities> {
    serde_json::from_str::<CachedProbe>(text)
        .ok()?
        .capabilities()
}

/// Sondeos que guarda la caché, uno por huella, del último al más antiguo: dos ejecutables de
/// coroom en el mismo equipo (el instalado y uno compilado, o dos versiones) no se borran el
/// sondeo el uno al otro.
const CACHED_PROBES: usize = 4;

/// Los sondeos guardados en `path` (vacío si no hay o no se entiende).
fn read_cache(path: &Path) -> Vec<CachedProbe> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Las capacidades guardadas en `path` con la huella `fingerprint`.
fn load_cached(path: &Path, fingerprint: &str) -> Option<Capabilities> {
    read_cache(path)
        .iter()
        .find(|cached| cached.fingerprint == fingerprint)?
        .capabilities()
}

/// Guarda `caps` con su huella en `path`, delante de los demás sondeos (escribe un temporal y lo
/// renombra: un arranque a la vez nunca lee un fichero a medias).
fn store_cached(path: &Path, fingerprint: String, caps: &Capabilities) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut probes = read_cache(path);
    probes.retain(|cached| cached.fingerprint != fingerprint);
    probes.insert(0, CachedProbe::new(fingerprint, caps));
    probes.truncate(CACHED_PROBES);
    write_cache(path, &probes)
}

fn write_cache(path: &Path, probes: &[CachedProbe]) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(probes).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, json)?;
    std::fs::rename(&temporary, path)
}

/// El cerrojo de la caché (`codecs.json.lock`), mientras viva el fichero devuelto (`None` si no se
/// pudo tomar: se sigue sin él).
fn lock_cache(path: &Path) -> Option<std::fs::File> {
    let parent = path.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("json.lock"))
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

/// Quita de `path` el sondeo de `fingerprint` (los de otros ejecutables se quedan).
fn forget_cached(path: &Path, fingerprint: &str) -> std::io::Result<()> {
    let _lock = lock_cache(path);
    let mut probes = read_cache(path);
    let before = probes.len();
    probes.retain(|cached| cached.fingerprint != fingerprint);
    if probes.len() == before {
        return Ok(());
    }
    write_cache(path, &probes)
}

/// Lo guardado en `path` para `fingerprint` (`true`: salió de la caché) o, si no hay, lo que diga
/// `probe` (`(capacidades, terminó)`), que se guarda si terminó. Con el cerrojo de
/// `codecs.json.lock`: dos procesos que arrancan a la vez con la caché vieja (el anfitrión y el
/// invitado tras una actualización) no sondean los dos, que es el doble de largo porque el driver
/// no abre dispositivos en paralelo; el segundo espera al primero y usa lo que guardó.
fn cached_or_probe(
    path: &Path,
    fingerprint: &str,
    probe: impl FnOnce() -> (Capabilities, bool),
) -> (Capabilities, bool) {
    if let Some(caps) = load_cached(path, fingerprint) {
        return (caps, true);
    }
    let lock = lock_cache(path);
    // Quien tenía el cerrojo pudo dejarlo guardado mientras se esperaba.
    if let Some(caps) = load_cached(path, fingerprint) {
        return (caps, true);
    }
    let (caps, complete) = probe();
    // Un sondeo que no terminó no se guarda: el próximo arranque lo repite.
    if complete {
        let _ = store_cached(path, fingerprint.to_owned(), &caps);
    }
    drop(lock);
    (caps, false)
}

/// Huella de la máquina para la caché (`None`: sin caché en esta plataforma o sin FFmpeg).
fn fingerprint() -> Option<String> {
    #[cfg(feature = "ffmpeg")]
    {
        crate::ffmpeg::fingerprint::current()
    }
    #[cfg(not(feature = "ffmpeg"))]
    {
        None
    }
}

fn sort<B: Ord + Copy>(entries: &mut Vec<(Codec, B)>) {
    let rank = |codec: Codec| Codec::PREFERENCE.iter().position(|c| *c == codec);
    entries.sort_by_key(|&(codec, backend)| (rank(codec), backend));
    entries.dedup();
}

/// Como [`sort`], con los codificadores en el orden de [`Backend::encoder_rank`] (en Windows,
/// los nativos del fabricante antes que Vulkan Video).
fn sort_encoders(entries: &mut Vec<(Codec, Backend)>) {
    let rank = |codec: Codec| Codec::PREFERENCE.iter().position(|c| *c == codec);
    entries.sort_by_key(|&(codec, backend)| (rank(codec), backend.encoder_rank()));
    entries.dedup();
}

impl Capabilities {
    /// Sondea la máquina la primera vez (de cientos de milisegundos a unos 13 s, según los
    /// drivers) y devuelve el resultado guardado las siguientes. Con [`Self::use_cache_file`],
    /// la primera vez del proceso lee el resultado del disco si la huella de la máquina no cambió.
    pub fn probe() -> Capabilities {
        PROBED
            .get_or_init(|| {
                let cache = CACHE_FILE
                    .get()
                    .and_then(|path| Some((path, fingerprint()?)));
                let (caps, from_cache) = match &cache {
                    Some((path, fingerprint)) => {
                        cached_or_probe(path, fingerprint, Self::run_probe)
                    }
                    None => (Self::run_probe().0, false),
                };
                let _ = FROM_CACHE.set(from_cache);
                caps
            })
            .clone()
    }

    /// Guarda el sondeo en `path` y lo reutiliza en los arranques siguientes mientras la huella
    /// de la máquina sea la misma. Hay que darlo antes del primer [`Self::probe`]; después no
    /// cambia nada.
    pub fn use_cache_file(path: PathBuf) {
        let _ = CACHE_FILE.set(path);
    }

    /// Borra de la caché el sondeo de este ejecutable (los de otros se quedan): el siguiente
    /// arranque vuelve a sondear. Se llama cuando algo que el sondeo dio por bueno no abre, y desde
    /// `coroom doctor` (que siempre sondea de verdad).
    pub fn forget_cache() {
        if let Some(path) = CACHE_FILE.get()
            && let Some(fingerprint) = fingerprint()
        {
            let _ = forget_cached(path, &fingerprint);
        }
    }

    /// Si el sondeo de este proceso ya terminó (o salió de la caché): [`Self::probe`] no espera.
    pub fn is_probed() -> bool {
        PROBED.get().is_some()
    }

    /// Si el sondeo de este proceso salió de la caché del disco (`None` si aún no se hizo).
    pub fn from_cache() -> Option<bool> {
        FROM_CACHE.get().copied()
    }

    /// El sondeo, en un proceso hijo cuando el binario lo activó (`ffmpeg::probe_child`): así no
    /// traba la interfaz de la aplicación. `false` si no terminó (el hijo murió o se colgó):
    /// entonces queda solo OpenH264, con el motivo en [`Self::unavailable`].
    fn run_probe() -> (Capabilities, bool) {
        #[cfg(feature = "ffmpeg")]
        match crate::ffmpeg::probe_child::isolated_probe() {
            Some(Ok(caps)) => return (caps, true),
            Some(Err(reason)) => {
                let mut caps = Self::openh264_only();
                caps.unavailable.push(reason);
                return (caps, false);
            }
            None => {}
        }
        (Self::probe_in_process(), true)
    }

    /// El sondeo en este mismo proceso.
    pub(crate) fn probe_in_process() -> Capabilities {
        let mut caps = Self::openh264_only();
        #[cfg(feature = "ffmpeg")]
        {
            let probed = crate::ffmpeg::probe::probe();
            caps.encoders.extend(probed.encoders);
            caps.decoders.extend(probed.decoders);
            caps.rejected = probed.rejected;
            caps.unavailable = probed.unavailable;
            caps.ffmpeg_version = Some(crate::ffmpeg::version());
            caps.ffmpeg_dir = Some(crate::ffmpeg::library_dir());
        }
        sort_encoders(&mut caps.encoders);
        sort(&mut caps.decoders);
        caps
    }

    /// Solo H.264 por software con OpenH264, sin sondear nada.
    pub fn openh264_only() -> Capabilities {
        Capabilities {
            encoders: vec![(Codec::H264, Backend::Software)],
            decoders: vec![(Codec::H264, DecoderBackend::Software)],
            ffmpeg_version: None,
            ffmpeg_dir: None,
            rejected: Vec::new(),
            unavailable: Vec::new(),
        }
    }

    /// Capacidades con listas dadas (para pruebas y para limitar lo que se anuncia).
    pub fn with(
        mut encoders: Vec<(Codec, Backend)>,
        mut decoders: Vec<(Codec, DecoderBackend)>,
    ) -> Capabilities {
        sort_encoders(&mut encoders);
        sort(&mut decoders);
        Capabilities {
            encoders,
            decoders,
            ffmpeg_version: None,
            ffmpeg_dir: None,
            rejected: Vec::new(),
            unavailable: Vec::new(),
        }
    }

    /// Si algún codificador o decodificador usa la GPU.
    pub fn has_hardware(&self) -> bool {
        self.encoders.iter().any(|(_, b)| b.is_hardware())
            || self.decoders.iter().any(|(_, b)| b.is_hardware())
    }
}

impl fmt::Display for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.ffmpeg_version, &self.ffmpeg_dir) {
            (Some(version), Some(dir)) => writeln!(f, "FFmpeg {version} (bibliotecas en {dir})")?,
            (Some(version), None) => writeln!(f, "FFmpeg {version}")?,
            _ => writeln!(f, "FFmpeg: no compilado en este binario (solo OpenH264)")?,
        }
        if !self.has_hardware() {
            writeln!(
                f,
                "Solo software: sin GPU detectada o sin códecs acelerados."
            )?;
        }
        let join = |names: Vec<String>| {
            if names.is_empty() {
                "—".to_owned()
            } else {
                names.join(", ")
            }
        };
        let rows: Vec<(String, String, String)> = Codec::PREFERENCE
            .into_iter()
            .map(|codec| {
                let encoders = join(
                    self.encoders
                        .iter()
                        .filter(|(c, _)| *c == codec)
                        .map(|(c, b)| encoder_label(*c, *b))
                        .collect(),
                );
                let decoders = join(
                    self.decoders
                        .iter()
                        .filter(|(c, _)| *c == codec)
                        .map(|(c, b)| decoder_label(*c, *b))
                        .collect(),
                );
                (codec.to_string(), encoders, decoders)
            })
            .collect();
        // La columna «Codificar» se ensancha con la lista más larga (por caracteres, no por
        // bytes: las etiquetas llevan tildes) para que «Decodificar» quede alineada.
        let width = |pick: fn(&(String, String, String)) -> &String, header: &str| {
            rows.iter()
                .map(|row| pick(row).chars().count())
                .chain([header.chars().count()])
                .max()
                .unwrap_or(0)
        };
        let codec_width = width(|row| &row.0, "Códec");
        let encoder_width = width(|row| &row.1, "Codificar");
        let pad = |text: &str, width: usize| {
            let mut out = text.to_owned();
            out.extend(std::iter::repeat_n(
                ' ',
                width.saturating_sub(text.chars().count()),
            ));
            out
        };
        writeln!(
            f,
            "{} {} Decodificar",
            pad("Códec", codec_width),
            pad("Codificar", encoder_width)
        )?;
        for (codec, encoders, decoders) in &rows {
            writeln!(
                f,
                "{} {} {decoders}",
                pad(codec, codec_width),
                pad(encoders, encoder_width)
            )?;
        }
        if !self.unavailable.is_empty() {
            writeln!(f, "Por hardware que no abren en este equipo:")?;
            for line in &self.unavailable {
                writeln!(f, "  {line}")?;
            }
        }
        if !self.rejected.is_empty() {
            writeln!(
                f,
                "Descartados por la comprobación de imagen (abren, pero su vídeo sale dañado):"
            )?;
            for line in &self.rejected {
                writeln!(f, "  {line}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lo que el hijo del sondeo aislado escribe es lo que la aplicación lee, en una sola línea.
    #[test]
    fn capabilities_cross_the_child_process_intact() {
        let mut caps = Capabilities::with(
            vec![
                (Codec::Av1, Backend::Nvenc),
                (Codec::Hevc, Backend::VulkanVideo),
                (Codec::H264, Backend::Software),
            ],
            vec![
                (Codec::Av1, DecoderBackend::Software),
                (Codec::H264, DecoderBackend::Software),
            ],
        );
        caps.ffmpeg_version = Some("8.1".to_owned());
        caps.rejected = vec!["codificar AV1 con Vulkan Video: imagen dañada".to_owned()];
        caps.unavailable = vec!["codificar HEVC con QSV: sin GPU\ny otra línea".to_owned()];
        let wire = to_wire(&caps);
        assert!(!wire.contains('\n'), "{wire}");
        assert_eq!(from_wire(&wire), Some(caps));
        assert_eq!(from_wire("no es json"), None);
    }

    /// La columna «Decodificar» queda alineada aunque la lista de codificadores sea larga.
    #[test]
    fn the_table_keeps_its_columns_aligned() {
        let caps = Capabilities::with(
            vec![
                (Codec::Av1, Backend::VulkanVideo),
                (Codec::Av1, Backend::Nvenc),
                (Codec::Av1, Backend::Software),
                (Codec::H264, Backend::Software),
            ],
            vec![(Codec::Av1, DecoderBackend::Software)],
        );
        let text = caps.to_string();
        let rows: Vec<&str> = text
            .lines()
            .skip_while(|line| !line.starts_with("Códec"))
            .take(4)
            .collect();
        assert_eq!(rows.len(), 4, "{text}");
        let column = |line: &str| {
            line.char_indices()
                .map(|(i, _)| i)
                .find(|&i| {
                    line[i..].starts_with("Decodificar")
                        || line[i..].starts_with("dav1d")
                        || (line[i..].starts_with('—') && line[..i].ends_with(' '))
                })
                .map(|byte| line[..byte].chars().count())
        };
        let header = column(rows[0]).expect("cabecera");
        for row in &rows[1..] {
            let position = row
                .char_indices()
                .map(|(i, _)| i)
                .filter(|&i| row[..i].ends_with(' ') && !row[i..].starts_with(' '))
                .map(|byte| row[..byte].chars().count())
                .max()
                .expect("última columna");
            assert_eq!(position, header, "fila sin alinear: {row:?} en\n{text}");
        }
    }

    #[test]
    fn probing_never_fails_and_lists_the_software_codecs() {
        let caps = Capabilities::probe();
        eprintln!("{caps}");
        assert!(caps.encoders.contains(&(Codec::H264, Backend::Software)));
        assert!(
            caps.decoders
                .contains(&(Codec::H264, DecoderBackend::Software))
        );
        #[cfg(feature = "ffmpeg")]
        {
            assert!(caps.ffmpeg_version.is_some());
            assert!(
                caps.encoders.contains(&(Codec::Av1, Backend::Software)),
                "{caps}"
            );
            assert!(
                caps.decoders
                    .contains(&(Codec::Av1, DecoderBackend::Software)),
                "{caps}"
            );
            assert!(
                caps.decoders
                    .contains(&(Codec::Hevc, DecoderBackend::Software)),
                "{caps}"
            );
        }
        // Cacheado: la segunda vez es lo mismo, sin volver a sondear.
        assert_eq!(Capabilities::probe(), caps);
    }

    #[test]
    fn entries_are_sorted_by_preference() {
        let caps = Capabilities::with(
            vec![
                (Codec::H264, Backend::Software),
                (Codec::Av1, Backend::Software),
                (Codec::Av1, Backend::Nvenc),
            ],
            vec![(Codec::H264, DecoderBackend::Software)],
        );
        assert_eq!(
            caps.encoders,
            vec![
                (Codec::Av1, Backend::Nvenc),
                (Codec::Av1, Backend::Software),
                (Codec::H264, Backend::Software)
            ]
        );
        assert!(caps.has_hardware());
        let text = caps.to_string();
        assert!(text.contains("NVENC, SVT-AV1"), "{text}");
        assert!(
            Capabilities::openh264_only()
                .to_string()
                .contains("Solo software")
        );
    }

    fn probed(name: &str) -> Capabilities {
        let mut caps = Capabilities::openh264_only();
        caps.ffmpeg_version = Some(name.to_owned());
        caps
    }

    /// Dos ejecutables (dos huellas) usan la misma caché sin borrarse el sondeo; se guardan los
    /// [`CACHED_PROBES`] últimos.
    #[test]
    fn the_cache_keeps_one_probe_per_fingerprint() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = dir.path().join("codecs.json");
        let never = || -> (Capabilities, bool) { panic!("debía salir de la caché") };
        assert!(!cached_or_probe(&path, "a", || (probed("a"), true)).1);
        assert!(!cached_or_probe(&path, "b", || (probed("b"), true)).1);
        let (caps, from_cache) = cached_or_probe(&path, "a", never);
        assert!(from_cache);
        assert_eq!(caps.ffmpeg_version.as_deref(), Some("a"));
        assert_eq!(
            cached_or_probe(&path, "b", never)
                .0
                .ffmpeg_version
                .as_deref(),
            Some("b")
        );
        // Uno que no terminó no se guarda.
        assert!(!cached_or_probe(&path, "c", || (probed("c"), false)).1);
        assert!(load_cached(&path, "c").is_none());
        for name in ["c", "d", "e", "f"] {
            cached_or_probe(&path, name, || (probed(name), true));
        }
        assert_eq!(read_cache(&path).len(), CACHED_PROBES);
        assert!(load_cached(&path, "f").is_some());
        assert!(load_cached(&path, "a").is_none(), "el más antiguo sale");
        // Olvidar el de un ejecutable (`coroom doctor`) deja los de los demás.
        forget_cached(&path, "f").expect("olvidar");
        assert!(load_cached(&path, "f").is_none());
        assert_eq!(read_cache(&path).len(), CACHED_PROBES - 1);
        assert!(load_cached(&path, "e").is_some());
    }

    /// Dos procesos que arrancan a la vez sin caché: sondea uno y el otro usa su resultado.
    #[test]
    fn concurrent_starts_probe_only_once() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = dir.path().join("codecs.json");
        let probes = std::sync::atomic::AtomicU32::new(0);
        let (first, second) = std::thread::scope(|scope| {
            let start = || {
                cached_or_probe(&path, "a", || {
                    probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    (probed("a"), true)
                })
            };
            let first = scope.spawn(start);
            let second = scope.spawn(start);
            (first.join().expect("hilo"), second.join().expect("hilo"))
        });
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(first.0.ffmpeg_version, second.0.ffmpeg_version);
        assert!(first.1 != second.1, "uno sondeó y el otro leyó la caché");
    }
}
