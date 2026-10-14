# Cambios

Registro de las versiones de coroom. Formato de
[Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/); versiones con
[versionado semántico](https://semver.org/lang/es/). La versión del **protocolo** (la que tienen que
compartir cliente y servidor) va aparte, en `PROTOCOL_VERSION` y en `GET /version`.

## [Sin publicar]

### Añadido

- **Bitrate máximo «Sin límite»** (Ajustes → Vídeo y el panel «Transmisión» de la sala), además
  de 150 Mbit/s: el control de tasa sube mientras el camino no haga cola, hasta llenar el enlace
  (techo técnico de 500 Mbit/s). Arranca en 40 Mbit/s como mucho, para no llenar un enlace lento
  con el primer intra, y si el codificador no acepta un bitrate tan alto, sigue con el que tenía
  en vez de cortar la sesión.
- **Más diagnóstico en el registro**: la línea de estadísticas del anfitrión dice cuántos
  fotogramas dio la fuente antes de la cadencia y cuántos sobraban, llegaron solo con el cursor o
  repetían la imagen anterior (`captura 30.0 fps (la fuente da 60.0: …)`); en Windows se anota
  qué ventana se captura, su tamaño y si está minimizada; y el sondeo dice por qué no abre cada
  codificador por hardware que trae FFmpeg (también en `coroom doctor`, «Por hardware que no
  abren en este equipo»).
- **Cada uno decide a quién oye** (protocolo 9: hay que actualizar también el servidor). El
  anfitrión manda a los invitados quién está en la sala, con su nombre; el invitado los ve en el
  panel «Personas en la sala» de la barra y silencia o vuelve a oír a cada uno con el icono de
  auriculares. El anfitrión hace lo mismo desde la tarjeta de cada invitado. Solo cambia lo que suena
  en ese equipo: los demás le siguen oyendo, y no depende de lo que elija el anfitrión.
- **Dirección de la conexión**: la sala muestra por qué IP y puerto va cada invitado («directo
  192.0.2.24:50412») y el invitado ve la del anfitrión (o la del relay); también en el registro.
- **Cambios con la sala abierta** (panel «Transmisión» de la sala): qué se comparte (pantalla o
  aplicación, y el monitor o la ventana), las aplicaciones que suenan y el códec y el bitrate
  máximo, sin cerrar la sala ni echar a nadie. Al cambiar de fuente, los invitados ven la última
  imagen hasta el primer fotograma de la nueva (si tiene otro tamaño, el codificador se reabre
  solo); si la nueva no arranca, sigue la de antes. En Linux, el teclado y el ratón de los
  invitados pasan a la sesión del portal nueva. El audio cambia sin cortarse; el bitrate se aplica
  al momento al control de tasa de cada invitado (también subiéndolo por encima del de al abrir) y
  un códec o codificador nuevo se renegocia con los invitados (un intra). El códec, el codificador
  y el bitrate de Ajustes → Vídeo también se aplican a la sala abierta; la resolución, los fps,
  4:4:4 y HDR, a la siguiente.
- **Registro en un fichero** (`<datos>/logs/coroom.log`, el de la ejecución anterior en
  `coroom.1.log`; `coroom doctor` dice dónde): sesiones, códec y backends elegidos, errores del
  decodificador, formato del audio, una línea cada 5 s con los fotogramas por segundo y tiempos de
  cada etapa en cada lado, si el vídeo recibido va sin copia y las extensiones de Vulkan Video que
  el driver ofrece sin activar.
- **Mandos tal cual** (protocolo 8). Con anfitrión Linux, el mando del invitado llega con su
  VID/PID, su nombre y su descriptor HID (UHID), y sus informes sin traducir: Steam, SDL y los
  juegos lo ven como ese mando, con sus botones extra, y la vibración y las luces vuelven al
  invitado. Un mando de Xbox por cable llega con su identidad. El anfitrión elige por invitado
  «Tal cual» (por defecto) o «Convertido a Xbox 360» y ve qué mando tiene cada uno; si no se puede
  tal cual, va convertido y se dice por qué. Consola: `allow N raw` / `deny N raw`.
- **Mandos tal cual con anfitrión e invitado Windows** (driver de mandos, protocolo 3; hay que
  volver a ejecutar `install.ps1`). El driver crea al momento, con la Virtual HID Framework, el
  mando del invitado con su VID/PID y su descriptor, y tiene cuatro mandos de **Xbox 360** para
  XInput: los convertidos van a ellos (ahora los ven también los juegos que solo usan XInput, sin
  Steam Input) y un Xbox por cable tal cual llega con su identidad. Ya no hay mandos fantasma: los
  juegos solo ven los que coroom está usando. El invitado Windows lee su mando en crudo con
  `hidapi`.
- El invitado elige qué mando envía (panel de mandos, `Ctrl+Alt+J`): automático (el último que
  conecte) o uno concreto, que se recuerda. Conectar y desconectar mandos en plena sesión se
  avisa y se sigue al momento.
- Objetivo de fuzzing `proto_hid` para el parser de descriptores HID.

- Compartir tiene dos modos. **Transmitir pantalla**: un monitor, con el audio de todas las
  aplicaciones salvo las que quites (se recuerdan). **Transmitir una app**: una ventana, con el
  audio solo de esa aplicación. La lista de audio es igual en Linux y en Windows y coroom nunca se
  captura a sí misma, así que la voz de la sala no vuelve como eco.
- Windows: captura de una ventana (con paso automático a su monitor si sale en negro por la
  pantalla completa exclusiva) y elección del monitor.
- `--audio apps-except A,B` y `--audio apps-only A,B`.
- *Ajustes → Vídeo → Avanzado*: codificador del anfitrión y decodificador del invitado.
- El invitado avisa cuando el vídeo llega dañado (muchos fotogramas que el decodificador rechaza).
- Linux (Wayland): «Elegir…» / «Cambiar…» abre el selector del escritorio antes de transmitir, la
  pantalla muestra lo elegido y al empezar se repite sin volver a preguntar.

### Corregido

- **El Flatpak 1.6.2 se cerraba al abrirlo** (fallo de segmentación dentro de FFmpeg al comprobar
  los codificadores): el `rpath` que se añade al compilar apuntaba a las bibliotecas del sistema
  del runtime y pasaba por delante de la extensión `codecs-extra`, así que se cargaba el FFmpeg
  recortado de la Platform en vez del de la extensión. El `rpath` ya no apunta nunca a un
  directorio del sistema, y el registro (al arrancar) y `coroom doctor` dicen de qué directorio se
  cargó FFmpeg de verdad.
- **Anfitrión Linux a 120 fps daba unos 30**, a ratos ninguno: con el ratón en movimiento,
  PipeWire manda búferes solo con el cursor, y la cadencia pedida los contaba como fotogramas. El
  hueco lo ocupaba uno de esos y el fotograma del juego que llegaba detrás se descartaba; si solo
  se movía el cursor, no pasaba ninguno. Ahora la cadencia solo cuenta los búferes con imagen.
- **Panel «Transmisión» en Windows**: con un título de ventana largo, el desplegable se quedaba
  con todo el ancho de la fila y el texto de al lado salía con una letra por línea. La ventana (o
  el monitor) va ahora en su propia fila, con un desplegable de ancho fijo y los títulos largos
  recortados con «…»; lo mismo en la pantalla de compartir.
- **Cortes de 100-350 ms con pérdida de paquetes**: cuando se perdía un paquete del vídeo, el
  aviso del invitado para pedirlo (`Nack`) y la petición de intra iban solo por el stream de
  control de QUIC; si ese aviso también se perdía, QUIC no lo reenviaba hasta su temporizador
  (60-70 ms con un RTT de 20 ms) y el fotograma vencía. Ahora salen además como datagrama
  (pista `Control`, compatible con la 1.6.0: quien no lo entiende lo ignora) y el anfitrión y el
  relay no reenvían dos veces lo mismo. QUIC pide además confirmaciones en 5 ms (extensión ACK
  frequency) en vez de 25. El aviso se pide en cuanto falta un paquete por debajo del último
  recibido (como RACK en TCP) o tras un silencio a la medida del ritmo del grupo, no al llegar a
  medio plazo.
- **Anfitrión Windows: los invitados se oían a sí mismos** en el audio del juego. Cada aplicación
  se capturaba con `new_application_loopback_client(pid, false)`, que en WASAPI no es «el proceso
  sin sus hijos» sino «todo el sistema salvo ese proceso»: cada captura traía todo lo demás,
  también las voces de la sala que reproduce coroom (y el resto del audio repetido una vez por
  aplicación). Ahora cada aplicación se captura con su árbol de procesos (sin repetir un proceso
  cuyo padre ya se captura) y, cuando no se separa por aplicación, se captura todo salvo coroom.
  En Windows anterior a 10 20348 no hay forma de quitar a coroom: se sigue capturando toda la
  salida.
- **Cuelgues de 17 s del vídeo**: si se perdían más de 8 fotogramas enteros seguidos (un corte de
  wifi de unos 150 ms a 60 fps), el receptor se quedaba esperando un fotograma que nadie iba a
  pedir y descartaba todo lo demás hasta reiniciar la pista 1024 fotogramas después. Ahora esos
  fotogramas se dan por perdidos al momento, se pide un intra y, en cuanto llega uno entero, se
  entrega sin esperar a los plazos de lo anterior.
- **Recuperación tras un corte de red en menos de 500 ms** (antes, segundos): la ventana de
  congestión de QUIC se llenaba con lo enviado durante el corte y la conexión no volvía hasta el
  temporizador de pérdida de QUIC, que se duplica en cada intento; ahora aguanta 2 s de corte al
  bitrate objetivo. Un enlace atascado ya no acumula fotogramas viejos que retrasan el intra que lo
  arregla. Test con cortes de 600 ms y 1 % de pérdida: el vídeo vuelve en 100–130 ms.
- `Nack` de más con bitrates altos: se pedía lo que faltaba de un fotograma 12,5 ms después de su
  primer paquete, cuando el pacing aún lo estaba enviando (80 Mbit/s: unas 150 peticiones y 3000
  paquetes repetidos cada 5 s en una LAN). Ahora se pide tras un silencio del fotograma, y el plazo
  de un intra incluye los dos intervalos en que se reparte (antes vencía a mitad de camino y
  empezaba una tormenta de intra).
- Si el vídeo no va en 4:4:4 o HDR aunque se pidió, el anfitrión ve por qué: qué invitado no lo
  acepta (desactivado en su equipo o sin decodificador) o que su codificador no lo hace.
- Cambiar el códec o el bitrate con la sala abierta lo confirma con un aviso y queda en el
  registro; el panel muestra el bitrate que se usa ahora.
- Los descartes de la comprobación de imagen dicen el códec («codificar HEVC con AMF»).
- **Comprobación de imagen de los códecs por GPU al abrirse.** El sondeo ya no solo mira que
  cada codificador abra: los de hardware codifican 300 fotogramas de un patrón en movimiento (unos
  10 s, con fotogramas predichos), se decodifican por software y cada uno se compara con el
  original; los decodificadores de hardware, igual con una muestra. Los que dan vídeo dañado en ese
  equipo quedan fuera de Automático, con el motivo en `coroom doctor` y en el registro, y se
  vuelven a usar solos cuando el driver lo arregle. Se va la lista fija que dejaba AV1 por Vulkan
  Video solo a mano: en las NVIDIA RTX 50 (Blackwell), cuyo driver codifica mal AV1 por Vulkan
  (fallo conocido de NVIDIA), lo descarta la comprobación, y en las demás GPU se usa.
- **La cadencia pedida (30, 60 o 120 fps) sale igual en Windows y en Linux, con cualquier
  monitor.** Se le pedía a la captura un intervalo mínimo de `1 / fps` (WGC en Windows,
  `maxFramerate` de PipeWire en Linux), y eso redondea al múltiplo del refresco: 60 fps daban 48
  con un monitor de 144 Hz y 56,7 con uno de 170. Ahora la captura recibe cada refresco (en Linux,
  el compositor hasta 4 veces la cadencia y al menos 240) y se queda con los de la cadencia pedida
  por su marca de tiempo (`SystemRelativeTime` en Windows, la de presentación del compositor en
  Linux), alternando huecos de dos y tres refrescos; los que sobran se descartan antes de copiar
  nada.
- **Con anfitrión Linux y codificador por GPU, la captura se paraba a los pocos fotogramas**
  (invitados en «Entrando en la sala… Vídeo» con el audio sonando, o con un solo fotograma, en
  cualquier códec). Los filtros de Vulkan de FFmpeg sueltan su entrada al reutilizar su contexto
  de ejecución, decenas de fotogramas después, y el DMA-BUF mapeado retenía el búfer de PipeWire:
  el compositor se quedaba sin búferes y dejaba de mandar fotogramas. Ahora FFmpeg recibe
  duplicados de los descriptores y el búfer vuelve a PipeWire en cuanto sale el paquete del
  fotograma. Con `COROOM_ZERO_COPY=0` no pasaba porque la captura iba por memoria de CPU.
- **Invitado sin imagen con anfitrión Linux** cuando la ventana o la pantalla capturada deja de
  redibujarse (un juego en pausa al perder el foco, una ventana tapada o minimizada): con la
  captura de PipeWire por DMA-BUF no había nada que repetir y el invitado se quedaba en el primer
  fotograma o sin entrar, pidiendo intra en vano. Ahora se retiene el último búfer (uno de los 4 a
  16 que se negocian) y el intra sale con él.
- El registro del invitado lleva también una línea de **audio** cada 5 s: paquetes, tramas
  ocultadas, tardíos, veces que se vació la cola, recortes y estiramientos y las faltas de la
  salida.
- **Vídeo que no llegaba o se congelaba** («Entrando en la sala» con el audio sonando, un solo
  fotograma y nada más, al volver a entrar en una sala): el invitado pide un intra mientras no
  tiene imagen, sin esperar a que lleguen fotogramas; el anfitrión manda uno cuando el invitado
  está listo y repite el último con la pantalla quieta; tras una pérdida no se decodifica contra
  referencias que faltan (con AV1 salía basura) y un decodificador que falla una y otra vez da
  paso al siguiente o a otro códec.
- **Cortes de vídeo periódicos, descartados y saltos de latencia por internet**: cada fotograma
  sale repartido a un ritmo cercano al bitrate (antes, en 4 ms: un intra a 120 fps desbordaba el
  búfer del socket del receptor y se pedía otro igual de grande), con búferes de socket de 8 MB,
  el envío empieza a la mitad del máximo, un solo intra en vuelo a la vez y los reenvíos grandes
  también con ritmo. En los codificadores que no cambian de bitrate en marcha (todos menos NVENC y
  QSV), los cambios pequeños ya no reinician el codificador.
- **Audio robotizado en un invitado Windows**: WASAPI reproduce a demanda, como PipeWire, y llena
  el búfer del dispositivo hasta unos 20 ms en cada vuelta (antes, 10 ms como mucho: con un
  periodo distinto del de 480 muestras se quedaba sin audio unas cien veces por segundo). La
  sincronía con el vídeo ya no deja el colchón del audio por debajo de 20 ms.
- **«Abriendo la sala…» para siempre y desconectado del servidor** tras terminar una sala sola:
  la sesión anterior se cierra aparte, el arranque tiene un límite de tiempo y, si falla, se dice
  por qué y se sigue conectado; los errores de la sesión se muestran en lugar de «Has dejado de
  compartir». En el servidor, un «desconectado» que llega tarde ya no pisa al «conectado».
- **«Buscando…» en Compartir** hasta tocar algo: la lista se pide al entrar y el refresco
  automático funciona (antes la ventana se redibujaba sin parar); los errores al listar pantallas
  y ventanas se muestran.
- Windows: el ahorro de energía ya no anula los temporizadores de 1 ms de coroom.
- La señalización usa el control de congestión estándar de QUIC.

- **Driver de mandos de Windows** (el de la v1.3.0 puede no arrancar: hay que volver a ejecutar
  `install.ps1` con el paquete nuevo). El INF pedía UMDF 2.35, la última del WDK con el que se
  compila, en lugar de 2.31 (Windows 10 22H2), y Windows no arrancaba el dispositivo de control
  en los sistemas sin esa versión; `build.ps1` comprueba ahora la versión del INF. Los Xbox 360
  llevaban `xinputhid` como filtro: donde no existe, no arrancaban (código 19), y donde existe se
  cargaba sobre un dispositivo que no es HID. Ahora no llevan filtros, como uno por cable.
- La CI instala el driver en un Windows (job «Prueba del driver instalado» de `driver.yml`) y lo
  usa con el cliente de coroom antes de publicarlo: mando tal cual con su VID/PID, informes,
  salidas y características; Xbox 360 en XInput; mando genérico; un solo cliente; desinstalación
  sin restos (`crates/input/tests/windows_driver.rs`).
- 8BitDo Ultimate 2 en Linux: el stick derecho se leía como gatillos (faltaba su mapeo de SDL).
- Flatpak: `--device=all` en lugar de `--device=input`, que no daba `/dev/hidraw*` ni `/dev/uhid`.

- Audio: en la 1.1.3, tras un parón de la red, el audio podía sonar 30-70 ms tarde durante
  varios segundos (la cola solo se recortaba una trama por segundo). Ahora, si sobran dos tramas o
  más, se recorta una cada 100 ms; la que sobra sola sigue esperando un segundo.
- «Cambiar de servidor» ya no cierra la sesión: la lista de servidores tiene «Volver» a la
  cuenta abierta, y elegir un servidor con la sesión guardada entra sin pedir la contraseña (solo
  la pide si no la hay o ya no vale).
- KDE Plasma: compartir solo ofrecía «control remoto» sobre todos los monitores, sin dejar elegir
  uno ni una ventana. La captura y el control remoto van ahora en dos sesiones del portal: primero
  el selector de pantallas y ventanas del escritorio, después el permiso de teclado y ratón. Con un
  monitor, el ratón del invitado va a ese monitor; con una ventana, se mueve en relativo.
- La pantalla «Compartir» vuelve al diseño aprobado: dos tarjetas con icono, lo elegido a la vista
  con «Cambiar…», la lista de audio con su estado y «Empezar a transmitir».
- Linux: compartir pantalla se quedaba en «Abriendo la sala…» sin mostrar el diálogo, y el portal
  Secret no respondía. La conexión a D-Bus de los portales vivía en un runtime que ya había
  terminado; ahora `ashpd` usa su propio ejecutor (`async-io`).
- Linux: cada paso del portal de captura tiene un límite de tiempo y explica qué falló. Si el
  portal RemoteDesktop falla, se comparte la pantalla sin control remoto y se avisa.
- Windows: la aplicación se cerraba al compartir con Vulkan Video (texturas D3D11 importadas en
  Vulkan). Esa ruta va con copia salvo `COROOM_ZERO_COPY=1`; NVENC, AMF y Quick Sync siguen sin
  copias.
- Windows: NVENC, AMF y Quick Sync van antes que Vulkan Video, y AV1 por Vulkan Video ya no se
  elige solo (la imagen se volvía gris al cabo de unos segundos).
- Las notas de la versión decían que el driver de mando virtual se instalaba con la firma de
  prueba; el paquete va sin firmar y hay que firmarlo en tu equipo.
- En Flatpak sobre KDE Plasma, iniciar sesión, crear una sala y unirse se quedaban esperando al
  portal Secret sin respuesta. Ahora se espera como mucho 5 s y, si no contesta, se usa un fichero.
- Crear y unirse a una sala respetan `COROOM_DATA_DIR` al cargar la identidad, como el login.
- «Cancelar» mientras se entra en una sala vuelve al inicio en el momento.
- La aplicación escribe sus logs en stderr (`RUST_LOG` elige el nivel; por defecto, avisos).

### Cambiado

- **El Flatpak usa el FFmpeg del runtime** (8.1.3, el mismo que antes se compilaba aparte) y, al
  ejecutarse, el de la extensión `org.freedesktop.Platform.codecs-extra`, que se instala sola con
  el runtime: los codificadores de Vulkan Video, el decodificador HEVC, VA-API, NVENC/NVDEC, AMF,
  SVT-AV1 y dav1d. Con esa extensión, el FFmpeg que carga el Flatpak es GPL (x264 y x265); coroom
  no lo distribuye.
- CI más rápida: el Flatpak ya no compila FFmpeg y guarda entre ejecuciones el `target/` de cargo;
  la imagen del servidor se construye en un runner nativo por arquitectura, con el `target/`
  guardado, y «Publicación» une y etiqueta esas imágenes sin volver a compilar.
  `tools/prune-cargo-target.py` borra lo que la última compilación no usó, para que esas cachés no
  crezcan.
- Audio con unos 13 ms menos de latencia en Linux (mediana del audio del juego en el test de
  extremo a extremo: de 46,7 a 33,4 ms; desfase A−V medio de 26 a 15 ms). La salida de PipeWire
  reproduce a demanda: su callback pide cada trama justo cuando el dispositivo la necesita, en vez
  de tomarla de una cola llenada una trama antes. Además, el buffer de jitter ya no se queda una
  trama por encima de su objetivo: se ceba en el objetivo y recorta cuando durante un segundo le
  sobra una trama. WASAPI sigue con la cola.
- El audio se recibe y se descifra en un hilo propio (`audio-net`) en vez de en el runtime de la
  red, que comparte sus hilos con el vídeo, QUIC y el control: un fotograma grande ya no retrasa
  el audio. Los hilos de audio piden prioridad de tiempo real: en Linux `SCHED_FIFO` si el límite
  lo permite o, si no, RealtimeKit (o el portal Realtime en Flatpak), como PipeWire; en Windows,
  MMCSS «Pro Audio», como el motor de audio del sistema. Si no se concede, siguen con prioridad
  normal.
- Windows: el driver de mando virtual pasa de KMDF sobre Virtual HID Framework a un minidriver HID
  de UMDF2 en modo usuario bajo el `mshidumdf.sys` de Windows. Se instala sin firma de prueba y
  con el arranque seguro y la integridad de memoria activados: la CI firma cada paquete con un
  certificado autofirmado de un solo uso (solo firma de código, clave borrada al terminar) que
  `install.ps1` instala en el equipo y `uninstall.ps1` quita. Los mandos existen desde la
  instalación (`install.ps1 -Pads`, 4 por defecto) y `coroom` les manda el estado por una colección
  HID de control (protocolo 2 del driver); si coroom se cierra, vuelven al reposo en menos de un
  segundo. Los paquetes de la release se llaman `coroomvhid-X.Y.Z-x64.zip` y `-ARM64.zip`. Sin
  probar todavía en un Windows real.
- La pantalla de compartir ya no pide el nombre de la aplicación ni el PID del proceso.
- El Compose del servidor usa solo la imagen publicada, pasa cada variable de `.env` con su valor
  por defecto y guarda todo en `./data`, también el certificado (`./data/certs`).
- El modo TLS por defecto del Compose es `files`: el certificado lo pone el operador.

### Eliminado

- `build.ps1 -TestSign` y las instrucciones de firma de prueba y por atestación del driver de
  mando.
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
