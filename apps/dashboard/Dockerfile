FROM rust:1.90-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends             pkg-config libssl-dev ca-certificates             && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY apps ./apps
COPY libs ./libs
COPY sdks ./sdks

ENV SQLX_OFFLINE=true
RUN cargo build --release -p shardd-dashboard

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends             ca-certificates libssl3             && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/shardd-dashboard /usr/local/bin/shardd-dashboard

ENV RUST_LOG=info
EXPOSE 3001

CMD ["shardd-dashboard"]
