# Build the two Rust executables once; Compose selects the executable per service.
FROM rust:1.85-bookworm AS builder

WORKDIR /workspace

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --locked --release \
    --package mongo-pg-proxy \
    --package mongo-pg-schema-discovery

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates postgresql-client \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /workspace/target/release/mongo-pg-proxy /usr/local/bin/mongo-pg-proxy
COPY --from=builder /workspace/target/release/mongo-pg-schema-discovery /usr/local/bin/mongo-pg-schema-discovery

USER 65532:65532
