FROM archlinux:base-devel AS builder

RUN pacman -Syu --noconfirm rustup && rustup default stable

WORKDIR /build

# Copy workspace manifests first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY apps/node/Cargo.toml apps/node/Cargo.toml
COPY apps/cli/Cargo.toml apps/cli/Cargo.toml
COPY libs/types/Cargo.toml libs/types/Cargo.toml
COPY libs/storage/Cargo.toml libs/storage/Cargo.toml

# Create stub source files so cargo can resolve deps
RUN mkdir -p apps/node/src apps/cli/src libs/types/src libs/storage/src && \
    echo "fn main() {}" > apps/node/src/main.rs && \
    echo "fn main() {}" > apps/cli/src/main.rs && \
    echo "" > libs/types/src/lib.rs && \
    echo "" > libs/storage/src/lib.rs

# Cache dependency build
RUN cargo build --release --bin shardd-node 2>/dev/null || true
RUN rm -rf apps libs

# Copy real source
COPY apps/ apps/
COPY libs/ libs/

# Touch to invalidate cache
RUN touch apps/node/src/main.rs apps/cli/src/main.rs libs/types/src/lib.rs libs/storage/src/lib.rs

RUN cargo build --release --bin shardd-node

# ── Runtime ──
FROM archlinux:base

RUN pacman -Syu --noconfirm ca-certificates && pacman -Scc --noconfirm

COPY --from=builder /build/target/release/shardd-node /usr/local/bin/shardd-node

ENTRYPOINT ["shardd-node"]
