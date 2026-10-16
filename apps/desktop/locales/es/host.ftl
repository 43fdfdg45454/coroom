# Textos de la pantalla «host» (identificadores con el prefijo `host-`; los comunes están en coroom.ftl).

## Qué compartir

host-que-compartir = Qué compartir
host-modo-pantalla = Pantalla
host-modo-ventana = Ventana
host-pantalla-ayuda = Un monitor entero, con el audio de todas las aplicaciones que dejes marcadas.
host-ventana-ayuda = Una ventana, con el audio de todas las aplicaciones que dejes marcadas.
host-ventana-en-negro = Si la ventana se ve en negro (pantalla completa exclusiva), se transmite su monitor.
host-se-puede-cambiar-dentro = Se puede cambiar dentro de la sala sin cerrarla.
host-vista-previa-al-abrir = La vista previa aparece al abrir la sala
host-usar-la-fuente-de-prueba = Usar la fuente de prueba
host-fuente-de-prueba-fila = Un patrón animado en vez de lo que elijas arriba: sirve para probar la conexión sin enseñar nada tuyo.
host-la-fuente-de-prueba = La fuente de prueba
host-fuente-de-prueba-ayuda = Se transmitirá la fuente de prueba: un patrón animado, sin enseñar nada tuyo.
host-volver-a-la-pantalla = Volver a la pantalla
host-buscando = Buscando…
host-monitor = Monitor
host-monitor-de-size = Monitor · { $size }
host-ventana-de-size = Ventana de { $size }
host-elegir = Elegir…
host-cambiar-puntos = Cambiar…
host-elegir-otra = Elegir otra…
host-elige-en-el-selector-del-sistema = Elige en el selector del sistema…
host-aun-no-has-elegido-monitor = Todavía no has elegido monitor
host-aun-no-has-elegido-ventana = Todavía no has elegido ventana
host-si-no-eliges-ahora = Si no eliges ahora, el sistema te lo pregunta al abrir la sala.
host-no-se-pudo-abrir-el-selector = No se pudo abrir el selector del sistema: { $error }
host-no-se-pudieron-listar-los-monitores = No se pudieron listar los monitores: { $error }
host-no-se-pudieron-listar-las-ventanas = No se pudieron listar las ventanas: { $error }
host-no-se-pudieron-listar-los-monitores-titulo = No se pudieron listar los monitores
host-no-se-pudieron-listar-las-ventanas-titulo = No se pudieron listar las ventanas

## A quién invitar

host-a-quien-invitar = A quién invitar
host-en-vivo-hace-segundos = en vivo · hace { $n } s
host-en-vivo-hace-minutos = en vivo · hace { $n } min
host-cargando-tus-amigos = Cargando tus amigos…
host-sin-amigos = Todavía no tienes amigos en coroom: añádelos desde Inicio. El código de la sala sirve igual.
host-reintentar = Reintentar
host-invitacion-enviada = Invitación enviada a { $names }
host-y-last = { $valor } y { $last }
host-invitar = Invitar
host-invitar-a-mas-amigos = Invitar a más amigos

## Calidad

host-calidad = Calidad
host-calidad-ayuda = Se puede cambiar dentro de la sala
host-perfil-min-latencia = Mínima latencia
host-perfil-min-latencia-corto = Mín. latencia
host-perfil-min-latencia-ayuda = Para shooters, lucha y ritmo. Prioriza la respuesta sobre la nitidez.
host-perfil-equilibrado = Equilibrado
host-perfil-equilibrado-ayuda = Lo recomendado para casi todo. Buena imagen sin perder respuesta.
host-perfil-max-calidad = Máxima calidad
host-perfil-max-calidad-corto = Máx. calidad
host-perfil-max-calidad-ayuda = Para estrategia, lectura y ver jugar. Puede añadir unos ms.
host-perfil-personalizado = Personalizado
host-perfil-personalizado-ayuda = Lo que elegiste en Avanzado.
host-nativa = Nativa
host-nativa-hasta-4k = Nativa (hasta 4K)
host-fps = { $fps } fps
host-auto = Auto
host-en-vivo = En vivo
host-al-abrir-la-sala = Al abrir la sala
host-cuando-grupo = { $when }: { $valor }
host-opcion-resolucion = resolución
host-opcion-fps = fps
host-opcion-codec = códec
host-guardado-para-la-proxima-sala = Guardado para la próxima sala
host-video-de-la-sala-cambiado = Vídeo de la sala cambiado: { $valor }.
host-no-se-pudo-cambiar-el-video = No se pudo cambiar el vídeo de la sala: { $error }

## Avanzado

host-avanzado = Avanzado
host-avanzado-plegado = Resolución, fps, códec, croma 4:4:4 y HDR (el bitrate es automático)
host-avanzado-desplegado = Cambiar algo aquí convierte el modo en «Personalizado»
host-avanzado-sin-cambios = { $perfil }, sin cambios
host-resolucion-maxima = Resolución máxima
host-resolucion-ayuda = La de la fuente o menos
host-fotogramas-por-segundo = Fotogramas por segundo
host-codec = Códec
host-codec-ayuda = Automático elige el mejor que decodifiquen todos los invitados.
host-codec-en-uso = En uso: { $codec }. Cambiarlo corta el vídeo un momento.
host-automatico = Automático
host-codificador = Codificador
host-codificador-ayuda = Automático prueba primero el del fabricante de tu tarjeta gráfica. Si eliges uno que este equipo no tiene, la sala no se abre y te lo dice.
host-por-software = Por software
host-bitrate = Bitrate
host-bitrate-ayuda = Se ajusta solo para cada invitado: sube hasta lo que da su conexión y baja antes de que el vídeo se corte.
host-bitrate-sin-sala = Con la sala abierta, aquí se ve lo que pide ahora el invitado más lento.
host-bitrate-ahora = Ahora: { $valor }.
host-croma = Croma 4:4:4
host-croma-lead = Texto nítido en escritorios y juegos de estrategia.
host-croma-comprobando = { $lead } Comprobando qué códecs lo admiten en este equipo…
host-croma-disponible-con = { $lead } Disponible con { $valor }.
host-croma-no-con-este-codec = { $lead } No disponible con este códec ({ $codec }); sí con { $valor }.
host-croma-no-disponible = { $lead } No disponible para compartir: ningún codificador de este equipo lo admite (hace falta NVENC con H.264 o HEVC, o Vulkan Video).{ $decode }
host-croma-como-invitado = {" "}Como invitado sí lo recibes en { $valor }.
host-hdr-comprobando = HDR10 (10 bits, BT.2020 PQ). Comprobando este equipo…
host-hdr-disponible-con = Compartes en HDR10 con { $valor } cuando la pantalla captura en HDR.
host-hdr-no-con-este-codec = No disponible con este códec ({ $codec }) para compartir; sí con { $valor }.
host-hdr-no-disponible = No disponible para compartir: ningún codificador de este equipo hace 10 bits.
host-hdr-pantalla-si = Esta pantalla muestra HDR: el vídeo HDR se ve en HDR.
host-hdr-pantalla-no = Esta pantalla no expone HDR a las aplicaciones: el vídeo HDR se ve con tone mapping a SDR.
host-hdr-pantalla-quiza = Si la pantalla no expone HDR, el vídeo HDR se ve con tone mapping a SDR.

## Audio y voz

host-audio-que-se-envia = Audio que se envía
host-este-equipo-no-separa-el-audio = Todo lo que suena: este equipo no separa el audio por aplicación ({ $why }).
host-ninguna-aplicacion-suena-ahora = Ninguna aplicación suena ahora.
host-las-aplicaciones-que-empiecen-a-sonar = Las aplicaciones que empiecen a sonar después entran solas, salvo las que hayas quitado.
host-sonando = sonando
host-quitada-siempre = quitada siempre · recordada
host-coroom-nunca-se-envia-a-si = coroom nunca se envía a sí misma: la voz de la sala no vuelve como eco.
host-hablar-por-la-sala-con-el = Hablar por la sala con el micrófono
host-los-invitados-te-oyen-y-hablan = Los invitados te oyen siempre y hablan desde que entran, salvo que entren callados. Es el ajuste de Ajustes → Audio y voz.
host-los-invitados-entran-callados = Los invitados entran callados
host-entran-callados-ayuda = Entran sin permiso de voz y se lo das a cada uno desde su fila. Es el ajuste de Ajustes → Sala.

## Abrir la sala

host-el-codigo-aparece = El código de la sala aparece al abrirla.
host-invitaras-a-uno = Invitarás a 1 amigo. El código de la sala aparece al abrirla.
host-invitaras-a-n = Invitarás a { $n } amigos. El código de la sala aparece al abrirla.
host-abrir-la-sala = Abrir la sala
host-abrir-la-sala-e-invitar = Abrir la sala e invitar
host-abriendo-la-sala = Abriendo la sala… (si el sistema lo pide, elige qué compartir en su diálogo)
host-comprobando-los-codecs = Comprobando qué códecs de vídeo funcionan en este equipo… (solo la primera vez y al cambiar los drivers; unos segundos)
host-no-se-pudo-abrir-la-sala = No se pudo abrir la sala: { $error }

## Sala abierta

host-sala-de = Sala de { $name }
host-tu-sala = Tu sala
host-codigo = Código { $code }
host-creando-la-sala = Creando la sala en el servidor…
host-sin-invitados = sin invitados
host-un-invitado = 1 invitado
host-n-invitados = { $n } invitados
host-ruta-directa = directa
host-ruta-relay = por relay
host-ruta-mixta = directa y relay
host-transmitiendo = Transmitiendo
host-esperando-invitados = Esperando invitados
host-eligiendo-codec = Eligiendo el códec
host-copiar-codigo = Copiar el código
host-cerrar-la-sala = Cerrar la sala
host-la-entrada-de-todos-los-invitados = La entrada de todos los invitados está en pausa: se ha soltado todo lo pulsado.
host-reanudar = Reanudar
host-sin-relay-relay-solo-podran-entrar = Sin relay ({ $relay }): solo podrán entrar los invitados con conexión directa.
host-sin-vista-previa = Sin vista previa: la captura va directa a la GPU, sin copias
host-codifica = codifica
host-codec-por = { $codec } por { $encoder }
host-esperando-al-primer-invitado = Esperando al primer invitado para elegir el códec
host-eligiendo-el-codec-con-tus-invitados = Eligiendo el códec con tus invitados…
host-cambiar-fuente = Cambiar fuente
host-al-cambiar-la-ultima-imagen = Al cambiar, los invitados ven la última imagen un momento; no salen de la sala.
host-compartir-esto = Compartir esto
host-cambiando-a = Cambiando a { $que }…
host-ahora-compartes-otra-cosa = Listo: los invitados ya ven lo nuevo.
host-no-se-pudo-cambiar-la-fuente = No se pudo cambiar lo que compartes (sigue lo de antes): { $error }
host-voz-entran-hablando = Los invitados entran con la voz permitida. Puedes cambiarlo en Ajustes → Sala («Los invitados entran callados»).
host-voz-entran-callados = Los invitados entran callados: dales la voz con el micrófono de su fila. Puedes cambiarlo en Ajustes → Sala.

## Invitados

host-integrantes = Integrantes ({ $valor })
host-anfitrion = Anfitrión
host-hablando = Hablando
host-tu = Tú
host-tu-nombre = { $name } (tú)
host-sin-microfono-en-la-sala = No compartes el micrófono en esta sala (Ajustes → Audio y voz, al abrir la próxima).
host-te-has-silenciado = Te has silenciado: nadie te oye.
host-tu-voz-ayuda = El deslizador es lo alto que te oyen los demás.
host-activar-tu-microfono = Activar tu micrófono
host-silenciarte = Silenciarte para toda la sala
host-mas = Más
host-modo-solo-voz = Solo voz
host-solo-voz-ayuda = La sala se abre sin compartir nada: voz y chat. Desde la sala puedes compartir la pantalla o una ventana cuando quieras.
host-solo-voz-detalle = Nadie verá tu pantalla: los invitados verán «Nadie está compartiendo».
host-no-compartes-nada = No compartes nada
host-no-compartes-nada-detalle = La sala es de voz y chat. Tus invitados ven «Nadie está compartiendo».
host-compartir-pantalla-o-ventana = Compartir pantalla o ventana
host-solo-voz-y-chat = Solo voz y chat
host-dejar-de-compartir = Dejar de compartir
host-pasarle-la-sala = Pasarle la sala
host-esperando-a-que-acepte = Esperando a que { $name } acepte la sala…
host-pasando-la-sala-a = Pasando la sala a { $name }: todos entráis en la suya.
host-name-no-acepto-la-sala = { $name } no aceptó la sala.
host-no-se-pudo-pasar-la-sala = No se pudo pasar la sala: { $error }
host-solo-tu-voz = Solo tu voz: sin nada compartido no hay audio del juego.
host-ya-no-compartes-nada = Ya no compartes nada: la sala sigue con voz y chat.
host-tu-microfono = Tu micrófono
host-donde-oyes-la-sala = Dónde oyes la sala
host-cambia-al-momento = Cambia al momento, sin cortar a nadie.
host-pausar-la-entrada = Pausar la entrada
host-reanudar-la-entrada = Reanudar la entrada
host-todavia-no-ha-entrado-nadie = Todavía no ha entrado nadie
host-name-pide-entrar = { $name } pide entrar
host-name-pide-entrar-en-tu-sala = { $name } pide entrar en tu sala
host-pidio-hace = hace { $ago } s · caduca en { $left } s
host-dejar-entrar = Dejar entrar
host-ahora-no = Ahora no
host-comparte-el-codigo-de-la-sala = Comparte el código de la sala o invita a un amigo conectado. Los invitados entran solo mirando: tú les das teclado, mando o voz.
host-directa = Directa
host-relay = Relay
host-cgnat = CGNAT
host-conectando = Conectando…
host-rtt = { $ms } ms
host-calidad-excelente = Excelente
host-calidad-buena = Buena
host-calidad-aceptable = Aceptable
host-calidad-mala = Mala
host-dar-teclado-y-raton = Dar teclado y ratón
host-quitar-teclado-y-raton = Quitar teclado y ratón
host-dar-un-mando = Dar un mando
host-quitar-el-mando = Quitar el mando
host-dar-la-voz = Dar la voz
host-quitar-la-voz = Quitar la voz
host-dejar-de-oir = Dejar de oír a { $name } (solo en este equipo)
host-volver-a-oir = Volver a oír a { $name }
host-volumen-porcentaje = { $percent } %
host-numero-de-seguridad = Número de seguridad
host-expulsar = Expulsar
host-esperando-la-clave-de-entrada-pareja = Esperando la clave de entrada pareja a pareja: su entrada se aplica en cuanto llegue.
host-mando = Mando
host-sin-mando = Sin mando
host-ranura-slot = Ranura { $slot }
host-sin-ranuras-libres = No quedan ranuras de mando libres
host-la-slot-ya-es-de-otro = La { $slot } ya es de otro invitado
host-tal-cual = Tal cual
host-convertido-a-xbox-360 = Convertido a Xbox 360
host-su-mando-name = Su mando: { $name }
host-solo-convertido = solo convertido
host-todavia-no-ha-conectado-ningun-mando = Todavía no ha conectado ningún mando.
host-va-convertido-note = Va convertido: { $note }.
host-name-ha-entrado-en-la-sala = { $name } ha entrado en la sala ({ $valor })
host-name-ha-salido-de-la-sala = { $name } ha salido de la sala
host-name-ha-sido-expulsado-las-claves = { $name } ha sido expulsado; las claves de la sala se han rotado
host-el-invitado = El invitado { $valor }
host-la-sala-se-cerro-reason = La sala se cerró: { $reason }
host-has-dejado-de-compartir = Has cerrado la sala

