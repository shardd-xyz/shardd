FROM archlinux:base-devel AS builder

RUN pacman -Syu --noconfirm rustup curl && rustup default stable
RUN rustup target add wasm32-unknown-unknown

# Install dx from pre-built binary
RUN curl -fsSL https://github.com/DioxusLabs/dioxus/releases/latest/download/dx-x86_64-unknown-linux-gnu.tar.gz \
    | tar -xz -C /usr/local/bin/

WORKDIR /build

# Copy workspace manifests for layer caching
COPY Cargo.toml Cargo.lock ./
COPY apps/node/Cargo.toml apps/node/Cargo.toml
COPY apps/cli/Cargo.toml apps/cli/Cargo.toml
COPY apps/dashboard/Cargo.toml apps/dashboard/Cargo.toml
COPY apps/dashboard/Dioxus.toml apps/dashboard/Dioxus.toml
COPY libs/types/Cargo.toml libs/types/Cargo.toml
COPY libs/storage/Cargo.toml libs/storage/Cargo.toml

# Stubs for dep caching
RUN mkdir -p apps/node/src apps/cli/src apps/dashboard/{src,assets} libs/types/src libs/storage/src && \
    echo "fn main() {}" > apps/node/src/main.rs && \
    echo "fn main() {}" > apps/cli/src/main.rs && \
    echo "fn main() {}" > apps/dashboard/src/main.rs && \
    echo "" > libs/types/src/lib.rs && \
    echo "" > libs/storage/src/lib.rs && \
    touch apps/dashboard/assets/main.css

RUN cd apps/dashboard && dx build --release 2>/dev/null || true
RUN rm -rf apps libs

# Copy real source
COPY apps/ apps/
COPY libs/ libs/

RUN touch apps/dashboard/src/main.rs libs/types/src/lib.rs libs/storage/src/lib.rs
RUN cd apps/dashboard && dx build --release

# Inject Tailwind CDN into generated index.html
RUN sed -i 's|</head>|<script src="https://cdn.tailwindcss.com"></script>\n<script>tailwind.config={darkMode:"class",theme:{extend:{fontFamily:{mono:["JetBrains Mono","Fira Code","SF Mono","monospace"]}}}}</script>\n</head>|' \
    /build/target/dx/shardd-dashboard/release/web/public/index.html
# Add dark class to html element
RUN sed -i 's|<html>|<html class="dark">|' \
    /build/target/dx/shardd-dashboard/release/web/public/index.html

# Runtime: static files via caddy
FROM caddy:2-alpine

COPY --from=builder /build/target/dx/shardd-dashboard/release/web/public /srv

COPY <<'CADDYFILE' /etc/caddy/Caddyfile
:80 {
    root * /srv
    file_server
    try_files {path} /index.html
}
CADDYFILE

EXPOSE 80
