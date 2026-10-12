# Guía de coroom

coroom sirve para jugar con amigos a un juego que solo está en el equipo de uno de vosotros: el
**anfitrión** comparte su pantalla y su audio, y los **invitados** lo ven y lo oyen en el suyo y
manejan el juego con su teclado, su ratón o su mando, con la menor latencia posible. Todo va cifrado
de extremo a extremo: el servidor que os pone en contacto no puede ver ni oír nada.

Esta guía es para quien usa la aplicación. Para montar un servidor, mira la
[guía del operador](../deploy/README.md); para compilar coroom, el [`README.md`](../README.md).

## Lo primero: el servidor y tu cuenta

1. Abre coroom. La primera vez pide la **dirección del servidor** (por ejemplo,
   `coroom.example.com`): te la da quien lo administra. coroom comprueba al momento que responde,
   su versión y si el relay está activo. Si el servidor usa un certificado propio (sin dominio
   público), te enseña su huella para que la compares con la que te dio el administrador y pulses
   **Confiar**.
2. **Entrar**: con tu usuario y contraseña. Si tienes un **código de invitación**, elige **Crear
   cuenta**: escribes el código, tu nombre de usuario, tu nombre visible y tu contraseña (al menos
   12 caracteres).
3. Ponle **nombre a este equipo** («Portátil», «Sobremesa»): tu cuenta puede tener varios equipos y
   cada uno tiene su propia identidad criptográfica, que se guarda en el llavero del sistema.

Tu sesión dura 30 días en cada equipo. En Ajustes → Dispositivos ves tus equipos y puedes cerrar la
sesión de uno que ya no uses.

## Alojar: compartir tu juego

1. En **Inicio**, pulsa **Compartir mi pantalla**.
2. Elige **qué compartir**: tu pantalla o una ventana (en Linux lo eliges en el diálogo del sistema
   al abrir la sala; en Windows se comparte el monitor principal), o la **fuente de prueba** para
   comprobar la conexión sin enseñar nada tuyo.
3. Elige el **audio**: todo lo que suena, solo una aplicación (Linux: su nombre en PipeWire; mira
   Ajustes → Diagnóstico) o solo un proceso (Windows: el número del juego), o sin audio. Activa la
   **voz** si quieres hablar con el micrófono.
4. Pulsa **Abrir la sala**. Aparece el **código de la sala** (6 letras y números, sin `0`, `1`, `I`
   ni `O`): dáselo a tus invitados, o pulsa **Invitar** junto a un amigo conectado.

Mientras la sala está abierta ves a cada invitado con su conexión (directa o por el relay), su RTT,
su latencia de entrada y su **número de seguridad**.

## Unirte a una sala

1. En **Inicio**, escribe el código en **Unirme a una sala** y pulsa **Entrar**, o acepta una
   invitación de un amigo.
2. coroom te enseña los pasos: señalización, claves de la sala, conexión con el anfitrión (directa
   entre los dos equipos o por el relay del servidor) y, antes del vídeo, la **calidad esperada**
   según el RTT medido. Pulsa **Ver ya** o espera.
3. Al principio solo miras: el anfitrión decide si puedes usar el teclado y el ratón, un mando o la
   voz.

Si se corta la conexión, coroom **reconecta solo** (hasta 5 intentos, de medio segundo a 8 s); con
**Reintentar ya** no esperas.

## Permisos

El anfitrión da y quita, a cada invitado y en cualquier momento:

- **Teclado y ratón**: el invitado escribe y mueve el ratón en el equipo del anfitrión.
- **Mando**: una **ranura** (1 a 8) para un mando; cada ranura es de un solo invitado.
- **Voz**: el invitado habla y oye a la sala.

Además puede **pausar la entrada de todos** (se suelta todo lo pulsado) y **expulsar** a un
invitado: sale al momento y las claves de la sala cambian, así que ya no puede ver nada aunque
guarde lo que recibía.

### Número de seguridad

Cada pareja anfitrión–invitado tiene un número de 60 dígitos que solo depende de vuestras dos
identidades. Comparadlo por otro medio (en persona o por una llamada): si coincide, nadie se ha
puesto en medio, ni siquiera el servidor. coroom lo recuerda en Ajustes → Dispositivos.

## Atajos de teclado

| Atajo | Qué hace |
|---|---|
| `Ctrl+,` | Ajustes |
| `Ctrl+Q` | Salir de coroom (si compartes, pregunta antes) |
| `F11` | Pantalla completa |
| `Esc` | Salir de pantalla completa o cerrar el diálogo |
| `Tab` / `Mayús+Tab` | Campo siguiente / anterior (da la vuelta al llegar al final) |
| `Intro` | Envía el formulario del campo |
| `Ctrl+Alt+G` | Invitado: modo inmersivo (el ratón se queda en el juego) o escritorio |
| `Ctrl+Alt+O` | Invitado: estadísticas de la conexión |
| `Ctrl+Alt+M` | Silenciar o activar el micrófono |
| `Ctrl+Alt+V` | Silenciar o activar el audio del juego |
| `Ctrl+Alt+Q` | Invitado: salir de la sala |
| `F12` (pulsar para hablar) | Hablar mientras se mantiene, si lo activas en Ajustes → Audio |

Las letras de `Ctrl+Alt` se cambian en Ajustes → Entrada (no pueden coincidir entre sí).

## Ajustes

- **Vídeo**: resolución, fps (30, 60 o 120), bitrate máximo, códec preferido, 4:4:4 y HDR (solo si
  el anfitrión, el equipo y todos los invitados lo admiten; si no, Ajustes te dice por qué).
- **Audio**: dispositivos, voz por actividad o pulsar para hablar.
- **Entrada**: atajos y mandos.
- **Red**: puerto local y **Usar siempre el relay** (para redes que bloquean el UDP entre equipos).
- **Dispositivos**: tus equipos y los números de seguridad recordados.
- **Apariencia**: tema (el del sistema, claro u oscuro), acento y fuente.
- **Acerca de**: versión, protocolo y aviso de versión nueva (una consulta al día a GitHub, sin
  datos tuyos; se puede desactivar).
- **Diagnóstico**: lo que ve `coroom doctor`: sistema, captura, códecs, audio, entrada y GPU.

## Si algo no va

| Qué pasa | Qué hacer |
|---|---|
| «No se pudo conectar con el servidor» | Comprueba la dirección y tu red. El servidor necesita su puerto **UDP** abierto (HTTP/3); si solo funciona por TCP, avisa al administrador (`/health` dice si el UDP se alcanza desde fuera). |
| La conexión siempre va «por el relay» | Alguna de las dos redes no deja la conexión directa (NAT simétrica, cortafuegos). Funciona igual, con algo más de latencia y gastando la cuota del relay del anfitrión. |
| «El anfitrión agotó su cuota del relay» | Este mes solo funciona la conexión directa; el administrador puede subir la cuota. |
| El vídeo va a saltos o con poca calidad | Abre las estadísticas (`Ctrl+Alt+O`): pérdida, RTT y bitrate. coroom baja el bitrate solo si hay cola en la red; con wifi, prueba con cable. Por encima de 50 ms de RTT la experiencia ya no es buena para juegos rápidos. |
| No se oye el juego | Mira el audio elegido al abrir la sala y, en Linux, el nombre de la aplicación en Ajustes → Diagnóstico. El invitado puede haber silenciado el juego (`Ctrl+Alt+V`). |
| El juego no recibe mi teclado o mando | El anfitrión tiene que darte el permiso; mira también si la entrada está en pausa. En Linux, el anfitrión debe aceptar el diálogo de «Control remoto» del sistema; en Windows, el mando necesita el driver de coroom (sin él, `coroom host --gamepad-to-keys` convierte el mando en teclas). Algunos juegos con anti-trampas no aceptan dispositivos virtuales. |
| Aviso «la clave X-Wing del anfitrión no verifica» | Alguien manipuló el intercambio de claves en el servidor: la entrada se desactiva en esa sala. Avisa al administrador. |
| «Este equipo no puede decodificar…» | Mira Ajustes → Diagnóstico: los códecs que tu equipo decodifica. coroom elige el mejor que tengáis todos. |
| «Demasiados códigos de sala que no existen» | Has probado 10 códigos que no existen en un minuto: espera un minuto y comprueba el código con el anfitrión. |
| La pantalla compartida sale negra (Linux) | Aceptaste el diálogo del portal con otra pantalla, o el compositor no ofrece la captura: prueba con la fuente de prueba y mira Ajustes → Diagnóstico. |

Con `coroom doctor` en una terminal tienes el mismo diagnóstico en texto, para pegarlo al pedir
ayuda (no incluye secretos).
