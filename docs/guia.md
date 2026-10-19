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
   su versión y si el relay está activo. Si el servidor usa un certificado autofirmado, te enseña su huella para que la compares con la que te dio el administrador y pulses
   **Confiar**.
2. **Entrar**: con tu usuario y contraseña. Si tienes un **código de invitación**, elige **Crear
   cuenta**: escribes el código, tu nombre de usuario, tu nombre visible y tu contraseña (al menos
   12 caracteres).
3. Ponle **nombre a este equipo** («Portátil», «Sobremesa»): tu cuenta puede tener varios equipos y
   cada uno tiene su propia identidad criptográfica, que se guarda en el llavero del sistema.

Tu sesión dura 30 días en cada equipo. En Ajustes → Cuenta y servidor ves tus equipos y puedes cerrar la
sesión de uno que ya no uses.

Ahí mismo, en **Avatar**, eliges una imagen tuya (PNG, JPEG o WebP) con el selector de ficheros del
sistema: se recorta al cuadrado central y se reduce antes de subirla. La ve cualquiera con cuenta
en tu servidor (tus amigos en su lista, quien comparte sala contigo en la sala y en el chat). Sin
imagen, se ven tus iniciales.

## Alojar: compartir tu juego

1. En **Inicio**, pulsa **Compartir mi pantalla**.
2. Elige **qué compartir**:
   - **Pantalla**: un monitor entero.
   - **Ventana**: una sola ventana. Si se ve en negro porque el juego usa pantalla completa
     exclusiva, se transmite su monitor.
   - **Solo voz**: la sala se abre sin compartir nada, para hablar y escribir; tus invitados ven
     «Nadie está compartiendo». Desde la sala compartes la pantalla o una ventana cuando quieras.
   - La **fuente de prueba**, para comprobar la conexión sin enseñar nada tuyo.

   En los dos modos, en **Audio que se envía** salen todas las aplicaciones que suenan, marcadas:
   desmarca las que no quieras (por ejemplo, el programa con el que hablas). Las que quitas se
   recuerdan, y las que empiezan a sonar después entran solas.

   Si el sistema elige el monitor o la ventana con su propio diálogo, lo eliges al abrir la sala,
   y solo la primera vez: en Linux coroom recuerda el permiso (por separado para pantalla y
   ventana) y el diálogo no vuelve a salir en las siguientes ejecuciones, tampoco con
   `coroom host` ni `coroom probe`. Para que pregunte de nuevo, pulsa **Olvidar el permiso de
   pantalla** en Ajustes → Sala, o revócalo desde Ajustes del sistema → Aplicaciones → Compartir
   pantalla (KDE y GNOME).

   coroom nunca se envía a sí misma, así que la voz de la sala no vuelve como eco. Si el equipo no
   puede separar el audio por aplicación (Windows anterior a 10 20348), se envía todo lo que suena,
   también las voces de la sala que reproduce coroom.
3. Activa la **voz** si quieres hablar con el micrófono. Los invitados entran hablando; si
   prefieres darles la voz uno a uno, marca **Los invitados entran callados**.
4. Pulsa **Abrir la sala**. Aparece el **código de la sala** (6 letras y números, sin `0`, `1`, `I`
   ni `O`): dáselo a tus invitados, o pulsa **Invitar** junto a un amigo conectado.

Mientras la sala está abierta ves a cada invitado con su conexión (directa o por el relay, con la
dirección IP y el puerto por los que va), su RTT, su latencia de entrada y su **número de
seguridad**. «Directo» quiere decir que los dos equipos hablan entre sí sin pasar por el servidor;
por el relay, la dirección que ves es la del servidor.

### Cambiar con la sala abierta

En **Transmisión**, debajo del código, cambias sin cerrar la sala (los invitados siguen dentro):

- **Qué compartes**: elige pantalla o aplicación y el monitor o la ventana, y pulsa **Compartir
  esto** (en Linux, **Cambiar…** abre el selector del sistema y lo elegido se comparte al
  aceptarlo). Los invitados ven la última imagen un momento hasta que llega la nueva; si la nueva
  no se puede capturar, sigue la de antes y te lo dice.
- **Dejar de compartir**: la sala sigue abierta con voz y chat; tus invitados ven «Nadie está
  compartiendo» y no reciben ni la última imagen ni el audio de tu equipo, y su teclado, ratón y
  mandos dejan de llegarte. **Compartir pantalla o ventana** vuelve a compartir.
- **Audio**: marca o desmarca aplicaciones; se aplica al momento, sin cortar.
- **Tu micrófono** y **dónde oyes la sala**: cambian al momento, sin cortar a nadie.
- **Códec preferido**, **resolución** y **fps**: cambiar de códec vuelve a negociar con los
  invitados y corta el vídeo un instante. El codificador de «Avanzado» también se aplica a la sala
  abierta; 4:4:4 y HDR, a la siguiente. El bitrate no se elige: lo ajusta solo cada invitado.

### Pasar la sala a otro

En el menú **Más** de un invitado, **Pasarle la sala**: si acepta, abre una sala nueva (solo voz;
desde ella comparte lo suyo) y todos, tú incluido, entráis en ella solos, sin escribir el código
(quien tenía el micrófono silenciado sigue así). Mientras no contesta, la sala lo dice y puedes
**Retirar la oferta**. A quien entra desde Android no se le puede pasar: el teléfono solo entra en
salas, y el botón lo explica.

## Unirte a una sala

1. En **Inicio**, escribe el código en **Unirme a una sala** y pulsa **Entrar**, o acepta una
   invitación de un amigo. Si tienes una sala abierta con tu cuenta en otro dispositivo, Inicio la
   ofrece como **Tu sala en …**: entras con **Entrar**, sin código. Puedes estar en una sala
   desde varios dispositivos a la vez; el anfitrión ve el nombre del dispositivo junto al tuyo.
2. coroom te enseña los pasos: señalización, claves de la sala, conexión con el anfitrión (directa
   entre los dos equipos o por el relay del servidor) y, antes del vídeo, la **calidad esperada**
   según el RTT medido. Pulsa **Ver ya** o espera.
3. Al principio solo miras: el anfitrión decide si puedes usar el teclado y el ratón, un mando o la
   voz.

Si se corta la conexión, coroom **reconecta solo** (hasta 5 intentos, de medio segundo a 8 s); con
**Reintentar ya** no esperas.

## Permisos

El anfitrión da y quita, a cada invitado y en cualquier momento:

- **Teclado y ratón**: el invitado escribe y mueve el ratón en el equipo del anfitrión. Como en
  QEMU, no se envía nada hasta que el invitado **hace clic en la imagen** (ese clic no llega al
  juego); `Ctrl+Alt+R` o cambiar de ventana lo sueltan sin perder el permiso, y otro clic lo
  recupera. Mientras tenga el permiso, un aviso junto a la píldora de latencia dice cuál de las
  dos cosas toca (a pantalla completa, solo con la barra). Los mandos no dependen de esto.
- **Mando**: una **ranura** (1 a 8) para un mando; cada ranura es de un solo invitado. Tu mando
  llega **tal cual** (el juego y Steam lo ven como ese mando, con sus botones) o **convertido a
  Xbox 360**, como decida el anfitrión. La vibración del juego llega a tu mando de las dos formas
  (convertido, si tu mando vibra). Si tienes varios mandos, eliges cuál envías en el panel de
  mandos de la barra (`Ctrl+Alt+J`).
- **Voz**: el invitado habla a la sala. Lo trae al entrar, salvo que la sala se abriera con **Los
  invitados entran callados**; entonces se lo da el anfitrión. Oír al anfitrión y a los demás no
  necesita permiso: todo invitado oye la voz de la sala desde que entra.

Los permisos de cada invitado (y su número de seguridad, pasarle la sala o expulsarle) están en el
menú **Más** de su fila en **Integrantes**.

Aparte de los permisos, cada uno elige a quién oye en su equipo, en **Integrantes** (el anfitrión,
en Compartir; el invitado, en la pestaña **Sala** del panel): el deslizador de cada persona es lo
alto que se la oye aquí, y su altavoz la silencia solo aquí (tachado y con la barra apagada). No
cambia nada para los demás. En tu propia fila, la primera, el deslizador es lo alto que te oyen y el
altavoz te silencia para toda la sala.

Como invitado, arriba a la izquierda del vídeo ves **quién está hablando**: una fila por persona
con su avatar, en el orden en que empezaron a hablar (se quita en Ajustes → Sala). En
**Integrantes**, el avatar de quien habla lleva un anillo verde, también como anfitrión. La pestaña **Ajustes** del panel reúne lo de tu equipo: VSync, decodificar por software,
micrófono y salida (cambian al momento), volumen del juego y el mando que envías.

Además puede **pausar la entrada de todos** (se suelta todo lo pulsado) y **expulsar** a un
invitado: sale al momento y las claves de la sala cambian, así que ya no puede ver nada aunque
guarde lo que recibía.

### Número de seguridad

Cada pareja anfitrión–invitado tiene un número de 60 dígitos que solo depende de vuestras dos
identidades. Comparadlo por otro medio (en persona o por una llamada): si coincide, nadie se ha
puesto en medio, ni siquiera el servidor. coroom lo recuerda en Ajustes → Cuenta y servidor.

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
| `Ctrl+Alt+R` | Invitado: soltar el teclado y el ratón (un clic en la imagen los recupera) |
| `Ctrl+Alt+S` | Invitado: estadísticas de la conexión |
| `Ctrl+Alt+C` | Chat de la sala (también en la sala de espera) |
| `Ctrl+Alt+M` | Silenciar o activar el micrófono |
| `Ctrl+Alt+V` | Silenciar o activar el audio del juego |
| `Ctrl+Alt+Q` | Invitado: salir de la sala |
| `F12` (pulsar para hablar) | Hablar mientras se mantiene, si lo activas en Ajustes → Audio y voz |

Las letras de `Ctrl+Alt` se cambian en Ajustes → Mandos (no pueden coincidir entre sí).
El `Alt` de los atajos es el izquierdo (o el derecho en distribuciones sin AltGr): con AltGr,
como en la latinoamericana, `AltGr+Q` escribe `@` en el juego o en el chat y no sale de la sala.

## Ajustes

- **Vídeo**: resolución, fps (30, 60 o 120), códec preferido, 4:4:4 y HDR (solo si
  el anfitrión, el equipo y todos los invitados lo admiten; si no, Ajustes te dice por qué). El
  bitrate no se elige: es automático para cada invitado, sube hasta lo que da su conexión y baja
  antes de que el vídeo se corte (también en redes que tiran lo que sobra sin avisar).
  En **Avanzado**, el codificador del anfitrión y el decodificador del invitado: déjalos en
  Automático salvo que el vídeo se vea gris o corrupto o la aplicación se cierre al compartir. En
  Windows, Automático usa NVENC, AMF o Quick Sync antes que Vulkan Video. Al abrirse, coroom
  comprueba cada codificador y decodificador por GPU: codifica unos segundos de vídeo de prueba y
  mira que se decodifique igual que el original. Los que dan vídeo dañado con tu driver (AV1 por
  Vulkan Video con algunos drivers de NVIDIA, por ejemplo) no se usan en
  Automático, y `coroom doctor` y el registro dicen cuáles y por qué; cuando actualices un driver
  que lo arregle, se vuelven a usar solos. Si el invitado recibe muchos fotogramas dañados, la
  aplicación lo avisa.
- **Audio**: dispositivos (el **predeterminado del sistema** dice cuál es; con una sala abierta,
  cambian al momento), **volumen de tu voz** (lo alto que te oyen), **Probar el micrófono** (el
  nivel y, con **Oírte**, tu voz por la salida) y voz por actividad o pulsar para hablar.
- **Entrada**: atajos y mandos.
- **Red**: **Abrir el puerto en el router** (activado: coroom pide al router por UPnP, NAT-PMP o
  PCP que deje entrar la conexión directa y anuncia la dirección WAN del router; sirve también
  detrás de la NAT de la operadora, entre clientes de la misma), puerto local y **Usar siempre el
  relay** (para redes que bloquean el UDP entre equipos).
- **Dispositivos**: tus equipos y los números de seguridad recordados.
- **Apariencia**: tema (el del sistema, claro u oscuro), acento y fuente.
- **Acerca de**: versión, protocolo y aviso de versión nueva (una consulta al día a GitHub, sin
  datos tuyos; se puede desactivar).
- **Diagnóstico**: lo que ve `coroom doctor`: sistema, captura, códecs, audio, entrada y GPU.

## Si algo no va

| Qué pasa | Qué hacer |
|---|---|
| «No se pudo conectar con el servidor» | Comprueba la dirección y tu red. El servidor necesita su puerto **UDP** abierto (HTTP/3); si solo funciona por TCP, avisa al administrador (`/health` dice si el UDP se alcanza desde fuera). |
| La conexión siempre va «por el relay» | Alguna de las dos redes no deja la conexión directa (NAT simétrica, NAT de la operadora, cortafuegos). Activa UPnP (o NAT-PMP/PCP) en el router de los dos y deja marcado **Abrir el puerto en el router** en Ajustes → Cuenta y servidor; `coroom doctor` dice si el router respondió. Funciona igual por el relay, con algo más de latencia y gastando la cuota del relay del anfitrión. |
| «El anfitrión agotó su cuota del relay» | Este mes solo funciona la conexión directa; el administrador puede subir la cuota. |
| El vídeo va a saltos o con poca calidad | Abre las estadísticas (`Ctrl+Alt+S`): pérdida, RTT y bitrate. coroom baja el bitrate solo si hay cola en la red; con wifi, prueba con cable. Por encima de 50 ms de RTT la experiencia ya no es buena para juegos rápidos. |
| No se oye el juego | Mira el audio elegido al abrir la sala y, en Linux, el nombre de la aplicación en el diagnóstico (botón del servidor en la cabecera). El invitado puede haber silenciado el juego (`Ctrl+Alt+V`). |
| El juego no recibe mi teclado o mando | El anfitrión tiene que darte el permiso; mira también si la entrada está en pausa. En Linux, el anfitrión debe aceptar el diálogo de «Control remoto» del sistema; en Windows, el mando necesita el driver de coroom, que se instala con `install.ps1` sin tocar la seguridad de Windows (sin él, `coroom host --gamepad-to-keys` convierte el mando en teclas); si el juego solo usa XInput y el mando va «Tal cual», ponlo en «Convertido a Xbox 360» o lanza el juego desde Steam con Steam Input. Algunos juegos con anti-trampas no aceptan dispositivos virtuales. |
| Aviso «la clave X-Wing del anfitrión no verifica» | Alguien manipuló el intercambio de claves en el servidor: la entrada se desactiva en esa sala. Avisa al administrador. |
| «Este equipo no puede decodificar…» | Mira el diagnóstico (botón del servidor en la cabecera): los códecs que tu equipo decodifica. coroom elige el mejor que tengáis todos. |
| «Demasiados códigos de sala que no existen» | Has probado 10 códigos que no existen en un minuto: espera un minuto y comprueba el código con el anfitrión. |
| La pantalla compartida sale negra (Linux) | Aceptaste el diálogo del portal con otra pantalla, o el compositor no ofrece la captura: prueba con la fuente de prueba y mira el diagnóstico. |

Con `coroom doctor` en una terminal tienes el mismo diagnóstico en texto, para pegarlo al pedir
ayuda (no incluye secretos).

La aplicación guarda un **registro** de cada ejecución en `logs/coroom.log`, dentro del
directorio de datos (`%APPDATA%\coroom\logs` en Windows, `~/.local/share/coroom/logs` en Linux, o
el de `COROOM_DATA_DIR`); la ejecución anterior queda en `coroom.1.log`. `coroom doctor` dice la
ruta exacta. Si algo falla (una sala que no abre, el vídeo que se corta), cierra coroom y manda
esos dos ficheros junto con el diagnóstico. En Android, **Ajustes → Compartir registro** (o el
botón del panel de conexión de la sala) los manda con la aplicación que elijas.
