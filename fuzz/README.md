# Fuzzing de coroom

Objetivos de [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) para todo lo que
lee datos de la red (ver `docs/seguridad.md`, §3). Es un workspace aparte, excluido del raíz,
porque `cargo fuzz` necesita nightly y sus propias banderas; en stable solo se comprueba que compila.

```bash
# Que compila (stable, reutiliza target/ del workspace raíz):
cargo check --manifest-path fuzz/Cargo.toml --target-dir target --bins

# Ejecutar un objetivo (nightly; el corpus que crece va a fuzz/corpus, las semillas no se tocan):
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
cargo +nightly fuzz run --target-dir target/fuzz proto_media \
  fuzz/corpus/proto_media fuzz/seeds/proto_media -- -max_total_time=60

# Regenerar las semillas desde los tests:
cargo test -p coroom-session --test fuzz_seeds -- --ignored
```

Un fallo deja la entrada en `fuzz/artifacts/<objetivo>/`; se reproduce con
`cargo +nightly fuzz run --target-dir target/fuzz <objetivo> fuzz/artifacts/<objetivo>/<fichero>`
y se corrige con un test de regresión en la crate afectada. La CI (`.github/workflows/fuzz.yml`)
ejecuta cada objetivo 120 s con AddressSanitizer cada semana y a mano.

## Objetivos y formato de su entrada

`Bytes::chunk` (en `src/lib.rs`) es un trozo con su longitud delante (`u16` big-endian).

| Objetivo | Qué prueba | Entrada |
|---|---|---|
| `proto_media` | `ObjectHeader::parse` y `parse_datagram`; ida y vuelta | El datagrama |
| `proto_audio` | `AudioHeader::parse`; ida y vuelta | El paquete |
| `proto_input` | `InputPacket`, `InputMessage`, datagramas, preámbulo y `StreamFrames` | Byte de modo (`% 4`) y el resto (en modo 3, trozos) |
| `proto_control` | `FrameBuffer` + `ControlMessage`; ida y vuelta | Trozos del stream |
| `proto_signal` | `FrameBuffer::with_max(MAX_SIGNAL_MESSAGE)` + `SignalMessage` | Trozos del stream |
| `proto_api` | Tipos JSON de la API (24 tipos) y hexadecimal | Byte de tipo y el cuerpo |
| `transport_media` | `ReceiverCore`: FEC, `Nack`, plazos; memoria acotada | Órdenes: `op % 4 == 0` avanza `u8` ms y vence plazos; `0xFF` fija el intervalo del vídeo (`u16` × 10 µs); si no, un datagrama (trozo) |
| `transport_loopback` | `Reassembler` del bucle local | Datagramas (trozos) |
| `crypto_sframe` | `FrameDecryptor` con dos épocas y tres emisores instalados | Modo 0: paquetes (trozos); modo 1: hoja, pista, posición a alterar y texto a cifrar |
| `crypto_input` | `InputCipher::open` y la ventana anti-repetición | Modo 0: sellados (trozos); modo 1: canal, salto de secuencia y texto (trozo) por mensaje |
| `crypto_mls` | `RoomGroup::process`, `join` y `add_member` | Byte de modo (`% 4`) y el mensaje |
| `server_api` | `routes::dispatch` del servidor con SQLite temporal (arnés `coroom_server::fuzzing`) | Método, sesión (0 ninguna, 1 administrador, 2 falsa), ruta hasta `0x00`, cuerpo |
| `server_signal` | `Hub::run`: una sesión de señalización entera | Byte (impar: `Auth` de desarrollo delante) y los bytes del stream |
| `codec_slices` | `split_slices` de H.264, HEVC y AV1 | Byte de códec y el paquete |
| `audio_jitter` | `JitterBuffer` con secuencias arbitrarias | Órdenes: `op % 3 == 0` avanza y saca; si no, mete un paquete (secuencia, captura, banderas, avance y trozo) |
