# syntax=docker/dockerfile:1

FROM rust:bookworm AS builder

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends libpq-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --locked -p api

FROM debian:bookworm-slim AS runner

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libpq5 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /usr/sbin/nologin faber

WORKDIR /app

ENV API_PORT=3001

COPY --from=builder /app/target/release/api /usr/local/bin/faber-api

USER faber

EXPOSE 3001

CMD ["faber-api"]
