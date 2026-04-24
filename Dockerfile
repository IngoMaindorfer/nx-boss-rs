FROM rust:slim AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl

COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY defaults.yaml ./

RUN cargo build --release --target x86_64-unknown-linux-musl


FROM scratch

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/nx-boss /nx-boss

VOLUME ["/data", "/config"]
EXPOSE 10447

ENTRYPOINT ["/nx-boss"]
CMD ["--config", "/config/config.yaml", "--host", "0.0.0.0", "--port", "10447"]
