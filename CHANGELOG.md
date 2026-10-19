# Cambios

Registro de las versiones de coroom. Formato de
[Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/); versiones con
[versionado semántico](https://semver.org/lang/es/). La versión del **protocolo** (la que tienen que
compartir cliente y servidor) va aparte, en `PROTOCOL_VERSION` y en `GET /version`.

## [Sin publicar]

### Añadido

- **Vibración con todos los mandos compartidos** (protocolo 18; hay que actualizar también el
  servidor y, en un anfitrión Windows, **reinstalar el driver de mandos** con el `install.ps1` de
  esta versión: el anterior no sirve y los mandos no se crean hasta actualizarlo): además de los
  mandos tal cual por HID, ahora vibran los **convertidos a Xbox 360** y los **mandos de Xbox por
  cable**. En Linux el mando virtual acepta la vibración del juego como un mando de Xbox 360 real;
  en Windows, el driver recoge la que el juego pide por XInput. El invitado la aplica a su mando,
  sea cual sea, si vibra (si no, se avisa una vez en el registro); al soltar el mando, cambiar de
  mando o salir de la sala, los motores se paran. En un invitado Linux la vibración llega al
  momento y ya no puede cortar la de un mando tal cual (antes, la biblioteca de mandos podía
  mandarle los motores a cero cada 50 ms).
- **Imagen del servidor para arm/v7**: además de amd64 y arm64, `ghcr.io/…/coroom-server` se publica
  para Raspberry Pi con un sistema de 32 bits. La CI la compila de forma cruzada desde amd64.
- **Avatares** (protocolo 16; hay que actualizar también el servidor): cada uno elige una imagen en
  Ajustes → Cuenta y servidor (PNG, JPEG o WebP; se recorta al centro y se reduce a 256 px). El
  servidor la valida y guarda su propio PNG, y avisa del cambio a los amigos y a quien comparte sala.
  El administrador ve los avatares en `/admin` y puede quitar cualquiera.
  Se ve en la cabecera, los amigos, las invitaciones, los integrantes, el chat y el indicador de
  quién habla; sin imagen, las iniciales con un color fijo por nombre.
- **Quién habla, una línea por persona** con su avatar, en el orden en que empezaron a hablar (hasta
  4 y «y N más»); en Integrantes, un aro verde en el avatar de quien habla.
- **Sonidos de aviso** en una página nueva, «Notificaciones»: entra o sale alguien, mensaje del chat,
  alguien pide entrar (solo el anfitrión), te invitan, te dejan entrar, ahora eres el anfitrión y
  solicitud de amistad. Nueve tonos propios, elegibles por evento, con «Probar», «Ninguno» o un
  fichero propio, y un volumen de avisos. Suenan por la salida de «Audio y voz» y nunca llegan a la
  sala; varios a la vez suenan una sola vez. Las solicitudes de amistad llegan al momento.
- **El teclado y el ratón se toman con un clic**, como en QEMU: con el permiso, no se envía nada
  hasta hacer clic en la imagen; Ctrl+Alt+R (configurable en Ajustes → Mandos) los suelta sin
  perder el permiso, y perder el foco también. Un aviso junto a la latencia dice cómo soltarlos o
  tomarlos.

- **Salas solo de voz y «Dejar de compartir»** (protocolo 14; hay que actualizar también el
  servidor): en Compartir, «Solo voz» abre la sala sin compartir nada; dentro, «Compartir pantalla
  o ventana» empieza y «Dejar de compartir» para sin cerrarla. Los invitados ven «Nadie está
  compartiendo» y no reciben ni la última imagen (tampoco quien entra después y pide un intra).
  `coroom host --source none` abre una así.
- **Pasar la sala a otro** (protocolo 14): «Pasarle la sala» en el menú de un invitado; si acepta,
  abre una sala solo de voz, contesta con su código y todos (el anfitrión de antes también)
  entran en ella sin escribirlo.
- **Integrantes**: una sola lista en la sala, igual para anfitrión e invitado (primero tú, después
  el anfitrión y los demás), con quién habla. En las filas ajenas el deslizador es lo alto que se
  oye a esa persona aquí y su altavoz la silencia solo aquí (tachado y con la barra apagada); en
  la propia, lo alto que te oyen y el altavoz te silencia para todos. Los permisos, el número de
  seguridad, pasar la sala y expulsar van en el menú «Más» de cada invitado.
- **Quién habla**, arriba a la izquierda del vídeo del invitado (Ajustes → Sala, activado).
- **Volumen de tu voz** (lo que se envía), recordado, y **micrófono y salida que cambian con la
  sala abierta**, sin reconectar: desde la sala, el panel del invitado o Ajustes.
- **Probar el micrófono** en Ajustes → Audio y voz: el nivel y, con «Oírte», tu voz por la salida.
- La pestaña **Ajustes** del panel del invitado: VSync, decodificar por software (apagado de
  partida; cambia al momento, sin reconectar), micrófono y salida, volumen del juego y el mando; «Conexión» queda solo con datos.
- El texto del chat **se selecciona** con el ratón como en cualquier chat: arrastrar (también a
  través de varias líneas), doble clic para la palabra, triple para la línea y Mayús+clic para
  ampliar; se copia con `Ctrl+C` o el botón derecho. Los enlaces siguen subrayados y se abren con
  un clic sin arrastrar. El mensaje entero, con el botón que aparece al pasar por encima o con el
  botón derecho sin nada seleccionado.
- **«Predeterminado del sistema (…)»** en los selectores de audio, con el dispositivo que es.

- Volumen del audio del juego en el invitado, aparte del de cada participante (0-200 %, recordado):
  deslizador «Volumen del juego» encima de «Personas en la sala», con el botón de silenciarlo
  delante.
- **Volumen por participante**: junto al botón de oír o no a cada persona (la tarjeta de cada
  invitado en Compartir y «Personas en la sala» del invitado) hay un deslizador de 0 a 200 % (de
  5 en 5, Ctrl+clic vuelve al 100 %) que sube o baja su voz solo en este equipo; a más del 100 %
  el limitador suave del mezclador evita que recorte. El volumen se recuerda por nombre de cuenta
  (`voice_volumes` en los ajustes locales) y se aplica solo cuando esa persona vuelve a
  coincidir en una sala. La propia voz no tiene deslizador.
- **El permiso de compartir la pantalla se recuerda** (Linux): el diálogo del portal sale una
  vez; el token de restauración se guarda en `portal.json` del directorio de datos (solo legible
  por el usuario), por separado para pantalla y ventana, y lo usan la aplicación, `coroom host`
  y `coroom probe`. Si deja de valer (revocado desde Ajustes del sistema, sesión caducada) se
  pregunta de nuevo una sola vez. Ajustes → Sala tiene «Olvidar el permiso de pantalla».
- **«Pedir entrar»** (protocolo 13; hay que actualizar también el servidor): la lista de amigos
  de Inicio dice qué hace cada uno (Disponible, En sala, Jugando o Desconectado) al momento, y a
  quien tiene una sala abierta se le puede pedir entrar con un botón; al anfitrión le aparece en
  Compartir una tarjeta «X pide entrar» con «Dejar entrar» (que invita) y «Ahora no» (que avisa).
  La petición caduca a los 60 s en los dos lados, el servidor solo la acepta entre amigos y una
  vez cada 5 s, y la invitación del anfitrión al que pidió entrar se acepta sola. En la consola de
  `join --headless`, `ask NOMBRE`.
- **Sonido del chat en Windows** (`PlaySound`), con un tono propio sintetizado (sin ficheros con
  derechos) cuando no hay tema de sonidos; **tono personalizado** en Ajustes → Sala (WAV en
  Windows; WAV, OGG o FLAC en Linux) con «Probar», «Elegir…» por el portal de ficheros y «Quitar».
  El sonido sigue pudiendo apagarse (ajuste local).
- **El codificador «auto» se calibra midiendo** (decisión del propietario): al abrir la sala, si
  hay más de un backend para el códec, se codifican 30 fotogramas con cada uno y se elige el de
  menor p95 de latencia; el resultado se guarda en `calibration.json` por GPU, driver, códec,
  resolución y formato, y solo se repite si cambian. En una GPU NVIDIA de escritorio NVENC tarda
  2,1 ms y Vulkan Video 3,0 ms (H.264 y AV1) y «auto» elegía el segundo. La calibración también
  codifica 6 fotogramas difíciles (bloques de color al azar, con un intra forzado) y mide el
  mayor contra lo que cabe en un intervalo del techo: los backends que pasan de 1,1× (VA-API con
  QVBR llega a ~6× con partículas) quedan detrás de los que lo respetan, aunque tarden menos. Las
  calibraciones guardadas sin esa medida se repiten una vez.
- **Vigilante de medios** en el invitado: 3 s sin datagramas ni `Pong` con el códec ya acordado
  y se pasa a reconectar, sin esperar al idle de QUIC (que baja de 15 a 8 s; keep-alive a 2,5 s).
- `coroom doctor` dice la versión de libva que hay y si sirve, y alinea la tabla de códecs.

- **Audio del juego con 30-45 ms de latencia propia** (medida de captura a altavoz, no
  estimada). PipeWire captura y reproduce en el hilo de tiempo real (`RT_PROCESS`) y cada trama
  lleva la hora real en que el dispositivo la capturó (`pw_stream_get_nsec` más el retardo del
  stream; en Windows, la hora QPC de cada búfer de WASAPI y la posición de `IAudioClock` al
  reproducir). El búfer de captura guarda como mucho 3 tramas y nunca se queda más de una por
  detrás de la mezclada; el ritmo de envío se mide con el periodo que informa el dispositivo; la
  sincronía A/V se recalcula cada 100 ms y nunca retrasa el audio más de 60 ms; el búfer de
  jitter mide y decide en tramas de 10 ms; y la reproducción ya no asigna memoria ni toma
  cerrojos: las estadísticas, el emparejamiento con el vídeo y los flujos nuevos van en un hilo
  aparte. Lo comprueban `crates/audio/tests/latency.rs` (30,7-39,7 ms en el banco) y el test de
  audio del escritorio (mediana 35-44 ms de extremo a extremo).
- **El vídeo se presenta en cuanto llega**: el invitado dibuja cada fotograma decodificado al
  momento, en su propia pasada sobre el mismo dispositivo de la ventana, sin esperar al redibujado
  de la interfaz.
- **Chat de texto en la sala** (protocolo 12; hay que actualizar también el servidor), cifrado de
  extremo a extremo con la clave de la época MLS (`crates/crypto/src/chat.rs`): panel lateral en
  ventana y superpuesto a pantalla completa (`Ctrl+Alt+C`), burbuja de 5 s con los mensajes
  nuevos, sonido y notificación de escritorio en Linux (se apagan en Ajustes → Sala), enlaces
  clicables con el dominio visible, historial de los últimos 50 mensajes para quien entra tarde,
  también en la sala de espera, 2000 caracteres y 10 mensajes cada 10 s por persona; el anfitrión
  puede vaciar el historial. En la consola de `join --headless`, `say` y `clear-chat`.
- **Interfaz nueva** con cinco pantallas rediseñadas (Inicio, Compartir, Sala, Ajustes y
  Diagnóstico) y una cabecera común con el servidor y su RTT siempre a la vista: Inicio con las
  invitaciones, las solicitudes y los amigos ordenados por conexión; Compartir con perfiles de
  calidad (Mínima latencia, Equilibrado, Máxima calidad o Personalizado) y las tarjetas de los
  invitados; la sala del invitado con barra flotante que se oculta sola a pantalla completa
  (también la píldora de latencia, 2,5 s con el ratón quieto), panel lateral con Chat, Conexión
  y Sala, y sincronización vertical opcional; Ajustes en seis páginas que se guardan al cambiarlos
  (Cuenta y servidor, Audio y voz, Sala, Mandos, Apariencia y Acerca de); y Diagnóstico como
  panel sobre cualquier pantalla, con la ruta, los candidatos directos, la latencia por etapa y
  «Copiar informe» sin direcciones ni nombres. Los textos viven en catálogos Fluent por pantalla
  (`apps/desktop/locales/es/`) y un test comprueba que no queda ninguno fuera.
- **Bitrate siempre «Auto»**, con un tope opcional («Auto hasta X» o «Auto sin tope», en
  Compartir y en el panel «Transmisión» de la sala): el control de tasa sube mientras el camino
  no haga cola, hasta el tope o hasta llenar el enlace (techo técnico de 500 Mbit/s). Antes de
  arrancar, una **sonda de enlace** (dos trenes de 48 datagramas por la pista Probe) mide lo que
  cabe y el vídeo arranca al 70 % de lo medido, en vez de a un valor fijo; si el codificador no
  acepta un bitrate tan alto, sigue con el que tenía en vez de cortar la sesión.
- **Abrir el puerto en el router** (PCP, NAT-PMP o UPnP, implementados en
  `coroom_transport::nat`, sin salida a internet): el anfitrión pide al router que deje entrar la
  conexión directa y anuncia su dirección WAN como candidato; si la red es CGNAT se dice y se va
  por el relay. Ajuste «Abrir el puerto en el router» (activado por defecto), `--no-port-mapping`
  y la sección «Router» de `coroom doctor`. El diagnóstico de la conexión dice qué candidato
  directo se probó y por qué se acabó en el relay.
- **Toda la entrada por datagramas** (protocolo 11): cada paquete lleva el estado completo de
  teclado, ratón y mandos, con reconciliación por diferencia ante huecos y repeticiones a 3, 8 y
  20 ms, así una pérdida no deja una tecla pulsada ni se pierde un clic; la latencia de la entrada
  se mide por etapas (cola, red, inyección), los mandos se leen a 1 ms (gilrs) y el driver
  virtual de Windows actualiza a 500 Hz.
- **Los invitados entran hablando**: todo invitado oye la sala sin pedir permiso y su micrófono
  se abre al entrar salvo que el anfitrión active «Los invitados entran callados» (Ajustes →
  Sala); el permiso «Voz» solo gobierna el micrófono.
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

- **Parpadeo en el escritorio con NVENC, AMF H.264 e Intel H.264/HEVC**: esos codificadores
  usaban *intra-refresh*, que recodifica desde cero una franja de la pantalla en cada fotograma
  para renovar la imagen entera en un segundo. Con la imagen quieta, esa franja se veía como un
  temblor constante (en un juego queda tapada por el movimiento) y además gastaba bits sin que
  nada cambiara. Ahora ningún codificador repinta por su cuenta: como ya hacían VA-API, Vulkan
  Video y SVT-AV1, sale un intra al empezar y cuando el invitado lo pide tras una pérdida, y en
  medio solo viajan diferencias.
- **Invitados que decodificaban 4:4:4 en la CPU**: la app daba por hecho que toda NVIDIA
  decodifica HEVC 4:4:4 por hardware; las anteriores a Turing no pueden, y FFmpeg seguía por
  software sin avisar. Un invitado así recibía 1440p a 120 fps y decodificaba unos 50, con saltos,
  intras pedidos sin parar y cortes de audio. Ahora el sondeo decodifica una muestra 4:4:4 de
  HEVC y de AV1 con cada decodificador por hardware y solo cuenta los que la sacan de la GPU con
  la imagen correcta (`coroom doctor` los lista en «Decodificar 4:4:4 por hardware»). Además, el
  invitado solo acepta 4:4:4 en un códec si lo decodifica el mismo decodificador que usaría en
  4:2:0: pedir 4:4:4 ya no pasa el vídeo de la GPU a la CPU.
- **Cambiar de cuenta en el mismo servidor** (hay que actualizar también el servidor): cerrar
  sesión y entrar con otra cuenta fallaba con «esta identidad ya está vinculada a otra cuenta». La
  identidad del equipo era de la primera cuenta para siempre. Ahora pasa a la cuenta que entra; la
  anterior pierde ese dispositivo y su sesión en él, y vuelve a tenerlo al entrar otra vez.
- **Quitar servidores de la lista**: el texto emergente «Más acciones» tapaba la única opción del
  menú «⋯». Cada servidor tiene ahora un botón de papelera que pregunta antes y borra también la
  sesión guardada en este equipo (el de la cuenta abierta no se quita: primero se cierra sesión).
- **El pie del riel ancho ya no se ve desfasado**: la sala, el micrófono y «Volver a la sala» (o
  las peticiones, como anfitrión) quedan centrados en su fila y a la misma distancia. Antes el
  icono y el nombre de los botones iban pegados arriba y, como invitado, la fila de la sala quedaba
  casi encima del micrófono. Sus textos emergentes salen a la derecha, como los de los destinos, y
  no tapan la fila de abajo.
- **El relay funciona desde la misma red que el servidor** (protocolo 22; hay que actualizar
  también el servidor). El servidor anunciaba el relay en `COROOM_RELAY_ADDR`, normalmente la IP
  pública, y un cliente en la red local del servidor, detrás de un router que no devuelve hacia
  adentro el tráfico a su propia IP pública, no lograba ligarse («ligadura con el relay: tiempo de
  espera agotado») y, sin conexión directa, no entraba. El relay atiende en el mismo puerto UDP que
  la señalización, así que ahora cada cliente se liga a la dirección por la que ya llegó al
  servidor, sea de la LAN, de una VPN o pública.
- **La calidad por el relay se estabiliza en unos 4 segundos, no en 30 o 40.** La sonda del
  enlace al arrancar podía medir de menos por el relay (46 Mbit/s en un camino que pasaba más de
  60), el objetivo se quedaba en el 70 % de eso y, sin pérdida ni techo de ráfaga, nadie volvía a
  sondear: con el codificador gastando una parte del objetivo, lo recibido tampoco lo subía. Ahora,
  mientras lo medido frena el objetivo y el camino está limpio, sale un tren a doble ritmo cada
  medio segundo hasta que ya no mide más o pierde. En directo no cambia nada (la sonda ya mide el
  ritmo máximo de un tren al arrancar). Además, un tren pedido por encima de 100 Mbit/s salía
  recortado y su resultado no se reconocía como sonda periódica.
- **El relay ya no descifra nada: solo reenvía paquetes** (protocolo 19; hay que actualizar el
  servidor y todos los clientes a la vez). Antes, el relay terminaba la conexión QUIC de cada par:
  descifraba cada paquete, lo volvía a cifrar para cada invitado y además repartía el vídeo y
  contestaba los NACK. En un servidor pequeño (una placa ARM) eso llenaba el procesador mucho antes
  que la red y el bitrate por el relay se quedaba en ~25 Mbit/s. Ahora funciona como un servidor
  TURN (RFC 8656): el anfitrión y cada invitado hablan su propia conexión cifrada de punta a punta
  a través del relay, que solo copia los paquetes UDP de uno a otro (en lotes, con GSO y GRO), sin
  claves ni cabeceras que mirar. Por el relay el bitrate lo limita la red, igual que en directo, y
  el operador ya no puede ver ni las cabeceras de los medios. El anfitrión sube una copia por
  invitado que entra por el relay, como con los que entran en directo; los NACK, el control de tasa
  y el RTT pasan a ser de punta a punta. La conexión por el relay se prepara a la vez que la
  directa y se usa si esta no llega, sin sumar tiempo. `COROOM_RELAY_MAX_KBPS` pasa a ser el límite
  **por invitado** y sentido (antes, por sala). Por el mismo motivo, en la conexión con el servidor
  (la señalización) el servidor impone ChaCha20-Poly1305 (RFC 8439) si su CPU no acelera AES-GCM,
  y el anfitrión hace lo mismo en las conexiones de pares cuando la suya es la que no lo acelera.
- **Calidad por el relay: el bitrate ya no se queda clavado abajo**, con anfitrión Linux o
  Windows. El invitado pasaba sus marcas de llegada al reloj del anfitrión con la mediana de los
  últimos ocho `Ping`; por el relay, el jitter del RTT la movía de 3 a 5 ms cada pocos segundos y
  el anfitrión lo tomaba por cola: recortaba sin pérdida ni cola de verdad, olvidaba lo que había
  medido la sonda y, sin techo de ráfaga, no volvía a sondear. Ahora el desfase sale de la muestra
  de menor RTT y se mueve como mucho 100 µs por segundo, y tras una congestión el enlace se vuelve
  a sondear al ritmo medido antes. Por el relay real con la fuente pesada: objetivo de 8 a
  19 Mbit/s y de 51 a 3 informes de cola en AV1; en directo, igual que antes (72 Mbit/s, sin
  recortes).
- **El audio del juego suena igual con anfitrión Linux o Windows**: la mezcla ya no comprime todo
  lo que pasa de −3 dBFS (lo hacía hasta con una sola aplicación, en el anfitrión y otra vez en cada
  invitado): una sola fuente sale intacta y el limitador solo actúa si la suma pasa del fondo de
  escala, sin recortes duros. La mezcla por aplicación sigue el reloj de la captura en lugar de uno
  propio (ya no mete silencios de 10 ms), el loopback de WASAPI rellena con silencio medido los
  huecos que marca el dispositivo, y en Linux «Todo el sistema» ya no captura las voces de la sala
  que reproduce el propio coroom.
- **La vibración llega con el mando compartido tal cual**: un anfitrión Linux reenvía también las
  vibraciones que el driver manda como `SET_REPORT` (DualShock 3 por USB, mandos DragonRise,
  Logitech y otros), los DualShock 4 y DualSense por Bluetooth ya se comparten tal cual (sus
  descriptores declaran informes de audio de más de 512 bytes que hacían caer al modo convertido), y
  cada informe se ajusta al tamaño que declara el mando antes de escribirlo (Windows los rellenaba
  hasta el más grande). Si el mando del invitado no acepta la escritura, un aviso dice por qué
  (permisos de udev, Steam con el mando abierto) en lugar de perderlo en silencio.
- **Los iconos y textos del riel lateral ya no saltan**: como invitado, al pasar del vídeo a Inicio,
  Amigos o Ajustes sin salir de la sala (o al volver), el riel empezaba 48 px más arriba que en las
  demás páginas.
- **El relay ya no ahoga el vídeo** (protocolo 17; hay que actualizar también el servidor): el
  servidor anuncia su límite por sala (`COROOM_RELAY_MAX_KBPS`) en el `RelayToken` y el anfitrión no
  apunta por encima (deja sitio a la FEC más alta), en lugar de descubrirlo a base de pérdidas que
  el control de tasa tomaba por congestión. Lo que suben los invitados (entrada, voz, `Feedback`,
  `Nack`) tiene su propio cubo: ya no se descarta porque el vídeo del anfitrión vació el de la sala.

- **La pantalla ya no se bloquea mientras miras una transmisión**: como invitado, con el vídeo
  delante y el anfitrión compartiendo, coroom impide que el escritorio se bloquee o apague la
  pantalla por inactividad (aunque solo uses un mando, que el sistema no cuenta). En Linux con el
  portal `Inhibit`, también en Flatpak; en Windows con `SetThreadExecutionState`. Se suelta al
  salir de la sala o cuando el anfitrión deja de compartir.
- **Cambiar de servidor con la sala abierta pregunta antes**: la sala se cierra para todos, como al
  salir de la aplicación.
- **Cerrar la ventana (o `Ctrl+Q`) dentro de una sala como invitado pregunta antes**, igual que
  `Esc`.
- **Con tu sala abierta no se puede entrar en otra**: «Entrar» con un código o desde una invitación
  queda desactivado y explica que hay que cerrar la sala propia.

- **El ratón del invitado coincide con el del anfitrión Windows con varios monitores o escalado**:
  el movimiento absoluto lleva el cursor a su píxel exacto con `SetCursorPos` después de
  `SendInput` (con dos monitores quedaba desplazado, más hacia las esquinas), y la inyección y la
  captura van en píxeles físicos (*per-monitor DPI aware v2*) también sin ventana.
- **Cambiar de fuente ya no se queda colgado**: si la fuente nueva no arranca en 10 s (en Windows,
  la captura se colgó con un juego en pantalla completa exclusiva), el cambio falla con un aviso en
  vez de dejar a los invitados con la imagen congelada mientras el anfitrión cree que comparte otra
  cosa. La del portal de Linux, que puede esperar a que elijas, no tiene plazo.
- **Aviso cuando la captura no da imagen**: si compartiendo algo pasan 10 s sin fotogramas nuevos,
  el anfitrión ve un aviso (un juego en pantalla completa exclusiva no se deja capturar en
  Windows: en ventana sin bordes sí).
- **Al pasar la sala, nadie se queda fuera por un corte al conectar**: entrar en una sala reintenta
  la conexión con el servidor tres veces (0,5, 1 y 2 s) si se corta al conectar; un rechazo o un
  servidor que no responde no se reintentan. Al anfitrión que pasaba la sala, el servidor le cortó
  la conexión justo al entrar en la nueva y tuvo que pedir entrar a mano.
- El texto del chat solo empieza a seleccionarse sobre las letras, no en el hueco a su derecha.
- Los desplegables (el «Más» de cada integrante, los selectores) eligen al soltar el botón, no al
  pulsarlo.
- La cabecera queda centrada, a la misma distancia de la barra de título del sistema que del borde
  izquierdo.
- Linux: con varios monitores, el puntero del invitado va al monitor compartido por el identificador
  que da el portal (`mapping_id`), no solo por su posición.
- AltGr nunca cuenta como el Alt de los atajos Ctrl+Alt.
- Linux: el sonido del chat ya no se colaba en el audio del juego que capta el anfitrión.

- **Silenciar tu micrófono como anfitrión, a la vista**: tu fila en «Integrantes» tiene ahora el
  botón «Silenciar tu micrófono» / «Activar tu micrófono», y el botón del deslizador de tu voz (en
  la sala y como invitado) es un micrófono: antes era un altavoz y parecía el volumen.

- Los fps y el bitrate que ve el invitado (barra de la sala, «Conexión», diagnóstico) se medían
  sobre los últimos 4096 fotogramas: al bajar de 120 a 30 fps seguía marcando unos 70 y bajaba
  durante minutos. Ahora son los del último segundo; el cambio de fps ya se aplicaba al momento.
- **«Dejar de compartir» ya no deja salir el audio del equipo**: sin nada compartido no se envía
  el audio del juego (ni en Linux ni en Windows); al volver a compartir vuelve con las
  aplicaciones elegidas. Y una sala «Solo voz» (o la que se abre al recibir la de otro) ya no se
  queda sin audio del juego al empezar a compartir.
- **Sin nada compartido, la entrada de los invitados no llega al equipo**: ni teclado ni ratón
  ni mandos (en Windows y con los mandos virtuales seguían funcionando); lo pulsado se suelta al
  dejar de compartir y todo vuelve al compartir de nuevo.
- Al pasar la sala, **quien tenía el micrófono silenciado sigue silenciado** en la sala nueva
  (el anfitrión de antes, el nuevo y los que se pasan).
- **Pasar la sala ya no se queda colgado**: cerrar la oferta con Esc o fuera del diálogo es no
  aceptarla, la oferta se acaba si el invitado sale y, mientras no contesta, la sala muestra
  «Esperando a que … acepte la sala…» con «Retirar la oferta» (si ya abría su sala, se le
  cierra).
- Al pasar la sala, el nuevo anfitrión ya no sale de la de antes hasta que esta confirma que
  recibió su código, y quien entra mientras se pasa también va a la nueva; la sala de antes se
  cierra cuando ya no queda nadie, en vez de tras una espera fija.
- Un invitado que entraba justo cuando el anfitrión dejaba de compartir (o empezaba) podía
  quedarse con lo contrario hasta el siguiente cambio.
- El altavoz propio del anfitrión ya no parpadea al silenciarse, y su fila ya no dice «sin
  micrófono» al abrir la sala.

- Un micrófono o una salida que se desconecta (o que no abre al entrar) ya no deja la sala sin voz
  o sin sonido hasta reconectar: se avisa una vez y elegir otro en la sala lo recupera.
- Cambiar de micrófono con la sala abierta ya no corta la voz mientras se abre el nuevo (hasta 5 s
  si no responde): se abre aparte y la voz sigue con el anterior hasta tenerlo.
- Si el micrófono o la salida elegidos con la sala abierta no abren, se vuelve al anterior (también
  en los ajustes guardados) y se explica por qué, en vez de quedarse elegido sin funcionar.
- La prueba del micrófono sigue el volumen de tu voz mientras se arrastra el deslizador.
- Con una sala abierta, «Oírte» en la prueba del micrófono queda desactivado (con el porqué): tu
  voz salía por la salida, volvía a entrar por el micrófono de la sala y los demás oían eco. El
  nivel sigue funcionando.
- Los avisos de los dispositivos de audio y los errores de la prueba del micrófono, en textos de la
  interfaz y sin el error interno tal cual.
- Con el volumen de tu voz al 0 % ya no se envía nada ni sales como «Hablando».
- Dejar de oír a alguien en la sala se recuerda por su nombre, como su volumen: sigue así si
  reconecta, vuelve a entrar o la sala pasa a otro.
- En «Integrantes» del invitado, la fila propia también dice «Hablando».
- Los micrófonos de Windows salían todos como «Micrófono»: ahora con su nombre (el producto).
- Cambiar la resolución con la sala abierta dejaba la imagen del invitado corrupta hasta cambiar de
  códec con algunos decodificadores por hardware: el decodificador se vuelve a abrir al cambiar el
  tamaño del vídeo.
- **Tirones en enlaces con limitador** (tiran lo que sobra sin hacer cola) y al empezar cada
  sesión: el control de tasa tomaba esa pérdida por aleatoria y no bajaba, y el pacing mandaba
  cada fotograma a unas tres veces la media. Ahora lleva un techo de ráfaga con AIMD que se
  comprueba con lo que predice y el pacing no lo pasa. Con 28 Mbit/s y limitador, de 95
  fotogramas perdidos tras los primeros 5 s a unos 10 (60 y 120 fps), con más bitrate.

- **La sala ya no se cierra cuando el codificador elegido no lee las imágenes de la captura.**
  Al abrir la sala, la captura entregaba lo que importa el primer codificador de GPU de la lista
  (en Linux, DMA-BUF para Vulkan Video); si la calibración elegía otro que no las importa ni las
  descarga (NVENC, o software con D3D11 en Windows), el primer fotograma fallaba con «no puede
  leer imágenes DMA-BUF de la captura» y la sala se cerraba. Ahora la captura sigue al codificador
  real: al negociarlo, si no lee imágenes de GPU, PipeWire renegocia el formato a memoria
  compartida y WGC copia a CPU, las dos sin reiniciar la captura; y si el fallo aparece al
  codificar, el hilo de codificación descarta ese fotograma, pide la memoria de CPU y sigue. La
  captura que se adopta al cambiar de fuente arranca ya en CPU. Error nuevo del códec,
  `GpuImport`, para distinguirlo de una configuración no admitida.
- **Al renegociar el códec con la sala abierta se abría el primer codificador de la lista**
  (Vulkan Video en Linux) aunque la calibración hubiera elegido otro: cuando entraba un invitado
  que solo decodifica H.264, una GPU NVIDIA pasaba de AV1 por NVENC a H.264 por Vulkan Video,
  que allí da menos de la mitad del bitrate. Sin medir en el aire, ahora se abre lo calibrado
  para ese códec o, si no se calibró, el backend que ganó para otro códec en la misma GPU.
- **Una resolución impar al abrir la sala** (`--width 1001`) ya no impide abrirla: se recorta a
  par, como cada fotograma de una ventana impar.
- **El invitado se caía a más de 125 fps** (144, 165, 240 Hz): el hilo de red entraba en pánico
  («min <= max» de `clamp`) al calcular cuándo pedir un paquete perdido, porque el cuarto de
  intervalo (o, desde 167 fps, el medio) quedaba por debajo del mínimo de 2 ms (3 ms). A 144 y
  165 fps pasaba en cuanto faltaba un paquete; a 240 fps, con el primero. Ahora el mínimo cede
  ante el máximo. Lo encontró `tests/qa_stream.rs`, la batería de QA con los códecs reales de la
  máquina (resoluciones de 64×64 a 8K y de 10 a 240 fps, cambios en caliente de fuente, códec y
  bitrate, entradas y salidas, expulsiones, reconexiones y pérdida), que se lanza a mano porque
  necesita GPU.
- **Abrir la primera sala tardaba ~13 s tras arrancar** sin decir por qué: esperaba al sondeo de
  códecs, que se repetía en cada arranque y en una GPU NVIDIA con Linux crea unos 15 dispositivos
  Vulkan que el driver no deja crear en paralelo. Ahora el resultado se guarda en
  `codecs.json` (directorio de datos) con la huella de la máquina (ejecutable, FFmpeg, GPU,
  núcleo, drivers de Vulkan y VA-API, y en Flatpak los commits del runtime y sus extensiones) y
  se reutiliza mientras no cambie: `coroom probe` pasa de 14,6 s a 2,0 s en esa máquina. Si algo
  que el sondeo dio por bueno no abre, la caché se borra; `coroom doctor` siempre sondea de
  verdad. Mientras se sondea, la sala dice «Comprobando qué códecs de vídeo funcionan…».
- **La vista previa de la sala no trabaja si no se ve**: en otra página o con la ventana tapada
  o minimizada, la toma no reduce ningún fotograma ni despierta la interfaz (antes, cada 500 ms
  estuvieras donde estuvieras). Al volver a Compartir aparece en medio segundo como mucho.
- **La vista previa de la sala se veía negra** en el equipo del anfitrión: iba por el compositor,
  que pone el vídeo debajo de toda la interfaz, y la página opaca la tapaba. Ahora se sube como
  capa del propio recuadro.
- **La página de Compartir volvía al principio** al tocar una opción (4:4:4, HDR, un perfil) si en
  ese momento aparecía o se iba un toast, y también al abrir o cerrar el menú de la cuenta o el
  diagnóstico: la raíz de la interfaz cambiaba de forma e iced tiraba el desplazamiento. Ahora la
  forma es siempre la misma.
- El hilo de entrada pide por sí mismo temporizadores de 1 ms en Windows (y la exención de la
  regulación de Windows 11 sin ventana): las repeticiones de estado a 3, 8 y 20 ms dependían de
  que el transporte ya lo hubiera pedido y, sin eso, el planificador de 15,6 ms las retrasaba.
- **La caché del sondeo de códecs (`codecs.json`) funciona también en Windows**: la huella de la
  máquina recoge la versión del sistema, los adaptadores de DXGI con la versión de su driver, los
  drivers de Vulkan del registro y las DLL de NVENC/NVDEC, AMF, QSV y Direct3D, sin abrir la GPU;
  antes el sondeo se repetía en cada arranque, como en Linux.
- El aviso «Los invitados entran con la voz permitida» de Compartir va en tono neutro: el tono
  de información lleva el color de acento del sistema y con un acento rojo se leía como error.
- «Audio que se envía» muestra el nombre del ejecutable («reproductor») cuando PipeWire solo da el
  envoltorio genérico de pipewire-alsa («ALSA plug-in [reproductor.bin]»).
- **Cabecera pegada a la barra de título** del sistema (KDE): la cabecera (también la de la
  sala del invitado) lleva ahora el tono de las tarjetas, 56 px de alto con el título centrado y
  una línea fina por encima y por debajo, así no se funde con la barra gris de KDE.
- En la sala del anfitrión, **aire entre la barra de desplazamiento y el chat**, y el **botón de
  enviar** del chat mide lo mismo que la caja de texto con una línea.
- **El vídeo del invitado en ventana real se quedaba colgado**: el compositor tomaba su candado
  mientras iced presentaba la interfaz y, dentro de esa presentación, el primitivo de vídeo volvía
  a tomarlo desde el mismo hilo. Ahora el área del vídeo solo se anota y la interfaz se dibuja en
  una textura doble fuera del candado, así que el hilo de presentación nunca espera al render de
  la interfaz; un fotograma saltado por la cola cuenta como descarte, no como presentado.
- **Audio con una trama menos de espera**: el buffer de jitter arranca en 1 trama (10 ms) en vez
  de 2 y su margen es el p99 del jitter medido (en tiempo, no en tramas; dos colas aisladas no lo
  mueven, tres sí), con ocultación de la trama perdida con la cola vacía. Un parón del propio
  dispositivo de salida se detecta por la cadencia de sus peticiones y el remanente se recorta en
  esa misma petición (antes costaba unos 200 ms a +50 ms de latencia); un parón de la red se
  recorta de golpe tras 10 tramas de confirmación. Banco aislado 30-31 ms (antes 31-40; desfases 5
  y 9 ms: 26 y 30 ms); juego de captura a altavoz mediana 29-34 ms y p95 40 (antes 37-39 y 56),
  voz boca-oído 30 ms (antes 36), con pérdidas 48 ms (antes 51). El banco ya no es inestable:
  descarta las pasadas con parones del planificador en vez de medirlos.
- **PipeWire escribía 480 muestras fijas** por ciclo e ignoraba lo que pedía el grafo (que
  redondea el cuanto a potencia de 2: 480 → 256): el reloj de reproducción se desviaba hasta
  4,7 ms y bajar la latencia pedida no servía. Ahora escribe exactamente `requested`, `played_at`
  sale de `pw_time.delay`, la captura vacía todos los búferes del ciclo y el cuanto pedido es
  128/48000 (en el equipo del propietario el retardo del dispositivo baja de 16 a 13,3 ms; los
  10,7 ms restantes son el `headroom` de 512 muestras del USB, ajeno a coroom). Windows usa
  `IAudioClient3` en modo compartido de baja latencia con el periodo mínimo que informe el
  dispositivo, o el actual si el motor lo tiene fijado (conversión propia al formato de mezcla si
  hace falta).
- **La sonda de enlace medía Gbit/s**: marcaba los datagramas al procesarlos, no al llegar, y se
  leían de golpe tras el segundo de medición de RTT, así que el vídeo arrancaba al máximo del
  usuario. Ahora cada datagrama guarda su llegada, los trenes imposibles se rechazan y la sonda va
  en paralelo con el vídeo. El reloj remoto no se usa hasta tener 8 muestras de `Pong` (con una o
  dos, el control de tasa partía de un retardo falso y bajaba a 5 fps).
- **Primer fotograma 5-6 s antes con CGNAT y relay**: los candidatos locales se anuncian al
  instante y la dirección WAN después, cuando el router responde; el relay se abre en paralelo
  con la perforación y se cierra si gana la directa; las capacidades se envían al conectar.
- **Entrada**: el anfitrión publicaba estadísticas tras cada datagrama (clonar y ordenar 4×2048
  muestras hasta 1000 veces por segundo, dentro de la etapa de inyección): ahora cada 100 ms,
  31 µs. Una pérdida que se comía un «soltar tecla» la dejaba pulsada hasta el idle de 15 s: hay
  repetición de mantenimiento (100 ms con algo pulsado, 500 ms en reposo) y el anfitrión suelta
  todo tras 1 s sin paquetes. Mantener una tecla en un anfitrión Windows ahora repite con el
  retardo y la velocidad del sistema (`SendInput` no repite solo). Los eventos de un paquete se
  inyectan por lotes (una trama `SYN` por lote en uinput: los dos ejes de la palanca llegan en la
  misma muestra; un `SendInput` por paquete). El hilo de mandos espera en `next_event_blocking`
  en vez de dormir 1 ms sin compensar, sin asignar memoria por vuelta; el emisor tampoco asigna
  por datagrama.
- **Interfaz del invitado con ratón de 1 kHz**: cada evento de ventana reconstruía la interfaz y
  pedía redibujo, y la posición salía después con la marca de tiempo tardía. Ahora la marca se
  toma al entrar el evento, los eventos se agrupan en una actualización por redibujo y la
  posición se envía antes con una prueba de impacto barata. Sin redibujos a 60 fps mientras
  conecta o reconecta ni en «Abriendo la sala…» (que ahora sí avanza); los ajustes se guardan en
  un hilo aparte y los últimos cambios no se pierden al cerrar; el chat muestra las últimas 150
  entradas con «Ver anteriores» y claves estables; la paleta oscura se calcula una vez; el cursor
  remoto se mueve también entre fotogramas; el motivo del fin de sesión viaja como enum, no como
  texto.
- **Vídeo**: la salida de decodificación por CPU hacía tres copias y un relleno con ceros por
  fotograma (todo Windows y todos los decodificadores por software): un búfer agrupado y
  reciclado con los planos por desplazamiento; `wait_for_gpu` ya no bloquea el hilo de
  decodificación (sondea el semáforo del fotograma anterior); las importaciones DMA-BUF se
  cachean por identidad del búfer (PipeWire recicla 4-16); `encode_into` sin copias; la vista
  previa fuera del hilo de captura; copias de PipeWire y WGC desde un pool; las marcas de captura
  usan el `pts` del compositor y el QPC de WGC.
- **Captura de pantalla en KDE con NVIDIA sin fotogramas**: faltaba la ronda de fijación del
  modificador DMA-BUF (se pedían búferes con el modificador sin fijar). Ahora se fija el que
  propone el compositor (con respaldo a memoria compartida), cada paso se registra a nivel
  `info` y, si en 5 s no llega nada, la captura falla con un error que dice el estado del stream
  y el formato negociado. Si el diálogo del portal caduca, la sesión del portal se cierra.
- **`coroom doctor` se caía** con un cargador Vulkan viejo (SIGSEGV dentro de FFmpeg al abrir
  `*_vulkan`) o una libva anterior a 2.21 (abort de los trampolines): los códecs Vulkan se
  sondean en un proceso hijo y libva se comprueba con `dlopen` antes de usarla; en los dos casos
  se dice «no disponible» y por qué.
- **FEC a cero con el enlace limpio**: cada pérdida aislada costaba un RTT de NACK y frenaba los
  grupos siguientes; con el RTT del invitado (medido, no el del relay) mayor que 2 intervalos de
  fotograma, todo grupo de 4 paquetes o más lleva al menos un símbolo de reparación. `Feedback`
  viaja también por datagrama (y se descarta el duplicado o viejo; el relay reescribe la etiqueta
  del emisor con el id autenticado, así nadie puede firmar informes por otro), el cursor va en
  datagramas «gana el último» con copia por stream cada 100 ms, el relay abre su socket con
  búferes de 8 MB (sysctl en `deploy/README.md`) y su límite por sala solo recorta vídeo. El
  pacing reparte el fotograma en 0,3 del intervalo según la capacidad medida (antes 0,8 fijo) y
  las intras en 1 intervalo solo cuando lo medido deja el doble de holgura (si no, en 2). En una
  LAN con conexión directa ya no queda abierta una conexión ociosa con el relay.

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
  Video solo a mano: en las GPU cuyo driver codifica mal AV1 por Vulkan Video
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

- **Un codificador por hardware que no acaba la comprobación de imagen dice por qué**: antes solo
  se leía «la comprobación de imagen no terminó en 20 s». Ahora el diagnóstico y el registro dicen
  cuántos de los 300 fotogramas llegó a codificar, cuánto tardó cada uno (mediana y máximo) y
  cuántos no salieron a la primera.
- **Pruebas en dos clases** (decisión del propietario): las funcionales (que el vídeo, el audio y la
  entrada lleguen y estén enteros) hacen fallar la CI; los umbrales que dependen de la velocidad de
  la máquina (latencias, p95, fps o Mbit/s logrados, huecos) van con `timing_assert!` de la crate
  nueva `coroom-testkit` y solo avisan, en el paso «Avisos de tiempo» de la CI. Las esperas fijas
  que hacían de sincronización pasan a sondeos con plazo, y las pruebas largas se acortaron donde lo
  que prueban se sostiene.
- **La suite tarda menos de la mitad**: cargo-nextest corre los binarios en paralelo
  (`.config/nextest.toml`, con un grupo para las pruebas en tiempo real) y las sesiones de prueba
  son más cortas. En un equipo de 4 núcleos, de unos 8 minutos de ejecución a 100-130 s; la de
  capturas, de 51 a 10 s (solo escribe los PNG con `COROOM_SHOTS_DIR`, y el PNG se compila
  optimizado).

- **El codificador gasta lo que pide la imagen**, no el objetivo entero, en todos los backends: el
  objetivo del control de tasa es solo el techo de cada fotograma. NVENC va en VBR con suelo de
  QP; SVT-AV1, Vulkan Video (AV1 y H.264), AMF y QSV (H.264 y HEVC), en CBR con suelo de QP y sin
  relleno (`filler_data=0` en AMF, sin conformidad HRD en QSV); QSV AV1, que no admite suelo, en
  VBR; VA-API en QVBR y Vulkan Video HEVC en VBR, porque hay controladores que en CBR no hacen caso
  del suelo. Con 20 Mbit/s de techo a 1080p60, un escritorio quieto pasa de ~20 Mbit/s a
  ~1,4–2 Mbit/s con AV1 por hardware (NVENC y Vulkan Video) y a ~0,8 con VA-API, y una ráfaga de
  partículas sigue llegando al techo sin pasarlo. Si un controlador se pasa del techo (en un
  segundo o en más de un 10 % de sus fotogramas), se le baja el objetivo en proporción hasta que
  caben, y lo recupera en pasos de 1,25× cada 2 s donde el bitrate cambia en marcha (NVENC, QSV)
  o de una vez cuando gasta menos de la mitad del techo donde cambiarlo reabre el codificador y
  saca un intra (VA-API, Vulkan Video, AMF, SVT-AV1: FFmpeg 8.1 no admite otro control de tasa en
  marcha en ninguno de ellos); en VA-API y Vulkan HEVC el recorte va en el
  mismo fotograma que se pasa (y en QVBR también sube la calidad). Nunca vuelve a tasa
  constante. `cargo test --release -p coroom-codec --test rate_modes -- --ignored --nocapture`
  repite la comparación (`RATE_ONLY=VulkanVideo` para uno solo).
- **El control de tasa se asienta al conectar**: la sonda sale en escalera y, contra un
  limitador, fija el techo de ráfaga desde el primer segundo (aunque el vídeo ya esté saliendo);
  sin pérdida el techo no se mueve (sin diente de sierra), lo suben sondas cortas solo cuando
  frena, y una subida que pierde vuelve al techo de antes. La pérdida aleatoria baja (wifi) ya no
  hunde el bitrate, una pérdida que sube tras un recorte no cuenta como acierto, un intra con techo
  cabe en lo que espera el receptor, y con varios invitados el ritmo se reparte a todos. Lo vigila
  `s_a_policed_link_settles_within_five_seconds`.
- **El bitrate ya no tiene tope** en ninguna parte (ni en Compartir ni en `coroom host`, que pierde
  `--bitrate-kbps`): lo negocia el control de tasa de cada invitado con su enlace, y Diagnóstico
  dice que el límite es el enlace. Su mínimo baja a 0,5 Mbit/s para no inundar un enlace lento.
- El diálogo de cerrar la sala dice «¿Cerrar la sala?» (antes «¿Dejar de compartir?», que ahora es
  otra cosa).

- En «Personas en la sala» del invitado, el icono de altavoz delante del deslizador de cada
  persona es el botón de dejar de oírla o volver a oírla («Silenciar» / «Activar sonido»); el
  botón de auriculares desaparece. El del juego hace lo mismo que el de la barra y `Ctrl+Alt+V`.
- **El pacing reparte cada fotograma al ritmo que midió el enlace, no al bitrate máximo**: la
  sonda del enlace sale a diez veces el máximo configurado (hasta 100 Mbit/s) para medir lo que
  sobra por encima de él, y el emisor reparte cada fotograma al 70 % de lo medido (o 1,25 × el
  objetivo si es más, con techo de 100 Mbit/s), como mucho el 30 % del intervalo, igual que hasta
  ahora. Con 10 Mbit/s de máximo en LAN, el tramo «cifrado → enviado» baja de 5 ms a ~2,4 ms a
  60 fps y de 10 ms a ~4,8 ms a 30 fps; sin sonda ni estimación (o en un enlace que solo da el
  máximo) todo sigue igual. El indicador del enlace enseña ahora lo que midió la sonda, no el
  máximo.
- **Resolución máxima y fps con la sala abierta**: ya no esperan a la próxima sala. La captura
  adopta la cadencia y el tamaño nuevos, el codificador se reabre con un intra y los invitados
  siguen viendo sin cortes; 4:4:4 y HDR siguen aplicándose al abrir la sala, porque deciden cómo
  arranca la captura. En pantalla, la captura solo puede bajar fps en marcha; subirlos por encima
  de lo negociado espera a la próxima captura.
- **La resolución máxima es un tope real** también para la captura de pantalla y de ventana, en
  Linux y Windows: lo que la captura trae por encima se reduce con su proporción (en el mismo
  filtro de la GPU que la conversión de formato en la ruta sin copias, `scale_vulkan`,
  `scale_vaapi` y `scale_qsv`, y `scale_d3d11` para NVENC y AMF; con libswscale en un solo paso
  en la ruta de CPU, con el coste medido en el registro), y `HostFrameSize` y `CodecSelected`
  anuncian el tamaño que de verdad se envía. Antes el codificador se reabría al tamaño que
  entregaba la captura y una pantalla 4K iba a 4K aunque se pidiera 1080p.
- Un tope de resolución impar con la sala abierta (321×181) se redondea a par hacia abajo, como
  al abrir la sala, en vez de rechazarse.
- El control de tasa del vídeo sigue al enlace medido: VBV de un fotograma que acompaña al
  bitrate (SVT-AV1 exige al menos 20 ms), tope en 1,25 × lo recibido, un segundo de espera tras
  cada recorte, vía rápida con dos informes seguidos de 50 ms o más de cola, la FEC descontada
  del presupuesto y contrapresión entre el codificador y el envío (sin intras por desalojo).
- El anfitrión Windows usa un solo dispositivo D3D11 para toda la captura, y el sondeo de MTU
  tiene techo en la MTU del adaptador menos 48 (entre 1252 y 1452), en vez de probar a ciegas.
- Los atajos del invitado: `Ctrl+Alt+S` abre las estadísticas (antes `Ctrl+Alt+O`) y
  `Ctrl+Alt+C` el chat; las letras se cambian en Ajustes → Mandos.
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

- `COROOM_RELAY_ADDR` (y `--relay-addr`): el servidor ya no anuncia una dirección para el relay.
  Puede quedar en el `.env`; no se usa.
- El modo **«Juego»** de Compartir (una ventana con el audio solo de su aplicación): confundía
  con «Ventana», y el audio ya se elige aplicación por aplicación en «Audio que se envía». Quedan
  Pantalla y Ventana.
- El bitrate máximo «Sin límite» y los valores fijos: el único modo es «Auto», con o sin
  tope.
- La pantalla «Vídeo» de Ajustes: el códec, el bitrate y la calidad se eligen en Compartir y en
  el panel «Transmisión» de la sala.
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
