# Arquitectura de coroom

## 1. Propósito y alcance

coroom sirve para jugar en remoto con amigos, al estilo de Parsec: un anfitrión ejecuta un juego en
su PC y los invitados, desde el suyo, ven el vídeo y oyen el audio del juego con la mínima latencia
posible y manejan teclado, ratón o mandos virtuales en el equipo del anfitrión; opcionalmente hablan
por voz dentro de la sala. Hay un único cliente de escritorio en Rust (modo anfitrión y modo
invitado) para Linux, con Wayland nativo, y Windows 11 (Windows 10 22H2 como mínimo), y un servidor
que aloja el propio usuario con Docker: cuentas, amigos, salas, señalización y relay en un solo
binario que habla HTTP/3 y WebTransport sobre un único puerto UDP. Vídeo, audio, voz y entrada van
cifrados de extremo a extremo (MLS, SFrame y HPKE): el servidor nunca tiene las claves, ni siquiera
cuando retransmite. Quedan fuera macOS, móvil, cliente web, grabación, chat de texto, portapapeles
remoto, transferencia de ficheros, escritorio remoto de uso general y salas públicas.

## 2. Presupuesto de latencia

### 2.1 Máximo teórico para una buena experiencia

Lo que cuenta es la latencia que el streaming **añade** respecto a jugar delante del anfitrión
(jugar en local ya cuesta 20-40 ms). La regla práctica a 60 Hz es **sobrecoste ≈ RTT + 7 ms** con
el pipeline afinado y **≈ RTT + 25 ms** en el caso típico; a 120 Hz la captura, el juego y la
presentación se dividen por dos y el pipeline propio baja de ~25 ms a ~13 ms.

| Nivel | Sobrecoste añadido | RTT máx. (pipeline típico, 25 ms) | RTT máx. (pipeline afinado, 10 ms, 120 Hz) |
|---|---:|---:|---:|
| Excelente (shooters, lucha, ritmo) | ≤ 40 ms | ≤ 15 ms | ≤ 30 ms |
| **Buena** (acción, plataformas, cooperativo) | **≤ 70 ms** | **≤ 45 ms** | **≤ 60 ms** |
| Aceptable (estrategia, turnos, casual) | ≤ 110 ms | ≤ 85 ms | ≤ 100 ms |
| Mala | > 110 ms | — | — |

**Máximo para una buena experiencia**: RTT ≈ 50 ms entre anfitrión e invitado; 1080p60 en AV1 a
8-13 Mbit/s (HEVC 10-17, H.264 15-24; 1440p60 con ≥ 25 Mbit/s por invitado); 4 jugadores con mando
y 4 espectadores por sala (8 jugadores con relay y juegos que lo admitan); voz hasta 16
participantes; sobrecoste total ≤ 70 ms. La aplicación mide el RTT y muestra el nivel esperado
antes de entrar en la sala. La subida útil del anfitrión debe ser ≥ 4× el bitrate de un flujo, y
se usa *intra-refresh* en lugar de keyframes (un IDR pesa 3-5 fotogramas).

### 2.2 Presupuesto por etapa (glass-to-glass, 1080p60, en ms)

| Etapa | Mejor caso (LAN) | Típico (Internet, RTT 30 ms) |
|---|---:|---:|
| Muestreo de entrada en el invitado (ratón 1000 Hz / mando 250 Hz) | 0,5 | 2 |
| Cifrado y envío del paquete de entrada | 0,1 | 0,1 |
| Red invitado → anfitrión | 0,5 | 15 |
| Inyección (libei / uinput / `SendInput`) y lectura por el juego | 0,5 | 1 |
| Juego (simulación, render, present; existe también en local) | 16,7 | 25 |
| Captura (WGC / PipeWire) | 1 | 8 |
| Codificación por hardware (sin B-frames) | 2 | 4 |
| Empaquetado, cifrado y serialización en la subida | 0,3 | 4 |
| Red anfitrión → invitado | 0,5 | 15 |
| Buffer de jitter | 0 | 3 |
| Decodificación por hardware | 1,5 | 3 |
| Presentación (0 con VRR o modo inmediato) | 0 | 8 |
| Respuesta del monitor | 2 | 5 |
| **Total** | **≈ 26** | **≈ 93** |
| **Sobrecoste del streaming** | **≈ 6-7** | **≈ 53 (30 son RTT)** |

Audio y voz: Opus en tramas de 10 ms, buffer de jitter adaptativo de 20-60 ms, sincronía A/V con
|A−V| < 40 ms y voz boca-oído en LAN < 80 ms. La reproducción es **a demanda** en PipeWire: el
callback del dispositivo pide cada trama justo cuando la necesita (sin cola intermedia, que añadía
una trama), y el buffer de jitter se queda en su objetivo sin una trama de más. WASAPI sigue con la
cola de una trama. Los hilos de audio propios (recepción y descifrado en `audio-net`, fuera del
runtime de la red; captura; reproducción con cola) piden prioridad de tiempo real: `SCHED_FIFO`,
RealtimeKit o el portal Realtime en Linux, MMCSS «Pro Audio» en Windows. El cifrado no entra como etapa apreciable: con las dos
capas (SFrame y QUIC) el peor caso queda por debajo de 0,2 ms por fotograma.

### 2.3 Cómo se mide

- **Marcas por etapa**: cada fotograma lleva un id y ocho marcas (captura, codificado, cifrado,
  enviado, recibido, descifrado, decodificado, presentado) en `crates/session` (`latency`). El
  anfitrión envía las suyas por el control (`FrameTiming`) y el invitado las sitúa en su reloj con
  la diferencia de reloj estimada con `Ping`/`Pong`. El overlay y el panel de estadísticas muestran el desglose.
- **`coroom probe`**: prueba local por etapas sin red ni juego
  (`cargo run --release -p coroom -- probe --headless --frames 600`, con `--codec`, `--encoder`,
  `--decoder` y `--json`); objetivo p95 total < 25 ms a 1080p60. La CI aplica una cota de regresión
  de 80 ms a 720p.
- **Benches con umbral** en `crates/crypto` (falla el build si se superan): SFrame cifrar + descifrar
  un fotograma de 128 KB < 150 µs; HPKE sellar + abrir < 1 ms; `commit` MLS con 8 miembros < 20 ms;
  cifrado de la entrada < 5 µs.
- **Tests de extremo a extremo** en proceso: sesión remota directa, por relay y con pérdida
  simulada; A/B con SFrame activado y desactivado (diferencia del p50 total < 0,5 ms; la opción de
  desactivarlo solo existe en builds de desarrollo); ratón a 1000 Hz sin pérdida; matriz de red con
  perfiles `netem` (`tools/netem.sh`) y prueba de larga duración (`soak`).
- **Modo destello**: el anfitrión dibuja un marcador y se mide hasta la presentación en el invitado
  (verificable con fotodiodo).

## 3. Decisiones de la pila

Principio: la opción más nueva respaldada por un estándar o un proyecto activo, nada descontinuado,
y un respaldo declarado cuando dependa de drivers o de borradores.

| Tema | Decisión | Respaldo cuando falta la capacidad |
|---|---|---|
| Transporte | **QUIC** (`quinn`): datagramas (RFC 9221) para vídeo, audio y entrada; streams para control y MLS; ECN/L4S (RFC 9330); TLS 1.3 con claves públicas crudas (RFC 7250) fijadas a la identidad e intercambio híbrido X25519MLKEM768 (`rustls` + `aws-lc-rs`) | X25519 puro si el híbrido no se negocia |
| Señalización y API | **HTTP/3** (`h3`) y **WebTransport** en el mismo puerto UDP; sin WebSocket | HTTP/2 con TLS por TCP con `Alt-Svc`; streams QUIC propios con los mismos mensajes si WebTransport no casa |
| Protocolo de medios | Propio y mínimo con el esquema de **Media over QUIC** (pistas, grupos, objetos) y cabecera binaria fija; control con `postcard` | Es ya el respaldo: se adoptará `moq-transport` cuando el IETF lo cierre |
| FEC, NACK y control de tasa | FEC **RaptorQ** (RFC 6330) adaptativa a la pérdida, NACK con caché, cargas de 1200 B; control tipo **SCReAM** (RFC 8298) con controlador de congestión propio en `quinn`, informe cada 50 ms, cola medida con el mínimo del OWD de 500 ms (y vía rápida con un informe para colas ≥ 50 ms), crecimiento acotado a 1,25 × lo que el receptor recibe, espera de 1 s tras cada recorte, umbrales de pérdida de GCC; arranque en el 70 % de lo que mide la **sonda del enlace** (dos trenes de 48 datagramas por invitado al conectar, dispersión medida por el receptor; 8 Mbit/s si no llega) ; FEC descontada del bitrate del codificador; VBV de un fotograma que sigue al bitrate | Sin ECN, la cola se estima por OWD; escalera de resolución/fps bajo el suelo del modo |
| NAT y relay | Candidatos (IPv6 primero, reflejado, locales) y apertura simultánea QUIC desde un solo socket | Relay QUIC en el servidor, transparente para los medios |
| Criptografía | **MLS** (RFC 9420, `openmls`) para la sala, **SFrame** (RFC 9605, AES-256-GCM) por fotograma y trama, **HPKE** (RFC 9180) con **X-Wing** (X25519 + ML-KEM-768) para la entrada; identidad Ed25519; firma híbrida Ed25519 + **ML-DSA-65** tras la `feature` `pq-sign` (desactivada por defecto) | ChaCha20-Poly1305 si la CPU no acelera AES; X-Wing **sin** respaldo a X25519 (la oferta manipulada se rechaza) |
| Captura Linux | **PipeWire** por el portal `ScreenCast` (`ashpd`), DMA-BUF con modificadores explícitos, *restore token*; sin XShm | Copia a CPU si la importación sin copias falla |
| Captura Windows | **Windows.Graphics.Capture** con las mejoras de Windows 11 24H2 (regiones sucias, sin borde); sin DXGI Desktop Duplication | WGC básico en versiones anteriores |
| Códecs y negociación | **Vulkan Video** por FFmpeg 8 (`h264_vulkan`, `hevc_vulkan`, `av1_vulkan`); orden **AV1 → HEVC → H.264** por capacidades reales de ambos extremos, sondeadas al arrancar (`coroom doctor`); baja latencia (IPPP, una referencia, CBR, intra-refresh, 4 slices) | NVENC/NVDEC, AMF, QSV, VA-API en el mismo FFmpeg; software SVT-AV1, dav1d, OpenH264. FFmpeg LGPL dinámico, sin GPL |
| Sin copias | DMA-BUF → `VkImage` en Linux; textura D3D11 de WGC al pool D3D11 del codificador en Windows; decodificación en el `VkDevice` de `wgpu` | Respaldo a CPU en cada paso, contado en el resumen y el overlay |
| Audio | Linux: **PipeWire** nativo. Windows: **WASAPI** con loopback por proceso. **Opus** (libopus 1.6.1) a 48 kHz, tramas 10 ms, DRED en Linux y FEC en banda; voz con `webrtc-audio-processing` (NS, AGC2, AEC3) y VAD propio | Loopback de la salida (`--audio system`); FEC en banda y PLC sin DRED; puerta de ruido sin AEC (auriculares) en Windows |
| Entrada | Linux: **libei** por el portal `RemoteDesktop` (`reis`), mandos convertidos por **uinput** y **tal cual** por **UHID** (el mando del invitado con su VID/PID y su descriptor HID, leído en el invitado por `hidraw` en Linux o `hidapi` en Windows; protocolo 8: `PadData`, `GamepadMode`, `GamepadOutput`). Windows: `SendInput` y driver propio de mandos en modo usuario, **UMDF2** (`drivers/windows-vhid/`): mandos tal cual como fuente de la **Virtual HID Framework** (`vhf.sys`) y convertidos a cuatro **Xbox 360** de XInput (la interfaz XUSB de `xusb22`); sin firma de prueba ni tocar el arranque seguro o HVCI | Aviso si falta libei (uinput no depende de él); mando a teclas (`--gamepad-to-keys`) sin driver |
| Render | `wgpu` con backend Vulkan para importar las texturas decodificadas, `winit` con Wayland nativo; BT.709 y HDR10 (BT.2020 PQ) con superficie `Rgba16Float`; 4:4:4 y 120 fps cuando todos lo admiten | D3D12 en Windows si la importación lo exige; *tone mapping* a SDR sin superficie HDR |
| UI | `iced` 0.14 a bajo nivel (`iced_winit`, `iced_wgpu`) sobre el mismo `wgpu::Device` que el vídeo; sistema de diseño `coroom-ui` de estilo Adwaita con tema, acento y fuente del sistema; textos en Fluent | Fuente Inter embebida si no hay la del sistema |
| Servidor | `coroom-server`: `h3` y relay en el mismo socket (ALPN), `hyper` + `tokio-rustls` para TCP, **SQLite** (`sqlx`), certificado del operador en PEM (o autofirmado), imagen distroless; sin gestión de certificados ni dependencia de internet | — |
| Empaquetado | **Flatpak** (runtime freedesktop 26.08, portales declarados), **MSIX** firmado y `.zip` portátil, imagen multiarquitectura en GHCR; publicación automática en cada push a `main` con la versión que calcula GitVersion (etiqueta `vX.Y.Z` y release con los paquetes) | — |
| Observabilidad | `tracing` y overlay de estadísticas con marcas por etapa; sin telemetría externa | — |

## 4. Cómo se conectan los clientes

```
Invitado ── HTTP/3 + WebTransport ──▶ Servidor (UDP) ◀── HTTP/3 + WebTransport ── Anfitrión
   │                                  API, /admin, señalización,                       │
   │                                  relay (solo texto cifrado)                       │
   └════════════════ QUIC directo, o QUIC ↔ relay ↔ QUIC si falla ═════════════════════┘
```

- **Un solo puerto UDP**: el socket QUIC del servidor demultiplexa por ALPN entre `h3` (API REST,
  interfaz de administración y sesiones WebTransport de señalización en `/signal`) y
  `coroom-relay/1` (medios). TCP con HTTP/2 y TLS queda para clientes sin HTTP/3 y para anunciar
  `Alt-Svc`. Con Docker, los puertos del host son los de `COROOM_PUBLIC_UDP_PORT` y
  `COROOM_PUBLIC_TCP_PORT` (443 por defecto); en desarrollo, UDP 4433 y TCP 8443.
- **Señalización**: sesión WebTransport autenticada que transporta candidatos de red, claves
  públicas y mensajes MLS/HPKE cifrados para sus destinatarios; el servidor solo enruta y nunca
  interpreta MLS ni medios.
- **Conexión directa**: la señalización sale del mismo `quinn::Endpoint` que los medios, así el
  servidor devuelve la dirección reflejada (`Observed`) del socket real. Cada par intercambia
  candidatos y lanza en paralelo conexiones a todos (IPv6 primero, luego reflejado, luego locales)
  mientras acepta entrantes, con un tiempo de apertura de 3 s; gana la primera conexión con la
  identidad verificada.
- **Relay de respaldo**: si ninguna llega, el par se conecta al relay con un `RelayToken`. El relay
  termina QUIC salto a salto, reenvía los objetos leyendo solo la cabecera en claro (pista, grupo,
  secuencia, marca de intra), abre en abanico el flujo del anfitrión a N invitados y responde NACK
  desde una caché corta por sala. Tiene cuotas por cuenta y por sala y el operador decide si lo
  habilita. QUIC migra la ruta al cambiar de red y el invitado reconecta solo.
- **Cuentas por servidor**: sin registro público. El primer administrador se crea en `/admin` en el
  primer arranque; después, según `COROOM_REGISTRATION`, el administrador emite códigos de
  invitación de un solo uso que caducan a las 72 h (`invite`, por defecto) o da de alta él mismo
  (`admin`). Contraseñas con Argon2id; tokens de sesión opacos de 30 días, uno por dispositivo.
- **Identidad de dispositivo**: par Ed25519 generado en el primer arranque y guardado en el llavero
  del sistema (o sellado con el portal `Secret` en Flatpak). Es la credencial MLS y la clave cruda
  del certificado QUIC; el dispositivo se vincula a la cuenta firmando un desafío y solo las claves
  vinculadas abren señalización y relay. Dos amigos verifican sus claves con un número de seguridad.
- **Amigos y salas**: amistad por solicitud, con presencia; una sala tiene un anfitrión y N
  invitados, se abre con un código corto temporal o con invitación a amigos, y es un grupo MLS.
  Permisos por invitado (ranura de mando, teclado y ratón, solo mirar), aprobación de entrada,
  pausa, expulsión (época MLS nueva) y voz activable.

## 5. Estructura del repositorio

| Ruta | Contenido |
|---|---|
| `crates/proto` | Cabeceras de medios y mensajes de control y señalización; sin IO |
| `crates/crypto` | Identidad, MLS, SFrame, HPKE/X-Wing, número de seguridad, cifrado de la entrada; benches |
| `crates/transport` | `quinn`, datagramas, RaptorQ, NACK, control de tasa, NAT, cliente del relay y de la API |
| `crates/capture` | Trait `Capturer`; backends PipeWire, WGC y sintético |
| `crates/codec` | Traits `Encoder`/`Decoder`; FFmpeg con Vulkan Video y respaldos, OpenH264, color |
| `crates/audio` | PipeWire, WASAPI, Opus, jitter, mezcla, procesado de voz, sincronía A/V |
| `crates/input` | Captura en el invitado; inyección con libei, uinput, `SendInput` y HID virtual |
| `crates/render` | Dispositivo `wgpu` compartido con Vulkan Video, capa de vídeo, HDR, cursor local |
| `crates/session` | Máquinas de estado anfitrión e invitado, permisos, reloj, marcas por etapa |
| `crates/ui` | Sistema de diseño `coroom-ui` para `iced`: tokens, tema, fuentes, iconos, componentes |
| `apps/desktop` | Binario `coroom` (aplicación, `doctor`, `probe`, `host`, `join`, `login`…); `locales/` |
| `apps/server` | Binario `coroom-server` (HTTP/3, WebTransport, relay, SQLite, `/admin`) |
| `drivers/windows-vhid` | Driver de mandos virtuales de UMDF2 (modo usuario): mandos VHF tal cual y Xbox 360 para XInput |
| `vendor/` | `iced_wgpu` y `cryoglyph` adaptados a `wgpu` 30 |
| `deploy/`, `packaging/` | Docker Compose, `.env.sample` ficticio; Flatpak y MSIX |
| `fuzz/`, `tools/` | Objetivos de `cargo-fuzz` (workspace aparte); perfiles `netem` y FFmpeg |

Reglas de capas:

- `proto` y `crypto` no dependen de ninguna crate del workspace; `ui` tampoco.
- `transport` solo depende de `proto` y `crypto`.
- `session` orquesta `capture`, `codec`, `audio`, `input` y `transport` (además de `proto` y `crypto`).
- `apps/*` solo dependen de `session`, `render`, `transport` y `ui`.
- Cada backend de plataforma va detrás de un trait y una `feature` de Cargo; `cfg(target_os)` solo
  dentro del backend.

`apps/desktop/tests/layers.rs` lee el `Cargo.toml` de cada miembro y falla si una crate depende de
otra que su capa no permite (cuentan las dependencias normales y de build, también las de
`[target.'cfg(..)']`; las de desarrollo no). Incluye un test de que el comprobador detecta una
violación.

## 6. Seguridad

Cada salto va por QUIC con TLS 1.3 e intercambio híbrido poscuántico, y por encima el contenido
viaja cifrado de extremo a extremo: la sala es un grupo MLS cuyas entradas y salidas rotan la clave
con secreto hacia delante; las claves SFrame de cada emisor se derivan del exportador MLS y la época
viaja en la cabecera, así que la rotación nunca bloquea el flujo; la entrada usa una clave pareja a
pareja HPKE con X-Wing, de modo que ningún invitado puede leer ni falsificar la de otro. El relay ve
y usa la cabecera de los medios, nunca su contenido, y la capa SFrame se mantiene también en directo
para que las dos rutas se comporten igual.

El análisis de amenazas (servidor curioso o malicioso, invitado malicioso, red hostil), la revisión
por capas, los hallazgos con su corrección y su test, el fuzzing de todo lo que parsea datos de la
red y el registro de etiquetas de dominio están en [`docs/seguridad.md`](seguridad.md). El límite
conocido más importante es que el servidor actúa de directorio de claves: el número de seguridad lo
mitiga y conviene compararlo la primera vez.

## 7. Validaciones pendientes y deuda técnica

Lo siguiente está implementado y cubierto por tests en proceso o en la CI, pero falta validarlo en
hardware real:

| Área | Qué falta comprobar |
|---|---|
| GPU | Matriz NVIDIA, AMD e Intel con Vulkan Video y NVENC/AMF/QSV/VA-API; cero copias a CPU y < 8 ms de codificación + decodificación a 1080p60; 4:4:4 y 10 bits por hardware; `coroom probe` a 1080p60 con p95 < 25 ms en el equipo de referencia |
| Windows real | Compilación con MSVC, la aplicación, WGC con D3D11, WASAPI con loopback por proceso, `SendInput`, el driver de mando con juegos, Steam y Driver Verifier (la CI ya lo instala y lo usa con el cliente), el MSIX instalado y el `.zip` portátil |
| Dos NAT distintas | Dos equipos en redes distintas en directo y con `--force-relay`, `docker compose up` con el certificado del operador, migración de ruta al cambiar de red |
| Dispositivos de audio y entrada | PipeWire y WASAPI con altavoces (AEC), |A−V| y boca-oído en la LAN; libei y uinput con un cooperativo local de 4 mandos |
| Pantalla real | Superficie HDR (Windows con HDR, Linux con compositor con gestión de color), captura HDR de WGC y PipeWire, 120 fps en pantalla de 120 Hz, HiDPI fraccionario, tema y acento del portal, decoraciones y modo inmersivo en GNOME y KDE |
| Red y duración | Matriz `netem_matrix` y `soak` de 8 h en el equipo de referencia; en los runners de la CI los pasos con `tc netem` son todavía informativos: en el primer intento la matriz perdió la conexión de señalización al pasar al perfil «media» (2 % de pérdida, 30 ± 10 ms) y queda por diagnosticar |
| Publicación | Flatpak y MSIX construidos en la CI real, imagen arm64 y ficha de AppStream con capturas |

Las mismas pruebas en proceso ya dan referencias: sesión remota sintética a 640×360 con total p50
8,8 ms en directo y 9,9 ms por relay en `127.0.0.1`; con 2 % de pérdida y 10 ± 5 ms de jitter
simulados, 300 de 300 fotogramas presentados y hueco máximo de 66 ms; audio con |A−V| p95 21 ms y
boca-oído p95 57 ms.

Deuda técnica conocida:

- **`vendor/`**: `iced` 0.14 pide `wgpu` 27, y el vídeo sin copias necesita `wgpu` 30; `iced_wgpu` y
  `cryoglyph` van adaptados en `vendor/` hasta que haya un `iced` publicado sobre `wgpu` ≥ 30
  (detalle en `vendor/README.md`).
- **Driver de mando de Windows**: es UMDF2 (modo usuario), así que no necesita la firma de
  Microsoft que exige el código de kernel: la CI firma cada paquete con un certificado autofirmado
  de un solo uso cuya clave borra al terminar, e `install.ps1` confía en él en el equipo
  (`drivers/windows-vhid/README.md`, «Firma»). La CI lo instala en un Windows y lo prueba con el
  cliente; falta probarlo con juegos, Steam y Smart App Control. Los mandos HID se crean al momento con la Virtual HID Framework desde un
  dispositivo de control; los cuatro Xbox 360 existen desde la instalación, pero los juegos solo
  los ven mientras coroom los usa. Los IOCTL de `xusb22` no están documentados: se siguen los
  valores de HIDMaestro.
- **Identificadores de los mandos virtuales**: el mando convertido es un Xbox 360 (`045E:028E`)
  en Linux (uinput) y en Windows (XUSB), para que `xpad`, XInput, SDL y los juegos lo reconozcan
  sin mapear; el mando **tal cual** (UHID en Linux, VHF en Windows) lleva el VID/PID y el
  descriptor del físico, así que el sistema, SDL y Steam lo tratan como ese mando. Solo el mando
  HID genérico de Windows (cuando no quedan Xbox 360 libres) va con VID/PID 0; lo correcto a medio
  plazo es un PID propio de pid.codes.
- **Desafío de dispositivo**: no lleva etiqueta de dominio propia (separado por formato y longitud,
  probado); se añadirá en el próximo cambio de protocolo.
