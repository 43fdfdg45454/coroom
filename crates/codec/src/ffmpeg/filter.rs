//! Grafo de filtros de FFmpeg para fotogramas de GPU: `buffer` con un contexto de *hw
//! frames* → una cadena (`scale_vulkan`, `scale_vaapi`, `hwmap`…) → `buffersink`.
//!
//! `rsmpeg` envuelve `AVFilterGraph`, pero no `av_buffersrc_parameters_set`, que es la única forma
//! de darle a `buffer` el contexto de *hw frames* de la entrada; por eso el grafo se monta con la
//! API de C directamente. Todo el `unsafe` lleva su `// SAFETY:`.

use std::ffi::{CStr, CString};
use std::ptr::{self, NonNull};

use rsmpeg::avutil::{AVFrame, AVHWFramesContext};
use rsmpeg::ffi;

use super::describe_code;

/// Grafo `buffer → cadena → buffersink` para fotogramas de GPU.
pub(crate) struct HwFilter {
    graph: NonNull<ffi::AVFilterGraph>,
    src: *mut ffi::AVFilterContext,
    sink: *mut ffi::AVFilterContext,
    chain: String,
}

// SAFETY: el grafo y sus filtros solo se tocan a través de `&mut self` (o `&self` para leer el
// contexto de salida), así que nunca los usan dos hilos a la vez; FFmpeg no ata un grafo a un
// hilo.
unsafe impl Send for HwFilter {}

impl std::fmt::Debug for HwFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HwFilter")
            .field("chain", &self.chain)
            .finish_non_exhaustive()
    }
}

fn check(what: &str, ret: i32) -> Result<(), String> {
    if ret < 0 {
        Err(format!("{what}: {}", describe_code(ret)))
    } else {
        Ok(())
    }
}

/// Si esta build de FFmpeg trae el filtro `name` (los de GPU dependen de cómo se compiló).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn exists(name: &CStr) -> bool {
    // SAFETY: `avfilter_get_by_name` solo lee la tabla estática de filtros con una cadena válida.
    unsafe { !ffi::avfilter_get_by_name(name.as_ptr()).is_null() }
}

/// Filtro de FFmpeg por nombre.
fn filter(name: &CStr) -> Result<*const ffi::AVFilter, String> {
    // SAFETY: `avfilter_get_by_name` solo lee la tabla estática de filtros con una cadena válida.
    let filter = unsafe { ffi::avfilter_get_by_name(name.as_ptr()) };
    if filter.is_null() {
        Err(format!(
            "esta build de FFmpeg no tiene el filtro {}",
            name.to_string_lossy()
        ))
    } else {
        Ok(filter)
    }
}

impl HwFilter {
    /// Monta el grafo para fotogramas de `format` (de GPU) de `width × height` del contexto
    /// `frames`, con la cadena `chain` (sintaxis de `-vf`).
    pub(crate) fn new(
        frames: &AVHWFramesContext,
        format: ffi::AVPixelFormat,
        (width, height): (u32, u32),
        chain: &str,
    ) -> Result<Self, String> {
        let chain_c = CString::new(chain).map_err(|_| "cadena de filtros no válida".to_owned())?;
        // SAFETY: `avfilter_graph_alloc` devuelve un grafo nuevo (o nulo), que pasa a ser nuestro.
        let graph = NonNull::new(unsafe { ffi::avfilter_graph_alloc() })
            .ok_or_else(|| "sin memoria para el grafo de filtros".to_owned())?;
        // A partir de aquí, `Drop` libera el grafo (y sus filtros) si algo falla.
        let mut this = Self {
            graph,
            src: ptr::null_mut(),
            sink: ptr::null_mut(),
            chain: chain.to_owned(),
        };
        let buffer = filter(c"buffer")?;
        let buffersink = filter(c"buffersink")?;
        // SAFETY: `graph` es válido; `avfilter_graph_alloc_filter` crea el filtro dentro del grafo
        // (lo libera el grafo).
        this.src = unsafe {
            ffi::avfilter_graph_alloc_filter(this.graph.as_ptr(), buffer, c"entrada".as_ptr())
        };
        if this.src.is_null() {
            return Err("no se pudo crear la entrada del grafo".to_owned());
        }
        // SAFETY: `av_buffersrc_parameters_alloc` reserva unos parámetros con valores por defecto
        // que se rellenan y se liberan con `av_free` después de aplicarlos; `buffer` toma su propia
        // referencia del contexto de *hw frames* (no consume la de `frames`).
        unsafe {
            let params = ffi::av_buffersrc_parameters_alloc();
            if params.is_null() {
                return Err("sin memoria para los parámetros del grafo".to_owned());
            }
            (*params).format = format;
            (*params).width = i32::try_from(width).unwrap_or(i32::MAX);
            (*params).height = i32::try_from(height).unwrap_or(i32::MAX);
            (*params).time_base = ffi::AVRational { num: 1, den: 1000 };
            (*params).hw_frames_ctx = frames.as_ptr() as *mut ffi::AVBufferRef;
            // RGB de rango completo con la matriz BT.709 de destino, lo mismo que llevan
            // los fotogramas (`import::GpuImporter::prepare`): `scale_vulkan` saca de aquí los
            // coeficientes de la conversión a YUV.
            (*params).color_space = ffi::AVCOL_SPC_BT709;
            (*params).color_range = ffi::AVCOL_RANGE_JPEG;
            let ret = ffi::av_buffersrc_parameters_set(this.src, params);
            ffi::av_free(params.cast());
            check("parámetros de la entrada del grafo", ret)?;
            check(
                "entrada del grafo",
                ffi::avfilter_init_str(this.src, ptr::null()),
            )?;
            check(
                "salida del grafo",
                ffi::avfilter_graph_create_filter(
                    &mut this.sink,
                    buffersink,
                    c"salida".as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    this.graph.as_ptr(),
                ),
            )?;
        }
        this.link(&chain_c)?;
        // SAFETY: el grafo está completo (entrada, cadena y salida enlazadas).
        check("configuración del grafo", unsafe {
            ffi::avfilter_graph_config(this.graph.as_ptr(), ptr::null_mut())
        })
        .map_err(|err| format!("{err} (cadena «{chain}»)"))?;
        Ok(this)
    }

    /// Enlaza `entrada → chain → salida` con `avfilter_graph_parse_ptr`.
    fn link(&mut self, chain: &CStr) -> Result<(), String> {
        // SAFETY: `outputs` describe la salida libre de `entrada` y `inputs` la entrada libre de
        // `salida`, como pide `avfilter_graph_parse_ptr`; los nombres se duplican con `av_strdup`
        // porque `avfilter_inout_free` los libera; las dos listas se liberan siempre (aunque sean
        // nulas tras el análisis).
        unsafe {
            let mut outputs = ffi::avfilter_inout_alloc();
            let mut inputs = ffi::avfilter_inout_alloc();
            let ret = if outputs.is_null() || inputs.is_null() {
                ffi::AVERROR(ffi::ENOMEM)
            } else {
                (*outputs).name = ffi::av_strdup(c"in".as_ptr());
                (*outputs).filter_ctx = self.src;
                (*outputs).pad_idx = 0;
                (*outputs).next = ptr::null_mut();
                (*inputs).name = ffi::av_strdup(c"out".as_ptr());
                (*inputs).filter_ctx = self.sink;
                (*inputs).pad_idx = 0;
                (*inputs).next = ptr::null_mut();
                ffi::avfilter_graph_parse_ptr(
                    self.graph.as_ptr(),
                    chain.as_ptr(),
                    &mut inputs,
                    &mut outputs,
                    ptr::null_mut(),
                )
            };
            ffi::avfilter_inout_free(&mut inputs);
            ffi::avfilter_inout_free(&mut outputs);
            check("cadena de filtros", ret)
        }
    }

    /// Contexto de *hw frames* de la salida (`None` si la cadena termina en memoria de CPU).
    pub(crate) fn output_frames(&self) -> Option<AVHWFramesContext> {
        // SAFETY: `sink` es un `buffersink` configurado; `av_buffersink_get_hw_frames_ctx`
        // devuelve una referencia prestada (o nulo) y `av_buffer_ref` crea una propia.
        let raw = unsafe {
            let borrowed = ffi::av_buffersink_get_hw_frames_ctx(self.sink);
            if borrowed.is_null() {
                return None;
            }
            ffi::av_buffer_ref(borrowed)
        };
        // SAFETY: `raw` es una referencia nueva a un `AVHWFramesContext`, que pasa a ser nuestra.
        NonNull::new(raw).map(|raw| unsafe { AVHWFramesContext::from_raw(raw) })
    }

    /// Mete un fotograma (se queda con una referencia nueva; `frame` sigue siendo del llamante) y
    /// devuelve los que salgan.
    pub(crate) fn run(&mut self, frame: &mut AVFrame) -> Result<Vec<AVFrame>, String> {
        let flags = super::BUFFERSRC_KEEP_PUSH;
        // SAFETY: `src` es el `buffer` de este grafo y `frame` un fotograma válido del formato y
        // contexto con que se configuró; con `KEEP_REF` el filtro toma referencias propias.
        check("entrada del grafo", unsafe {
            ffi::av_buffersrc_add_frame_flags(self.src, frame.as_mut_ptr(), flags)
        })?;
        let mut out = Vec::new();
        loop {
            let mut next = AVFrame::new();
            // SAFETY: `sink` es el `buffersink` de este grafo y `next` un fotograma vacío nuestro.
            let ret = unsafe { ffi::av_buffersink_get_frame(self.sink, next.as_mut_ptr()) };
            if ret == ffi::AVERROR(ffi::EAGAIN) || ret == ffi::AVERROR_EOF {
                return Ok(out);
            }
            check("salida del grafo", ret)?;
            out.push(next);
        }
    }
}

impl Drop for HwFilter {
    fn drop(&mut self) {
        let mut graph = self.graph.as_ptr();
        // SAFETY: el grafo es nuestro; `avfilter_graph_free` libera también sus filtros y deja el
        // puntero a nulo. No se vuelve a usar.
        unsafe { ffi::avfilter_graph_free(&mut graph) };
    }
}
