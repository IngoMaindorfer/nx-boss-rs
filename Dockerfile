FROM caddy:2.11.4-builder-alpine AS caddy-builder

# DNS-01 for private LAN addresses needs the Cloudflare module.
RUN xcaddy build v2.11.4 --with github.com/caddy-dns/cloudflare@a8737d0

FROM caddy:2.11.4-alpine AS caddy
COPY --from=caddy-builder /usr/bin/caddy /usr/bin/caddy
COPY scripts/start-caddy.sh /usr/local/bin/start-caddy.sh

FROM rust:1.99.1-slim-trixie AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    musl-tools \
    g++ \
    make \
    perl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl

COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY resources/defaults.yaml ./resources/defaults.yaml
COPY docs/openapi.json ./docs/openapi.json
COPY templates/ templates/
COPY static/ static/

ARG GIT_SHA=dev
ENV GIT_SHA=$GIT_SHA

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --locked --release --target x86_64-unknown-linux-musl \
    && cp target/x86_64-unknown-linux-musl/release/nx-boss-rs /nx-boss-rs

FROM scratch AS app

COPY --from=builder /nx-boss-rs /nx-boss-rs
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

VOLUME ["/data", "/config"]

ENTRYPOINT ["/nx-boss-rs"]
CMD ["--config", "/config/config.yaml", "--host", "0.0.0.0", "--port", "10447", "--admin-host", "0.0.0.0", "--admin-port", "10448"]
