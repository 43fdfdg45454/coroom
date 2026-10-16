# Textos de la pantalla «diagnostics» (identificadores con el prefijo `diagnostics-`; los comunes están en coroom.ftl).

## Cabecera

diagnostics-titulo = Diagnóstico
diagnostics-sala-de-host = Sala de { $host } · sigue en curso
diagnostics-sala-de-host-reconectando = Sala de { $host } · reconectando
diagnostics-sala-del-anfitrion = Sala del anfitrión · sigue en curso
diagnostics-tu-sala = Tu sala · sigue en curso
diagnostics-sin-sala = Sin sala
diagnostics-copiar-informe = Copiar informe
diagnostics-informe-copiado = Informe copiado
diagnostics-insignia-directa = Directa · { $rtt }
diagnostics-insignia-relay = Relay · { $rtt }
diagnostics-insignia-directa-sin-rtt = Directa
diagnostics-insignia-relay-sin-rtt = Relay
diagnostics-insignia-conectando = Conectando
diagnostics-insignia-un-invitado = 1 invitado
diagnostics-insignia-invitados = { $count } invitados

## Banner de la ruta

diagnostics-relay-titulo = Vas por el relay de { $host }.
diagnostics-relay-motivo = La conexión directa falló: { $reason }.
diagnostics-relay-sin-probar = No se probó la conexión directa: el relay está forzado en los ajustes o el anfitrión no anunció direcciones.
diagnostics-relay-router = Tu router tampoco abrió un puerto (UPnP, NAT-PMP y PCP).
diagnostics-relay-reintento = La sesión no la reintenta sola: «Reintentar ahora» vuelve a entrar en la sala (el vídeo se corta un momento).
diagnostics-reintentar-ahora = Reintentar ahora
diagnostics-directa-titulo = Directa · { $rtt }
diagnostics-directa-titulo-sin-rtt = Conexión directa
diagnostics-directa-texto = Vas en directo con el anfitrión: el relay no hace falta.
diagnostics-conectando-titulo = Conectando con el anfitrión…
diagnostics-conectando-texto = Se prueban las direcciones directas y, si ninguna responde, el relay.
diagnostics-reconectando-titulo = Reconectando (intento { $attempt } de { $of })
diagnostics-sin-relay-titulo = Esta sala no tiene relay
diagnostics-sin-relay-texto = Solo vale la conexión directa: { $reason }.
diagnostics-anfitrion-relay-titulo = { $relayed } de { $total } van por el relay de { $host }.
diagnostics-anfitrion-relay-texto = Como anfitrión no puedes reintentar la directa: cada invitado puede hacerlo desde su diagnóstico.
diagnostics-anfitrion-directa-titulo = Todos van en directo
diagnostics-anfitrion-directa-texto = Nadie necesita el relay del servidor.
diagnostics-anfitrion-vacio-titulo = Nadie ha entrado todavía
diagnostics-anfitrion-vacio-texto = Cuando entre alguien verás aquí su ruta, las direcciones probadas y la sonda del enlace.
diagnostics-lobby-titulo = Sin sala
diagnostics-lobby-texto = Abre o entra en una sala para ver la ruta, las direcciones probadas y la latencia por etapa. Aquí tienes el servidor y este equipo.

## Candidatos

diagnostics-candidatos-titulo = Candidatos de conexión directa
diagnostics-candidatos-detalle = hace { $secs } s · { $timeout } s por candidato
diagnostics-candidatos-detalle-sin-intento = { $timeout } s por candidato
diagnostics-candidatos-vacio = Aún no se ha probado ninguna dirección.
diagnostics-clase-ipv6 = IPv6 global
diagnostics-clase-ipv4 = IPv4 pública (reflejada por el servidor o WAN del router)
diagnostics-clase-cgnat = IPv4 de la operadora (CGNAT)
diagnostics-clase-privada = Dirección privada (misma red)
diagnostics-router = WAN de tu router (UPnP · NAT-PMP · PCP)
diagnostics-relay-de-host = Relay ({ $host })
diagnostics-resultado-en-uso = En uso
diagnostics-resultado-respondio = Respondió
diagnostics-resultado-fallo = Falló
diagnostics-resultado-sin-respuesta = Sin respuesta
diagnostics-resultado-no-probado = No probado
diagnostics-resultado-abierto = Abierto
diagnostics-resultado-no-hace-falta = No hace falta
diagnostics-resultado-esperando = Esperando
diagnostics-resultado-no-disponible = No disponible
diagnostics-por-aqui = La conexión va por esta dirección.
diagnostics-duplicado = Respondió, pero se eligió otra dirección.
diagnostics-fallo-err = El intento falló: { $err }.
diagnostics-cancelado = La apertura terminó antes de que le tocara.
diagnostics-sin-respuesta-ipv6 = Nada en { $timeout } s: alguna de las dos redes no tiene IPv6 o un cortafuegos lo bloquea.
diagnostics-sin-respuesta-ipv4 = Nada en { $timeout } s: la NAT de uno de los dos no deja pasar la apertura simultánea.
diagnostics-sin-respuesta-cgnat = Nada en { $timeout } s: la operadora (CGNAT) no pasa tráfico entre sus clientes.
diagnostics-sin-respuesta-privada = Nada en { $timeout } s: solo sirve si los dos estáis en la misma red.
diagnostics-router-abierto = El router abrió el puerto UDP por { $protocol }.
diagnostics-router-solo-externa = { $protocol } respondió, pero el router no abrió el puerto: { $error }.
diagnostics-router-sin-respuesta = El router no respondió a UPnP, NAT-PMP ni PCP. Actívalo en el router para probar la directa.
diagnostics-router-rechazo = El router no abrió el puerto: { $error }.
diagnostics-router-sin-puerta = Sin puerta de enlace IPv4: { $error }.
diagnostics-relay-en-uso-rtt = Conectado · RTT { $rtt } · ruta en uso
diagnostics-relay-en-uso = Conectado · ruta en uso
diagnostics-relay-respaldo = La directa funciona: el relay queda de respaldo.
diagnostics-relay-espera = Se usa si ninguna dirección directa responde.

## Prueba de enlace

diagnostics-enlace-titulo = Prueba de enlace
diagnostics-enlace-detalle = por la ruta en uso
diagnostics-rtt-anfitrion = RTT al anfitrión
diagnostics-rtt-invitado = RTT con el invitado
diagnostics-rtt-tramo-relay = tramo entre tú y el relay
diagnostics-perdida = Pérdida en la red
diagnostics-perdida-detalle = desde que entraste, antes de FEC y NACK
diagnostics-mtu = MTU útil
diagnostics-mtu-detalle = la sesión aún no lo expone
diagnostics-sonda = Sonda del enlace
diagnostics-sonda-hacia-ti = anfitrión → tú, antes del vídeo
diagnostics-sonda-hacia-invitado = tú → invitado, antes del vídeo
diagnostics-sonda-perdidos = { $lost } de { $sent } datagramas perdidos
diagnostics-bitrate = Bitrate objetivo / real
diagnostics-bitrate-objetivo = Bitrate objetivo
diagnostics-bitrate-lo-limita-el-enlace = Sin tope: lo limita solo el enlace (sube mientras llegue todo y baja si se pierde)
diagnostics-bitrate-valor = { $target } / { $real } Mbit/s
diagnostics-entrada = Entrada (ratón → juego)
diagnostics-entrada-detalle = p50 / p95, captura → inyectado

## Latencia por etapa

diagnostics-etapas-titulo = Latencia por etapa
diagnostics-etapas-detalle = { $percentile } de los últimos { $frames } fotogramas · medida con marcas de tiempo
diagnostics-etapas-vacio = Las etapas aparecen cuando llega el vídeo.
diagnostics-etapa-codificacion = captura y codificación
diagnostics-etapa-envio = cifrado y envío
diagnostics-etapa-red = red
diagnostics-etapa-decodificacion = decodificación
diagnostics-etapa-presentacion = presentación
diagnostics-video-total = Vídeo total
diagnostics-anadido = Añadido de extremo a extremo
diagnostics-etapas-resumen = Vídeo total { $video } · entrada (ratón → juego) { $input } · añadido de extremo a extremo ≈ { $added }
diagnostics-etapas-anfitrion = Las etapas del vídeo se miden en el equipo de cada invitado: pídele que copie su informe desde su diagnóstico.

## Servidor y este equipo

diagnostics-servidor = Servidor
diagnostics-servidor-estado = Estado
diagnostics-servidor-responde = Responde · { $rtt }
diagnostics-servidor-responde-sin-rtt = Responde
diagnostics-servidor-no-responde = No responde
diagnostics-servidor-sin-comprobar = Sin comprobar
diagnostics-servidor-relay = Relay
diagnostics-servidor-relay-si = Disponible
diagnostics-servidor-relay-no = Desactivado en el servidor
diagnostics-equipo = Este equipo
diagnostics-equipo-detalle = lo mismo que va en el informe, sin rutas ni nombres
diagnostics-equipo-analizando = Analizando este equipo (captura, adaptador y códecs)…
diagnostics-equipo-sin-datos = Sin datos de este equipo todavía.
diagnostics-tu-router = Tu router

## Informe (texto plano, sin direcciones ni nombres)

diagnostics-informe-titulo = Informe de diagnóstico de coroom { $version }
diagnostics-informe-papel = Papel
diagnostics-informe-ruta = Ruta
diagnostics-informe-ruta-directa = directa
diagnostics-informe-ruta-relay = relay del servidor
diagnostics-informe-ruta-conectando = conectando
diagnostics-informe-motivo = Motivo del relay
diagnostics-informe-nadie = Nadie en la sala.
diagnostics-informe-etapas = Latencia por etapa (p50 / p95, últimos { $frames } fotogramas)

## Sustitutos de lo privado

diagnostics-anon-anfitrion = anfitrión
diagnostics-anon-invitado = invitado
diagnostics-anon-invitado-n = invitado { $n }
diagnostics-anon-participante = participante
diagnostics-anon-amigo = amigo
diagnostics-anon-equipo = este equipo
diagnostics-anon-servidor = servidor
diagnostics-anon-ruta = [ruta]
diagnostics-anon-ipv6 = [IPv6 global]
diagnostics-anon-ipv4 = [IPv4 pública]
diagnostics-anon-cgnat = [IPv4 de la operadora]
diagnostics-anon-privada = [red local]
