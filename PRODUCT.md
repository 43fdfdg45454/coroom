# Product

<!-- impeccable:product-schema 1 -->

> Registro de producto para el trabajo de diseño. Se escribió sin entrevista: el propietario pidió
> el rediseño sin poder contestar preguntas, así que todo sale del repositorio (`README.md`,
> `CLAUDE.md`, `docs/guia.md`, `docs/ARQUITECTURA.md`, el código de las interfaces). Lo marcado
> **(inferido)** no lo ha confirmado nadie y conviene revisarlo.

## Platform

adaptive

Dos aplicaciones nativas con un mismo producto: la de escritorio (Rust + iced, Linux con Wayland y
Windows) y el invitado Android (Kotlin + Jetpack Compose, Material 3). Cada una sigue las
convenciones de su sistema; no hay interfaz web salvo el `/admin` del servidor.

## Users

- **Quien aloja**: tiene el juego en su equipo (Linux o Windows) y quiere que sus amigos lo
  jueguen con él. Abre una sala, decide qué comparte y qué puede hacer cada invitado.
- **Quien se une**: un amigo con cuenta en el mismo servidor, desde su escritorio o desde un móvil
  o tableta Android, con teclado y ratón, un mando o la pantalla táctil.
- **Quien administra el servidor**: suele ser uno del grupo; monta `coroom-server` en una LAN, una
  VPN o fuera y reparte códigos de invitación.

Situación de uso **(inferido)**: grupos pequeños de amigos que quedan para jugar, a menudo de
noche y con el mando en la mano; durante la partida la interfaz tiene que estorbar lo mínimo y
decir de un vistazo si la conexión va bien.

## Product Purpose

Juego remoto con amigos al estilo de Parsec: un anfitrión comparte su juego y los invitados lo
juegan desde su equipo con la menor latencia posible, con voz y chat en la sala. Funciona bien
cuando abrir una sala o entrar en una cuesta un clic, cuando la latencia y el estado de la
conexión están siempre a la vista y cuando nadie fuera de la sala, ni el servidor, puede ver ni
oír nada.

## Positioning

Servidor autoalojado en un binario, sin depender de internet pública ni de cuentas de terceros, y
cifrado de extremo a extremo de todo el contenido (MLS, SFrame, HPKE con X-Wing) con un número de
seguridad que las dos personas pueden comparar. Cliente nativo de Linux (Wayland) de primera, no
un port.

## Operating Context

- Primer arranque: dirección del servidor, huella del certificado si es autofirmado, entrar o
  crear cuenta con un código de invitación, nombre del equipo.
- **Inicio**: compartir la pantalla, unirse con un código de 6 caracteres o aceptar una
  invitación, amigos conectados.
- **Alojar**: elegir pantalla, ventana, solo voz o fuente de prueba; audio por aplicación; voz;
  abrir la sala. Con la sala abierta: código, vista previa, transmisión, integrantes con su
  conexión (directa o relay), RTT, latencia de entrada, permisos (teclado y ratón, ranura de
  mando 1 a 8, voz), número de seguridad, pasar la sala, expulsar, pausar a todos.
- **Invitado**: pasos de conexión con la calidad esperada, vídeo a ventana o pantalla completa con
  una barra que se oculta, píldora de latencia, quién habla, panel de sala, chat, estadísticas
  por etapa, mandos.
- **Amigos**, **Ajustes** (vídeo, audio, entrada, red, dispositivos, apariencia, acerca de,
  diagnóstico) y actualizaciones desde las publicaciones de GitHub.
- Android es solo invitado: servidor, entrar, inicio, amigos, ajustes y la sala (siempre en
  oscuro, con el vídeo a pantalla completa y controles superpuestos).

## Capabilities and Constraints

- Vídeo hasta 4K y 120 fps, 4:4:4 y HDR10 cuando todos lo admiten; AV1, HEVC o H.264.
- Hasta 8 mandos por sala, cada uno tal cual o convertido a Xbox 360.
- Conexión QUIC directa o por el relay del servidor; el estado de la ruta es un dato que se enseña.
- Tiene que funcionar igual en una LAN, una VPN o expuesto fuera, sin salida a internet.
- Textos visibles en español, en catálogos Fluent (`apps/desktop/locales/`) y recursos de Android;
  nombres de código en inglés.
- Todos los colores del escritorio salen de `crates/ui/src/theme.rs` y todo texto cumple
  contraste AA con cualquier acento del sistema (lo comprueban los tests de `crates/ui`).
- Regla de privacidad del repositorio: datos de ejemplo siempre ficticios («Ana», «Bea»,
  `coroom.example.com`).

## Brand Commitments

Del `CLAUDE.md` del proyecto, vinculantes:

- Las interfaces se sienten **de Linux**: convenciones de GNOME y KDE, tema claro y oscuro del
  sistema, color de acento del sistema, tipografía y espaciado del escritorio, atajos habituales,
  Wayland nativo, HiDPI, diálogos y notificaciones por los portales. En Windows, la misma
  interfaz con el tema del sistema.
- Buena UX: lo importante a un clic, estados vacíos y errores explicados en español, nada que
  bloquee sin decir por qué, indicadores de latencia y conexión siempre visibles.
- UI bonita: jerarquía visual clara, animaciones breves, nada de widgets por defecto sin estilo.
- Nombre «coroom», en minúsculas. Icono propio en `packaging/icons/coroom.svg`. Tipografía Inter
  embebida en el escritorio e iconos Lucide.
- En Android, Material 3 con el color dinámico del sistema (Android 12+) y un esquema propio de
  respaldo; la sala va siempre en oscuro.

## Evidence on Hand

- Capturas con datos ficticios de cada pantalla del escritorio: `cargo test -p coroom --test shots`
  (`apps/desktop/src/ui/shots.rs`); cuatro publicadas en `docs/screenshots/`.
- Guía de uso (`docs/guia.md`) con los flujos reales y sus textos.
- No hay testimonios, cifras de uso ni comparativas publicadas: no inventarlas.

## Product Principles

1. La partida manda: durante la sala, la interfaz se aparta y solo interrumpe por algo que el
   usuario tiene que saber.
2. El estado de la conexión nunca se esconde: latencia, ruta (directa o relay) y calidad siempre
   visibles y dichas en palabras.
3. Quién puede hacer qué se ve y se cambia en un sitio: permisos por persona, claros y reversibles.
4. De su sistema: cada plataforma con sus convenciones, su tema y su acento.
5. La privacidad se puede comprobar, no solo prometer: número de seguridad, huella del
   certificado, ruta de la conexión.

## Accessibility & Inclusion

Contraste AA en todo el texto con cualquier acento, navegación completa con el teclado, respeto
de la preferencia de movimiento reducido del sistema y, en Android, objetivos táctiles de 48 dp y
tamaños en `sp`.
