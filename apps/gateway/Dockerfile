FROM rust:1.88-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY apps ./apps
COPY libs ./libs
COPY sdks ./sdks

RUN cargo build --release -p shardd-gateway

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/shardd-gateway /usr/local/bin/shardd-gateway

ENTRYPOINT ["shardd-gateway"]
