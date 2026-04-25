FROM rust:slim AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl

COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY defaults.yaml ./
COPY templates/ templates/

RUN cargo build --release --target x86_64-unknown-linux-musl

FROM alpine:3 AS tools
RUN apk add --no-cache wget

FROM scratch

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/nx-boss-rs /nx-boss-rs
COPY --from=tools /usr/bin/wget /wget
# wget needs SSL certs for https; for http-only healthcheck we just need the binary
COPY --from=tools /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

VOLUME ["/data", "/config"]
EXPOSE 10447

ENTRYPOINT ["/nx-boss-rs"]
CMD ["--config", "/config/config.yaml", "--host", "0.0.0.0", "--port", "10447"]
