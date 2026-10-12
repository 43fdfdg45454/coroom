# Despliegue de coroom-server

`coroom-server` es un único binario que termina TLS él mismo (`docs/ARQUITECTURA.md` §3 y §4):

- por **UDP** (443), un solo socket QUIC que demultiplexa por ALPN entre HTTP/3 (`h3`: la API
  `/api/v1`, la administración `/admin`, `/health`, `/version` y la señalización WebTransport en
  `/signal`) y el relay de medios (`coroom-relay/1`);
- por **TCP** (443), HTTP/2 y HTTP/1.1 **con TLS** como respaldo (navegadores y redes sin UDP), que
  anuncia HTTP/3 con `Alt-Svc` y resuelve el reto ACME **TLS-ALPN-01**.

Las cuentas, los dispositivos vinculados, las invitaciones, los amigos y el uso del relay viven
en SQLite (`coroom.db`) dentro del volumen de datos; las salas, en memoria mientras el anfitrión
está conectado. El servidor nunca ve claves privadas ni contenido: los medios, la entrada y la voz
van cifrados de extremo a extremo.

## Requisitos

- Un **dominio** (o subdominio) que apunte a la IP pública del servidor, por ejemplo
  `coroom.example.com`: el certificado ACME (Let's Encrypt) se emite para ese nombre y los clientes
  se conectan con `https://coroom.example.com`.
- Docker con Compose v2 y BuildKit (por defecto en las versiones actuales).
- Abrir hacia el servidor **UDP 443** (HTTP/3, señalización y relay) y **TCP 443** (respaldo
  HTTP/2 y reto TLS-ALPN-01). Con `COROOM_ACME_CHALLENGE=http-01`, además **TCP 80**. Por ejemplo,
  con `ufw`:

  ```bash
  sudo ufw allow 443/udp
  sudo ufw allow 443/tcp
  sudo ufw allow 80/tcp     # solo con COROOM_ACME_CHALLENGE=http-01
  ```

  En un router doméstico, redirige los mismos puertos a la máquina del servidor.
- Sin reverse proxy por delante: WebTransport y el relay necesitan el UDP directo y el reto
  TLS-ALPN-01 necesita el TCP 443 directo. Si se pone uno, debe hablar HTTP/3, pasar UDP y dejar
  pasar el ALPN `acme-tls/1` (o usar HTTP-01); no es la configuración recomendada.

## Imagen

Cada push a `main` que pasa la CI publica una versión (`.github/workflows/ci.yml`, job
«Publicación») y sube la imagen a GitHub Container Registry para **amd64** y **arm64**:

```
ghcr.io/43fdfdg45454/coroom-server:X.Y.Z    # una versión concreta (recomendado)
ghcr.io/43fdfdg45454/coroom-server:latest   # la última versión estable
```

`docker-compose.yml` usa `ghcr.io/43fdfdg45454/coroom-server:${COROOM_VERSION}` (`COROOM_VERSION`
en `.env`, `0.1.0` por defecto). Fija la versión en vez de `latest`: los clientes y el servidor
tienen que hablar la misma versión del protocolo. Para compilar la imagen desde el repositorio en
lugar de descargarla, `docker compose up -d --build` (necesita BuildKit; la etapa de compilación es
cruzada, `docker buildx build --platform linux/arm64 -f deploy/Dockerfile .` sirve también desde
un equipo amd64).

## Primer arranque

```bash
cd deploy
cp .env.sample .env        # edita .env: COROOM_PUBLIC_HOST, COROOM_RELAY_ADDR, cuotas…
docker compose pull        # o `docker compose up -d --build` para compilarla aquí
docker compose up -d
docker compose logs -f server
```

En el log verás los dos `bind`, el modo TLS y la línea `pidiendo certificado ACME`; mientras llega
(unos segundos) el servidor sirve un certificado autofirmado temporal. Cuando aparezca
`certificado ACME instalado`, abre **`https://coroom.example.com/admin`**: la primera vez pide
crear el **administrador** (nombre y contraseña de al menos 12 caracteres). Después la página
queda protegida por login.

Para probar sin dominio, pon `COROOM_TLS_MODE=self-signed` (solo desarrollo): los clientes fijan el
certificado por la huella que imprime el log (`fingerprint=…`) con `--trust-fingerprint`, y el
navegador mostrará un aviso.

Para no gastar el límite de Let's Encrypt mientras pruebas, usa su entorno de pruebas
(`COROOM_ACME_DIRECTORY=https://acme-staging-v02.api.letsencrypt.org/directory`); sus
certificados no son de confianza, así que los clientes necesitarán `--trust-fingerprint` con la
huella que muestra `/admin` → Ajustes.

## Usuarios e invitaciones

Sin registro público. Desde `/admin`:

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

Todas con prefijo `COROOM_` (ver `.env.sample`, con valores ficticios):

| Variable | Por defecto | Uso |
|---|---|---|
| `COROOM_PUBLIC_HOST` | `localhost` | Nombre público: el del certificado y el de la URL de los clientes |
| `COROOM_TLS_MODE` | `self-signed` (`acme` en `.env.sample`) | `acme`, `files` o `self-signed` (solo desarrollo) |
| `COROOM_ACME_DIRECTORY` | Let's Encrypt producción | Directorio ACME |
| `COROOM_ACME_CONTACT` | — | Correo de contacto para la autoridad ACME (opcional) |
| `COROOM_ACME_CHALLENGE` | `tls-alpn-01` | `tls-alpn-01` (TCP 443) o `http-01` (TCP 80) |
| `COROOM_TLS_CERT`, `COROOM_TLS_KEY` | — | PEM del modo `files` |
| `COROOM_BIND_UDP` | `[::]:4433` | HTTP/3, señalización y relay (Compose publica `443:4433/udp`) |
| `COROOM_BIND_TCP` | `[::]:8443` | HTTP/2 y HTTP/1.1 con TLS (Compose publica `443:8443/tcp`) |
| `COROOM_BIND_HTTP` | `[::]:8080` con `http-01` | Reto HTTP-01 y redirección a HTTPS (Compose: `80:8080/tcp`) |
| `COROOM_PUBLIC_UDP_PORT` | `443` | Puerto UDP público (`Alt-Svc` y autocomprobación); `0` = el local |
| `COROOM_PUBLIC_TCP_PORT` | `443` | Puerto TCP público (redirección de HTTP-01, ajustes); `0` = el local |
| `COROOM_TCP_PLAIN` | `false` | Solo desarrollo: TCP sin TLS |
| `COROOM_REGISTRATION` | `invite` | `invite` (administrador e invitaciones) o `admin` (solo administrador) |
| `COROOM_RELAY_ENABLED` | `true` | Relay de medios; sin él, las salas solo funcionan en directo |
| `COROOM_RELAY_QUOTA_GB_PER_MONTH` | `0` | Cuota mensual del relay por anfitrión en GB (10⁹ bytes, admite decimales); `0` = sin límite |
| `COROOM_RELAY_MAX_KBPS` | `0` | Bitrate máximo por sala en el relay (lo que pase se descarta y se cuenta); `0` = sin límite |
| `COROOM_RELAY_ADDR` | dirección local de la conexión + puerto UDP | Dirección UDP pública del relay en `RelayToken`; **obligatoria en Docker**: `<IP pública>:443` |
| `COROOM_MAX_GUESTS_PER_ROOM` | `8` | Invitados como mucho por sala |
| `COROOM_DATA_DIR` | `./data` (`/data` en la imagen) | `coroom.db`, `acme/` y el certificado autofirmado |
| `COROOM_SOURCE_URL` | el repositorio de coroom | Repositorio del código que enlazan `GET /version` y `/admin` (AGPL-3.0); si ejecutas un servidor modificado, el de tu fork |
| `COROOM_DEV_TOKEN` | — | **Solo desarrollo**: la señalización acepta también este token sin cuenta (el log avisa) |
| `RUST_LOG` | `info` | Nivel de logs (nunca incluyen tokens ni contraseñas) |

Si el sistema no tiene IPv6, el servidor sustituye `[::]` por `0.0.0.0` y lo avisa en el log.

### Cuotas del relay

Los bytes que el relay reenvía en una sala se cargan al **anfitrión** de la sala, por día. Con una
cuota mensual, al agotarla el relay corta las salas de ese anfitrión (los miembros reciben
`QuotaExceeded`) y las salas nuevas se crean sin relay: los clientes lo dicen y conectan solo en
directo. El uso es visible en `/admin` → Uso del relay y para cada usuario con `coroom whoami`.

### Certificados ACME

La cuenta ACME (`acme/account.json`, permisos `0600`), el certificado (`acme/cert.pem`) y su clave
(`acme/key.pem`, `0600`) viven en el volumen. La renovación es automática: 30 días antes de
caducar (en certificados de vida corta, al pasar dos tercios de su vida), con reintentos con espera
exponencial (de 1 minuto a 6 horas) si falla, y el cambio de certificado es en caliente para QUIC y
TCP. Si cambias `COROOM_ACME_DIRECTORY`, se crea una cuenta nueva en ese directorio.

## Copias de seguridad

```bash
docker compose exec server coroom-server backup /data/copia-$(date +%F).db
```

Es una copia en línea y consistente de SQLite (`VACUUM INTO`), válida con el servidor en marcha.
Para restaurarla: para el servidor, pon la copia como `coroom.db` en el volumen (borrando
`coroom.db-wal` y `coroom.db-shm` si existen) y arráncalo. Guarda también `acme/` si no quieres
volver a pedir el certificado.

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

Si compilas tú la imagen: `git pull` y `docker compose up -d --build`.

Las migraciones de la base se aplican solas al arrancar; los clientes y el servidor deben hablar
la misma versión del protocolo (`GET /version`, y la señalización lo comprueba y lo explica).

## Seguridad del servidor

La revisión de la 1.0 está en [`docs/seguridad.md`](../docs/seguridad.md). Lo que conviene saber
como operador:

- **Lo que ve el servidor**: cuentas, amigos, quién entra en qué sala y cuándo, direcciones IP y el
  volumen y el ritmo del tráfico del relay. No ve vídeo, audio, voz ni entrada (cifrados de extremo
  a extremo), ni claves privadas: los tokens se guardan como SHA-256 y las contraseñas con Argon2id.
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
- **Modo desarrollo**: no definas `COROOM_DEV_TOKEN` ni `COROOM_TCP_PLAIN` en un servidor público.
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

- **`udp_reachable: false`**: el UDP 443 no llega. Revisa el cortafuegos, la redirección de puertos
  del router y que Compose publique `443:4433/udp`. Ojo: una NAT **sin hairpin** (el router no
  devuelve a la red local el tráfico a su propia IP pública) da `false` aunque desde fuera
  funcione; compruébalo desde otra red (`curl --http3 https://coroom.example.com/health`).
- **No llega el certificado ACME**: el log dice por qué. Comprueba que el dominio apunta a este
  servidor, que el TCP 443 (o el 80 con HTTP-01) llega al contenedor y que nada delante (un proxy,
  un CDN) intercepta TLS. Mientras tanto se sirve un certificado autofirmado temporal.
- **El relay no conecta desde fuera**: falta `COROOM_RELAY_ADDR=<IP pública>:443`; dentro de
  Docker el servidor no conoce su IP pública ni el puerto publicado.
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
cargo test -p coroom-server     # cuentas, ACME con un directorio falso, señalización, relay
```

Con `COROOM_DEV_TOKEN=change-me` la señalización acepta además ese token sin cuenta (modo
desarrollo); `COROOM_TCP_PLAIN=true` sirve TCP sin TLS.
