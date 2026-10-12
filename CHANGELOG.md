# Cambios

Registro de las versiones de coroom. Formato de
[Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/); versiones con
[versionado semántico](https://semver.org/lang/es/). La versión del **protocolo** (la que tienen que
compartir cliente y servidor) va aparte, en `PROTOCOL_VERSION` y en `GET /version`.

## [Sin publicar]

### Cambiado

- El Compose del servidor usa solo la imagen publicada, pasa cada variable de `.env` con su valor
  por defecto y guarda todo en `./data`, también el certificado (`./data/certs`).
- El modo TLS por defecto del Compose es `files`: el certificado lo pone el operador.

### Eliminado

- La gestión automática de certificados por ACME (`COROOM_TLS_MODE=acme`, los retos TLS-ALPN-01 y
  HTTP-01, `COROOM_ACME_*` y `COROOM_BIND_HTTP`). El servidor usa el certificado del operador o uno
  autofirmado, y no necesita salida a internet.

## [1.0.0] — 2026-10-12

Primera versión pública, con el protocolo 7. Licencia Apache-2.0 para todo salvo el servidor
(`apps/server`), que es AGPL-3.0-or-later.

### Cliente

- Aplicación de escritorio `coroom` sobre `iced` 0.14 con el sistema de diseño `coroom-ui` (estilo
  Adwaita, contraste AA, tema, acento y fuente del sistema): servidores e inicio de sesión, inicio,
  amigos, anfitrión con vista previa, invitado con estimación de calidad y estadísticas, ajustes y
  diagnóstico; capturas de todas las pantallas en claro y oscuro.
- Órdenes de terminal `doctor`, `probe`, `host`, `join`, `login`, `logout`, `whoami`, `friends` e
  `invite`.
- Reconexión automática del invitado y aviso de versión nueva en Inicio y en Acerca de.
- Textos en catálogos Fluent (`apps/desktop/locales/es/coroom.ftl`), listos para traducir, con
  tests de que ningún texto visible queda fuera y de que código y catálogo cuadran.
- Accesibilidad: `Tab` y `Mayús+Tab` dan la vuelta al llegar al último o al primer campo, con test
  del orden de foco de cada pantalla.
- Guía de usuario (`docs/guia.md`), arquitectura (`docs/ARQUITECTURA.md`), `CONTRIBUTING.md` y
  este `CHANGELOG.md`.

### Vídeo

- Captura por PipeWire (portal `ScreenCast`, DMA-BUF) en Linux y Windows.Graphics.Capture con
  D3D11 en Windows, directa a los codificadores por hardware.
- FFmpeg 8 con sondeo de GPU (Vulkan Video, NVENC/NVDEC, AMF, QSV, VA-API), negociación
  AV1 → HEVC → H.264, AV1 por software (SVT-AV1 → dav1d), OpenH264 de respaldo y provisión fijada
  de FFmpeg en `tools/ffmpeg.lock`.
- Decodificación a imágenes Vulkan importadas en `wgpu`, vídeo como *widget* de shader sobre el
  mismo dispositivo que el decodificador, slices e intra-refresh por backend y contadores de copias
  a CPU.
- BT.709 y HDR10 (BT.2020 PQ) con *tone mapping* de respaldo, 4:4:4 negociado y 120 fps.
- Cursor con su forma real por la red y prueba de latencia local por etapas (`coroom probe`).

### Audio

- Audio del juego por PipeWire y WASAPI (con loopback por proceso), voz en estrella con Opus 1.6 y
  DRED, procesado de voz de WebRTC, buffer de jitter, mezcla y sincronía A/V.

### Entrada

- Entrada cifrada pareja a pareja por stream y datagramas, captura en la ventana, mandos del
  invitado, inyección por libei y uinput en Linux y `SendInput` en Windows, permisos, ranuras,
  pausa y consola del anfitrión.
- Driver KMDF de mando virtual para Windows sobre Virtual HID Framework, con paquete, scripts, CI
  propia y cliente en coroom.

### Red y E2EE

- Identidad persistente, número de seguridad, grupo MLS de la sala, SFrame y HPKE con X-Wing, con
  benches y umbrales.
- Protocolo de medios v2, QUIC entre pares con claves crudas, FEC y NACK, apertura de NAT con
  respaldo a relay, señalización WebTransport y relay con abanico.
- Control de tasa: la cola se estima con el mínimo del retardo de ida de los últimos 500 ms (el
  jitter no la infla) y la pérdida, medida sobre el último segundo, lleva su propia estimación
  con los umbrales de GCC (crecer por debajo del 2 %, mantener hasta el 10 %, recortar por encima);
  el objetivo es el mínimo de las dos. Con 2 % de pérdida y 30 ± 10 ms el bitrate no se hunde
  hasta el mínimo, y con 5 % se recupera de los recortes por cola.
- Reenvíos de vídeo a tiempo con RTT alto: el invitado usa los fps que anuncia el anfitrión para
  los plazos del reensamblado, el vídeo espera su reenvío hasta 3 RTT + 1 intervalo (400 ms como
  mucho) en lugar de descartar el fotograma y pedir un intra, la cola hacia el decodificador admite
  16 fotogramas y se puede pedir otro intra en cuanto llega uno. Con 5 % de pérdida y 60 ± 20 ms,
  0-1 congelaciones por intra cada 10 s.
- La pérdida que mide el receptor no cuenta como perdidos los originales que llegan tarde por el
  jitter después de pedir su reenvío.
- Sesión remota cifrada (`coroom host`, `coroom join`) con overlay.

### Servidor

- `coroom-server`: HTTP/3 y WebTransport en un puerto UDP, HTTP/2 con TLS por TCP, `/health` y
  `/version`.
- Cuentas en SQLite con Argon2id, dispositivos vinculados por desafío firmado, invitaciones, amigos
  con presencia e invitaciones a salas entre amigos.
- Cuotas y límites del relay, TLS por ACME (TLS-ALPN-01 y HTTP-01), página de administración
  embebida, órdenes de cuenta y amigos y copias de seguridad en línea.
- La autocomprobación de UDP reutiliza el endpoint QUIC del servidor, sin crecimiento de memoria.

### Empaquetado

- Flatpak (`io.github._43fdfdg45454.coroom`, runtime 26.08, FFmpeg 8.1.3 LGPL propio), MSIX y
  `.zip` para Windows, imagen `coroom-server` multiarquitectura en GHCR y publicación automática en cada push
  a `main` con la versión que calcula GitVersion.
- Dockerfile y Docker Compose con `.env.sample` ficticio.

### Seguridad

- Revisión del protocolo criptográfico y del servidor en `docs/seguridad.md`, con el modelo de
  amenazas y qué protege cada capa.
- El receptor de medios no reserva memoria sin cota con grupos hostiles de un par o del relay
  (64 grupos abiertos por pista, 131 072 objetos pendientes como mucho).
- La señalización rechaza mensajes de más de 256 KiB sin esperar a su cuerpo.
- Límite de 10 códigos de sala inexistentes por minuto y cuenta: no se pueden adivinar salas.
- La pila TCP corta a los 10 s las peticiones cuyas cabeceras no terminan (*slowloris*), espera el
  cuerpo 20 s como mucho, hace `PING` en HTTP/2 y admite 1 024 conexiones a la vez.
- Sin degradación de X-Wing: si la clave X-Wing del anfitrión llega manipulada, el invitado no cae a
  X25519 (avisa y deja la entrada sin clave) y el anfitrión rechaza las ofertas X25519.
- `Strict-Transport-Security` en todas las respuestas y cabeceras anti-marcos en la API.
- Registro de todas las etiquetas de dominio de la criptografía con tests de unicidad, y de los
  dominios de firma de la identidad con test de que son disjuntos.
- Test de que el registro del servidor, incluso en `trace`, no contiene contraseñas ni tokens.
- Firma híbrida Ed25519 + ML-DSA-65 de la identidad (FIPS 204, `ml-dsa` de RustCrypto) tras la
  `feature` `pq-sign`, desactivada por defecto.

### Pruebas

- Pruebas de extremo a extremo en proceso: sesión remota directa, por relay y con pérdida simulada,
  expulsión, A/B del cifrado, negociación de códec, entrada a 1000 Hz y audio.
- Fuzzing con `cargo-fuzz` (`fuzz/`): 15 objetivos (cabeceras de medios, control, señalización,
  entrada, audio, API JSON, reensamblado con FEC y `Nack`, SFrame, cifrado de entrada, MLS, rutas
  HTTP y señalización del servidor, slices de vídeo y buffer de jitter), semillas generadas desde
  los tests y workflow semanal `fuzz.yml`.
- Matriz de red: perfiles `lan`, `buena`, `media`, `mala` y `cuello` en `tools/netem.sh` y en el
  proxy en proceso (`LossyConfig::profile`), con los tests `lossy_matrix` y `netem_matrix` (fps,
  pérdida, bitrate, huecos y |A−V| con cotas por perfil).
- Prueba de larga duración `soak` (memoria residente, descriptores e hilos cada minuto, con
  regresión lineal), 8 h a mano y 20 min cada semana en la CI.
- Los tests sensibles a la carga escalan sus cotas de tiempo con un factor de carga medido al
  empezar (codificar 30 fotogramas frente al equipo de referencia).
- CI para Linux y Windows con `fmt`, `clippy`, tests, `cargo deny` y comprobación de las capas
  entre crates.

[1.0.0]: https://github.com/43fdfdg45454/coroom/releases/tag/v1.0.0
