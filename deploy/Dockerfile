# syntax=docker/dockerfile:1
# Imagen de coroom-server. Se construye desde la raíz del repositorio:
#   docker build -f deploy/Dockerfile -t coroom-server .
#
# Multi-arquitectura (`release.yml`): la etapa de compilación corre siempre en la
# plataforma de quien construye y compila de forma cruzada para la de destino (amd64 o arm64), así
# arm64 no pasa por QEMU:
#   docker buildx build --platform linux/amd64,linux/arm64 -f deploy/Dockerfile .

FROM --platform=$BUILDPLATFORM rust:1-bookworm AS build
ARG TARGETARCH
WORKDIR /src
# La versión de Rust fijada en rust-toolchain.toml (la misma que la CI), con el triple de la
# plataforma de destino y, si es otra arquitectura, su compilador de C (aws-lc-sys y la SQLite de
# sqlx compilan C).
COPY rust-toolchain.toml ./
RUN case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu; packages=x86-64-linux-gnu ;; \
      arm64) triple=aarch64-unknown-linux-gnu; packages=aarch64-linux-gnu ;; \
      *) echo "arquitectura no admitida: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && echo "$triple" > /triple \
    && if [ "$(uname -m)" != "${triple%%-*}" ]; then \
         apt-get update \
         && apt-get install -y --no-install-recommends "gcc-$packages" "g++-$packages" "libc6-dev-${TARGETARCH}-cross" \
         && rm -rf /var/lib/apt/lists/*; \
       fi \
    && channel="$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)" \
    && rustup toolchain install "$channel" --profile minimal --no-self-update \
    && rustup target add "$triple" --toolchain "$channel"
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++ \
    AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar
COPY . .
# Las cachés de BuildKit guardan el registro de crates y target/ (una por arquitectura) entre
# compilaciones; el binario se copia fuera de la caché en el mismo paso.
# Las dos arquitecturas se compilan a la vez: cada una con su registro de crates (si lo comparten,
# las dos desempaquetan las mismas crates a la vez y cargo falla con «File exists»).
RUN --mount=type=cache,id=coroom-registry-${TARGETARCH},target=/usr/local/cargo/registry \
    --mount=type=cache,id=coroom-target-${TARGETARCH},target=/src/target \
    triple="$(cat /triple)" \
    && cargo build --release --locked -p coroom-server --target "$triple" \
    && install -D -m 0755 "target/$triple/release/coroom-server" /out/coroom-server \
    && install -d -m 0700 /out/data

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/coroom-server /usr/local/bin/coroom-server
# El volumen hereda el propietario del directorio de la imagen: el usuario sin privilegios
# (nonroot, uid 65532) puede escribir la base de datos, la cuenta ACME y los certificados.
COPY --from=build --chown=65532:65532 /out/data /data
ENV COROOM_DATA_DIR=/data \
    COROOM_BIND_UDP=[::]:4433 \
    COROOM_BIND_TCP=[::]:8443 \
    RUST_LOG=info
VOLUME ["/data"]
# 4433/udp: HTTP/3, señalización y relay; 8443/tcp: HTTP/2 con TLS y TLS-ALPN-01;
# 8080/tcp: solo con COROOM_ACME_CHALLENGE=http-01.
EXPOSE 4433/udp 8443/tcp 8080/tcp
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/coroom-server"]
