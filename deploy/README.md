# Despliegue de coroom-server

`coroom-server` es un único binario que termina TLS él mismo (`docs/ARQUITECTURA.md` §3 y §4):

- por **UDP**, un solo socket con HTTP/3 (`h3`: la API `/api/v1`, la administración `/admin`,
  `/health`, `/version` y la señalización WebTransport en `/signal`) y el relay de medios, que
  reenvía los paquetes de las conexiones de pares sin descifrarlos (como un servidor TURN);
- por **TCP**, HTTP/2 y HTTP/1.1 **con TLS** como respaldo (navegadores y redes sin UDP), que
  anuncia HTTP/3 con `Alt-Svc`.

Las cuentas, los dispositivos vinculados, las invitaciones, los amigos y el uso del relay viven
en SQLite (`coroom.db`) dentro de `./data`; las salas, en memoria mientras el anfitrión
está conectado. El servidor nunca ve claves privadas ni contenido: los medios, la entrada y la voz
van cifrados de extremo a extremo.

El servidor no necesita internet: sirve igual en una LAN, en una VPN o expuesto fuera. Tampoco
gestiona certificados: usa el tuyo o uno autofirmado.

## Requisitos

- Un **nombre** por el que los clientes lleguen al servidor, por ejemplo `coroom.example.com`: un
  nombre de tu DNS, de la LAN o de la VPN. Es el del certificado y el de la URL de los clientes.
- Un **certificado** para ese nombre (cadena completa y clave en PEM), de la autoridad que uses,
  o el modo autofirmado, en el que los clientes confían por su huella.
- Docker con Compose v2.
- Que los clientes lleguen a **un puerto UDP** (HTTP/3, señalización y relay) y, recomendado, **un
  puerto TCP** (`/admin` desde el navegador y respaldo de la app si una red bloquea UDP). Por
  defecto son el 443 de cada uno; se cambian con `COROOM_PUBLIC_UDP_PORT` y
  `COROOM_PUBLIC_TCP_PORT` en `.env`. Usar el mismo número para los dos deja una sola URL. Por
  ejemplo, con `ufw`:

  ```bash
  sudo ufw allow 443/udp
  sudo ufw allow 443/tcp
  ```

- Sin reverse proxy por delante: WebTransport y el relay necesitan el UDP directo. Si se pone uno,
  debe hablar HTTP/3 y pasar UDP; no es la configuración recomendada.
- **Búferes UDP del núcleo** (recomendado para el relay): el servidor pide 8 MB de recepción y de
  envío para su socket UDP y Linux los recorta a `net.core.rmem_max` / `net.core.wmem_max`
  (208 KB por defecto, menos de un fotograma clave; una ráfaga que no cabe se pierde entera y el
  invitado pide otro intra). En el host de Docker:

  ```bash
  sudo sysctl -w net.core.rmem_max=8388608 net.core.wmem_max=8388608
  echo -e "net.core.rmem_max=8388608\nnet.core.wmem_max=8388608" | sudo tee /etc/sysctl.d/90-coroom.conf
  ```

  El log del servidor dice al arrancar lo que concedió el sistema (`búferes del socket UDP`, con
  `recv` y `send` en bytes) y avisa si la recepción se queda por debajo de 1 MB.

## Imagen

Cada push a `main` que pasa la CI publica una versión (`.github/workflows/ci.yml`, job
«Publicación») y sube la imagen a GitHub Container Registry para **amd64**, **arm64** y
**arm/v7** (Raspberry Pi con un sistema de 32 bits, como Raspberry Pi OS de 32 bits en una Pi 2,
3 o 4):

```
ghcr.io/43fdfdg45454/coroom-server:X.Y.Z    # una versión concreta (recomendado)
ghcr.io/43fdfdg45454/coroom-server:latest   # la última versión estable
```

`docker-compose.yml` usa `ghcr.io/43fdfdg45454/coroom-server:${COROOM_VERSION}`. Fija la versión
en vez de `latest`: los clientes y el servidor tienen que hablar la misma versión del protocolo.
Para compilar la imagen tú mismo: `docker buildx build -f deploy/Dockerfile -t
ghcr.io/43fdfdg45454/coroom-server:X.Y.Z .` desde la raíz del repositorio (la compilación es
cruzada: `--platform linux/arm64` o `--platform linux/arm/v7` sirven también desde un equipo
amd64).

## Primer arranque

Solo hacen falta `docker-compose.yml` y `.env` en el mismo directorio. Todo lo persistente vive en
`./data` junto a ellos, que la imagen usa con el usuario sin privilegios (uid 65532). Con el modo
por defecto, `files`, el certificado va en `./data/certs`:

```bash
cd deploy
cp .env.sample .env        # edita .env: COROOM_PUBLIC_HOST, cuotas…
mkdir -p data/certs
cp /ruta/a/fullchain.pem /ruta/a/privkey.pem data/certs/
sudo chown -R 65532:65532 data
docker compose pull
docker compose up -d
docker compose logs -f server
```

Compose lee las variables de `.env` y se niega a arrancar si falta `COROOM_PUBLIC_HOST`. En `./data` quedan `coroom.db` (cuentas, invitaciones, amigos, uso del relay),
`certs/` (tu certificado) y, en modo autofirmado, `server.crt` y `server.key`.

En el log verás los dos `bind`, el modo TLS y la huella del certificado. Abre
**`https://coroom.example.com/admin`** (con `:PUERTO` si no es el 443): la primera vez pide crear
el **administrador** (nombre y contraseña de al menos 12 caracteres). Después la página queda
protegida por login.

### Certificado

El servidor lee el certificado al arrancar. Cuando lo renueves, copia los ficheros nuevos a
`./data/certs` y reinicia con `docker compose restart server` (las salas abiertas se cortan; los
clientes reconectan solos). El certificado debe ser para `COROOM_PUBLIC_HOST`.

Sin certificado, `COROOM_TLS_MODE=self-signed`: el servidor genera uno la primera vez y lo guarda
en `./data`. Los clientes lo aceptan comparando la huella que muestran con la del log
(`fingerprint=…`) o la de `/admin` → Ajustes, y el navegador avisa al abrir `/admin`.

## Usuarios e invitaciones

Sin registro abierto. Desde `/admin`:

- **Usuarios**: crear (nombre, nombre visible, contraseña inicial), hacer o quitar administrador,
  restablecer la contraseña (cierra sus sesiones) y deshabilitar (no puede entrar y sus sesiones
  se cierran al momento). Siempre queda al menos un administrador habilitado.
- **Invitaciones** (con `COROOM_REGISTRATION=invite`, el valor por defecto): códigos de un solo
  uso (`XXXX-XXXX-XXXX`) que caducan a las 72 h (configurable al crearlos). El código solo se
  muestra al crearlo; dáselo a tu amigo y él crea su cuenta con
  `coroom login --server https://coroom.example.com --invitation XXXX-XXXX-XXXX`. Con
  `COROOM_REGISTRATION=admin` solo el administrador da de alta.
- **Uso del relay** por usuario y mes, **salas activas** (solo recuentos y bytes) y **ajustes**
  (modo TLS, caducidad del certificado, cuotas, autocomprobación de UDP, versión).

Todo está también en la API (`/api/v1/admin/...`).

Cada jugador inicia sesión desde su equipo con `coroom login`: la app presenta la clave pública de
su identidad y firma un desafío del servidor, que vincula ese **dispositivo** a la cuenta. Solo los
dispositivos vinculados abren la señalización y el relay.

## Variables

`docker-compose.yml` pasa al contenedor estas variables, que se definen en `.env` (ver
`.env.sample`, con valores ficticios). Las que no definas toman el valor de la columna central.

| Variable | Por defecto | Uso |
|---|---|---|
| `COROOM_VERSION` | `1.0.0` | Versión de la imagen; la misma que la de los clientes |
| `COROOM_PUBLIC_HOST` | **obligatoria** | Nombre por el que llegan los clientes: el del certificado y el de su URL |
| `COROOM_PUBLIC_UDP_PORT` | `443` | Puerto UDP del host al que llegan los clientes: HTTP/3, señalización y relay |
| `COROOM_PUBLIC_TCP_PORT` | `443` | Puerto TCP del host al que llegan los clientes: `/admin` y HTTP/2 de respaldo |
| `COROOM_TLS_MODE` | `files` | `files` (tu certificado) o `self-signed` (generado por el servidor) |
| `COROOM_TLS_CERT`, `COROOM_TLS_KEY` | `/data/certs/fullchain.pem`, `/data/certs/privkey.pem` | PEM del modo `files` (`./data/certs` en el host) |
| `COROOM_REGISTRATION` | `invite` | `invite` (administrador e invitaciones) o `admin` (solo administrador) |
| `COROOM_MAX_GUESTS_PER_ROOM` | `8` | Invitados como mucho por sala |
| `COROOM_RELAY_ENABLED` | `true` | Relay de medios; sin él, las salas solo funcionan en directo |
| `COROOM_RELAY_QUOTA_GB_PER_MONTH` | `0` | Cuota mensual del relay por anfitrión en GB (10⁹ bytes, admite decimales); `0` = sin límite |
| `COROOM_RELAY_MAX_KBPS` | `0` | Bitrate máximo de vídeo por invitado en el relay; se anuncia al anfitrión, que no apunta por encima (lo que pase igual se descarta y se cuenta). Lo que sube cada invitado tiene el mismo límite aparte; `0` = sin límite. Con N invitados por el relay, el servidor recibe N copias del anfitrión y manda N |
| `COROOM_SOURCE_URL` | el repositorio de coroom | Repositorio que enlazan `GET /version` y `/admin` (AGPL-3.0); con un servidor modificado, el de tu fork |
| `RUST_LOG` | `info` | Nivel de logs (nunca incluyen tokens ni contraseñas) |

Las direcciones internas (`COROOM_BIND_UDP` `[::]:4433`, `COROOM_BIND_TCP` `[::]:8443`) y
`COROOM_DATA_DIR` (`/data`) las fija la imagen. `COROOM_DEV_TOKEN` y
`COROOM_TCP_PLAIN` son solo para desarrollo y no se pasan en el Compose. Si el sistema no tiene
IPv6, el servidor sustituye `[::]` por `0.0.0.0` y lo avisa en el log.

### Cuotas del relay

Los bytes que el relay reenvía en una sala se cargan al **anfitrión** de la sala, por día. Con una
cuota mensual, al agotarla el relay corta las salas de ese anfitrión (los miembros reciben
`QuotaExceeded`) y las salas nuevas se crean sin relay: los clientes lo dicen y conectan solo en
directo. El uso es visible en `/admin` → Uso del relay y para cada usuario con `coroom whoami`.

## Copias de seguridad

```bash
docker compose exec server coroom-server backup /data/copia-$(date +%F).db
```

Es una copia en línea y consistente de SQLite (`VACUUM INTO`), válida con el servidor en marcha;
queda en `./data` del host. Para restaurarla: para el servidor, pon la copia como
`./data/coroom.db` (borrando `coroom.db-wal` y `coroom.db-shm` si existen) y arráncalo.

Recuperar el acceso si se olvida la contraseña del administrador:

```bash
docker compose exec -it server coroom-server admin reset-password admin
```

`coroom-server migrate` aplica las migraciones y termina (el servidor también lo hace al arrancar).

## Actualizar

Con la imagen de GHCR: sube `COROOM_VERSION` en `.env` a la versión nueva (la de la publicación,
la misma que tendrán los clientes) y

```bash
docker compose pull
docker compose up -d
```

Las migraciones de la base se aplican solas al arrancar; los clientes y el servidor deben hablar
la misma versión del protocolo (`GET /version`, y la señalización lo comprueba y lo explica).

## Seguridad del servidor

La revisión de la 1.0 está en [`docs/seguridad.md`](../docs/seguridad.md). Lo que conviene saber
como operador:

- **Lo que ve el servidor**: cuentas, amigos, quién entra en qué sala y cuándo, direcciones IP y el
  volumen y el ritmo del tráfico del relay. No ve vídeo, audio, voz, entrada ni chat, ni siquiera
  las cabeceras de los medios (el relay reenvía paquetes cifrados de punta a punta entre los pares), ni claves privadas: los tokens se guardan como SHA-256 y las contraseñas con Argon2id.
- **Límites ya puestos** (no hay que configurarlos): 5 fallos de login por minuto por cuenta y por
  IP; 10 códigos de sala que no existen por minuto por cuenta; cuerpos de 64 KiB; mensajes de
  señalización de 256 KiB; cabeceras HTTP en 10 s y cuerpos en 20 s por TCP; 1 024 conexiones TCP a
  la vez; `idle_timeout` de QUIC de 15 s.
- **Cabeceras**: todas las respuestas llevan `Strict-Transport-Security` (un año, sin
  subdominios): una vez que un navegador abre `/admin` por HTTPS con ese nombre, no vuelve a probar
  HTTP en claro. `/admin` lleva además una CSP estricta y no se puede enmarcar; la sesión web dura
  12 horas y vive en la pestaña (sin cookies).
- **Registro**: `RUST_LOG` (por defecto `info`). Ni en `trace` aparecen contraseñas ni tokens (lo
  comprueba un test), pero `debug` y `trace` registran IP y rutas: úsalos solo para diagnosticar.
- **Modo desarrollo**: no definas `COROOM_DEV_TOKEN` ni `COROOM_TCP_PLAIN` en un servidor que usen
  otras personas.
- **Actualizaciones**: sigue las publicaciones (`CHANGELOG.md`); el servidor y los clientes deben
  hablar la misma versión del protocolo.

## Comprobación y problemas frecuentes

```bash
curl -s https://coroom.example.com/health
```

`/health` publica `udp_reachable` y `udp_checked_at` (la **autocomprobación de UDP**: al arrancar y
cada 10 minutos el servidor hace un handshake QUIC consigo mismo en
`COROOM_PUBLIC_HOST:COROOM_PUBLIC_UDP_PORT`), `users`, `rooms`, `relay_bytes` (desde el arranque),
`relay_bytes_today` y `relay_rooms` (bytes por sala, sin identificarlas).

- **`udp_reachable: false`**: el puerto UDP no llega. Revisa el cortafuegos, la redirección de
  puertos si la hay y que `COROOM_PUBLIC_HOST` resuelva a este servidor desde él mismo. Ojo: detrás
  de una NAT **sin hairpin** (el router no devuelve a la red local el tráfico a su propia IP
  externa) da `false` aunque desde fuera funcione; compruébalo desde un cliente
  (`curl --http3 https://coroom.example.com/health`).
- **El servidor no arranca con `files`**: el log nombra el fichero que no pudo leer. Comprueba que
  están en `./data/certs`, que el usuario 65532 puede leerlos y que la cadena es la completa.
- **El relay no conecta**: el relay atiende en el mismo puerto UDP que la señalización y cada
  cliente se liga a la dirección por la que llegó al servidor (de la LAN, de una VPN o pública),
  así que no hay que configurar ninguna. Sin relay, la sala va solo en directo: lo dice el aviso
  del cliente (`COROOM_RELAY_ENABLED=false` o cuota del anfitrión agotada).
- **«la sesión no es válida o ha caducado»** en un cliente: el token caduca a los 30 días o se
  revocó (contraseña cambiada, cuenta deshabilitada, `coroom logout`); `coroom login` otra vez.
- **Demasiados intentos**: tras 5 fallos de login en un minuto (por cuenta y por IP) el servidor
  responde lo mismo para todo hasta que pasa el minuto. Con los códigos de sala pasa igual tras 10
  códigos que no existen («espera un minuto»).
- **Un cliente dice que la clave X-Wing del anfitrión no verifica**: alguien manipuló el
  intercambio de claves en el servidor (o en algo delante de él). Revisa que la imagen sea la
  publicada y que nada intercepte el tráfico; la entrada de esa sala queda desactivada.

`docker stop` envía `SIGTERM` y el servidor se apaga limpiamente (vuelca el uso pendiente del relay
a la base); también con Ctrl+C.

## Desarrollo sin Docker

```bash
COROOM_TLS_MODE=self-signed COROOM_PUBLIC_HOST=localhost cargo run -p coroom-server
# HTTP/3 en UDP 4433, HTTP/2 con TLS en TCP 8443; abre https://localhost:8443/admin
curl -sk https://localhost:8443/health
cargo test -p coroom-server     # cuentas, señalización, relay, TLS
```

Con `COROOM_DEV_TOKEN=change-me` la señalización acepta además ese token sin cuenta (modo
desarrollo); `COROOM_TCP_PLAIN=true` sirve TCP sin TLS.
