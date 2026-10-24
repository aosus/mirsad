# syntax=docker/dockerfile:1
FROM rust:1.99-bookworm AS builder
WORKDIR /app
# Cache dependencies first for faster rebuilds.
COPY Cargo.toml Cargo.lock* ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release 2>/dev/null || true
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
RUN useradd -r -u 10001 mirsad && mkdir -p /data && chown mirsad /data
COPY --from=builder /app/target/release/mirsad /usr/local/bin/mirsad
USER mirsad
WORKDIR /app
VOLUME ["/data"]
ENV MIRSAD_STORAGE__SQLITE_PATH=/data/mirsad.db \
    MIRSAD_STORAGE__MATRIX_STORE_PATH=/data/matrix-store
ENTRYPOINT ["/usr/local/bin/mirsad"]
CMD ["--config", "/app/config/config.toml"]
