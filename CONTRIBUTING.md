# Contribuir a coroom

Gracias por echar una mano. Lo que sigue es lo práctico para compilar, probar y mandar cambios.

## Antes de empezar

- Las decisiones de arquitectura (pila, presupuesto de latencia, cómo se conectan los clientes,
  estructura y reglas de capas) están en [`docs/ARQUITECTURA.md`](docs/ARQUITECTURA.md), y el
  modelo de amenazas y la revisión de seguridad, en [`docs/seguridad.md`](docs/seguridad.md).
- Un cambio grande (una pantalla nueva, un mensaje nuevo del protocolo, una dependencia) se discute
  antes en un issue: qué se quiere conseguir, cómo se comprueba y, si es una pantalla, su boceto.
  Las pantallas nuevas se revisan con captura.
- [`CLAUDE.md`](CLAUDE.md) resume las convenciones del repositorio para quien toque el código.

## Compilar

Rust estable (lo fija `rust-toolchain.toml`) y, en Linux, las dependencias de sistema de la CI:

```bash
sudo apt-get install libpipewire-0.3-dev libclang-dev pkg-config mesa-vulkan-drivers \
  libxkbcommon-dev libwayland-dev libva2 libva-drm2 libvpl2 libudev-dev cmake meson ninja-build nasm
tools/fetch-meson-wraps.sh     # abseil para el procesado de voz de WebRTC
tools/fetch-ffmpeg.sh          # FFmpeg 8 LGPL fijado en tools/ffmpeg.lock (ver el README)
cargo build
```

En Windows: `tools/fetch-ffmpeg.ps1`, NASM y LLVM (`LIBCLANG_PATH`). Los detalles de FFmpeg, de
las GPU y de los paquetes están en el [`README.md`](README.md).

## Antes de mandar un cambio

Lo mismo que la CI, y todo en verde:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

Según lo que toques:

- **Criptografía**: `cargo test -p coroom-crypto --features pq-sign` y las puertas de coste,
  `cargo test --release -p coroom-crypto -- --ignored`. Una etiqueta de dominio nueva se registra
  en `crates/crypto/src/domains.rs` (el test falla si no).
- **Algo que lee datos de la red**: añade o amplía su objetivo en `fuzz/` y sus semillas
  (`cargo test -p coroom-session --test fuzz_seeds -- --ignored`); comprueba que compila con
  `cargo check --manifest-path fuzz/Cargo.toml --target-dir target --bins` y, si tienes nightly,
  ejecútalo un rato: `cargo +nightly fuzz run --target-dir target/fuzz <objetivo>
  fuzz/corpus/<objetivo> fuzz/seeds/<objetivo> -- -max_total_time=60`. Un fallo del fuzzer se
  corrige con su test de regresión.
- **Medios o control de tasa**: `cargo test -p coroom --test matrix -- --nocapture` (la matriz
  con pérdida y jitter en proceso) y, con `tc` y `sudo`, `netem_matrix`.
- **Memoria o hilos**: `COROOM_SOAK_MINUTES=15 cargo test --release -p coroom --test soak --
  --ignored --nocapture`, con `GLIBC_TUNABLES` como dice `apps/desktop/tests/soak.rs` (si no, los
  escalones del asignador de glibc parecen una fuga).
- **Interfaz**: `cargo test -p coroom --test shots` (las capturas, en `COROOM_SHOTS_DIR`),
  `--test focus` (orden de foco) y `--test l10n` (textos en el catálogo).

## Estilo

- Rust estable, edición 2024; `unsafe` solo en los backends nativos, con un comentario
  `// SAFETY:` que explique la invariante. Sin `unwrap()` fuera de los tests: errores con su
  causa.
- **Nombres de código, módulos y endpoints en inglés; todo lo que ve el usuario en español**, con
  frases completas, la causa y lo que puede hacer («no se pudo abrir la sala: el servidor no
  responde; comprueba la dirección en Ajustes → Cuenta y servidor»).
- Los textos de la aplicación van en `apps/desktop/locales/es/*.ftl` (uno por pantalla) y se usan con
  `t!("id")` o `t!("id", nombre = valor)`; el test `l10n` falla si queda un texto suelto o un
  identificador sin usar. Para traducir coroom, copia los catálogos a `locales/<idioma>/` y añádelos
  en `apps/desktop/src/l10n.rs`.
- Cada backend de plataforma va tras un trait y una `feature`; `cfg(target_os)` solo dentro del
  backend.
- Dependencias: mantenidas (nada descontinuado: es el principio rector), con versión fijada con
  `=` en el `Cargo.toml` del workspace y pasando `cargo deny check`. Si cambia `Cargo.lock`, hay
  que regenerar `packaging/flatpak/cargo-sources.json` (la CI de Flatpak lo comprueba).
- Tests: unitarios junto al código, de integración en `tests/` de cada crate, `proptest` en
  `proto`. Toda latencia se mide (marcas por etapa), no se estima; las cotas de tiempo de reloj de
  los tests escalan con `common::load_factor()`.

## Privacidad (obligatorio)

El repositorio es público: nunca correos ni nombres de personas reales, dominios, IP, puertos
expuestos ni rutas de servidores reales, contraseñas, claves, tokens o certificados. Usa
`coroom.example.com`, `admin@example.com`, `change-me` y las IP de documentación (`192.0.2.0/24`,
`198.51.100.0/24`, `203.0.113.0/24`); los valores reales del despliegue viven solo en `deploy/.env`.

## Commits

En imperativo y descriptivos («Limitar los intentos de códigos de sala por cuenta»), sin
identificadores de modelos de IA.

La versión no se edita a mano: la calcula GitVersion (`GitVersion.yml`) a partir del historial y
la CI la escribe en el workspace al compilar. Cada push a `main` que pasa la CI publica una versión
nueva con el parche subido (1.0.0 → 1.0.1), su etiqueta `vX.Y.Z` y su release con los paquetes.
Para subir menor o mayor, incluye en el mensaje de un commit `+semver: minor` o `+semver: major`
(`+semver: none` no publica incremento). `Cargo.toml` lleva `1.0.0` como base y en local los
binarios dicen esa versión; `CHANGELOG.md` se actualiza con lo que cambia cada versión.
