# syntax=docker/dockerfile:1

FROM rust:1.98.1-slim AS base

WORKDIR /app

RUN apt-get update && apt-get install -y \
    pkg-config \
    build-essential \
    ca-certificates \
    cmake \
    mold \
    clang \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /app/.cargo && \
    printf '[target.x86_64-unknown-linux-gnu]\nlinker = "clang"\nrustflags = ["-C", "link-arg=-fuse-ld=mold"]\n\n[target.aarch64-unknown-linux-gnu]\nlinker = "clang"\nrustflags = ["-C", "link-arg=-fuse-ld=mold"]\n' > /app/.cargo/config.toml

FROM base AS development

RUN cargo install cargo-watch

EXPOSE 5757

ENV RUST_LOG=info

CMD ["cargo", "watch", "--watch", "src", "--watch", "Cargo.toml", "--ignore", "target", "-x", "run"]

FROM base AS build

COPY Cargo.toml Cargo.lock ./

RUN mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/duckxy* target/release/deps/duckxy*

COPY src/ ./src/
COPY favicon.ico ./

RUN cargo build --release

FROM debian:13.7-slim AS final

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /app/target/release/duckxy /usr/local/bin/duckxy

RUN useradd \
    --system \
    --no-create-home \
    --shell /sbin/nologin \
    --uid 10001 \
    duckxy

USER duckxy

EXPOSE 5757

ENV RUST_LOG=info \
    DUCKXY_BIND=0.0.0.0:5757

CMD ["duckxy"]
