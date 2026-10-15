# vendor/

Copias de crates de terceros con cambios propios, enlazadas con `[patch.crates-io]` en el
`Cargo.toml` del workspace (y fuera de él: `exclude = ["vendor"]`). Cada una conserva su licencia.

| Copia        | Origen (crates.io) | Licencia                     | Por qué                                  |
|--------------|--------------------|------------------------------|------------------------------------------|
| `iced_wgpu/` | `iced_wgpu` 0.14.0 | MIT (`LICENSE`)              | `iced` 0.14 depende de `wgpu` 27; coroom usa `wgpu` 30 |
| `cryoglyph/` | `cryoglyph` 0.1.0  | MIT o Apache-2.0 o Zlib      | el texto de `iced_wgpu`, también en `wgpu` 27 |

## Por qué hacen falta

La ventana (`docs/ARQUITECTURA.md` §3, fila «UI») compone el vídeo y la interfaz **con el mismo
`wgpu::Device`**: el dispositivo lo abre `coroom-render` con las colas y
extensiones de Vulkan Video para importar sin copias las imágenes del decodificador. Eso obliga a
que `iced_wgpu` y el presentador usen la misma versión de `wgpu` (30, la que fija el resto del
workspace). `iced` 0.14.0 (diciembre de 2025, la última publicada) pide `wgpu` 27; la rama
principal de `iced` ya sigue a `wgpu` más reciente, pero no hay versión publicada.

## Qué cambia

Solo la adaptación a la API de `wgpu` 30 (los parches completos están en `patches/`, contra el
código publicado en crates.io):

- `PipelineLayoutDescriptor`: `push_constant_ranges` → `immediate_size: 0` y
  `bind_group_layouts` con `Option`.
- `RenderPipelineDescriptor`: `multiview` → `multiview_mask`; `vertex.buffers` con `Option`;
  `RenderPassDescriptor` con `multiview_mask: None`.
- `SamplerDescriptor`: `mipmap_filter` con `MipmapFilterMode`.
- `util::StagingBelt::new(device, tamaño)`, y `write_buffer`/`allocate` sin dispositivo.
- `get_mapped_range{,_mut}` devuelven `Result`; `BufferViewMut` se escribe por `slice`.
- `InstanceDescriptor` por valor (`new_without_display_handle`), `enumerate_adapters` asíncrono y
  `Surface::get_current_texture` → `CurrentSurfaceTexture` con `Queue::present` (solo en el
  compositor de ventanas de `iced_wgpu`, que coroom no usa).
- `iced_wgpu`: `resvg` 0.48 sin la función `text` (sin `fontdb` ni `rustybuzz`, que `cargo deny`
  marca sin mantenimiento); los SVG de iconos no llevan texto.
- `cryoglyph`: `lru` 0.18.5 (la 0.16 tiene un aviso de RustSec) y sin ejemplos ni benches.

Ningún cambio de comportamiento: los shaders son los mismos.

## Cuándo se quitan

Cuando salga un `iced` publicado sobre `wgpu` ≥ 30: borrar `vendor/`, el `[patch.crates-io]` y el
`exclude`, subir `iced_*` en el workspace y pasar `cargo deny check`.

Última comprobación: **12 de octubre de 2026**, en crates.io: `iced` e `iced_wgpu` siguen
en 0.14.0 (7 de diciembre de 2025), sobre `wgpu` 27, así que `vendor/` se queda en la 1.0. Para
volver a comprobarlo:

```bash
curl -s https://crates.io/api/v1/crates/iced_wgpu | python3 -c \
  "import sys, json; print(json.load(sys.stdin)['crate']['max_stable_version'])"
```
