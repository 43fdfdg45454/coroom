# CLAUDE.md — Lineamientos del proyecto coroom

coroom: juego remoto con amigos (tipo Parsec). Cliente de escritorio en Rust para Linux (Wayland) y
Windows; servidor autoalojado en Rust con HTTP/3, WebTransport y relay QUIC; E2EE con MLS, SFrame y
HPKE. Las decisiones de arquitectura y el presupuesto de latencia están en `docs/ARQUITECTURA.md`;
el modelo de amenazas y la revisión de seguridad, en `docs/seguridad.md`.

## Principio rector

**Nada descontinuado.** Siempre la opción más nueva y óptima respaldada por un estándar (RFC) o un
proyecto activo, con respaldo declarado cuando dependa de drivers (tabla en
`docs/ARQUITECTURA.md` §3). Antes de añadir una dependencia: comprobar que está mantenida, fijar
versión y pasar `cargo deny`.

## Interfaz de usuario

Las interfaces deben sentirse **de Linux** (convenciones de GNOME y KDE: tema claro y oscuro del
sistema, color de acento, tipografía y espaciado del escritorio, atajos de teclado habituales,
Wayland nativo, HiDPI, diálogos y notificaciones por los portales), con **buena UX** (lo importante
a un clic, estados vacíos y errores explicados en español, nada que bloquee sin decir por qué,
indicadores de latencia y conexión siempre visibles) y **UI bonita** (jerarquía visual clara,
animaciones breves, nada de widgets por defecto sin estilo). En Windows, la misma interfaz con el
tema del sistema. Toda pantalla nueva se diseña antes de implementarse (boceto en su issue) y se
revisa con captura de pantalla.

## Regla de privacidad (obligatoria)

Este repositorio es público. **Nunca** incluir en código, documentación, tests, logs de ejemplo,
commits ni issues: correos o nombres de personas reales; dominios, IP, puertos expuestos o rutas de
servidores reales; contraseñas, claves, tokens, certificados o cadenas de conexión. Usar valores
ficticios (`coroom.example.com`, `admin@example.com`, `change-me`) y mantenerlos en
`deploy/.env.sample`. Los valores reales viven solo en `deploy/.env` (en `.gitignore`).

## Estructura

```
crates/proto      cabeceras de medios y mensajes de control; sin IO
crates/crypto     identidad, MLS, SFrame, HPKE; benches con umbral
crates/transport  quinn, datagramas, FEC, NACK, control de tasa, NAT, cliente relay
crates/capture    trait Capturer; backends wgc (Windows) y pipewire (Linux)
crates/codec      traits Encoder/Decoder; FFmpeg con Vulkan Video y respaldos
crates/audio      PipeWire / WASAPI, Opus, jitter, mezcla, procesado de voz (WebRTC)
crates/input      captura en el invitado; inyección libei / uinput / SendInput / HID virtual
crates/render     dispositivo wgpu (Vulkan), capa de vídeo, importación sin copias, cursor local
crates/session    máquinas de estado anfitrión/invitado, permisos, reloj
crates/ui         sistema de diseño para iced (coroom-ui): tokens, tema, fuentes, iconos, componentes
apps/desktop      binario `coroom` (anfitrión + invitado), aplicación iced en src/ui
apps/desktop/locales/  catálogos Fluent de los textos de la aplicación
vendor/           iced_wgpu y cryoglyph parcheados a wgpu 30 (ver vendor/README.md)
apps/server       binario `coroom-server` (h3 + hyper/TLS + relay + sqlite + ACME + /admin)
drivers/windows-vhid  driver KMDF de mandos virtuales (Virtual HID Framework)
deploy/           Dockerfile, docker-compose.yml, .env.sample, README
packaging/        flatpak/, msix/
docs/             ARQUITECTURA.md, seguridad.md, guia.md, screenshots/
fuzz/             objetivos de cargo-fuzz (workspace aparte) y sus semillas
tools/            scripts netem (la prueba de latencia es `coroom probe`)
```

Dependencias entre crates (`docs/ARQUITECTURA.md` §5): `proto` y `crypto` no dependen del
workspace; `transport` solo de ellas; `session` orquesta el resto; `ui` no depende del workspace;
`apps/*` solo de `session`, `render`, `transport` y `ui`.

## Comandos

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo run -p coroom-server                 # HTTP/3 en UDP 4433 y HTTP/2 con TLS en TCP 8443 (desarrollo, autofirmado)
cargo run -p coroom-server -- backup data/copia.db       # copia en línea de la base (también admin reset-password NOMBRE, migrate)
cargo run -p coroom                        # la aplicación (COROOM_DATA_DIR=dir para un perfil aparte)
cargo run -p coroom -- doctor              # sistema, backends de captura, adaptador gráfico
cargo run --release -p coroom -- probe --headless --frames 600   # prueba de latencia local por etapas
cargo run -p coroom -- login --server https://localhost:4433 --trust-fingerprint <sha256> --tcp-port 8443 [--invitation CÓDIGO]
cargo run -p coroom -- whoami              # también logout, friends list|add|accept|remove NOMBRE, invite NOMBRE --code CÓDIGO
cargo run --release -p coroom -- host --source synthetic [--invite NOMBRE]        # con la cuenta de `coroom login`
cargo run --release -p coroom -- join [CÓDIGO]                                     # sin código, espera invitaciones
COROOM_DEV_TOKEN=change-me cargo run -p coroom-server            # modo desarrollo: host/join aceptan --dev-token change-me sin cuenta
cd deploy && cp .env.sample .env && docker compose up -d
```

## Convenciones

- Rust estable, edición 2024, sin `nightly`. `unsafe` solo en backends nativos, con comentario
  `// SAFETY:` que explique la invariante.
- Nombres de código, módulos y endpoints en inglés; textos visibles al usuario en español
  (i18n desde el principio, `apps/desktop/locales/`).
- Cada backend de plataforma tras un trait y una `feature` de Cargo; `cfg(target_os)` solo dentro
  del backend.
- Toda latencia es medible: las etapas del pipeline llevan marcas de tiempo y los umbrales de
  `docs/ARQUITECTURA.md` §2 se comprueban con benches y tests, no con estimaciones.
- Tests: unitarios junto al código; integración en `tests/` de cada crate; `proptest` en `proto`;
  benches `criterion` con umbral en `crypto`.
- Toolchain fijado en `rust-toolchain.toml`: CI, Dockerfile y desarrollo usan la misma versión; se
  sube cuando sale una stable nueva y pasa clippy.
- Licencias: Apache-2.0 salvo `apps/server` (AGPL-3.0-or-later).
- Commits en imperativo, descriptivos, sin identificadores de modelos de IA.
- **Versionado con GitVersion** (`GitVersion.yml`): la CI calcula `X.Y.Z` en cada push a `main`,
  crea la etiqueta `vX.Y.Z` y publica la release con los paquetes. Para subir menor o mayor,
  `+semver: minor` / `+semver: major` en el mensaje de un commit. No editar la versión a mano
  (`Cargo.toml` lleva `1.0.0` como base; la CI la sustituye al compilar).
