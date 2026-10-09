# syntax=docker/dockerfile:1
# Build from the repository root: docker build -f server/Dockerfile .

FROM rust:1-slim-trixie AS build
WORKDIR /src

# Workspace manifests with stub sources first, so dependencies build in their
# own layer and code changes don't recompile them.
COPY Cargo.toml Cargo.lock ./
COPY protocol/Cargo.toml protocol/
COPY server/Cargo.toml server/
COPY engine/Cargo.toml engine/
COPY desktop/Cargo.toml desktop/
COPY ffi/Cargo.toml ffi/
RUN for c in protocol server engine desktop ffi; do mkdir -p $c/src && touch $c/src/lib.rs; done \
    && echo 'fn main() {}' > server/src/main.rs \
    && echo 'fn main() {}' > ffi/uniffi-bindgen.rs \
    && cargo build --release --locked -p savesync-server \
    && rm -rf protocol/src server/src

COPY protocol/src protocol/src
COPY server/src server/src
RUN touch protocol/src/lib.rs server/src/lib.rs server/src/main.rs \
    && cargo build --release --locked -p savesync-server

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/savesync-server /usr/local/bin/savesync-server

ENV SAVESYNC_DATA_DIR=/data \
    SAVESYNC_BIND=0.0.0.0:8420
VOLUME /data
EXPOSE 8420
HEALTHCHECK --interval=60s --timeout=5s --start-period=10s CMD ["savesync-server", "healthcheck"]
ENTRYPOINT ["savesync-server"]
