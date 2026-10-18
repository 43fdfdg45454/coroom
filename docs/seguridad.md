# Revisión de seguridad de coroom 1.0

Revisión de seguridad de la versión 1.0.0 (protocolo 7), hecha el 12 de octubre de 2026 sobre su
código. Método: lectura del código de cada capa, tests para cada propiedad que se afirma aquí,
fuzzing de todo lo que parsea datos de la red (`fuzz/`) y la matriz de red con pérdida y jitter. Cada hallazgo lleva su riesgo, la corrección hecha (o por qué no se
hizo) y el test que lo cubre.

Resumen de los hallazgos (detalle en §4):

| Id | Capa | Hallazgo | Riesgo | Estado |
|---|---|---|---|---|
| M-1 | Medios | El receptor reservaba memoria sin cota con grupos hostiles (~1,5 GB con 1 024 datagramas) | Alto (DoS de memoria de un invitado por un par o un relay maliciosos) | Corregido |
| S-2 | Señalización | Mensajes de hasta 1 MiB con 256 en cola por sesión (256 MiB por sesión) | Medio (DoS de memoria del servidor por un usuario) | Corregido |
| S-3 | Señalización | Intentos ilimitados de códigos de sala (30 bits) | Medio (un usuario podía adivinar salas abiertas) | Corregido |
| S-4 | Servidor TCP | Sin tiempo límite de cabeceras ni de cuerpo ni tope de conexiones (*slowloris*) | Medio (DoS del puerto TCP) | Corregido |
| C-1 | E2EE | Un servidor malicioso podía degradar la clave de entrada de X-Wing a X25519 | Medio (se pierde la protección poscuántica de la entrada; la clásica sigue) | Corregido |
| C-2 | Firmas | El desafío de dispositivo no lleva etiqueta de dominio propia | Bajo (separado por formato y longitud; sin colisión posible hoy) | Documentado y probado; etiqueta en el próximo cambio de protocolo |
| H-1 | `/admin` y API | Sin `Strict-Transport-Security`; la API sin cabeceras anti-marcos | Bajo | Corregido |
| I-1 | Identidad | El servidor es el directorio de claves: puede presentar una clave falsa la primera vez | Medio con servidor malicioso | Mitigado (número de seguridad); propuesta futura |
| P-1 | Medios | El control de tasa se hundía con jitter y pérdida aleatoria; la pérdida medida triplicaba la real | Robustez (no de seguridad) | Corregido (§5) |
| P-2 | Servidor | La autocomprobación de UDP creaba un endpoint QUIC nuevo cada 10 minutos y la memoria residente subía a saltos | Robustez | Corregido (§5) |
| P-3 | Medios | Con 30 fps y más de ~45 ms de RTT ningún `Nack` llegaba a tiempo y cada pérdida acababa en intra y congelación | Robustez | Corregido (§5) |

## 1. Modelo de amenazas

| Actor | Qué puede | Qué no puede |
|---|---|---|
| **Servidor honesto pero curioso** (el operador mira la base de datos, el registro y el tráfico que pasa por el relay) | Ver cuentas, amigos, salas abiertas, quién entra en qué sala y cuándo, direcciones IP, volumen, tamaño y ritmo de los paquetes que reenvía el relay | Leer nada de la conexión de pares, ni siquiera las cabeceras de los medios: el relay no termina QUIC (reenvía los paquetes cifrados de punta a punta entre las identidades de los pares); leer vídeo, audio, voz, entrada o chat (además, SFrame, HPKE y la clave de chat de la época, que nunca tiene); saber qué se escribe o se juega |
| **Servidor malicioso** (o comprometido) | Todo lo anterior; negar el servicio; cortar o reordenar mensajes; presentar una clave de identidad falsa en el primer contacto (I-1); suplantar la oferta de clave pareja a pareja **no** (va firmada); degradar a X25519 **ya no** (C-1) | Descifrar ni falsificar medios, entrada o chat de una sala cuyos números de seguridad se comprobaron; hacer commits MLS (solo vale el del anfitrión, firmado) |
| **Invitado malicioso** (miembro de la sala) | Ver y oír lo que el anfitrión comparte mientras esté dentro; enviar entrada si tiene permiso; leer el chat mientras esté dentro y los últimos 50 mensajes al entrar; escribir en él (10 cada 10 s); intentar agotar recursos del anfitrión o de otros invitados con paquetes hostiles (acotado: M-1) | Leer o falsificar la entrada de otro invitado (clave pareja a pareja); escribir en el chat con el nombre de otro (el autor lo pone el anfitrión según el stream por el que llega); seguir leyendo la entrada, los medios o el chat tras la expulsión (época MLS nueva); hacer commits MLS |
| **Red hostil** (en el camino, sin el servidor) | Ver direcciones, tamaños y tiempos; tirar o retrasar paquetes | Leer o modificar nada: QUIC con TLS 1.3 (X25519MLKEM768) de punta a punta entre pares (también a través del relay), claves crudas fijadas entre pares y certificado del servidor fijado o validado |
| **Anfitrión** | Es de confianza para su sala: ve y decide todo, y la entrada se inyecta en su equipo; pone autor y hora a cada mensaje del chat y guarda el historial (en memoria, mientras dure la sala) | — |

**Qué protege cada capa y qué no**:

| Capa | Protege | No protege |
|---|---|---|
| QUIC/TLS 1.3 (de punta a punta entre pares; con el servidor para la señalización) | Confidencialidad e integridad frente a la red y frente al relay, que solo reenvía los paquetes; híbrido poscuántico X25519MLKEM768 | Tamaños y ritmo de los paquetes, y quién habla con quién (el relay lo sabe por la ligadura) |
| MLS (RFC 9420, `openmls`) | Pertenencia a la sala, secreto hacia delante y recuperación tras compromiso al entrar o salir alguien | Los mensajes de control MLS van como `PublicMessage` (firmados, no cifrados): el servidor ve quién entra y sale (ya lo sabe por la señalización) |
| SFrame (RFC 9605) | Contenido de vídeo, audio y voz frente al servidor y frente a quien reenvíe sin ser el emisor; repeticiones | Metadatos frente a los miembros de la sala: tamaños, ritmo, pista, marca de intra |
| HPKE (RFC 9180) con X-Wing | La entrada de cada invitado frente al resto de invitados y al servidor; poscuántica | — |
| Chat (exportador MLS por época, AES-256-GCM) | Texto, autor y hora frente al servidor y el relay; quien sale deja de leer lo que se escribe después; repeticiones; que un historial o una orden de vaciar pasen por un mensaje | Autenticación de cada miembro: la clave es de grupo (ver más abajo); el tamaño y el ritmo de los mensajes; nada frente al anfitrión |
| Cuentas | Que solo los dispositivos vinculados abran la señalización y el relay | Que el servidor no mienta sobre las claves (I-1) |

**El chat y la clave de grupo.** El chat va por el stream de control de cada invitado con el
anfitrión (estrella): el invitado sella su mensaje para el anfitrión, el anfitrión lo abre,
comprueba que lo selló la hoja MLS de **ese** invitado, le pone autor (id del invitado y hoja,
nombre de la señalización) y hora, lo guarda y lo vuelve a sellar para todos, el autor incluido.
El invitado solo acepta lo que sella la hoja del anfitrión. Como la clave es de grupo, un
sellado prueba que lo hizo un miembro de la época, no cuál: la autoría la garantiza el anfitrión,
que sabe por qué stream llegó cada mensaje. Un invitado no puede meter un
sellado en el stream de otro: cada stream va en la conexión de pares entre ese invitado y el
anfitrión, con TLS de punta a punta también a través del relay (que solo reenvía sus paquetes y no
tiene sus claves). Antes del protocolo 19, el relay terminaba TLS y un invitado en connivencia con
quien lo controlase podía hacerse pasar por el anfitrión en el chat; eso ya no es posible.

## 2. Revisión por capas

### 2.1 Identidad y almacenamiento de secretos

- Par Ed25519 generado con el generador del sistema (`getrandom`); la semilla vive en `Zeroizing` y
  en `SigningKey` (que la borra al soltarse). `Debug` de `Identity`, `PqSigningKey`, `SealKey`,
  `InputCipher`, `MediaKeyRing` y `KeyPackageBundle` no muestra secretos.
- Llavero del sistema (`keyring`, Secret Service / Credential Manager) por defecto; `FileStore`
  escribe con un temporal creado con `0600`, `fsync` y renombrado (nunca hay un secreto legible ni
  a medio escribir). En Flatpak, `SealedFile`: AES-256-GCM con una clave HKDF del secreto del portal
  `Secret`, nonce aleatorio de 96 bits por escritura (seguro hasta ~2³² escrituras por clave) y la
  ruta lógica como AAD, así que un fichero no se puede hacer pasar por otro.
- Tokens de sesión: 32 bytes aleatorios, opacos; el servidor guarda solo su SHA-256, uno por
  dispositivo, 30 días (12 h los de la web de administración). Contraseñas con Argon2id (OWASP).
- Observación sin cambio: `FileStore` no comprueba los permisos de un fichero de identidad que ya
  existía (si alguien lo hizo legible a mano). Lo crea siempre con `0600`.

### 2.2 Derivación de claves y nonces

Todas las etiquetas de dominio están registradas en `crates/crypto/src/domains.rs`, que las toma
de las constantes que usa cada módulo. Los tests comprueban que **ninguna se repite**, que las que
derivan del mismo secreto no son prefijo unas de otras y que **todo literal `"coroom…"` del código
de la crate está registrado** (`every_label_is_unique`,
`labels_of_the_same_secret_are_prefix_free`, `every_coroom_literal_in_the_sources_is_registered`).

| Etiqueta | Uso | Secreto |
|---|---|---|
| `coroom sframe v1` | Exportador MLS, contexto `hoja ‖ época` | Grupo MLS |
| `SFrame 1.0 Secret key/salt ‖ KID ‖ suite` | HKDF-SHA512 del RFC 9605 | Clave base de cada emisor |
| `coroom hpke v1\0` | Prefijo del `info` de HPKE para sellar | — |
| `coroom pairwise v1\0`, `coroom pairwise secret` | `info` y contexto del exportador del secreto pareja a pareja (con las dos identidades y la sala) | HPKE |
| `coroom input v1` (sal), `… guest→host`, `… host→guest`, `… key id` | Claves direccionales de la entrada | Secreto pareja a pareja |
| `coroom xwing v1` (sal) | Semilla X-Wing desde la semilla Ed25519 | Semilla Ed25519 |
| `coroom ml-dsa-65 v1` (sal, `pq-sign`) | Semilla ML-DSA desde la semilla Ed25519 | Semilla Ed25519 |
| `coroom` / `coroom/portal-secret/seal/v1` | Sal e `info` de la clave de sellado de Flatpak | Secreto del portal |
| `coroom safety number v1` | Prefijo del SHA-512 del número de seguridad | — |
| `coroom chat v1` | Exportador MLS, contexto `época (u64 BE)`, 32 bytes: clave AES-256-GCM del chat | Grupo MLS |

**Nonces**, por construcción y con test:

- SFrame: `sframe_salt XOR CTR` con un contador por emisor y época **compartido** por todos los
  cifradores del anillo (`encryptors_of_the_same_sender_never_reuse_a_counter`) y un KID por
  emisor y época; el anillo rechaza una época cuyos 16 bits bajos chocan con la anterior
  (`epochs_must_increase_and_leaves_must_fit`). Dos épocas con el mismo KID (a 65 536 de
  distancia) tienen claves base distintas (el exportador lleva la época en el contexto).
- Entrada: `dirección ‖ canal ‖ secuencia`, con clave distinta por dirección; `InputCipher::seal`
  rechaza una secuencia que no crece (`SequenceReused`).
- Chat: `hoja (u32 BE) ‖ contador (u64 BE)` con un contador por época en `ChatRing::seal`, que
  vuelve a 0 con la clave nueva (cada época tiene la suya); los datos asociados llevan
  `época (u64 BE) ‖ tipo` (mensaje, historial o vaciar), así un sellado no vale en otra época ni
  como otro tipo (`nonces_are_unique_per_sender_and_counter`,
  `sealed_messages_open_with_the_same_kind_leaf_counter_and_epoch`).
- HPKE: un `enc` nuevo en cada sellado (modo base, un solo mensaje).
- MLS: los de `openmls`.

### 2.3 Rotación de épocas: quién deja de leer cuándo

Solo el anfitrión hace commits; un commit de otra hoja se rechaza (`UnauthorizedCommit`). Al entrar
un invitado, el anfitrión añade su `KeyPackage` (con la regla credencial = clave de firma) y difunde
el commit; el nuevo recibe el `Welcome` de la época nueva y **no puede leer nada anterior**. Al
expulsar a alguien, el commit de expulsión rota la época: el expulsado no puede derivar el
exportador nuevo (`removed_guest_cannot_derive_the_new_exporter`) y el anfitrión cifra desde ese
momento con la época nueva (`d_kicked_guest_stops_presenting_and_cannot_decrypt`, de extremo a
extremo). El anillo SFrame conserva las dos últimas épocas para que la rotación no corte el flujo:
lo que ya estaba en vuelo cifrado con la anterior se sigue descifrando, nada nuevo se cifra con ella.

El chat sigue la misma época con su propio anillo (`ChatRing`, 4 épocas como mucho): al **entrar**
alguien se conservan las anteriores, porque todos sus miembros siguen dentro, y lo que estaba en
vuelo se abre; al **salir** o ser expulsado alguien se olvidan todas, porque el que salió las conoce.
El anfitrión sella siempre con la más nueva. Desde el commit de expulsión, el expulsado no recibe
nada más y, aunque lo recibiera (por ejemplo, a través del relay), no podría abrirlo
(`a_removed_member_cannot_read_what_is_written_after` en `crypto` y
`c_a_kicked_guest_cannot_read_what_comes_after` de extremo a extremo). Lo que escribió antes se
queda en el historial. El invitado guarda hasta 64 mensajes de una época que aún no ha procesado
(el anfitrión puede sellar con ella antes de que llegue el commit) y los abre al instalarla. Un
mensaje que un invitado selló con una época que el anfitrión acaba de olvidar (se escribió
mientras alguien salía) se descarta: el anfitrión lo dice en un `ChatRejected` y el invitado, al no
recibir el eco, recupera su hueco del límite a los 20 s.

### 2.4 Ventanas anti-repetición

- SFrame: 128 contadores por KID (`REPLAY_WINDOW`); el contador solo se anota cuando el paquete se
  ha autenticado (un paquete manipulado no consume el suyo); la tabla se limpia de los KID de
  épocas retiradas, y un KID desconocido se rechaza antes de mirar la ventana, así que su tamaño
  está acotado por los emisores instalados (`replays_are_rejected`).
- Entrada: 256 secuencias por canal (stream y datagramas aparte) con mapa de bits
  (`crypto::input`, tests de ventana).
- Chat: por época y hoja, solo contadores mayores que el último abierto (el stream de control es
  fiable y ordenado); el contador se anota tras autenticar; las épocas olvidadas se llevan su
  tabla (`replays_and_unknown_epochs_are_rejected`).
- Desafíos de dispositivo: un solo uso, 60 s, como mucho 4 096 pendientes.

### 2.5 Ligado de identidad en TLS y número de seguridad

Entre pares, QUIC usa **claves públicas crudas** (RFC 7250) que son la identidad Ed25519, y cada
extremo exige la clave que anunció la señalización. El servidor se fija por la huella de su
certificado (autofirmado, `ServerTrust::Fingerprint`, con «Confiar» la primera vez) o se valida con
las raíces del sistema (certificado del operador emitido por una autoridad). El número de seguridad (60 dígitos, SHA-512 de las dos claves
ordenadas) se ve en el overlay, en la sala y en Ajustes → Cuenta y servidor, y se recuerda por par.

**I-1 (sin corrección completa)**: el servidor es también el directorio de claves (manda la clave
del anfitrión en `Joined` y la del invitado en `PeerJoined`). Un servidor malicioso puede, en el
primer contacto, presentar una clave suya y hacer de intermediario; las firmas de MLS y de la
oferta HPKE no lo impiden porque firmaría con esa clave. Lo detecta **comparar el número de
seguridad**, que la interfaz pide en la sala. No se avisa automáticamente de un cambio de clave de
una cuenta porque una cuenta tiene varios dispositivos, cada uno con su identidad. Propuesta para
después de 1.0: lista de dispositivos de cada cuenta firmada por un dispositivo ya verificado
(*cross-signing*) y aviso cuando aparezca uno sin firmar.

### 2.6 Firma de desafíos y dominios de firma

La identidad Ed25519 firma cinco tipos de mensaje: el desafío de dispositivo, la clave X-Wing
publicada, la oferta pareja a pareja, los mensajes MLS (`SignWithLabel`) y el `CertificateVerify`
de TLS 1.3; con `pq-sign`, también la firma híbrida y el ligado de la clave ML-DSA.
`domains::SIGNATURE_DOMAINS` los describe por su primer byte y su longitud, y los tests comprueban
que **ningún mensaje puede pertenecer a dos dominios** (`signature_domains_are_pairwise_disjoint`)
y que los mensajes reales caen en el suyo (`real_signed_messages_fall_in_exactly_one_domain`, y
`the_device_challenge_is_its_own_signature_domain` en `coroom-transport` con `challenge_message`).

**C-2**: el desafío de dispositivo es `host ‖ 32 bytes del servidor`, sin etiqueta propia.
Un servidor malicioso elige los 32 bytes, pero el mensaje empieza por un carácter de nombre de host
y mide como mucho 287 bytes: no puede coincidir con una clave X-Wing firmada (1 268 bytes), una
oferta (empieza por `0x00` o, con X-Wing, `0x64` y mide más de 1 200), un mensaje MLS (`0x10`-`0x1f`)
ni TLS (`0x20`). El cliente comprueba además que el host del desafío sea el de la URL y que el
desafío mida 32 bytes. No se cambia ahora porque rompería el protocolo 7 sin necesidad; cuando se
suba el protocolo, el desafío pasará a `"coroom device challenge v1\0" ‖ len(host) ‖ host ‖ desafío`.

### 2.7 Límites de tasa, de tamaño y tiempos

| Dónde | Límite |
|---|---|
| Login | 5 fallos por minuto por cuenta y por IP (`too_many_attempts`) |
| Códigos de sala | 10 códigos que no existen por minuto por cuenta (o por sesión sin cuentas) (**S-3**) |
| Cuerpo HTTP | 64 KiB (HTTP/3 y TCP), salvo `PUT /api/v1/me/avatar`: 320 KiB; 20 s para llegar entero por TCP (**S-4**) |
| Avatares | PNG cuadrado de 16 a 256 píxeles de lado; la cabecera se lee antes de reservar y el decodificador tiene tope de memoria (§2.14) |
| Cabeceras HTTP/1.1 | 10 s por petición, también entre peticiones (**S-4**); `PING` de HTTP/2 cada 30 s |
| Conexiones TCP | 1 024 a la vez (**S-4**); handshake TLS en 10 s |
| QUIC | `idle_timeout` 15 s, `keep_alive` 5 s; streams por conexión de `quinn` |
| Señalización | `Auth` en 10 s; mensajes de 256 KiB como mucho (**S-2**); 256 en cola por sesión, que se cierra si se llena |
| Mensajes entre pares (control) | 1 MiB (cursor con su imagen) |
| Medios (receptor) | 64 grupos abiertos por pista y 131 072 objetos reservados en total; reparaciones por grupo ≤ objetos (**M-1**) |
| Entrada | 64 eventos por mensaje; tramas de stream acotadas |
| Chat | 2 000 caracteres y 8 000 bytes por mensaje, sin caracteres de control salvo salto de línea y tabulador; 10 mensajes cada 10 s por persona (el anfitrión descarta el resto; el cliente avisa antes con «Vas muy rápido»); 50 mensajes de historial, solo en la memoria del anfitrión; 64 mensajes en espera de una época nueva en el invitado |
| Relay | Cuotas mensuales por cuenta, límite por invitado y sentido (`COROOM_RELAY_MAX_KBPS`, cubo de fichas), invitados por sala; solo reenvía entre el anfitrión y los invitados de una sala; ninguna respuesta es más grande que la petición (el `Allocate` va relleno hasta el tamaño del `Challenge`) y la galleta de la ligadura, sin estado, prueba que el par recibe en su dirección antes de crear nada; el token no viaja (va su id y firmas HMAC-SHA-256) |

### 2.8 Administración (`/admin`) y cabeceras HTTP

- **CSRF**: no hay cookies. El token de la sesión web vive en `sessionStorage` (se borra al cerrar
  la pestaña) y viaja en `Authorization: Bearer`; un sitio ajeno no puede hacer que el navegador lo
  mande, y la API no responde a CORS (sin `Access-Control-Allow-*`), así que tampoco puede leer
  respuestas. La interfaz no usa `innerHTML` ni `localStorage`.
- **Cabeceras** de la página: `Content-Security-Policy` estricta (`default-src 'none'; script-src
  'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none';
  form-action 'none'; frame-ancestors 'none'`), `X-Frame-Options: DENY`,
  `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff`, `Cache-Control: no-cache` y,
  desde esta revisión, `Strict-Transport-Security: max-age=31536000` (**H-1**). La API añade
  `Cache-Control: no-store`, CSP sin nada, `X-Frame-Options`, `Referrer-Policy` y HSTS.
  Tests: `the_admin_page_has_every_security_header_and_no_cookies`,
  `errors_have_code_and_spanish_message`, `slow_headers_are_cut_and_normal_requests_still_work`.
- Las rutas `/api/v1/admin/*` exigen una sesión de administrador; el último administrador no se
  puede deshabilitar ni degradar.

### 2.9 Registro sin secretos

`apps/server/tests/logging.rs` arranca el servidor con **todo** el registro de coroom en `trace`
(y el resto de crates en `debug`), crea el administrador, da de alta un usuario, inicia sesión con
el desafío, abre la señalización, falla una contraseña y cambia otra, y comprueba que el texto
registrado no contiene ninguna contraseña ni ningún token. Con `COROOM_LOG_TEST_FILTER=trace`
(todas las crates en `trace`, 218 KB de registro) tampoco: `h3`, `hyper`, `rustls` y `sqlx` no
registran cabeceras ni parámetros. En producción el filtro por defecto es `info`.

### 2.10 Firmas poscuánticas (ML-DSA)

`ml-dsa` 0.1.1 de RustCrypto (FIPS 204 final, publicada en junio de 2026, licencia Apache-2.0 o
MIT; su README avisa de que no tiene auditoría independiente) se evaluó con la `feature` `pq-sign`
de `coroom-crypto`, **desactivada por defecto**:

- `Identity::pq_signing_key()`: clave ML-DSA-65 derivada de la semilla Ed25519 (nada más que
  guardar); `Identity::hybrid_public_key()` la publica ligada a la identidad con una firma Ed25519.
- `Identity::sign_hybrid` / `verify_hybrid`: Ed25519 y ML-DSA-65 sobre el mismo mensaje con
  prefijo `0x01 ‖ "coroom hybrid signature v1" ‖ 0x00` (y contexto FIPS 204
  `"coroom identity v1"`); hacen falta **las dos** mitades, y la mitad Ed25519 no vale como firma
  clásica del mismo mensaje.
- Tests: `the_pq_key_is_derived_deterministically_from_the_identity`,
  `hybrid_signatures_need_both_halves`, `a_pq_key_not_bound_to_the_identity_is_rejected` (en la CI
  con `cargo test -p coroom-crypto --features pq-sign`).

Ningún protocolo la usa todavía: hacerlo cambia el `KeyPackage` MLS (la suite de `openmls` sigue
siendo Ed25519), el desafío de dispositivo y el número de seguridad. Queda lista para cuando haya
una suite MLS con ML-DSA estable y se suba el protocolo.

### 2.11 Código `unsafe`

Recuento con `grep` (no se usa `cargo-geiger`: cuenta también las dependencias, que no se
auditan aquí) y `cargo clippy -W clippy::undocumented_unsafe_blocks -W clippy::missing_safety_doc`
sin avisos en las crates que lo usan:

| Crate | `unsafe` | Dónde |
|---|---:|---|
| `coroom-codec` | 63 | FFmpeg (`rsmpeg`) y Vulkan (`ash`) en la importación sin copias |
| `coroom-capture` | 30 | PipeWire (DMA-BUF) y Windows.Graphics.Capture |
| `coroom-render` | 14 | Importación de imágenes Vulkan en `wgpu` (`as_hal`, `texture_from_raw`) |
| `coroom-input` | 10 | `SendInput` y el cliente del driver de mandos de Windows (cfgmgr32, `CreateFileW` y `DeviceIoControl`); en Linux, `HIDIOCGFEATURE` sobre `hidraw` (informes de característica del mando tal cual). El invitado Windows lee el mando con `hidapi`, sin `unsafe` propio |
| `coroom-audio` | 10 | Sesiones de audio de WASAPI (COM) y prioridad de tiempo real (`sched_setscheduler`, `RLIMIT_RTTIME`, `gettid`; MMCSS en Windows) |
| `proto`, `crypto`, `transport`, `session`, `ui`, `coroom`, `coroom-server` | 0 | `unsafe_code = "deny"` en el workspace |

Todo lo que parsea datos de la red está en crates sin `unsafe`.

### 2.12 Mandos tal cual (HID en crudo)

Con el mando **tal cual**, el invitado manda el descriptor HID de su mando, sus informes de
característica y cada informe de entrada, y el anfitrión crea un dispositivo idéntico (mismo
VID/PID y descriptor): con UHID en Linux, con la Virtual HID Framework en Windows (driver de
coroom, §2.11). Eso deja al invitado definir **cualquier** dispositivo HID en el
equipo del anfitrión, así que:

- **Lo decide el anfitrión, por invitado.** Solo hay mando tal cual con una ranura concedida y el
  modo «Tal cual» (`Permissions::gamepad_raw`, por defecto sí al conceder la ranura; «Convertido a
  Xbox 360» lo quita al momento). Sin ranura no se crea nada, igual que antes.
- **El descriptor se valida antes de crear nada** (`proto::hid::parse`): como mucho 4 KiB, Push y
  Pop y colecciones emparejados, informes de 512 bytes como máximo; tiene que declarar un mando
  (Game Pad, Joystick o Multi-axis), y si además declara teclado, ratón, *system control* o
  *consumer control* hace falta el permiso de teclado y ratón (si no, va convertido y la tarjeta
  dice por qué). Así un «mando» no puede escribir en el anfitrión sin el permiso de teclado.
- **Límites**: nombre de 128 caracteres, 32 informes de característica de hasta 8 KiB en total,
  informes de entrada de 512 bytes como máximo y solo para la ranura del propio invitado (lo de
  otra ranura se descarta y se cuenta); en pausa se quita el dispositivo, y al expulsar, quitar la
  ranura o desconectar el mando, también.
- **Hacia el invitado** solo vuelven los informes de salida (vibración, luces) de su propio mando
  y, con el convertido o un Xbox por cable, el estado de sus dos motores (`GamepadRumble`, dos
  números), cifrados como el resto del control. Las peticiones `GET_REPORT` del sistema se contestan con los
  informes de característica que mandó el invitado al conectar, sin ida y vuelta por la red.
- El objetivo de fuzzing `proto_hid` cubre el parser (§3).

- **En Windows**, el dispositivo de control del driver es exclusivo (un programa a la vez) y solo
  lo abren SYSTEM, los administradores y el usuario con la sesión iniciada; al cerrarse el handle
  de coroom (o morir), el driver quita sus mandos. El driver vuelve a comprobar los límites de
  cada petición (tamaños del descriptor, de los informes y de las características) antes de tocar
  nada. Los Xbox 360 los abre cualquiera (los juegos), pero sus órdenes privadas solo valen para el
  handle que los activó.

Lo que no cubre: un mando tal cual con un descriptor válido puede llevar al kernel por caminos de
drivers HID específicos (`hid-playstation`, `hid-nintendo`… en Linux; los de clase HID de Windows)
según el VID/PID que declare. Es la
misma superficie que enchufar ese mando por USB; el anfitrión que no confíe en un invitado lo deja
en «Convertido» o sin mando.

### 2.13 Dependencias

`cargo deny check` (avisos de RustSec, licencias, crates prohibidas y duplicadas) en la CI; todas
las versiones fijadas con `=`. Las revisadas expresamente: `ml-dsa` (opcional), `fluent-bundle`,
`fluent-syntax` y `unic-langid` (Project Fluent, Apache-2.0 o MIT), `libfuzzer-sys` (solo en
`fuzz/`), `hidapi` (MIT, solo en Windows y con su backend en Rust, sin C: el mando del invitado en
crudo), `image` (MIT o Apache-2.0, solo PNG, JPEG y WebP, en Rust: la imagen que se elige como
avatar en el cliente; el servidor solo usa `png`).

### 2.14 Avatares

Cada usuario puede subir una imagen (`PUT /api/v1/me/avatar`, `DELETE` para quitarla). Es lo único
que el servidor guarda tal como lo elige un usuario y sirve a otros, así que nunca sirve los bytes
que llegan:

- **Validación** (`apps/server/src/avatar.rs`): como mucho 320 KiB (la pila HTTP corta antes con
  413, y solo admite ese tamaño si la petición trae `Authorization: Bearer`; sin token, el límite
  normal de 64 KiB), firma PNG, ancho y alto leídos de la cabecera y comprobados (cuadrado, de 16 a 256 px)
  **antes** de reservar los píxeles, y el decodificador con tope de memoria. Del primer fotograma
  solo se quedan los píxeles, que se vuelven a codificar en un PNG propio sin fragmentos auxiliares
  (texto, EXIF, perfiles de color, animación). Si algo falla, 400 `invalid_avatar` y nada cambia.
- **Versión**: los 53 bits altos del SHA-256 del PNG propio (cabe en un número de JSON). Viaja en
  `User`, `Friend`, `Joined`/`PeerJoined`, en los integrantes de la sala y en el aviso `Avatar` de
  la señalización (a los amigos, a quien comparte sala con él y a las sesiones propias). Es también
  el `ETag` de la descarga. El aviso lleva la versión que hay en la base cuando sale, de uno en
  uno, así que dos subidas seguidas nunca dejan a nadie con la penúltima.
- **Avisos acotados**: subir la misma imagen otra vez no escribe ni avisa, y cada cuenta puede
  cambiar el avatar 10 veces por minuto (`429` después); las solicitudes de amistad, 10 por minuto,
  y solo la primera a cada persona avisa (`FriendRequested`). Así nadie llena la cola de
  señalización de otro (256 mensajes, que al llenarse cierra su sesión) a fuerza de avisos.
- **Moderación**: el avatar de una cuenta deshabilitada no se sirve, y el administrador lo quita
  desde `/admin` («Quitar avatar», `DELETE /api/v1/admin/users/{id}/avatar`), con el mismo aviso.
- **Quién lo ve**: `GET /api/v1/users/{id}/avatar` vale con **cualquier sesión** del servidor, no
  solo entre amigos: en una sala hay invitados que no son amigos entre sí y tienen que verse. Es el
  mismo alcance que el nombre visible. Los id de usuario son correlativos, así que quien tenga cuenta
  puede bajar todos los avatares de su servidor; sin cuenta, nada (401). Se sirve con
  `Cache-Control: private, no-cache`, `X-Content-Type-Options: nosniff` y una CSP que no deja
  ejecutar nada.
- **En el cliente**: la imagen que se elige se lee con límites (40 MB, 12 000 px de lado, tope de
  memoria del decodificador, PNG, JPEG o WebP con su orientación EXIF), se recorta al cuadrado
  central y se reduce a 256 px antes de subirla; la EXIF no sale del equipo. Lo que se descarga se
  vuelve a decodificar con límites y se comprueba que su SHA-256 da la versión pedida, también lo
  que se lee de la caché de disco (`avatars/` en el directorio de datos, 400 ficheros como mucho).
- El objetivo de fuzzing `server_avatar` cubre la validación (§3), y `server_api` la ruta.

## 3. Fuzzing

`fuzz/` es un workspace aparte (`cargo-fuzz` con libFuzzer, nightly) con un objetivo por cada
formato que llega de la red; las semillas (`fuzz/seeds/`) salen de los tipos y vectores de los tests
(`cargo test -p coroom-session --test fuzz_seeds -- --ignored`), con paquetes SFrame y sellados de
entrada auténticos para las claves fijas de los objetivos. Cada objetivo comprueba, además de que
no hay pánicos, una propiedad: ida y vuelta exacta de cada formato, memoria acotada del receptor de
medios, rangos de slices sin huecos, que lo cifrado se descifra y lo manipulado no, que un error de
MLS no cambia la época, que las rutas HTTP nunca dan un 5xx con una entrada mala y que la
señalización no deja salas abiertas al cerrarse la sesión.

| Objetivo | Qué recibe | Ejecuciones en 60 s (aquí) | Aristas cubiertas |
|---|---|---:|---:|
| `proto_media` | Cabecera de objetos v2 | 63,4 M | 42 |
| `proto_audio` | Cabecera de audio | 76,7 M | 18 |
| `proto_input` | `InputPacket` (con `InputState` y su reconciliación), `InputMessage`, datagramas y stream de entrada | 24,6 M | 523 |
| `proto_hid` | Descriptores HID del mando tal cual | 2,4 M | — |
| `proto_control` | Mensajes de control enmarcados, en trozos | 11,0 M | 1 197 |
| `proto_signal` | Mensajes de señalización con el máximo del servidor | 8,2 M | 759 |
| `proto_api` | Cuerpos JSON de la API y hexadecimal | 8,8 M | 3 647 |
| `transport_media` | Datagramas, reloj e intervalo anunciado para el reensamblado con FEC y `Nack` | 0,66 M (repetido tras P-3) | 1 495 |
| `transport_loopback` | Datagramas del bucle local | 1,0 M | 160 |
| `crypto_sframe` | Paquetes SFrame con claves instaladas; cifrar y descifrar | 7,6 M | 208 |
| `crypto_input` | Sellados de entrada; sellar y abrir | 2,9 M | 111 |
| `crypto_mls` | `process`, `join` y `add_member` con mensajes arbitrarios | 0,75 M | 6 167 |
| `server_api` | Peticiones HTTP (método, sesión, ruta, cuerpo) al despacho real | 0,47 M | 4 351 |
| `server_signal` | Sesiones de señalización completas | 0,46 M | 2 632 |
| `server_avatar` | Ficheros subidos como avatar: límites y PNG propio estable | sin medir aún | — |
| `codec_slices` | Paquetes H.264, HEVC y AV1 | 16,5 M | 111 |
| `audio_jitter` | Secuencias de paquetes de audio y reloj | 2,2 M | 160 |

Resultado: **ningún pánico ni fallo de propiedad** en 15 minutos de fuzzing aquí (60 s por
objetivo, sin sanitizador por espacio en disco). El hallazgo M-1 salió de la revisión del código y
el objetivo `transport_media` comprueba ahora su cota en cada entrada. La CI (`fuzz.yml`, semanal y
a mano) da 120 s a cada objetivo con AddressSanitizer y sube lo que encuentre.

## 4. Hallazgos

### M-1 — Memoria sin cota en el receptor de medios (alto, corregido)

**Descripción.** `ReceiverCore::push` creaba el estado de un grupo con `vec![None; object_count]`
al ver su primer datagrama, sin límite de grupos abiertos (hasta 1 024 por delante del esperado) ni
de objetos. Un par o un relay maliciosos (el relay ve y puede inyectar datagramas: las cabeceras de
medios van en claro) reservaban ~1,5 GB por pista con 1 024 datagramas de 17 bytes, y las
reparaciones por grupo no tenían tope.
**Corrección.** `MAX_OPEN_GROUPS` (64 por pista) y `MAX_PENDING_OBJECTS` (131 072 objetos en
total): los datagramas que los superan cuentan como no válidos; como mucho tantas reparaciones como
objetos de origen por grupo. Un emisor legítimo no pasa de ~12 grupos abiertos a 120 fps.
**Test.** `media::tests::hostile_groups_cannot_reserve_unbounded_memory` y la aserción de cotas del
objetivo de fuzzing `transport_media`.

### S-2 — Mensajes de señalización de hasta 1 MiB (medio, corregido)

**Descripción.** El servidor leía la señalización con el máximo general de 1 MiB por mensaje, y
cada sesión admite 256 mensajes en cola: un usuario autenticado podía hacer que el servidor guardara
256 MiB por cada sesión destinataria que no leyera.
**Corrección.** `MAX_SIGNAL_MESSAGE` = 256 KiB (`FrameBuffer::with_max`), validado en cuanto llega
el prefijo, sin guardar el cuerpo. Lo más grande que viaja (el `Welcome` de una sala de 32
invitados) mide 7,5 KB.
**Tests.** `an_oversized_signalling_message_ends_the_session`,
`the_largest_signalling_messages_fit`, `a_smaller_limit_rejects_the_prefix_before_the_body`.

### S-3 — Códigos de sala sin límite de intentos (medio, corregido)

**Descripción.** Los códigos tienen 6 caracteres de 32 símbolos (30 bits). Sin límite, un usuario
con muchas sesiones podía probar miles por segundo; con salas abiertas, adivinar una era cuestión
de horas.
**Corrección.** 10 códigos que no existen por minuto **por cuenta** (que no se reinicia abriendo
otra sesión; por sesión en el modo de desarrollo); después, `RoomNotFound` con «espera un minuto».
**Test.** `guessing_room_codes_is_rate_limited_per_account`.

### S-4 — *Slowloris* en la pila TCP (medio, corregido)

**Descripción.** Tras el handshake TLS, `hyper` servía sin temporizador: sin tiempo límite de
cabeceras, de cuerpo ni de inactividad, y sin tope de conexiones; un cliente que mandara las
cabeceras byte a byte ocupaba conexiones indefinidamente.
**Corrección.** `HEADER_TIMEOUT` (10 s por petición),
`BODY_TIMEOUT` (20 s, responde 408), `PING` de HTTP/2 cada 30 s y `MAX_TCP_CONNECTIONS` (1 024).
HTTP/3 ya cerraba conexiones inactivas por el `idle_timeout` de QUIC.
**Test.** `apps/server/tests/hardening.rs`: la conexión con cabeceras a medias se cierra a los
10 s y una petición normal sigue respondiendo.

### C-1 — Degradación de X-Wing a X25519 (medio, corregido)

**Descripción.** El invitado recibe la clave X-Wing firmada del anfitrión por la señalización. Si
la firma no verificaba, caía a X25519 «para seguir funcionando»: un servidor malicioso podía
estropear la clave a propósito y obtener una clave de entrada sin protección poscuántica (guardar
hoy, descifrar con un ordenador cuántico mañana). La clásica seguía intacta.
**Corrección.** Sin respaldo: con la clave manipulada, el invitado avisa («posible manipulación en
el servidor») y no establece la clave de entrada; el anfitrión rechaza cualquier oferta que no sea
X-Wing (siempre publica la suya).
**Test.** `remote::wire::tests::a_tampered_host_key_is_refused_without_falling_back_to_x25519`.

### C-2 — Desafío de dispositivo sin etiqueta de dominio (bajo, documentado)

Ver §2.6. Probado por `signature_domains_are_pairwise_disjoint`,
`real_signed_messages_fall_in_exactly_one_domain` y
`the_device_challenge_is_its_own_signature_domain`.

### H-1 — HSTS y cabeceras de la API (bajo, corregido)

Ver §2.8. Tests: `the_admin_page_has_every_security_header_and_no_cookies` y
`errors_have_code_and_spanish_message`.

### I-1 — El servidor como directorio de claves (medio con servidor malicioso, mitigado)

Ver §2.5. Sin test automático posible (es una propiedad del diseño); la mitigación (número de
seguridad visible y recordado) está cubierta por los tests de la sala y de ajustes.

## 5. Otros hallazgos (robustez)

**P-1 — Control de tasa y pérdida medida con jitter.** La matriz de red (`apps/desktop/tests/
matrix.rs`) encontró que con el perfil `media` (2 %, 30 ± 10 ms) el bitrate caía de 1,5 Mbit/s al
mínimo (150 kbit/s) y la escalera bajaba a 11 fps. Dos causas: (1) la «cola» era la mediana del OWD
de cada informe menos el mínimo de 10 s, que con ±10 ms de jitter ronda el umbral de 10 ms sin
cola ninguna; (2) cualquier pérdida por encima del 2 % recortaba un 15 % y nunca se recuperaba, y la
pérdida medida triplicaba la real porque los originales retrasados por el jitter que llegaban
después del `Nack` contaban como perdidos. Correcciones: la cola es ahora el mínimo de los últimos
500 ms sobre el mínimo de 10 s; la pérdida sigue los umbrales de GCC (`draft-ietf-rmcat-gcc`):
crecer por debajo del 2 %, mantener hasta el 10 %, recortar `pérdida/2` por encima; y lo que llega
antes de medio RTT tras el `Nack` cuenta como llegado. Al repetir la matriz salió una tercera causa,
en `mala` (5 %): la pérdida de cada informe de 50 ms (unos 10 objetos) es ruidosa, un solo objeto
ya es un 10 %, y con una única estimación cualquier recorte por cola ya no se recuperaba porque con
un 5 % de pérdida no se puede crecer. Ahora, como en GCC, hay dos estimaciones, por retardo y por
pérdida, el objetivo es el mínimo y la pérdida es la de los informes del último segundo. Tests:
`random_loss_and_jitter_without_queue_hold_the_bitrate` (pérdida por informe aleatoria y una cola
pasajera de 1 s), `the_loss_is_measured_over_a_window_of_reports`,
`heavy_loss_without_queue_cuts_like_gcc`, `jitter_late_originals_after_a_nack_are_not_counted_as_lost`
y la propia matriz.

**P-2 — Memoria residente de la autocomprobación de UDP.** La prueba de larga duración subía la
memoria residente a saltos de 4-6 MB cada 10 minutos, el periodo de la autocomprobación de UDP del
servidor: cada comprobación creaba un `quinn::Endpoint` cliente con sus búferes de recepción y el
asignador no devolvía la memoria (80 comprobaciones seguidas: +27 MB). Ahora el endpoint se crea
una vez por familia de direcciones y se reutiliza (80 comprobaciones: +2 MB). Test:
`probe::tests::the_probe_reuses_one_client_endpoint_per_family`.

**P-3 — Reenvíos que nunca llegaban a tiempo.** También de la matriz: huecos de 400-770 ms en
`media` y `mala` sin que el bitrate lo explicara. Tres causas encadenadas: (1) el invitado suponía
60 fps para los plazos del reensamblado (no sabía los del anfitrión) y con 30 fps el plazo máximo
tras un `Nack` era de 67 ms, así que con más de ~45 ms de RTT ningún reenvío llegaba y cada
objeto perdido sin FEC suficiente acababa en un intra; (2) aunque el RTT no cupiera en 4
intervalos, descartar un fotograma de vídeo siempre sale peor que esperar su reenvío, porque el intra
que se pide tarda también un RTT y es el fotograma más grande (y el que más se pierde); (3) al
completarse un fotograma retenido, los que venían detrás llegaban de golpe a una cola de 2 hacia el
decodificador que descartaba los más antiguos, rompía la cadena de referencias y pedía otro intra.
Correcciones: `MediaReceiver::set_track_interval` con los fps de `CodecSelected` (acotados a
15-240 fps); en vídeo, plazo máximo `max(4 intervalos, 3 RTT + 1 intervalo)` con 400 ms como mucho;
cola de 16 fotogramas comprimidos hacia el decodificador (sin latencia añadida mientras el
decodificador va al día: lo que se descarta es lo ya decodificado, al presentar solo el último); y
al llegar un intra el invitado puede volver a pedir otro sin esperar 250 ms. Resultado: de 3-8
grupos descartados por sesión de 10 s en `media` y `mala` a 0-1. Tests:
`video_waits_for_the_retransmission_when_the_rtt_is_long`, `announced_track_intervals_are_clamped`
y la matriz.
